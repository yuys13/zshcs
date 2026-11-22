use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::{RwLock, mpsc, oneshot};
use tokio::time::timeout;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::completion::{
    CAPTURE_ZSH, CompletionRequest, ManCache, ZPTYRC_ZSH, extract_local_symbols_with_manager,
    merge_local_completions, resolve_completion_item_async, run_completion_daemon,
};
use crate::config::Config;
use crate::definition::find_definition;
use crate::diagnostics::check_syntax;
use crate::document::DocumentManager;
use crate::error::{ZshcsError, ZshcsResult};
use crate::hover::{extract_word_at_position, get_hover_info_cached};
use crate::symbols::extract_document_symbols;

#[derive(Debug)]
pub struct Backend {
    client: Client,
    document_manager: DocumentManager,
    _temp_dir: TempDir,
    completion_tx: mpsc::Sender<CompletionRequest>,
    config: Arc<RwLock<Config>>,
    man_cache: Arc<ManCache>,
    diagnostic_tasks: Arc<dashmap::DashMap<Url, (u64, tokio::task::AbortHandle)>>,
    diagnostic_task_seq: Arc<std::sync::atomic::AtomicU64>,
}

impl Backend {
    /// Creates a new `Backend` instance synchronously using default embedded scripts.
    pub fn new(client: Client) -> ZshcsResult<Self> {
        Self::new_with_scripts(client, CAPTURE_ZSH, ZPTYRC_ZSH)
    }

    /// Creates a new `Backend` instance synchronously with custom capture and zptyrc scripts.
    pub fn new_with_scripts(
        client: Client,
        capture_script: &str,
        zptyrc_script: &str,
    ) -> ZshcsResult<Self> {
        Self::new_with_scripts_and_cache(client, capture_script, zptyrc_script, None)
    }

    /// Creates a new `Backend` instance synchronously with custom scripts and an optional cache directory.
    pub fn new_with_scripts_and_cache(
        client: Client,
        capture_script: &str,
        zptyrc_script: &str,
        cache_dir: Option<PathBuf>,
    ) -> ZshcsResult<Self> {
        let temp_dir = tempfile::tempdir()?;
        let capture_path = temp_dir.path().join("capture.zsh");
        let zptyrc_path = temp_dir.path().join("zptyrc.zsh");

        std::fs::write(&capture_path, capture_script)?;
        std::fs::write(&zptyrc_path, zptyrc_script)?;

        let (tx, rx) = mpsc::channel(32);

        let client_clone = client.clone();
        tokio::spawn(run_completion_daemon(
            capture_path,
            cache_dir,
            rx,
            client_clone,
        ));

        Ok(Backend {
            client,
            document_manager: DocumentManager::new(),
            _temp_dir: temp_dir,
            completion_tx: tx,
            config: Arc::new(RwLock::new(Config::default())),
            man_cache: Arc::new(dashmap::DashMap::new()),
            diagnostic_tasks: Arc::new(dashmap::DashMap::new()),
            diagnostic_task_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        })
    }

    /// Creates a new `Backend` instance asynchronously using default embedded scripts and non-blocking I/O.
    pub async fn new_async(client: Client) -> ZshcsResult<Self> {
        Self::new_with_scripts_async(client, CAPTURE_ZSH, ZPTYRC_ZSH).await
    }

    /// Creates a new `Backend` instance asynchronously with custom scripts and non-blocking async I/O.
    pub async fn new_with_scripts_async(
        client: Client,
        capture_script: &str,
        zptyrc_script: &str,
    ) -> ZshcsResult<Self> {
        Self::new_with_scripts_and_cache_async(client, capture_script, zptyrc_script, None).await
    }

    /// Creates a new `Backend` instance asynchronously with custom scripts, optional cache directory, and non-blocking async I/O.
    pub async fn new_with_scripts_and_cache_async(
        client: Client,
        capture_script: &str,
        zptyrc_script: &str,
        cache_dir: Option<PathBuf>,
    ) -> ZshcsResult<Self> {
        let temp_dir = tempfile::tempdir()?;
        let capture_path = temp_dir.path().join("capture.zsh");
        let zptyrc_path = temp_dir.path().join("zptyrc.zsh");

        tokio::fs::write(&capture_path, capture_script.as_bytes()).await?;
        tokio::fs::write(&zptyrc_path, zptyrc_script.as_bytes()).await?;

        let (tx, rx) = mpsc::channel(32);

        let client_clone = client.clone();
        tokio::spawn(run_completion_daemon(
            capture_path,
            cache_dir,
            rx,
            client_clone,
        ));

        Ok(Backend {
            client,
            document_manager: DocumentManager::new(),
            _temp_dir: temp_dir,
            completion_tx: tx,
            config: Arc::new(RwLock::new(Config::default())),
            man_cache: Arc::new(dashmap::DashMap::new()),
            diagnostic_tasks: Arc::new(dashmap::DashMap::new()),
            diagnostic_task_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        })
    }

    pub fn document_manager(&self) -> &DocumentManager {
        &self.document_manager
    }

    pub fn config(&self) -> Arc<RwLock<Config>> {
        Arc::clone(&self.config)
    }

    pub fn man_cache(&self) -> Arc<ManCache> {
        Arc::clone(&self.man_cache)
    }

    pub async fn is_diagnostics_enabled(&self) -> bool {
        self.config.read().await.experimental_diagnostics()
    }

    pub async fn is_hover_enabled(&self) -> bool {
        self.config.read().await.experimental_hover()
    }

    pub async fn is_definition_enabled(&self) -> bool {
        self.config.read().await.experimental_definition()
    }

    pub async fn is_symbols_enabled(&self) -> bool {
        self.config.read().await.experimental_symbols()
    }

    pub fn active_diagnostic_task_count(&self) -> usize {
        self.diagnostic_tasks.len()
    }

    fn cancel_diagnostic_task(&self, uri: &Url) {
        if let Some((_, (_, handle))) = self.diagnostic_tasks.remove(uri) {
            handle.abort();
        }
    }

    fn cancel_all_diagnostic_tasks(&self) {
        self.diagnostic_tasks.retain(|_, (_, handle)| {
            handle.abort();
            false
        });
    }

    fn spawn_diagnostics_task(&self, uri: Url, version: i32, debounce: Option<Duration>) {
        let task_id = self
            .diagnostic_task_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let client = self.client.clone();
        let uri_clone = uri.clone();
        let doc_mgr = self.document_manager.clone();
        let config = Arc::clone(&self.config);
        let tasks = Arc::clone(&self.diagnostic_tasks);

        let join_handle = tokio::spawn(async move {
            if let Some(delay) = debounce {
                tokio::time::sleep(delay).await;
            }

            let text = match doc_mgr.get(&uri_clone) {
                Some(doc)
                    if doc.version() == version
                        && config.read().await.experimental_diagnostics() =>
                {
                    doc.text().to_string()
                }
                _ => {
                    tasks.remove_if(&uri_clone, |_, (id, _)| *id == task_id);
                    return;
                }
            };

            let diagnostics = check_syntax(&text).await;
            tasks.remove_if(&uri_clone, |_, (id, _)| *id == task_id);

            if let Some(doc_after) = doc_mgr.get(&uri_clone)
                && doc_after.version() == version
                && config.read().await.experimental_diagnostics()
            {
                client
                    .publish_diagnostics(uri_clone, diagnostics, Some(version))
                    .await;
            }
        });

        if let Some((_, prev_handle)) = self
            .diagnostic_tasks
            .insert(uri, (task_id, join_handle.abort_handle()))
        {
            prev_handle.abort();
        }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        tracing::info!("LSP initialize request received");
        tracing::debug!(
            process_id = ?params.process_id,
            root_uri = ?params.root_uri,
            capabilities = ?params.capabilities,
            "Client initialization parameters"
        );

        if let Some(options) = &params.initialization_options {
            let initial_config = Config::from_value(Some(options));
            tracing::info!(
                experimental_diagnostics = initial_config.experimental_diagnostics(),
                experimental_hover = initial_config.experimental_hover(),
                experimental_definition = initial_config.experimental_definition(),
                experimental_symbols = initial_config.experimental_symbols(),
                "Parsed initial server configuration"
            );
            *self.config.write().await = initial_config;
        }

        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "zshcs-language-server".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL, // Support Incremental sync
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(true),
                    trigger_characters: Some(vec![
                        "-".to_string(),
                        "$".to_string(),
                        "/".to_string(),
                        "~".to_string(),
                        ".".to_string(),
                        " ".to_string(),
                    ]),
                    work_done_progress_options: Default::default(),
                    all_commit_characters: None,
                    ..Default::default()
                }),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec!["zshcs/getDocumentContent".to_string()],
                    ..Default::default()
                }),
                ..ServerCapabilities::default()
            },
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            "LSP server initialized successfully"
        );
        self.client
            .log_message(MessageType::INFO, "server initialized!")
            .await;
        self.client
            .log_message(
                MessageType::INFO,
                format!("Server version: {}", env!("CARGO_PKG_VERSION")),
            )
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        tracing::info!("LSP server shutting down");
        self.cancel_all_diagnostic_tasks();
        let (tx, rx) = oneshot::channel();
        if self
            .completion_tx
            .send(CompletionRequest::Shutdown { responder: tx })
            .await
            .is_ok()
        {
            let _ = timeout(Duration::from_millis(1000), rx).await;
        }
        Ok(())
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        tracing::info!("workspace/didChangeConfiguration received");
        let new_config = Config::from_value(Some(&params.settings));
        let was_enabled;
        let is_enabled = new_config.experimental_diagnostics();
        {
            let mut conf = self.config.write().await;
            was_enabled = conf.experimental_diagnostics();
            *conf = new_config;
        }

        tracing::info!(
            was_enabled,
            is_enabled,
            "Updated server configuration from didChangeConfiguration"
        );

        if was_enabled && !is_enabled {
            // Cancel any pending diagnostics and clear all open documents
            self.cancel_all_diagnostic_tasks();
            for item in self.document_manager.iter() {
                let uri = item.key().clone();
                let version = item.value().version();
                self.client
                    .publish_diagnostics(uri, vec![], Some(version))
                    .await;
            }
        } else if !was_enabled && is_enabled {
            // Trigger diagnostics for all open documents
            for item in self.document_manager.iter() {
                let uri = item.key().clone();
                let version = item.value().version();
                self.spawn_diagnostics_task(uri, version, None);
            }
        }
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        let version = params.text_document.version;

        tracing::info!(uri = %uri, version, "textDocument/didOpen");
        tracing::trace!(text_len = text.len(), "Document content opened");

        self.document_manager
            .open(uri.clone(), version, text.clone());
        self.client
            .log_message(MessageType::INFO, format!("textDocument/didOpen: {uri}"))
            .await;

        if self.is_diagnostics_enabled().await {
            self.spawn_diagnostics_task(uri, version, None);
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;
        let changes_count = params.content_changes.len();

        tracing::debug!(uri = %uri, version, changes_count, "textDocument/didChange");

        if let Err(e) = self
            .document_manager
            .apply_changes(&uri, version, params.content_changes)
        {
            let err: ZshcsError = e.into();
            tracing::warn!(
                uri = %uri,
                error = %err,
                "Failed to apply incremental change"
            );
            self.client
                .log_message(
                    MessageType::WARNING,
                    format!("Failed to apply incremental change: {err} for document {uri}"),
                )
                .await;
        }

        self.client
            .log_message(MessageType::INFO, format!("textDocument/didChange: {uri}"))
            .await;

        if self.is_diagnostics_enabled().await {
            self.spawn_diagnostics_task(uri, version, Some(Duration::from_millis(150)));
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        tracing::info!(uri = %uri, "textDocument/didSave");

        if self.is_diagnostics_enabled().await
            && let Some(doc) = self.document_manager.get(&uri)
        {
            let version = doc.version();
            drop(doc);
            self.spawn_diagnostics_task(uri, version, None);
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.cancel_diagnostic_task(&uri);
        if self.document_manager.close(&uri).is_some() {
            tracing::info!(uri = %uri, "textDocument/didClose");
            self.client
                .log_message(MessageType::INFO, format!("textDocument/didClose: {uri}"))
                .await;
            if self.is_diagnostics_enabled().await {
                self.client.publish_diagnostics(uri, vec![], None).await;
            }
        } else {
            tracing::warn!(uri = %uri, "textDocument/didClose: document not found");
            self.client
                .log_message(
                    MessageType::WARNING,
                    format!("textDocument/didClose: document not found {uri}"),
                )
                .await;
        }
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        tracing::debug!(
            uri = %uri,
            line = position.line,
            character = position.character,
            "Completion request received"
        );

        let prefix = match self.document_manager.get_line_prefix(&uri, position) {
            Some(p) => p,
            None => {
                tracing::debug!(uri = %uri, "No line prefix found at position; returning None");
                return Ok(None);
            }
        };

        let cwd = uri
            .to_file_path()
            .ok()
            .and_then(|p| p.parent().map(|parent| parent.to_path_buf()));

        tracing::trace!(prefix = %prefix, ?cwd, "Dispatching completion request to daemon");

        // Request completion from the daemon
        let (tx, rx) = oneshot::channel();
        let req = CompletionRequest::Complete {
            prefix: prefix.clone(),
            cwd,
            responder: tx,
        };

        if let Err(e) = self.completion_tx.send(req).await {
            let err = ZshcsError::DaemonChannel(e.to_string());
            tracing::error!(error = %err, "Failed to send request to completion daemon");
            self.client
                .log_message(MessageType::ERROR, format!("{err}"))
                .await;
            return Ok(None);
        }

        let output_result = timeout(Duration::from_millis(6000), rx).await;

        match output_result {
            Ok(Ok(Ok(items))) => {
                tracing::debug!(
                    count = items.len(),
                    "Completion items retrieved successfully"
                );
                let local_symbols = if let Some(doc) =
                    self.document_manager.get(&uri).map(|d| d.clone())
                {
                    extract_local_symbols_with_manager(&doc, position, Some(&self.document_manager))
                } else {
                    Vec::new()
                };
                let merged = merge_local_completions(items, &local_symbols, &prefix);
                Ok(Some(CompletionResponse::Array(merged)))
            }
            Ok(Ok(Err(e))) => {
                tracing::error!(error = %e, "Completion daemon returned error");
                self.client
                    .log_message(MessageType::ERROR, format!("Daemon returned error: {e}"))
                    .await;
                Ok(None)
            }
            Ok(Err(e)) => {
                let err = ZshcsError::RequestCancelled(e);
                tracing::warn!(error = %err, "Completion request cancelled or responder dropped");
                self.client
                    .log_message(MessageType::ERROR, format!("{err}"))
                    .await;
                Ok(None)
            }
            Err(e) => {
                let err = ZshcsError::Timeout(e);
                tracing::error!(error = %err, "Completion request timed out");
                self.client
                    .log_message(MessageType::ERROR, format!("{err}"))
                    .await;
                Ok(None)
            }
        }
    }

    async fn completion_resolve(&self, params: CompletionItem) -> Result<CompletionItem> {
        tracing::debug!(
            label = %params.label,
            ?params.kind,
            "completionItem/resolve request received"
        );
        let resolved = resolve_completion_item_async(params, &self.man_cache).await;
        Ok(resolved)
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        if !self.is_hover_enabled().await {
            tracing::debug!("Hover requested but experimental hover is disabled");
            return Ok(None);
        }

        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        tracing::debug!(
            uri = %uri,
            line = position.line,
            character = position.character,
            "Hover request received"
        );

        let doc_text = match self.document_manager.get_content(&uri) {
            Some(t) => t,
            None => {
                tracing::debug!(uri = %uri, "Document not found in document_manager for hover");
                return Ok(None);
            }
        };

        let (word, range) = match extract_word_at_position(&doc_text, position) {
            Some((w, r)) => (w, r),
            None => {
                tracing::debug!(
                    uri = %uri,
                    line = position.line,
                    character = position.character,
                    "No word found at position for hover"
                );
                return Ok(None);
            }
        };

        tracing::debug!(
            word,
            ?range,
            "Extracted word for hover; querying hover info"
        );

        match get_hover_info_cached(word, &self.man_cache).await {
            Some(contents) => Ok(Some(Hover {
                contents,
                range: Some(range),
            })),
            None => {
                tracing::debug!(word, "No hover documentation found for word");
                Ok(None)
            }
        }
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        if !self.is_definition_enabled().await {
            tracing::debug!("Goto definition requested but experimental definition is disabled");
            return Ok(None);
        }

        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        tracing::debug!(
            uri = %uri,
            line = position.line,
            character = position.character,
            "Goto definition request received"
        );

        let doc_text = match self.document_manager.get_content(&uri) {
            Some(t) => t,
            None => {
                tracing::debug!(
                    uri = %uri,
                    "Document not found in document_manager for goto_definition"
                );
                return Ok(None);
            }
        };

        let response = find_definition(&doc_text, &uri, position);
        tracing::debug!(
            uri = %uri,
            found = response.is_some(),
            "Goto definition result resolved"
        );
        Ok(response)
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        if !self.is_symbols_enabled().await {
            tracing::debug!("Document symbol requested but experimental symbols is disabled");
            return Ok(None);
        }

        let uri = params.text_document.uri;
        tracing::debug!(uri = %uri, "Document symbol request received");

        let doc_text = match self.document_manager.get_content(&uri) {
            Some(t) => t,
            None => {
                tracing::debug!(
                    uri = %uri,
                    "Document not found in document_manager for document_symbol"
                );
                return Ok(None);
            }
        };

        let symbols = extract_document_symbols(&doc_text, &uri);
        tracing::debug!(
            uri = %uri,
            count = symbols.len(),
            "Document symbols extracted successfully"
        );
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn execute_command(&self, params: ExecuteCommandParams) -> Result<Option<Value>> {
        tracing::info!(command = %params.command, "execute_command invoked");
        tracing::debug!(args_count = params.arguments.len(), "Command arguments");

        if params.command == "zshcs/getDocumentContent"
            && let Some(uri) = params
                .arguments
                .first()
                .and_then(|v| serde_json::from_value::<Url>(v.clone()).ok())
        {
            let content = self.document_manager.get_content(&uri);
            let value = serde_json::to_value(content)
                .map_err(|e| tower_lsp::jsonrpc::Error::invalid_params(e.to_string()))?;
            return Ok(Some(value));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_lsp::LspService;

    #[tokio::test]
    async fn test_backend_new_sync_success() {
        let (_service, _socket) = LspService::new(|client| {
            let backend = Backend::new(client);
            assert!(backend.is_ok());
            backend.unwrap()
        });
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_backend_new_async_success() {
        let (service, _socket) = LspService::new(|client| {
            // Test that new_async can be called and succeeds
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let backend = Backend::new_async(client).await;
                    assert!(backend.is_ok());
                    backend.unwrap()
                })
            })
        });

        let init_params = InitializeParams::default();
        let res = service.inner().initialize(init_params).await;
        assert!(res.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_backend_new_with_scripts_async() {
        let (service, _socket) = LspService::new(|client| {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let backend = Backend::new_with_scripts_async(
                        client,
                        "#!/bin/zsh\nexit 0\n",
                        "# zptyrc test\n",
                    )
                    .await;
                    assert!(backend.is_ok());
                    backend.unwrap()
                })
            })
        });

        assert!(service.inner().document_manager().is_empty());
    }

    #[tokio::test]
    async fn test_backend_new_with_scripts_sync() {
        let (service, _socket) = LspService::new(|client| {
            let backend =
                Backend::new_with_scripts(client, "#!/bin/zsh\nexit 0\n", "# zptyrc test\n");
            assert!(backend.is_ok());
            backend.unwrap()
        });

        assert!(service.inner().document_manager().is_empty());
    }

    #[tokio::test]
    async fn test_backend_document_manager_getter() {
        let (service, _socket) = LspService::new(|client| Backend::new(client).unwrap());
        let mgr = service.inner().document_manager();
        assert_eq!(mgr.len(), 0);
    }

    #[tokio::test]
    async fn test_backend_completion_unopened_doc_returns_none() {
        let (service, _socket) = LspService::new(|client| Backend::new(client).unwrap());
        let uri = Url::parse("file:///unopened.zsh").unwrap();
        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position::new(0, 0),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };
        let res = service.inner().completion(params).await;
        assert!(res.is_ok());
        assert!(res.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_backend_execute_command_unknown() {
        let (service, _socket) = LspService::new(|client| Backend::new(client).unwrap());
        let params = ExecuteCommandParams {
            command: "unknown/command".to_string(),
            arguments: vec![],
            work_done_progress_params: Default::default(),
        };
        let res = service.inner().execute_command(params).await;
        assert!(res.is_ok());
        assert!(res.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_backend_completion_resolve() {
        let (service, _socket) = LspService::new(|client| Backend::new(client).unwrap());
        let item = CompletionItem {
            label: "cd".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        };
        let res = service.inner().completion_resolve(item).await;
        assert!(res.is_ok());
        let resolved = res.unwrap();
        assert!(resolved.documentation.is_some());
    }

    #[tokio::test]
    async fn test_backend_completion_resolve_external_command_caching() {
        let (service, _socket) = LspService::new(|client| Backend::new(client).unwrap());
        let backend = service.inner();

        // 1. Resolve external command git
        let git_item = CompletionItem {
            label: "git".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        };
        let res = backend.completion_resolve(git_item.clone()).await;
        assert!(res.is_ok());
        let resolved_git = res.unwrap();
        match resolved_git.documentation {
            Some(Documentation::MarkupContent(ref markup)) => {
                assert_eq!(markup.kind, MarkupKind::Markdown);
                assert!(markup.value.to_lowercase().contains("git"));
            }
            other => panic!("Expected MarkupContent for git, got {other:?}"),
        }

        // Cache must contain git
        assert!(backend.man_cache().contains_key("git"));
        assert!(backend.man_cache().get("git").unwrap().is_some());

        // 2. Second resolve should hit cache
        let res_cached = backend.completion_resolve(git_item).await;
        assert!(res_cached.is_ok());
        assert_eq!(
            res_cached.unwrap().documentation,
            resolved_git.documentation
        );

        // 3. Resolve nonexistent external command (negative caching)
        let dummy = "nonexistent_backend_cmd_xyz123";
        let dummy_item = CompletionItem {
            label: dummy.to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        };
        let res_dummy = backend.completion_resolve(dummy_item.clone()).await;
        assert!(res_dummy.is_ok());
        assert_eq!(res_dummy.unwrap().documentation, None);

        // Negative cache must contain None
        assert!(backend.man_cache().contains_key(dummy));
        assert_eq!(*backend.man_cache().get(dummy).unwrap(), None);

        // 4. Second resolve of nonexistent command returns None from negative cache
        let res_dummy_cached = backend.completion_resolve(dummy_item).await;
        assert!(res_dummy_cached.is_ok());
        assert_eq!(res_dummy_cached.unwrap().documentation, None);
    }

    #[tokio::test]
    async fn test_backend_concurrent_completion_resolve_shared_cache() {
        let (service, _socket) = LspService::new(|client| Backend::new(client).unwrap());
        let backend = service.inner();

        // Warm up cache once to avoid spawning 20 concurrent man processes on constrained CI runners
        let init_item = CompletionItem {
            label: "git".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        };
        let init_res = backend.completion_resolve(init_item).await.unwrap();
        assert!(init_res.documentation.is_some());

        let futures: Vec<_> = (0..20)
            .map(|_| {
                let item = CompletionItem {
                    label: "git".to_string(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    ..Default::default()
                };
                backend.completion_resolve(item)
            })
            .collect();

        let results = futures::future::join_all(futures).await;

        for res in results {
            assert!(res.is_ok());
            let resolved = res.unwrap();
            match resolved.documentation {
                Some(Documentation::MarkupContent(markup)) => {
                    assert_eq!(markup.kind, MarkupKind::Markdown);
                    assert!(markup.value.to_lowercase().contains("git"));
                }
                other => panic!("Expected MarkupContent in concurrent test, got {other:?}"),
            }
        }

        // Shared cache must contain git
        assert!(backend.man_cache().contains_key("git"));
        assert!(backend.man_cache().get("git").unwrap().is_some());
    }

    #[tokio::test]
    async fn test_backend_hover_shares_man_cache_with_completion_resolve() {
        let (service, _socket) = LspService::new(|client| Backend::new(client).unwrap());
        let backend = service.inner();

        let init_params = InitializeParams {
            initialization_options: Some(serde_json::json!({
                "zshcs": {
                    "experimental": {
                        "hover": true
                    }
                }
            })),
            ..Default::default()
        };
        backend.initialize(init_params).await.unwrap();

        let uri = Url::parse("file:///hover_cache_share.zsh").unwrap();
        backend
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "zsh".to_string(),
                    version: 1,
                    text: "git status\n".to_string(),
                },
            })
            .await;

        assert!(!backend.man_cache().contains_key("git"));

        // 1. Hover on `git` should populate Backend's shared man_cache
        let hover_res = backend
            .hover(HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position::new(0, 1),
                },
                work_done_progress_params: Default::default(),
            })
            .await
            .unwrap();
        assert!(hover_res.is_some());
        assert!(backend.man_cache().contains_key("git"));
        let cached_md = backend
            .man_cache()
            .get("git")
            .unwrap()
            .value()
            .clone()
            .expect("Expected cached markdown for git");

        // 2. Subsequent completionItem/resolve for `git` should reuse the exact cached markdown
        let resolved = backend
            .completion_resolve(CompletionItem {
                label: "git".to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                ..Default::default()
            })
            .await
            .unwrap();
        match resolved.documentation {
            Some(Documentation::MarkupContent(markup)) => {
                assert_eq!(markup.value, cached_md);
            }
            other => panic!("Expected MarkupContent from shared cache, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_backend_diagnostics_task_cancelled_on_superseding_change_and_close() {
        let (service, mut socket) = LspService::new(|client| Backend::new(client).unwrap());
        tokio::spawn(async move {
            use futures::StreamExt;
            while socket.next().await.is_some() {}
        });
        let backend = service.inner();

        backend
            .initialize(InitializeParams {
                initialization_options: Some(serde_json::json!({
                    "zshcs": {
                        "experimental": {
                            "diagnostics": true
                        }
                    }
                })),
                ..Default::default()
            })
            .await
            .unwrap();

        let uri = Url::parse("file:///diag_cancel.zsh").unwrap();
        backend
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "zsh".to_string(),
                    version: 1,
                    text: "echo ok\n".to_string(),
                },
            })
            .await;

        backend
            .did_change(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier::new(uri.clone(), 2),
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "echo v2\n".to_string(),
                }],
            })
            .await;
        assert_eq!(backend.active_diagnostic_task_count(), 1);

        // Superseding didChange should replace/abort the previous task handle, keeping count at 1
        backend
            .did_change(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier::new(uri.clone(), 3),
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "echo v3\n".to_string(),
                }],
            })
            .await;
        assert_eq!(backend.active_diagnostic_task_count(), 1);

        // Closing the document should immediately cancel the pending diagnostic task
        backend
            .did_close(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri },
            })
            .await;
        assert_eq!(backend.active_diagnostic_task_count(), 0);
    }

    #[tokio::test]
    async fn test_backend_shutdown_cancels_diagnostics_and_stops_daemon() {
        let (service, mut socket) = LspService::new(|client| Backend::new(client).unwrap());
        tokio::spawn(async move {
            use futures::StreamExt;
            while socket.next().await.is_some() {}
        });
        let backend = service.inner();

        backend
            .initialize(InitializeParams {
                initialization_options: Some(serde_json::json!({
                    "zshcs": {
                        "experimental": {
                            "diagnostics": true
                        }
                    }
                })),
                ..Default::default()
            })
            .await
            .unwrap();

        let uri = Url::parse("file:///shutdown_cleanup.zsh").unwrap();
        backend
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "zsh".to_string(),
                    version: 1,
                    text: "echo ".to_string(),
                },
            })
            .await;

        backend
            .did_change(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier::new(uri.clone(), 2),
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "echo hi".to_string(),
                }],
            })
            .await;
        assert_eq!(backend.active_diagnostic_task_count(), 1);

        backend.shutdown().await.unwrap();
        assert_eq!(backend.active_diagnostic_task_count(), 0);

        // Completion daemon loop should have exited cleanly, so completion returns Ok(None)
        let res = backend
            .completion(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position::new(0, 4),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            })
            .await;
        assert!(res.is_ok());
        assert!(res.unwrap().is_none());
    }
}

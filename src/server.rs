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

use crate::completion::{CAPTURE_ZSH, CompletionRequest, ZPTYRC_ZSH, run_completion_daemon};
use crate::config::Config;
use crate::diagnostics::check_syntax;
use crate::document::DocumentManager;
use crate::error::{ZshcsError, ZshcsResult};
use crate::hover::{extract_word_at_position, get_hover_info};

#[derive(Debug)]
pub struct Backend {
    client: Client,
    document_manager: DocumentManager,
    _temp_dir: TempDir,
    completion_tx: mpsc::Sender<CompletionRequest>,
    config: Arc<RwLock<Config>>,
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
        })
    }

    pub fn document_manager(&self) -> &DocumentManager {
        &self.document_manager
    }

    pub fn config(&self) -> Arc<RwLock<Config>> {
        Arc::clone(&self.config)
    }

    pub async fn is_diagnostics_enabled(&self) -> bool {
        self.config.read().await.experimental_diagnostics()
    }

    pub async fn is_hover_enabled(&self) -> bool {
        self.config.read().await.experimental_hover()
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
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(false),
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
            // Clear all diagnostics for all open documents
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
                let text = item.value().text().to_string();
                let client = self.client.clone();
                let doc_mgr = self.document_manager.clone();
                let config = Arc::clone(&self.config);
                tokio::spawn(async move {
                    let diagnostics = check_syntax(&text).await;
                    if let Some(doc) = doc_mgr.get(&uri)
                        && doc.version() == version
                        && config.read().await.experimental_diagnostics()
                    {
                        client
                            .publish_diagnostics(uri, diagnostics, Some(version))
                            .await;
                    }
                });
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
            let client = self.client.clone();
            let uri_clone = uri.clone();
            let doc_mgr = self.document_manager.clone();
            let config = Arc::clone(&self.config);
            tokio::spawn(async move {
                let diagnostics = check_syntax(&text).await;
                if let Some(doc) = doc_mgr.get(&uri_clone)
                    && doc.version() == version
                    && config.read().await.experimental_diagnostics()
                {
                    client
                        .publish_diagnostics(uri_clone, diagnostics, Some(version))
                        .await;
                }
            });
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
            let client = self.client.clone();
            let uri_clone = uri.clone();
            let doc_mgr = self.document_manager.clone();
            let config = Arc::clone(&self.config);
            tokio::spawn(async move {
                // Debounce delay to coalesce rapid keystrokes
                tokio::time::sleep(Duration::from_millis(150)).await;
                if let Some(doc) = doc_mgr.get(&uri_clone)
                    && doc.version() == version
                    && config.read().await.experimental_diagnostics()
                {
                    let text = doc.text().to_string();
                    drop(doc);
                    let diagnostics = check_syntax(&text).await;
                    if let Some(doc_after) = doc_mgr.get(&uri_clone)
                        && doc_after.version() == version
                        && config.read().await.experimental_diagnostics()
                    {
                        client
                            .publish_diagnostics(uri_clone, diagnostics, Some(version))
                            .await;
                    }
                }
            });
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        tracing::info!(uri = %uri, "textDocument/didSave");

        if self.is_diagnostics_enabled().await
            && let Some(doc) = self.document_manager.get(&uri)
        {
            let version = doc.version();
            let text = doc.text().to_string();
            drop(doc);
            let client = self.client.clone();
            let uri_clone = uri.clone();
            let doc_mgr = self.document_manager.clone();
            let config = Arc::clone(&self.config);
            tokio::spawn(async move {
                let diagnostics = check_syntax(&text).await;
                if let Some(doc_after) = doc_mgr.get(&uri_clone)
                    && doc_after.version() == version
                    && config.read().await.experimental_diagnostics()
                {
                    client
                        .publish_diagnostics(uri_clone, diagnostics, Some(version))
                        .await;
                }
            });
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
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
        let req = CompletionRequest {
            prefix,
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
                Ok(Some(CompletionResponse::Array(items)))
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

        match get_hover_info(word).await {
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
}

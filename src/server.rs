use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::completion::{CAPTURE_ZSH, CompletionRequest, ZPTYRC_ZSH, run_completion_daemon};
use crate::document::DocumentManager;
use crate::error::{ZshcsError, ZshcsResult};

#[derive(Debug)]
pub struct Backend {
    client: Client,
    document_manager: DocumentManager,
    _temp_dir: TempDir,
    completion_tx: mpsc::Sender<CompletionRequest>,
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
        let temp_dir = tempfile::tempdir()?;
        let capture_path = temp_dir.path().join("capture.zsh");
        let zptyrc_path = temp_dir.path().join("zptyrc.zsh");

        std::fs::write(&capture_path, capture_script)?;
        std::fs::write(&zptyrc_path, zptyrc_script)?;

        let (tx, rx) = mpsc::channel(32);

        let client_clone = client.clone();
        tokio::spawn(run_completion_daemon(capture_path, rx, client_clone));

        Ok(Backend {
            client,
            document_manager: DocumentManager::new(),
            _temp_dir: temp_dir,
            completion_tx: tx,
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
        let temp_dir = tempfile::tempdir()?;
        let capture_path = temp_dir.path().join("capture.zsh");
        let zptyrc_path = temp_dir.path().join("zptyrc.zsh");

        tokio::fs::write(&capture_path, capture_script.as_bytes()).await?;
        tokio::fs::write(&zptyrc_path, zptyrc_script.as_bytes()).await?;

        let (tx, rx) = mpsc::channel(32);

        let client_clone = client.clone();
        tokio::spawn(run_completion_daemon(capture_path, rx, client_clone));

        Ok(Backend {
            client,
            document_manager: DocumentManager::new(),
            _temp_dir: temp_dir,
            completion_tx: tx,
        })
    }

    pub fn document_manager(&self) -> &DocumentManager {
        &self.document_manager
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "zshcs-language-server".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL, // Support Incremental sync
                )),
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(false),
                    trigger_characters: None,
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
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        let version = params.text_document.version;
        self.document_manager.open(uri.clone(), version, text);
        self.client
            .log_message(MessageType::INFO, format!("textDocument/didOpen: {uri}"))
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;

        if let Err(e) = self
            .document_manager
            .apply_changes(&uri, version, params.content_changes)
        {
            let err: ZshcsError = e.into();
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
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        if self.document_manager.close(&uri).is_some() {
            self.client
                .log_message(MessageType::INFO, format!("textDocument/didClose: {uri}"))
                .await;
        } else {
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

        let prefix = match self.document_manager.get_line_prefix(&uri, position) {
            Some(p) => p,
            None => return Ok(None),
        };

        let cwd = uri
            .to_file_path()
            .ok()
            .and_then(|p| p.parent().map(|parent| parent.to_path_buf()));

        // Request completion from the daemon
        let (tx, rx) = oneshot::channel();
        let req = CompletionRequest {
            prefix,
            cwd,
            responder: tx,
        };

        if let Err(e) = self.completion_tx.send(req).await {
            let err = ZshcsError::DaemonChannel(e.to_string());
            self.client
                .log_message(MessageType::ERROR, format!("{err}"))
                .await;
            return Ok(None);
        }

        let output_result = timeout(Duration::from_millis(3000), rx).await;

        match output_result {
            Ok(Ok(Ok(items))) => Ok(Some(CompletionResponse::Array(items))),
            Ok(Ok(Err(e))) => {
                self.client
                    .log_message(MessageType::ERROR, format!("Daemon returned error: {e}"))
                    .await;
                Ok(None)
            }
            Ok(Err(e)) => {
                let err = ZshcsError::RequestCancelled(e);
                self.client
                    .log_message(MessageType::ERROR, format!("{err}"))
                    .await;
                Ok(None)
            }
            Err(e) => {
                let err = ZshcsError::Timeout(e);
                self.client
                    .log_message(MessageType::ERROR, format!("{err}"))
                    .await;
                Ok(None)
            }
        }
    }

    async fn execute_command(&self, params: ExecuteCommandParams) -> Result<Option<Value>> {
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

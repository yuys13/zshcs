use std::io::Write;
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

#[derive(Debug)]
pub struct Backend {
    client: Client,
    document_manager: DocumentManager,
    _temp_dir: TempDir,
    completion_tx: mpsc::Sender<CompletionRequest>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self::new_with_scripts(client, CAPTURE_ZSH, ZPTYRC_ZSH)
    }

    pub fn new_with_scripts(client: Client, capture_script: &str, zptyrc_script: &str) -> Self {
        let temp_dir = tempfile::tempdir().expect("Failed to create temp dir for zpty scripts");
        let capture_path = temp_dir.path().join("capture.zsh");
        let zptyrc_path = temp_dir.path().join("zptyrc.zsh");

        let mut capture_file = std::fs::File::create(&capture_path).unwrap();
        write!(capture_file, "{}", capture_script).unwrap();
        drop(capture_file); // flush and close

        let mut zptyrc_file = std::fs::File::create(&zptyrc_path).unwrap();
        write!(zptyrc_file, "{}", zptyrc_script).unwrap();
        drop(zptyrc_file); // flush and close

        let (tx, rx) = mpsc::channel(32);

        let client_clone = client.clone();
        tokio::spawn(run_completion_daemon(capture_path, rx, client_clone));

        Backend {
            client,
            document_manager: DocumentManager::new(),
            _temp_dir: temp_dir,
            completion_tx: tx,
        }
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
            self.client
                .log_message(
                    MessageType::WARNING,
                    format!("Failed to apply incremental change: {e} for document {uri}"),
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

        // Request completion from the daemon
        let (tx, rx) = oneshot::channel();
        let req = CompletionRequest {
            prefix,
            responder: tx,
        };

        if self.completion_tx.send(req).await.is_err() {
            self.client
                .log_message(
                    MessageType::ERROR,
                    "Failed to send request to completion daemon",
                )
                .await;
            return Ok(None);
        }

        let output_result = timeout(Duration::from_millis(3000), rx).await;

        match output_result {
            Ok(Ok(Ok(items))) => Ok(Some(CompletionResponse::Array(items))),
            Ok(Ok(Err(e))) => {
                self.client
                    .log_message(MessageType::ERROR, format!("Daemon returned error: {}", e))
                    .await;
                Ok(None)
            }
            Ok(Err(_)) => {
                self.client
                    .log_message(MessageType::ERROR, "Completion daemon responder dropped")
                    .await;
                Ok(None)
            }
            Err(_) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        "completion daemon timed out".to_string(),
                    )
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
            return Ok(Some(serde_json::to_value(content).unwrap()));
        }
        Ok(None)
    }
}

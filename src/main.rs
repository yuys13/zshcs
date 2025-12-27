use dashmap::DashMap;
use serde_json::Value;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

use std::sync::Arc;
use tempfile::NamedTempFile;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

const CAPTURE_ZSH: &str = include_str!("../bin/capture.zsh");
const ZSH_SCRIPT_PERMISSIONS: u32 = 0o755;

#[derive(Debug)]
struct Backend {
    client: Client,
    document_map: Arc<DashMap<Url, String>>,
    document_versions: Arc<DashMap<Url, i32>>,
    _temp_path: tempfile::TempPath,
}

impl Backend {
    fn new(client: Client) -> Self {
        // Create temp file for capture.zsh once on startup
        let temp_path =
            Self::create_capture_script().expect("Failed to create and prepare capture.zsh script");

        Backend {
            client,
            document_map: Arc::new(DashMap::new()),
            document_versions: Arc::new(DashMap::new()),
            _temp_path: temp_path,
        }
    }

    fn create_capture_script() -> std::io::Result<tempfile::TempPath> {
        let mut temp_file = NamedTempFile::new()?;
        write!(temp_file, "{}", CAPTURE_ZSH)?;
        temp_file.flush()?;

        // Make executable
        let mut perms = temp_file.as_file().metadata()?.permissions();
        perms.set_mode(ZSH_SCRIPT_PERMISSIONS);
        temp_file.as_file().set_permissions(perms)?;

        // Return the temp path. It will be deleted when the TempPath is dropped (RAII).
        Ok(temp_file.into_temp_path())
    }

    fn position_to_byte_offset(text: &str, position: Position) -> Option<usize> {
        let mut line_offset = 0;
        for _ in 0..position.line {
            line_offset = text[line_offset..].find('\n')? + line_offset + 1;
        }

        let line_text = &text[line_offset..].lines().next().unwrap_or("");
        let utf16_offset = position.character as usize;
        let mut byte_offset = 0;
        let mut utf16_count = 0;

        for (i, c) in line_text.char_indices() {
            if utf16_count >= utf16_offset {
                byte_offset = i;
                break;
            }
            utf16_count += c.len_utf16();
            if utf16_count >= utf16_offset {
                byte_offset = i + c.len_utf8();
                break;
            }
        }
        if utf16_count < utf16_offset {
            byte_offset = line_text.len();
        }

        Some(line_offset + byte_offset)
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
        self.document_map.insert(uri.clone(), text);
        self.document_versions.insert(uri.clone(), version);
        self.client
            .log_message(MessageType::INFO, format!("textDocument/didOpen: {uri}"))
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;

        for change in params.content_changes {
            if let Some(range) = change.range {
                // Incremental update
                let res = {
                    if let Some(mut doc) = self.document_map.get_mut(&uri) {
                        if let Some(start_offset) = Self::position_to_byte_offset(&doc, range.start)
                            && let Some(end_offset) = Self::position_to_byte_offset(&doc, range.end)
                            && start_offset <= end_offset
                        {
                            doc.replace_range(start_offset..end_offset, &change.text);
                            Ok(())
                        } else {
                            Err(format!("invalid range {range:?}"))
                        }
                    } else {
                        Err("document not found".to_string())
                    }
                };

                if let Err(e) = res {
                    self.client
                        .log_message(
                            MessageType::WARNING,
                            format!("Failed to apply incremental change: {e} for document {uri}"),
                        )
                        .await;
                }
            } else {
                // Full sync (range is None)
                self.document_map.insert(uri.clone(), change.text);
            }
        }
        self.document_versions.insert(uri.clone(), version);
        self.client
            .log_message(MessageType::INFO, format!("textDocument/didChange: {uri}"))
            .await;
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        let prefix = {
            let doc = self.document_map.get(&uri);
            if doc.is_none() {
                return Ok(None);
            }
            let text = doc.unwrap();

            let offset = Self::position_to_byte_offset(&text, position);
            if offset.is_none() {
                return Ok(None);
            }
            let offset = offset.unwrap();

            // Find start of line
            let line_start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
            text[line_start..offset].to_string()
        };

        // Run capture.zsh
        // Use tokio::try_join! or separate tasks if reading stderr/stdout simultaneously is needed for large outputs,
        // but for simple cases verify if Output capture is enough.
        // Using tokio::time::timeout to prevent hanging.
        use tokio::time::{Duration, timeout};
        let command_future = tokio::process::Command::new(&self._temp_path)
            .arg(prefix)
            .kill_on_drop(true)
            .output();

        let output_result = timeout(Duration::from_millis(3000), command_future).await;

        match output_result {
            Ok(Ok(output)) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    self.client
                        .log_message(
                            MessageType::ERROR,
                            format!("capture.zsh failed: {}", stderr),
                        )
                        .await;
                    return Ok(None);
                }
                let stdout = String::from_utf8_lossy(&output.stdout);
                let mut items = Vec::new();
                for line in stdout.lines() {
                    // Parse line: "candidate -- description" or "candidate"
                    let parts: Vec<&str> = line.splitn(2, " -- ").collect();
                    let label = parts[0].to_string();
                    let detail = if parts.len() > 1 {
                        Some(parts[1].to_string())
                    } else {
                        None
                    };

                    items.push(CompletionItem {
                        label: label.clone(),
                        kind: Some(CompletionItemKind::TEXT),
                        insert_text: Some(label),
                        detail,
                        ..Default::default()
                    });
                }
                Ok(Some(CompletionResponse::Array(items)))
            }
            Ok(Err(e)) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!("Failed to execute capture.zsh: {}", e),
                    )
                    .await;
                Ok(None)
            }
            Err(_) => {
                self.client
                    .log_message(MessageType::ERROR, "capture.zsh timed out".to_string())
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
            let content = self.document_map.get(&uri).map(|doc| doc.value().clone());
            return Ok(Some(serde_json::to_value(content).unwrap()));
        }
        Ok(None)
    }
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    // use futures::future::FutureExt; // Commented out because it was unused
    use serde::{Serialize, de::DeserializeOwned};
    use serde_json::Value;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
    use tower_lsp::jsonrpc::{
        Id,
        Response as JsonRpcResponse,
        // Notification as JsonRpcNotification, // Removed due to persistent import issues
        // Error as JsonRpcError // Marked as unused for now
    };
    use tower_lsp::lsp_types::{
        ClientCapabilities, DidChangeTextDocumentParams, DidOpenTextDocumentParams,
        InitializeParams, InitializeResult, InitializedParams, LogMessageParams, MessageType,
        TextDocumentContentChangeEvent, TextDocumentItem, TextDocumentSyncKind, Url,
        VersionedTextDocumentIdentifier,
        notification::{
            DidChangeTextDocument, DidOpenTextDocument, Initialized, LogMessage,
            Notification as LspNotificationTrait,
        },
        request::{Initialize, Request as LspRequestTrait},
    }; // Keep for JsonRpcResponse parts // For sharing DashMap across async tasks if needed, though Client is Clone

    async fn read_message(stream: &mut DuplexStream) -> Option<String> {
        // let mut len_buf = [0u8; 20]; // Unused variable
        let mut content_length = 0;

        // Read headers
        let mut header_buf = Vec::new();
        loop {
            let byte = stream.read_u8().await.ok()?;
            header_buf.push(byte);
            if header_buf.ends_with(b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&header_buf);
                for line in headers.lines() {
                    if let Some(stripped_line) = line.strip_prefix("Content-Length: ") {
                        content_length = stripped_line.trim().parse().ok()?;
                    }
                }
                break;
            }
            if header_buf.len() > 2048 {
                // Prevent infinite loop on malformed headers
                return None;
            }
        }

        if content_length == 0 {
            return None;
        }

        let mut content_buf = vec![0u8; content_length];
        stream.read_exact(&mut content_buf).await.ok()?;
        String::from_utf8(content_buf).ok()
    }

    async fn write_message(stream: &mut DuplexStream, message: &str) -> std::io::Result<()> {
        let message_len = message.len();
        let header = format!("Content-Length: {message_len}\r\n\r\n");
        stream.write_all(header.as_bytes()).await?;
        stream.write_all(message.as_bytes()).await?;
        stream.flush().await?;
        Ok(())
    }

    struct TestClient<'a> {
        stream: &'a mut DuplexStream, // Borrow the stream
        request_id_counter: i64,
    }

    impl<'a> TestClient<'a> {
        fn new(stream: &'a mut DuplexStream) -> Self {
            TestClient {
                stream,
                request_id_counter: 0,
            }
        }

        fn next_request_id(&mut self) -> i64 {
            self.request_id_counter += 1;
            self.request_id_counter
        }

        async fn send_request<R: LspRequestTrait>(&mut self, params: R::Params) -> Result<R::Result>
        where
            R::Params: Serialize,
            R::Result: DeserializeOwned,
        {
            let id = self.next_request_id();
            let params_value = serde_json::to_value(params).unwrap();
            let mut request_value = serde_json::json!({
                "jsonrpc": "2.0",
                "method": R::METHOD,
                "id": id
            });
            if !params_value.is_null() {
                request_value
                    .as_object_mut()
                    .unwrap()
                    .insert("params".to_string(), params_value);
            }

            let request_json = serde_json::to_string(&request_value).unwrap();
            write_message(self.stream, &request_json).await.unwrap();

            loop {
                let response_json = read_message(self.stream).await.unwrap();
                if response_json.contains("\"method\"") && !response_json.contains("\"id\"") {
                    eprintln!("Skipping notification: {response_json}");
                    continue;
                }
                let response: JsonRpcResponse = serde_json::from_str(&response_json).unwrap();
                let (response_id_val, result_val): (
                    Id,
                    std::result::Result<Value, tower_lsp::jsonrpc::Error>,
                ) = response.into_parts(); // Correctly expect Result directly

                let response_id_matches = match &response_id_val {
                    // Borrow response_id_val here
                    Id::Number(response_id_num) => response_id_num == &id, // Compare with borrowed id
                    Id::String(response_id_s) => response_id_s == &id.to_string(), // Compare with borrowed id.to_string()
                    Id::Null => false,
                };

                if response_id_matches {
                    match result_val {
                        // Now using result_val directly
                        Ok(value) => {
                            return serde_json::from_value(value).map_err(|e| {
                                let mut error = tower_lsp::jsonrpc::Error::parse_error();
                                error.message = format!("Failed to deserialize response: {e}").into();
                                // Storing original error message or part of it in `data`
                                error.data = Some(serde_json::json!({ "deserialization_error_details": e.to_string() }));
                                error
                            });
                        }
                        Err(err) => {
                            // This 'err' is from response.into_parts() if the response indicates a JSON-RPC error
                            // (e.g. method not found, invalid params on server side by spec).
                            return Err(err);
                        }
                    }
                } else {
                    eprintln!(
                        "Received response with unexpected ID: {response_id_val:?}, expected: {id}"
                    );
                    continue;
                }
            }
        }

        async fn send_notification<N: LspNotificationTrait>(&mut self, params: N::Params)
        where
            N::Params: Serialize,
        {
            let params_value = serde_json::to_value(params).unwrap();
            let notification_value = serde_json::json!({
                "jsonrpc": "2.0",
                "method": N::METHOD,
                "params": params_value
            });
            let notification_json = serde_json::to_string(&notification_value).unwrap();
            write_message(self.stream, &notification_json)
                .await
                .unwrap();
        }

        async fn read_notification<N: LspNotificationTrait>(&mut self) -> Option<N::Params>
        where
            N::Params: DeserializeOwned,
        {
            loop {
                let message_json = read_message(self.stream).await?;
                if let Ok(value) = serde_json::from_str::<Value>(&message_json)
                    && value.get("method").and_then(Value::as_str) == Some(N::METHOD)
                {
                    if let Some(params_value) = value.get("params") {
                        return serde_json::from_value(params_value.clone()).ok();
                    }
                    // If params are expected but not present (e.g. for notifications that must have params)
                    // or if the notification has no params and N::Params is some ZST like `()`,
                    // this logic might need adjustment. For now, assume params are present if method matches.
                    // If N::Params can be deserialized from `null` or missing params, it will work.
                    // Otherwise, if params are required, and not present, from_value will fail and return None.
                    // The "initialized" notification is special as its `params` field can be either `null` or entirely absent.
                    // When `params` is absent, `value.get("params")` is `None`.
                    // When `params` is `null`, `value.get("params").unwrap().is_null()` is `true`.
                    // In both cases, if `N::Params` can be deserialized from `Value::Null` (e.g., if `N::Params` is `()`),
                    // we attempt to do so. This handles the flexibility of the "initialized" notification's parameters.
                    if N::METHOD == "initialized"
                        && (value.get("params").is_none() || value.get("params").unwrap().is_null())
                    {
                        return serde_json::from_value(Value::Null).ok();
                    }
                    // If params are present but not deserializable to N::Params, or if method doesn't match,
                    // or if it's "initialized" but params are present and not null but still not deserializable,
                    // this will lead to returning None or continuing the loop.
                    // For notifications other than "initialized" with specific param requirements,
                    // if params are missing or null, serde_json::from_value(params_value.clone()).ok()
                    // would typically result in None if N::Params cannot be deserialized from null/missing.
                    return None; // Params not found, not the expected structure, or deserialization failed
                }
            }
        }

        async fn init_and_open(&mut self, file_uri: &Url, initial_text: &str) {
            // Initialize
            let initialize_params = InitializeParams::default();
            self.send_request::<Initialize>(initialize_params)
                .await
                .unwrap();
            self.send_notification::<Initialized>(InitializedParams {})
                .await;
            self.read_notification::<LogMessage>().await; // init
            self.read_notification::<LogMessage>().await; // version

            // Open document
            self.send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: file_uri.clone(),
                    language_id: "zsh".to_string(),
                    version: 1,
                    text: initial_text.to_string(),
                },
            })
            .await;
            self.read_notification::<LogMessage>().await; // didOpen
        }
    }

    fn setup_server() -> (DuplexStream, tokio::task::JoinHandle<()>) {
        let (client_stream, server_stream) = tokio::io::duplex(4096);
        let (service, client_socket) = LspService::new(Backend::new);

        let server_handle = tokio::spawn(async move {
            let (server_read, server_write) = tokio::io::split(server_stream);
            Server::new(server_read, server_write, client_socket)
                .serve(service)
                .await;
        });

        (client_stream, server_handle)
    }

    #[tokio::test]
    async fn test_initialize() {
        let (mut client_stream, _server_handle) = setup_server();
        let mut test_client = TestClient::new(&mut client_stream);

        let initialize_params = InitializeParams {
            process_id: Some(123),
            root_uri: None,
            capabilities: ClientCapabilities::default(),
            ..Default::default()
        };

        let result = test_client
            .send_request::<Initialize>(initialize_params)
            .await
            .unwrap();

        assert_eq!(
            result.server_info.as_ref().unwrap().name,
            "zshcs-language-server"
        );
        assert_eq!(
            result.capabilities.text_document_sync,
            Some(TextDocumentSyncCapability::Kind(
                TextDocumentSyncKind::INCREMENTAL
            ))
        );
        assert!(
            result.capabilities.completion_provider.is_some(),
            "Server should support completion"
        );
    }

    #[tokio::test]
    async fn test_initialized() {
        let (mut client_stream, _server_handle) = setup_server();
        let mut test_client = TestClient::new(&mut client_stream);

        // Send initialize request first
        let initialize_params = InitializeParams {
            process_id: Some(123),
            root_uri: None,
            capabilities: ClientCapabilities::default(),
            ..Default::default()
        };
        let _init_result: InitializeResult = test_client
            .send_request::<Initialize>(initialize_params)
            .await
            .unwrap();

        // Send initialized notification
        let initialized_params = InitializedParams {};
        test_client
            .send_notification::<Initialized>(initialized_params)
            .await;

        // Check for log message from server
        // The TestClient needs to be adapted to read notifications, or we need a separate reader
        // For simplicity, we'll assume the server processes it and might log.
        // A more robust test would listen for the logMessage notification.
        // We will try to read the log message from the server.
        // let log_message_params: Option<LogMessageParams> =
        //     test_client.read_notification::<LogMessage>().await; // This line is redundant and unused.

        let log_message_params1: Option<LogMessageParams> =
            test_client.read_notification::<LogMessage>().await;
        assert!(
            log_message_params1.is_some(),
            "Did not receive the first log message after initialized"
        );
        let log_message1 = log_message_params1.unwrap();
        assert_eq!(log_message1.typ, MessageType::INFO);

        let log_message_params2: Option<LogMessageParams> =
            test_client.read_notification::<LogMessage>().await;
        assert!(
            log_message_params2.is_some(),
            "Did not receive the second log message after initialized"
        );
        let log_message2 = log_message_params2.unwrap();
        assert_eq!(log_message2.typ, MessageType::INFO);

        // Determine which message is which, as the order is not guaranteed
        let (initialized_msg, version_msg) = if log_message1.message.contains("server initialized!")
        {
            (log_message1, log_message2)
        } else {
            (log_message2, log_message1)
        };

        assert!(
            initialized_msg.message.contains("server initialized!"),
            "Expected 'server initialized!' log message, got: {}",
            initialized_msg.message
        );
        assert!(
            version_msg.message.contains("Server version:"),
            "Expected 'Server version:' log message, got: {}",
            version_msg.message
        );
        assert!(
            version_msg.message.contains(env!("CARGO_PKG_VERSION")),
            "Server version message does not contain the correct version. Got: {}",
            version_msg.message
        );
    }

    #[tokio::test]
    async fn test_did_open() {
        let (mut client_stream, _server_handle) = setup_server();
        let mut test_client = TestClient::new(&mut client_stream);

        // Initialize first
        let initialize_params = InitializeParams {
            capabilities: ClientCapabilities::default(),
            ..Default::default()
        };
        test_client
            .send_request::<Initialize>(initialize_params)
            .await
            .unwrap();
        test_client
            .send_notification::<Initialized>(InitializedParams {})
            .await;
        // Consume log messages from initialized
        let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;
        let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

        // Send didOpen notification
        let doc_uri = Url::parse("file:///test.zsh").unwrap();
        let did_open_params = DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "echo hello".to_string(),
            },
        };
        test_client
            .send_notification::<DidOpenTextDocument>(did_open_params)
            .await;

        // At this point, the server should have stored the document.
        // We can't directly inspect the server's Backend state here.
        // A more comprehensive test might involve a custom server method for tests
        // or verifying behavior that depends on the document being open (e.g., completion).
        // For now, we assume if no panic/error, the notification was processed.
        // We can add a log message in the server's did_open to confirm it was called.
        let log_message: Option<LogMessageParams> =
            test_client.read_notification::<LogMessage>().await;
        assert!(
            log_message.is_some(),
            "No log message received after didOpen"
        );
        assert_eq!(log_message.as_ref().unwrap().typ, MessageType::INFO);
        assert!(
            log_message
                .unwrap()
                .message
                .contains("textDocument/didOpen: file:///test.zsh")
        );
    }

    #[tokio::test]
    async fn test_did_change_full_sync() {
        let (mut client_stream, _server_handle) = setup_server();
        let mut test_client = TestClient::new(&mut client_stream);

        let doc_uri = Url::parse("file:///test_change.zsh").unwrap();
        test_client.init_and_open(&doc_uri, "initial content").await;

        // Send didChange notification (full sync)
        let did_change_params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: doc_uri.clone(),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,        // Range is optional for full sync
                range_length: None, // Optional
                text: "new full content".to_string(),
            }],
        };
        test_client
            .send_notification::<DidChangeTextDocument>(did_change_params)
            .await;

        // Verify server logged the change
        let log_message: Option<LogMessageParams> =
            test_client.read_notification::<LogMessage>().await;
        assert!(
            log_message.is_some(),
            "No log message received after didChange"
        );
        assert_eq!(log_message.as_ref().unwrap().typ, MessageType::INFO);
        assert!(
            log_message
                .unwrap()
                .message
                .contains("textDocument/didChange: file:///test_change.zsh")
        );

        // Future: Add a way to query server for document content to verify it's updated
        // For now, logging confirms the method was called and processed the URI.
    }

    #[tokio::test]
    async fn test_did_change_incremental_sync() {
        let (mut client_stream, _server_handle) = setup_server();
        let mut test_client = TestClient::new(&mut client_stream);

        let doc_uri = Url::parse("file:///test_incremental.zsh").unwrap();
        let initial_text = "line1\nline2\nline3".to_string();
        test_client.init_and_open(&doc_uri, &initial_text).await;

        // 1. Send first incremental change: replace "line2" with "new line2"
        let first_change_params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: doc_uri.clone(),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range {
                    start: Position {
                        line: 1,
                        character: 0,
                    },
                    end: Position {
                        line: 1,
                        character: 5,
                    }, // "line2"
                }),
                text: "new line2".to_string(),
                range_length: None,
            }],
        };
        test_client
            .send_notification::<DidChangeTextDocument>(first_change_params)
            .await;
        let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

        // 2. Send second incremental change: insert " more" at the end of the new line 2
        let second_change_params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: doc_uri.clone(),
                version: 3,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range {
                    start: Position {
                        line: 1,
                        character: 9,
                    }, // end of "new line2"
                    end: Position {
                        line: 1,
                        character: 9,
                    },
                }),
                text: " more".to_string(),
                range_length: None,
            }],
        };
        test_client
            .send_notification::<DidChangeTextDocument>(second_change_params)
            .await;
        let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

        // Verify the final content of the document using the custom command
        let params = ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::to_value(&doc_uri).unwrap()],
            ..Default::default()
        };
        let result = test_client
            .send_request::<request::ExecuteCommand>(params)
            .await
            .unwrap();
        let content: Option<String> = serde_json::from_value(result.unwrap()).unwrap();

        let expected_text = "line1\nnew line2 more\nline3".to_string();
        assert_eq!(
            content,
            Some(expected_text),
            "Incremental changes were not applied correctly."
        );
    }

    #[tokio::test]
    async fn test_completion() {
        let (mut client_stream, _server_handle) = setup_server();
        let mut test_client = TestClient::new(&mut client_stream);

        let doc_uri = Url::parse("file:///test.zsh").unwrap();
        test_client.init_and_open(&doc_uri, "git s").await;

        let completion_params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(0, 5), // After "git s"
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };

        let response = test_client
            .send_request::<tower_lsp::lsp_types::request::Completion>(completion_params)
            .await
            .unwrap();

        // Assertions
        let response = response.expect("Expected completion response");
        match response {
            CompletionResponse::Array(items) => {
                assert!(!items.is_empty());
                // Check if "status" is in items
                let has_status = items.iter().any(|item| item.label == "status");
                assert!(has_status, "Expected 'status' in completion items");
            }
            CompletionResponse::List(list) => {
                assert!(!list.items.is_empty());
                let has_status = list.items.iter().any(|item| item.label == "status");
                assert!(has_status, "Expected 'status' in completion items");
            }
        }
    }

    #[tokio::test]
    async fn test_did_change_ordering() {
        let (mut client_stream, _server_handle) = setup_server();
        let mut test_client = TestClient::new(&mut client_stream);

        let doc_uri = Url::parse("file:///test_ordering.zsh").unwrap();
        test_client.init_and_open(&doc_uri, "").await;

        // Send two insertions in a single didChange
        // [Insert "A" at 0, Insert "B" at 0]
        // Step 1: Insert "A" at 0. Doc becomes "A".
        // Step 2: Insert "B" at 0. Doc becomes "BA".
        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: doc_uri.clone(),
                version: 2,
            },
            content_changes: vec![
                TextDocumentContentChangeEvent {
                    range: Some(Range::new(Position::new(0, 0), Position::new(0, 0))),
                    range_length: None,
                    text: "A".to_string(),
                },
                TextDocumentContentChangeEvent {
                    range: Some(Range::new(Position::new(0, 0), Position::new(0, 0))),
                    range_length: None,
                    text: "B".to_string(),
                },
            ],
        };
        test_client
            .send_notification::<DidChangeTextDocument>(params)
            .await;
        test_client.read_notification::<LogMessage>().await; // didChange

        // Verify content
        let res = test_client
            .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
                command: "zshcs/getDocumentContent".to_string(),
                arguments: vec![serde_json::to_value(&doc_uri).unwrap()],
                ..Default::default()
            })
            .await
            .unwrap();
        let content: Option<String> = serde_json::from_value(res.unwrap()).unwrap();
        assert_eq!(content, Some("BA".to_string()));
    }

    #[tokio::test]
    async fn test_did_change_mixed() {
        let (mut client_stream, _server_handle) = setup_server();
        let mut test_client = TestClient::new(&mut client_stream);

        let doc_uri = Url::parse("file:///test_mixed.zsh").unwrap();
        test_client.init_and_open(&doc_uri, "Old").await;

        // Mixed changes: Full replace followed by incremental append
        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: doc_uri.clone(),
                version: 2,
            },
            content_changes: vec![
                TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "New".to_string(),
                },
                TextDocumentContentChangeEvent {
                    range: Some(Range::new(Position::new(0, 3), Position::new(0, 3))),
                    range_length: None,
                    text: "!".to_string(),
                },
            ],
        };
        test_client
            .send_notification::<DidChangeTextDocument>(params)
            .await;
        test_client.read_notification::<LogMessage>().await;

        let res = test_client
            .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
                command: "zshcs/getDocumentContent".to_string(),
                arguments: vec![serde_json::to_value(&doc_uri).unwrap()],
                ..Default::default()
            })
            .await
            .unwrap();
        let content: Option<String> = serde_json::from_value(res.unwrap()).unwrap();
        assert_eq!(content, Some("New!".to_string()));
    }

    #[tokio::test]
    async fn test_position_to_byte_offset_utf8_utf16() {
        let text = "あa😊b";
        // "あ" is 3 bytes, 1 utf16 code unit
        // "a" is 1 byte, 1 utf16 code unit
        // "😊" is 4 bytes, 2 utf16 code units (surrogate pair)
        // "b" is 1 byte, 1 utf16 code unit

        // End of "あ": char 1 -> byte 3
        assert_eq!(
            Backend::position_to_byte_offset(text, Position::new(0, 1)),
            Some(3)
        );
        // End of "a": char 2 -> byte 4
        assert_eq!(
            Backend::position_to_byte_offset(text, Position::new(0, 2)),
            Some(4)
        );
        // Middle of "😊" (first surrogate): char 3 -> byte 8 (end of emoji)
        // Current implementation returns end of char if offset falls within it.
        assert_eq!(
            Backend::position_to_byte_offset(text, Position::new(0, 3)),
            Some(8)
        );
        // End of "😊": char 4 -> byte 8
        assert_eq!(
            Backend::position_to_byte_offset(text, Position::new(0, 4)),
            Some(8)
        );
        // End of "b": char 5 -> byte 9
        assert_eq!(
            Backend::position_to_byte_offset(text, Position::new(0, 5)),
            Some(9)
        );
    }

    #[tokio::test]
    async fn test_position_to_byte_offset_edge_cases() {
        let text = "line1\nline2";
        // Normal case
        assert_eq!(
            Backend::position_to_byte_offset(text, Position::new(0, 5)),
            Some(5)
        );
        // Next line
        assert_eq!(
            Backend::position_to_byte_offset(text, Position::new(1, 0)),
            Some(6)
        );
        // Non-existent line
        assert_eq!(
            Backend::position_to_byte_offset(text, Position::new(2, 0)),
            None
        );
        // Position beyond line length (should return end of line)
        assert_eq!(
            Backend::position_to_byte_offset(text, Position::new(0, 100)),
            Some(5)
        );
        // Empty text
        assert_eq!(
            Backend::position_to_byte_offset("", Position::new(0, 0)),
            Some(0)
        );
        assert_eq!(
            Backend::position_to_byte_offset("", Position::new(0, 1)),
            Some(0)
        );
    }

    #[tokio::test]
    async fn test_execute_command_edge_cases() {
        let (mut client_stream, _server_handle) = setup_server();
        let mut test_client = TestClient::new(&mut client_stream);

        // Server requires initialization
        let initialize_params = InitializeParams::default();
        test_client
            .send_request::<request::Initialize>(initialize_params)
            .await
            .unwrap();
        test_client
            .send_notification::<Initialized>(InitializedParams {})
            .await;

        // Unknown command
        let params = ExecuteCommandParams {
            command: "unknown".to_string(),
            ..Default::default()
        };
        let res = test_client
            .send_request::<request::ExecuteCommand>(params)
            .await
            .unwrap();
        assert!(res.is_none());

        // Incorrect arguments
        let params = ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::json!(123)], // Not a URL
            ..Default::default()
        };
        let res = test_client
            .send_request::<request::ExecuteCommand>(params)
            .await
            .unwrap();
        assert!(res.is_none());

        // Document not found
        let params = ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::json!("file:///notfound.zsh")],
            ..Default::default()
        };
        let res = test_client
            .send_request::<request::ExecuteCommand>(params)
            .await
            .unwrap();
        // The command returns Option<String>, so result should be Some(Value::Null) or similar
        let content: Option<String> = serde_json::from_value(res.unwrap_or(Value::Null)).unwrap();
        assert!(content.is_none());
    }

    #[tokio::test]
    async fn test_did_change_invalid_range() {
        let (mut client_stream, _server_handle) = setup_server();
        let mut test_client = TestClient::new(&mut client_stream);

        let doc_uri = Url::parse("file:///invalid_range.zsh").unwrap();
        test_client.init_and_open(&doc_uri, "original").await;

        // start > end range
        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 2),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(0, 5), Position::new(0, 2))),
                range_length: None,
                text: "X".to_string(),
            }],
        };
        test_client
            .send_notification::<DidChangeTextDocument>(params)
            .await;

        // Should receive a warning log message instead of crashing
        let log = test_client.read_notification::<LogMessage>().await.unwrap();
        assert_eq!(log.typ, MessageType::WARNING);
        assert!(log.message.contains("invalid range"));
    }

    #[tokio::test]
    async fn test_did_change_document_not_found() {
        let (mut client_stream, _server_handle) = setup_server();
        let mut test_client = TestClient::new(&mut client_stream);

        let doc_uri = Url::parse("file:///not_opened.zsh").unwrap();

        // Server requires initialization
        let initialize_params = InitializeParams::default();
        test_client
            .send_request::<request::Initialize>(initialize_params)
            .await
            .unwrap();
        test_client
            .send_notification::<Initialized>(InitializedParams {})
            .await;

        // Skip initialization log messages
        test_client.read_notification::<LogMessage>().await.unwrap(); // "server initialized!"
        test_client.read_notification::<LogMessage>().await.unwrap(); // "Server version: ..."

        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 1),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(0, 0), Position::new(0, 0))),
                range_length: None,
                text: "X".to_string(),
            }],
        };
        test_client
            .send_notification::<DidChangeTextDocument>(params)
            .await;

        let log = test_client.read_notification::<LogMessage>().await.unwrap();
        assert_eq!(log.typ, MessageType::WARNING);
        assert!(log.message.contains("document not found"));
    }

    #[tokio::test]
    async fn test_shutdown() {
        let (mut client_stream, _server_handle) = setup_server();
        let mut test_client = TestClient::new(&mut client_stream);

        // Server requires initialization
        let initialize_params = InitializeParams::default();
        test_client
            .send_request::<request::Initialize>(initialize_params)
            .await
            .unwrap();
        test_client
            .send_notification::<Initialized>(InitializedParams {})
            .await;

        // Shutdown expects no params, so we must use a request that doesn't send null if N::Params is ()
        // tower_lsp's request::Shutdown has Params = ()
        test_client
            .send_request::<request::Shutdown>(())
            .await
            .unwrap();
        // Success means it responded with Ok(()) which is null in JSON-RPC
    }

    #[test]
    fn test_create_capture_script() {
        let temp_path = Backend::create_capture_script().expect("Failed to create script");
        assert!(temp_path.exists());
        let metadata = std::fs::metadata(&temp_path).unwrap();
        assert!(metadata.is_file());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(
                metadata.permissions().mode() & 0o111 != 0,
                "Script should be executable"
            );
        }
    }

    #[tokio::test]
    async fn test_completion_consecutive() {
        let (mut client_stream, _server_handle) = setup_server();
        let mut test_client = TestClient::new(&mut client_stream);

        let doc_uri = Url::parse("file:///consecutive.zsh").unwrap();
        test_client.init_and_open(&doc_uri, "git ").await;

        // 1. First completion
        let res1 = test_client
            .send_request::<request::Completion>(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: doc_uri.clone(),
                    },
                    position: Position::new(0, 4),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            })
            .await
            .unwrap()
            .unwrap();

        let items1 = match res1 {
            CompletionResponse::Array(items) => items,
            CompletionResponse::List(list) => list.items,
        };
        assert!(items1.iter().any(|i| i.label == "status"));

        // 2. Second completion (at the same position)
        let res2 = test_client
            .send_request::<request::Completion>(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: doc_uri.clone(),
                    },
                    position: Position::new(0, 4),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            })
            .await
            .unwrap()
            .unwrap();

        let items2 = match res2 {
            CompletionResponse::Array(items) => items,
            CompletionResponse::List(list) => list.items,
        };
        assert!(items2.iter().any(|i| i.label == "status"));
    }

    #[tokio::test]
    async fn test_completion_after_change() {
        let (mut client_stream, _server_handle) = setup_server();
        let mut test_client = TestClient::new(&mut client_stream);

        let doc_uri = Url::parse("file:///after_change.zsh").unwrap();
        test_client.init_and_open(&doc_uri, "git ").await;

        // Change document: append "sta"
        test_client
            .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 2),
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: Some(Range::new(Position::new(0, 4), Position::new(0, 4))),
                    range_length: None,
                    text: "sta".to_string(),
                }],
            })
            .await;
        test_client.read_notification::<LogMessage>().await; // didChange

        let res = test_client
            .send_request::<request::Completion>(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: doc_uri.clone(),
                    },
                    position: Position::new(0, 7), // After "git sta"
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            })
            .await
            .unwrap()
            .unwrap();

        let items = match res {
            CompletionResponse::Array(items) => items,
            CompletionResponse::List(list) => list.items,
        };
        assert!(items.iter().any(|i| i.label == "status"));
    }

    #[tokio::test]
    async fn test_completion_with_description() {
        let (mut client_stream, _server_handle) = setup_server();
        let mut test_client = TestClient::new(&mut client_stream);

        let doc_uri = Url::parse("file:///description.zsh").unwrap();
        // "ls -" usually shows options with descriptions like "--all -- do not ignore entries starting with ."
        test_client.init_and_open(&doc_uri, "ls -").await;

        let res = test_client
            .send_request::<request::Completion>(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: doc_uri.clone(),
                    },
                    position: Position::new(0, 5),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            })
            .await
            .unwrap()
            .unwrap();

        let items = match res {
            CompletionResponse::Array(items) => items,
            CompletionResponse::List(list) => list.items,
        };

        // Check for any item that has a detail (description)
        // With "ls -", we expect options like "--all -- do not ignore entries starting with ."
        let has_detail = items.iter().any(|i| i.detail.is_some());
        assert!(
            has_detail,
            "Expected at least one completion item to have a description in detail field. Items: {items:?}"
        );
    }
}

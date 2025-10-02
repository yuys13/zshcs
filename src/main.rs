use dashmap::DashMap;
use serde_json::Value;
use std::sync::Arc;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

#[derive(Debug)]
struct Backend {
    client: Client,
    document_map: Arc<DashMap<Url, String>>,
    document_versions: Arc<DashMap<Url, i32>>,
}

impl Backend {
    fn new(client: Client) -> Self {
        Backend {
            client,
            document_map: Arc::new(DashMap::new()),
            document_versions: Arc::new(DashMap::new()),
        }
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

        let is_full_change =
            params.content_changes.len() == 1 && params.content_changes[0].range.is_none();

        if is_full_change {
            // Full document sync
            let text = params.content_changes[0].text.clone();
            self.document_map.insert(uri.clone(), text);
            self.document_versions.insert(uri.clone(), version);
            self.client
                .log_message(
                    MessageType::INFO,
                    format!("textDocument/didChange (full): {uri}"),
                )
                .await;
        } else {
            // Incremental sync
            if let Some(mut doc) = self.document_map.get_mut(&uri) {
                let mut content = doc.value().clone();
                // Apply changes in reverse order to avoid invalidating ranges.
                for change in params.content_changes.iter().rev() {
                    if let Some(range) = change.range {
                        if let (Some(start_offset), Some(end_offset)) = (
                            Self::position_to_byte_offset(&content, range.start),
                            Self::position_to_byte_offset(&content, range.end),
                        ) {
                            if end_offset >= start_offset {
                                content.replace_range(start_offset..end_offset, &change.text);
                            }
                        }
                    }
                }
                *doc = content; // Update the document in the map
            }
            self.document_versions.insert(uri.clone(), version);
            self.client
                .log_message(
                    MessageType::INFO,
                    format!("textDocument/didChange (incremental): {uri}"),
                )
                .await;
        }
    }

    async fn execute_command(&self, params: ExecuteCommandParams) -> Result<Option<Value>> {
        if params.command == "zshcs/getDocumentContent" {
            if let Some(uri_val) = params.arguments.get(0) {
                if let Ok(uri) = serde_json::from_value::<Url>(uri_val.clone()) {
                    let content = self.document_map.get(&uri).map(|doc| doc.value().clone());
                    return Ok(Some(serde_json::to_value(content).unwrap()));
                }
            }
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
        Request as JsonRpcRequest,
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
            let request = JsonRpcRequest::build(R::METHOD)
                .params(serde_json::to_value(params).unwrap())
                .id(id) // send i64 id
                .finish();
            let request_json = serde_json::to_string(&request).unwrap();
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

        // Initialize and open document
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
        let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await; // initialized log
        let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await; // version log

        let doc_uri = Url::parse("file:///test_change.zsh").unwrap();
        let did_open_params = DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "initial content".to_string(),
            },
        };
        test_client
            .send_notification::<DidOpenTextDocument>(did_open_params)
            .await;
        let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await; // didOpen log

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
                .contains("textDocument/didChange (full): file:///test_change.zsh")
        );

        // Future: Add a way to query server for document content to verify it's updated
        // For now, logging confirms the method was called and processed the URI.
    }

    #[tokio::test]
    async fn test_did_change_incremental_sync() {
        let (mut client_stream, _server_handle) = setup_server();
        let mut test_client = TestClient::new(&mut client_stream);

        // Initialize and open document
        let initialize_params = InitializeParams {
            capabilities: ClientCapabilities {
                workspace: Some(WorkspaceClientCapabilities {
                    execute_command: Some(ExecuteCommandClientCapabilities {
                        dynamic_registration: Some(true),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        test_client
            .send_request::<Initialize>(initialize_params)
            .await
            .unwrap();
        test_client
            .send_notification::<Initialized>(InitializedParams {})
            .await;
        let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;
        let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

        let doc_uri = Url::parse("file:///test_incremental.zsh").unwrap();
        let initial_text = "line1\nline2\nline3".to_string();
        let did_open_params = DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: initial_text.clone(),
            },
        };
        test_client
            .send_notification::<DidOpenTextDocument>(did_open_params)
            .await;
        let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

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
}

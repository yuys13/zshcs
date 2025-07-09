use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

#[derive(Debug)]
struct Backend {
    client: Client,
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
                // 今回は特に機能を提供しないため、デフォルトのまま
                ..ServerCapabilities::default()
            },
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "server initialized!")
            .await;
        self.client
            .log_message(MessageType::INFO, format!("Server version: {}", env!("CARGO_PKG_VERSION")))
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(|client| Backend { client });
    Server::new(stdin, stdout, socket).serve(service).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    // use futures::future::FutureExt; // 未使用だったのでコメントアウト
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
    use tower_lsp::jsonrpc::{
        Id, Request as JsonRpcRequest, Response as JsonRpcResponse,
        // Notification as JsonRpcNotification, // Removed due to persistent import issues
        // Error as JsonRpcError // Marked as unused for now
    };
    use tower_lsp::lsp_types::{
        notification::{Initialized, LogMessage, Notification as LspNotificationTrait},
        request::{Initialize, Request as LspRequestTrait},
        ClientCapabilities, InitializeParams, InitializedParams, LogMessageParams, MessageType,
        InitializeResult,
    };
    use serde::{Serialize, de::DeserializeOwned};
    use serde_json::Value; // Keep for JsonRpcResponse parts


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
                    if line.starts_with("Content-Length: ") {
                        content_length = line["Content-Length: ".len()..].trim().parse().ok()?;
                    }
                }
                break;
            }
            if header_buf.len() > 2048 { // Prevent infinite loop on malformed headers
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
        let header = format!("Content-Length: {}\r\n\r\n", message_len);
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
                .id(id.clone()) // send i64 id
                .finish();
            let request_json = serde_json::to_string(&request).unwrap();
            write_message(self.stream, &request_json).await.unwrap();

            loop {
                let response_json = read_message(self.stream).await.unwrap();
                if response_json.contains("\"method\"") && !response_json.contains("\"id\"") {
                    eprintln!("Skipping notification: {}", response_json);
                    continue;
                }
                let response: JsonRpcResponse = serde_json::from_str(&response_json).unwrap();
                let (response_id_val, option_result_val): (Id, Option<std::result::Result<Value, tower_lsp::jsonrpc::Error>>) = response.into_parts();

                match response_id_val {
                    Id::Number(response_id_num) if response_id_num == id => {
                        match option_result_val {
                            Some(Ok(value)) => return serde_json::from_value(value).map_err(|_| tower_lsp::jsonrpc::Error::internal_error()),
                            Some(Err(err)) => return Err(err),
                            None => return Err(tower_lsp::jsonrpc::Error::internal_error("Response missing result".to_string())),
                        }
                    }
                    Id::String(response_id_str) if response_id_str == id.to_string() => {
                        match option_result_val {
                            Some(Ok(value)) => return serde_json::from_value(value).map_err(|_| tower_lsp::jsonrpc::Error::internal_error()),
                            Some(Err(err)) => return Err(err),
                            None => return Err(tower_lsp::jsonrpc::Error::internal_error("Response missing result".to_string())),
                        }
                    }
                    _ => {
                        eprintln!("Received response with unexpected ID: {:?}", response_id_val);
                        continue;
                    }
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
            write_message(self.stream, &notification_json).await.unwrap();
        }

        async fn read_notification<N: LspNotificationTrait>(&mut self) -> Option<N::Params>
        where
            N::Params: DeserializeOwned,
        {
            loop {
                let message_json = read_message(self.stream).await?;
                if let Ok(value) = serde_json::from_str::<Value>(&message_json) {
                    if value.get("method").and_then(Value::as_str) == Some(N::METHOD) {
                        if let Some(params_value) = value.get("params") {
                            return serde_json::from_value(params_value.clone()).ok();
                        }
                        // If params are expected but not present (e.g. for notifications that must have params)
                        // or if the notification has no params and N::Params is some ZST like `()`,
                        // this logic might need adjustment. For now, assume params are present if method matches.
                        // If N::Params can be deserialized from `null` or missing params, it will work.
                        // Otherwise, if params are required, and not present, from_value will fail and return None.
                        if N::METHOD == "initialized" { // Initialized has empty params, which can be `null` or absent
                             if value.get("params").is_none() || value.get("params").unwrap().is_null() {
                                // Attempt to deserialize from null if N::Params is () or similar
                                return serde_json::from_value(Value::Null).ok();
                             }
                        }
                        return None; // Params not found or not the expected structure
                    }
                }
            }
        }
    }

    fn setup_server() -> (DuplexStream, tokio::task::JoinHandle<()>) {
        let (client_stream, server_stream) = tokio::io::duplex(4096);
        let (service, client_socket) = LspService::new(|client| Backend { client });

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

        let result = test_client.send_request::<Initialize>(initialize_params).await.unwrap();

        assert_eq!(result.server_info.as_ref().unwrap().name, "zshcs-language-server");
        assert!(result.capabilities.text_document_sync.is_none());
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
        let _init_result: InitializeResult = test_client.send_request::<Initialize>(initialize_params).await.unwrap();

        // Send initialized notification
        let initialized_params = InitializedParams {};
        test_client.send_notification::<Initialized>(initialized_params).await;

        // Check for log message from server
        // The TestClient needs to be adapted to read notifications, or we need a separate reader
        // For simplicity, we'll assume the server processes it and might log.
        // A more robust test would listen for the logMessage notification.
        // We will try to read the log message from the server.
        let log_message_params: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

        assert!(log_message_params.is_some(), "Did not receive log message after initialized");
        let log_message = log_message_params.unwrap();
        assert_eq!(log_message.typ, MessageType::INFO);
        assert!(log_message.message.contains("server initialized!") || log_message.message.contains("Server version:"));

        // We expect two log messages
        let log_message_params2: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;
        assert!(log_message_params2.is_some(), "Did not receive the second log message");
        let log_message2 = log_message_params2.unwrap();
        assert_eq!(log_message2.typ, MessageType::INFO);
        assert!(log_message2.message.contains("server initialized!") || log_message2.message.contains("Server version:"));

        // Ensure the two messages are different
        assert_ne!(log_message.message, log_message2.message);
    }
}

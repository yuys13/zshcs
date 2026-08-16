#![allow(dead_code)]

use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tower_lsp::jsonrpc::{Id, Response as JsonRpcResponse, Result};
use tower_lsp::lsp_types::{
    CompletionItem, CompletionResponse, DidOpenTextDocumentParams, InitializeParams,
    InitializedParams, TextDocumentItem, Url,
    notification::{
        DidOpenTextDocument, Initialized, LogMessage, Notification as LspNotificationTrait,
    },
    request::{Initialize, Request as LspRequestTrait},
};
use tower_lsp::{LspService, Server};
use zshcs::Backend;

pub async fn read_message(stream: &mut DuplexStream) -> Option<String> {
    let timeout_duration = Duration::from_secs(5);

    let result = tokio::time::timeout(timeout_duration, async {
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
    })
    .await;

    match result {
        Ok(msg_opt) => msg_opt,
        Err(_) => {
            eprintln!("Timeout reading message from stream");
            None
        }
    }
}

pub async fn write_message(stream: &mut DuplexStream, message: &str) -> std::io::Result<()> {
    let message_len = message.len();
    let header = format!("Content-Length: {message_len}\r\n\r\n");
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(message.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

pub struct TestClient<'a> {
    pub stream: &'a mut DuplexStream,
    pub request_id_counter: i64,
}

impl<'a> TestClient<'a> {
    pub fn new(stream: &'a mut DuplexStream) -> Self {
        TestClient {
            stream,
            request_id_counter: 0,
        }
    }

    pub fn next_request_id(&mut self) -> i64 {
        self.request_id_counter += 1;
        self.request_id_counter
    }

    pub async fn send_request<R: LspRequestTrait>(&mut self, params: R::Params) -> Result<R::Result>
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
        if let Some(obj) = request_value
            .as_object_mut()
            .filter(|_| !params_value.is_null())
        {
            obj.insert("params".to_string(), params_value);
        }

        let request_json = serde_json::to_string(&request_value)
            .map_err(|e| tower_lsp::jsonrpc::Error::invalid_params(e.to_string()))?;
        write_message(self.stream, &request_json)
            .await
            .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;

        loop {
            let response_json = read_message(self.stream)
                .await
                .ok_or_else(tower_lsp::jsonrpc::Error::internal_error)?;
            if response_json.contains("\"method\"") && !response_json.contains("\"id\"") {
                eprintln!("Skipping notification: {response_json}");
                continue;
            }
            let response: JsonRpcResponse = serde_json::from_str(&response_json)
                .map_err(|_| tower_lsp::jsonrpc::Error::parse_error())?;
            let (response_id_val, result_val): (
                Id,
                std::result::Result<Value, tower_lsp::jsonrpc::Error>,
            ) = response.into_parts();

            let response_id_matches = match &response_id_val {
                Id::Number(response_id_num) => response_id_num == &id,
                Id::String(response_id_s) => response_id_s == &id.to_string(),
                Id::Null => false,
            };

            if response_id_matches {
                match result_val {
                    Ok(value) => {
                        return serde_json::from_value(value).map_err(|e| {
                            let mut error = tower_lsp::jsonrpc::Error::parse_error();
                            error.message = format!("Failed to deserialize response: {e}").into();
                            error.data = Some(
                                serde_json::json!({ "deserialization_error_details": e.to_string() }),
                            );
                            error
                        });
                    }
                    Err(err) => {
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

    pub async fn send_notification<N: LspNotificationTrait>(&mut self, params: N::Params)
    where
        N::Params: Serialize,
    {
        let params_value = serde_json::to_value(params).unwrap();
        let notification_value = serde_json::json!({
            "jsonrpc": "2.0",
            "method": N::METHOD,
            "params": params_value
        });
        let notification_json = serde_json::to_string(&notification_value).unwrap_or_default();
        let _ = write_message(self.stream, &notification_json).await;
    }

    pub async fn read_notification<N: LspNotificationTrait>(&mut self) -> Option<N::Params>
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
                if N::METHOD == "initialized" && value.get("params").is_none_or(|p| p.is_null()) {
                    return serde_json::from_value(Value::Null).ok();
                }
                return None;
            }
        }
    }

    pub async fn init_and_open(&mut self, file_uri: &Url, initial_text: &str) {
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

pub fn setup_server() -> (DuplexStream, tokio::task::JoinHandle<()>) {
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

pub fn setup_server_with_scripts(
    capture_script: &'static str,
    zptyrc_script: &'static str,
) -> (DuplexStream, tokio::task::JoinHandle<()>) {
    let (client_stream, server_stream) = tokio::io::duplex(4096);
    let (service, client_socket) = LspService::new(move |client| {
        Backend::new_with_scripts(client, capture_script, zptyrc_script)
    });

    let server_handle = tokio::spawn(async move {
        let (server_read, server_write) = tokio::io::split(server_stream);
        Server::new(server_read, server_write, client_socket)
            .serve(service)
            .await;
    });

    (client_stream, server_handle)
}

pub fn get_completion_items(response: CompletionResponse) -> Vec<CompletionItem> {
    match response {
        CompletionResponse::Array(items) => items,
        CompletionResponse::List(list) => list.items,
    }
}

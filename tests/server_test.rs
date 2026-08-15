mod common;

use common::{TestClient, setup_server};
use tower_lsp::lsp_types::{
    ClientCapabilities, DidChangeTextDocumentParams, DidOpenTextDocumentParams,
    ExecuteCommandParams, InitializeParams, InitializeResult, InitializedParams, LogMessageParams,
    MessageType, Position, Range, TextDocumentContentChangeEvent, TextDocumentItem,
    TextDocumentSyncCapability, TextDocumentSyncKind, Url, VersionedTextDocumentIdentifier,
    notification::{DidChangeTextDocument, DidOpenTextDocument, Initialized, LogMessage},
    request::{self, Initialize},
};

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
    let (initialized_msg, version_msg) = if log_message1.message.contains("server initialized!") {
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

    let log_message: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;
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
            range: None,
            range_length: None,
            text: "new full content".to_string(),
        }],
    };
    test_client
        .send_notification::<DidChangeTextDocument>(did_change_params)
        .await;

    let log_message: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;
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
                },
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
                },
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
    let content: Option<String> = result
        .and_then(|v| serde_json::from_value(v).ok())
        .flatten();

    let expected_text = "line1\nnew line2 more\nline3".to_string();
    assert_eq!(
        content,
        Some(expected_text),
        "Incremental changes were not applied correctly."
    );
}

#[tokio::test]
async fn test_did_change_ordering() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///test_ordering.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "").await;

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
    test_client.read_notification::<LogMessage>().await;

    let res = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::to_value(&doc_uri).unwrap()],
            ..Default::default()
        })
        .await
        .unwrap();
    let content: Option<String> = res.and_then(|v| serde_json::from_value(v).ok()).flatten();
    assert_eq!(content, Some("BA".to_string()));
}

#[tokio::test]
async fn test_did_change_mixed() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///test_mixed.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "Old").await;

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
    let content: Option<String> = res.and_then(|v| serde_json::from_value(v).ok()).flatten();
    assert_eq!(content, Some("New!".to_string()));
}

#[tokio::test]
async fn test_did_change_invalid_range() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///invalid_range.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "original").await;

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

    let log = test_client.read_notification::<LogMessage>().await.unwrap();
    assert_eq!(log.typ, MessageType::WARNING);
    assert!(log.message.contains("invalid range"));
}

#[tokio::test]
async fn test_did_change_document_not_found() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///not_opened.zsh").unwrap();

    let initialize_params = InitializeParams::default();
    test_client
        .send_request::<request::Initialize>(initialize_params)
        .await
        .unwrap();
    test_client
        .send_notification::<Initialized>(InitializedParams {})
        .await;

    test_client.read_notification::<LogMessage>().await.unwrap();
    test_client.read_notification::<LogMessage>().await.unwrap();

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
async fn test_execute_command_edge_cases() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

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
        arguments: vec![serde_json::json!(123)],
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
    let content: Option<String> = res.and_then(|v| serde_json::from_value(v).ok()).flatten();
    assert!(content.is_none());
}

#[tokio::test]
async fn test_shutdown() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams::default();
    test_client
        .send_request::<request::Initialize>(initialize_params)
        .await
        .unwrap();
    test_client
        .send_notification::<Initialized>(InitializedParams {})
        .await;

    test_client
        .send_request::<request::Shutdown>(())
        .await
        .unwrap();
}

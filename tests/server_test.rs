mod common;

use common::{TestClient, setup_server_mock as setup_server};
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

#[tokio::test]
async fn test_initialize_execute_command_capabilities() {
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

    let exec_provider = result.capabilities.execute_command_provider;
    assert!(exec_provider.is_some());
    let commands = exec_provider.unwrap().commands;
    assert!(
        commands.contains(&"zshcs/getDocumentContent".to_string()),
        "Expected zshcs/getDocumentContent in execute command provider: {commands:?}"
    );
}

#[tokio::test]
async fn test_multi_document_isolation() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_a = Url::parse("file:///doc_a.zsh").unwrap();
    let doc_b = Url::parse("file:///doc_b.zsh").unwrap();

    test_client.init_and_open(&doc_a, "content A").await;

    // Open second document
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_b.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "content B".to_string(),
            },
        })
        .await;
    test_client.read_notification::<LogMessage>().await;

    // Modify doc_a
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_a.clone(), 2),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(0, 8), Position::new(0, 9))),
                range_length: None,
                text: "A modified".to_string(),
            }],
        })
        .await;
    test_client.read_notification::<LogMessage>().await;

    // Verify doc_b is untouched
    let res_b = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::to_value(&doc_b).unwrap()],
            ..Default::default()
        })
        .await
        .unwrap();
    let content_b: Option<String> = res_b.and_then(|v| serde_json::from_value(v).ok()).flatten();
    assert_eq!(content_b, Some("content B".to_string()));

    // Verify doc_a is modified
    let res_a = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::to_value(&doc_a).unwrap()],
            ..Default::default()
        })
        .await
        .unwrap();
    let content_a: Option<String> = res_a.and_then(|v| serde_json::from_value(v).ok()).flatten();
    assert_eq!(content_a, Some("content A modified".to_string()));
}

#[tokio::test]
async fn test_multi_document_did_close() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_a = Url::parse("file:///close_a.zsh").unwrap();
    let doc_b = Url::parse("file:///close_b.zsh").unwrap();

    test_client.init_and_open(&doc_a, "initial A").await;

    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_b.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "initial B".to_string(),
            },
        })
        .await;
    test_client.read_notification::<LogMessage>().await;

    // Close doc_a
    test_client
        .send_notification::<tower_lsp::lsp_types::notification::DidCloseTextDocument>(
            tower_lsp::lsp_types::DidCloseTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: doc_a.clone() },
            },
        )
        .await;
    let close_log = test_client.read_notification::<LogMessage>().await.unwrap();
    assert_eq!(close_log.typ, MessageType::INFO);
    assert!(
        close_log
            .message
            .contains("textDocument/didClose: file:///close_a.zsh")
    );

    // Verify doc_a is discarded from memory
    let res_a = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::to_value(&doc_a).unwrap()],
            ..Default::default()
        })
        .await
        .unwrap();
    let content_a: Option<String> = res_a.and_then(|v| serde_json::from_value(v).ok()).flatten();
    assert_eq!(content_a, None, "Closed doc_a must be freed from memory");

    // Edit doc_b
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_b.clone(), 2),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(0, 8), Position::new(0, 9))),
                range_length: None,
                text: "B updated".to_string(),
            }],
        })
        .await;
    test_client.read_notification::<LogMessage>().await;

    // Verify doc_b
    let res_b = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::to_value(&doc_b).unwrap()],
            ..Default::default()
        })
        .await
        .unwrap();
    let content_b: Option<String> = res_b.and_then(|v| serde_json::from_value(v).ok()).flatten();
    assert_eq!(content_b, Some("initial B updated".to_string()));
}

#[tokio::test]
async fn test_did_close_handler() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///test_close_standalone.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "echo closing").await;

    // Verify document exists
    let res = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::to_value(&doc_uri).unwrap()],
            ..Default::default()
        })
        .await
        .unwrap();
    let content: Option<String> = res.and_then(|v| serde_json::from_value(v).ok()).flatten();
    assert_eq!(content, Some("echo closing".to_string()));

    // Send didClose
    test_client
        .send_notification::<tower_lsp::lsp_types::notification::DidCloseTextDocument>(
            tower_lsp::lsp_types::DidCloseTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
            },
        )
        .await;

    let log = test_client.read_notification::<LogMessage>().await.unwrap();
    assert_eq!(log.typ, MessageType::INFO);
    assert!(
        log.message
            .contains("textDocument/didClose: file:///test_close_standalone.zsh")
    );

    // Verify document is removed
    let res_after = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::to_value(&doc_uri).unwrap()],
            ..Default::default()
        })
        .await
        .unwrap();
    let content_after: Option<String> = res_after
        .and_then(|v| serde_json::from_value(v).ok())
        .flatten();
    assert_eq!(content_after, None);
}

#[tokio::test]
async fn test_did_close_unopened_document() {
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
    test_client.read_notification::<LogMessage>().await.unwrap();
    test_client.read_notification::<LogMessage>().await.unwrap();

    let doc_uri = Url::parse("file:///never_opened.zsh").unwrap();

    // Send didClose for unopened file
    test_client
        .send_notification::<tower_lsp::lsp_types::notification::DidCloseTextDocument>(
            tower_lsp::lsp_types::DidCloseTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
            },
        )
        .await;

    let log = test_client.read_notification::<LogMessage>().await.unwrap();
    assert_eq!(log.typ, MessageType::WARNING);
    assert!(log.message.contains("document not found"));
}

#[tokio::test]
async fn test_did_change_outdated_version() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///outdated_version.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "initial text").await;

    // Apply change with version 2
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 2),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "version 2 text".to_string(),
            }],
        })
        .await;
    test_client.read_notification::<LogMessage>().await;

    // Attempt to apply change with version 1 (outdated)
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 1),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "stale version 1 text".to_string(),
            }],
        })
        .await;

    let log = test_client.read_notification::<LogMessage>().await.unwrap();
    assert_eq!(log.typ, MessageType::WARNING);
    assert!(log.message.contains("outdated version"));

    // Verify document content was preserved at version 2
    let res = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::to_value(&doc_uri).unwrap()],
            ..Default::default()
        })
        .await
        .unwrap();
    let content: Option<String> = res.and_then(|v| serde_json::from_value(v).ok()).flatten();
    assert_eq!(content, Some("version 2 text".to_string()));
}

#[tokio::test]
async fn test_completion_after_did_close() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///completion_closed.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "git s").await;

    // Close document
    test_client
        .send_notification::<tower_lsp::lsp_types::notification::DidCloseTextDocument>(
            tower_lsp::lsp_types::DidCloseTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
            },
        )
        .await;
    test_client.read_notification::<LogMessage>().await;

    // Request completion on closed document
    let res = test_client
        .send_request::<request::Completion>(tower_lsp::lsp_types::CompletionParams {
            text_document_position: tower_lsp::lsp_types::TextDocumentPositionParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: doc_uri },
                position: Position::new(0, 5),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await;

    assert!(res.is_ok());
    assert!(
        res.unwrap().is_none(),
        "Completion on closed document must return None"
    );
}

#[tokio::test]
async fn test_did_change_multiline_replacement() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///multiline_replace.zsh").unwrap();
    let initial_text = "line1\nline2\nline3\nline4";
    test_client.init_and_open(&doc_uri, initial_text).await;

    // Replace line 1 col 0 to line 3 col 0 ("line2\nline3\n") with "replaced\n"
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 2),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(1, 0), Position::new(3, 0))),
                range_length: None,
                text: "replaced\n".to_string(),
            }],
        })
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
    assert_eq!(content, Some("line1\nreplaced\nline4".to_string()));
}

#[tokio::test]
async fn test_did_change_insert_at_boundaries() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///boundaries.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "middle").await;

    // Insert at (0, 0)
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 2),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(0, 0), Position::new(0, 0))),
                range_length: None,
                text: "start\n".to_string(),
            }],
        })
        .await;
    test_client.read_notification::<LogMessage>().await;

    // Insert at end (1, 6)
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 3),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(1, 6), Position::new(1, 6))),
                range_length: None,
                text: "\nend".to_string(),
            }],
        })
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
    assert_eq!(content, Some("start\nmiddle\nend".to_string()));
}

#[tokio::test]
async fn test_did_change_delete_lines() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///delete_lines.zsh").unwrap();
    let initial_text = "alpha\nbeta\ngamma";
    test_client.init_and_open(&doc_uri, initial_text).await;

    // Delete line 1 ("beta\n") by replacing (1, 0)..(2, 0) with ""
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 2),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(1, 0), Position::new(2, 0))),
                range_length: None,
                text: "".to_string(),
            }],
        })
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
    assert_eq!(content, Some("alpha\ngamma".to_string()));
}

#[tokio::test]
async fn test_did_change_multibyte_text() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///multibyte_sync.zsh").unwrap();
    let initial_text = "日本語の\nテストです\n終わり";
    test_client.init_and_open(&doc_uri, initial_text).await;

    // Replace "テスト" (UTF-16 char 0 to 3 on line 1) with "置換"
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 2),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(1, 0), Position::new(1, 3))),
                range_length: None,
                text: "置換".to_string(),
            }],
        })
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
    assert_eq!(content, Some("日本語の\n置換です\n終わり".to_string()));

    // Replace emoji "🎉" (2 UTF-16 code units) with "🚀"
    let doc_uri2 = Url::parse("file:///emoji_sync.zsh").unwrap();
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri2.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "Hello 🎉 world".to_string(),
            },
        })
        .await;
    test_client.read_notification::<LogMessage>().await;

    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri2.clone(), 2),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(0, 6), Position::new(0, 8))),
                range_length: None,
                text: "🚀".to_string(),
            }],
        })
        .await;
    test_client.read_notification::<LogMessage>().await;

    let res2 = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::to_value(&doc_uri2).unwrap()],
            ..Default::default()
        })
        .await
        .unwrap();
    let content2: Option<String> = res2.and_then(|v| serde_json::from_value(v).ok()).flatten();
    assert_eq!(content2, Some("Hello 🚀 world".to_string()));
}

#[tokio::test]
async fn test_did_change_out_of_bounds_line() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///out_of_bounds_line.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "line1\nline2").await;

    let params = DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 2),
        content_changes: vec![TextDocumentContentChangeEvent {
            range: Some(Range::new(Position::new(50, 0), Position::new(50, 5))),
            range_length: None,
            text: "fail".to_string(),
        }],
    };
    test_client
        .send_notification::<DidChangeTextDocument>(params)
        .await;

    let log = test_client.read_notification::<LogMessage>().await.unwrap();
    assert_eq!(log.typ, MessageType::WARNING);
    assert!(log.message.contains("invalid range"));

    // Ensure document content is preserved
    let res = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::to_value(&doc_uri).unwrap()],
            ..Default::default()
        })
        .await
        .unwrap();
    let content: Option<String> = res.and_then(|v| serde_json::from_value(v).ok()).flatten();
    assert_eq!(content, Some("line1\nline2".to_string()));
}

#[tokio::test]
async fn test_execute_command_malformed_arguments_comprehensive() {
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

    // 1. Empty arguments array
    let res1 = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![],
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(res1.is_none());

    // 2. Null in arguments
    let res2 = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::Value::Null],
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(res2.is_none());

    // 3. Boolean in arguments
    let res3 = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::json!(true)],
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(res3.is_none());

    // 4. Invalid URL string
    let res4 = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::json!("not a valid uri ::: //")],
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(res4.is_none());

    // 5. Non-existent file URI
    let res5 = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::json!("file:///never_opened_file.zsh")],
            ..Default::default()
        })
        .await
        .unwrap();
    let content: Option<String> = res5.and_then(|v| serde_json::from_value(v).ok()).flatten();
    assert!(content.is_none());
}

#[tokio::test]
async fn test_interleaved_sync_and_completion() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///typing_simulation.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "git ").await;

    // 1. User types 's' -> "git s"
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 2),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(0, 4), Position::new(0, 4))),
                range_length: None,
                text: "s".to_string(),
            }],
        })
        .await;
    test_client.read_notification::<LogMessage>().await;

    let res1 = test_client
        .send_request::<request::Completion>(tower_lsp::lsp_types::CompletionParams {
            text_document_position: tower_lsp::lsp_types::TextDocumentPositionParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
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
    let items1 = common::get_completion_items(res1);
    assert!(items1.iter().any(|i| i.label == "status"));

    // 2. User types 't' -> "git st"
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 3),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(Position::new(0, 5), Position::new(0, 5))),
                range_length: None,
                text: "t".to_string(),
            }],
        })
        .await;
    test_client.read_notification::<LogMessage>().await;

    let res2 = test_client
        .send_request::<request::Completion>(tower_lsp::lsp_types::CompletionParams {
            text_document_position: tower_lsp::lsp_types::TextDocumentPositionParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(0, 6),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();
    let items2 = common::get_completion_items(res2);
    assert!(items2.iter().any(|i| i.label == "status"));
}

#[tokio::test]
async fn test_rapid_burst_completion_requests() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///burst.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "git ").await;

    for i in 0..5 {
        let res = test_client
            .send_request::<request::Completion>(tower_lsp::lsp_types::CompletionParams {
                text_document_position: tower_lsp::lsp_types::TextDocumentPositionParams {
                    text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
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
        let items = common::get_completion_items(res);
        assert!(
            items.iter().any(|item| item.label == "status"),
            "Burst request {i} failed to contain 'status'"
        );
    }
}

#[tokio::test]
async fn test_reopen_closed_document() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///reopen.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "initial text").await;

    // Close
    test_client
        .send_notification::<tower_lsp::lsp_types::notification::DidCloseTextDocument>(
            tower_lsp::lsp_types::DidCloseTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
            },
        )
        .await;
    test_client.read_notification::<LogMessage>().await;

    // Verify it is gone
    let res = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::to_value(&doc_uri).unwrap()],
            ..Default::default()
        })
        .await
        .unwrap();
    let content: Option<String> = res.and_then(|v| serde_json::from_value(v).ok()).flatten();
    assert_eq!(content, None);

    // Reopen
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "reopened text".to_string(),
            },
        })
        .await;
    test_client.read_notification::<LogMessage>().await;

    // Modify
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 2),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "modified reopened text".to_string(),
            }],
        })
        .await;
    test_client.read_notification::<LogMessage>().await;

    // Verify
    let res2 = test_client
        .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
            command: "zshcs/getDocumentContent".to_string(),
            arguments: vec![serde_json::to_value(&doc_uri).unwrap()],
            ..Default::default()
        })
        .await
        .unwrap();
    let content2: Option<String> = res2.and_then(|v| serde_json::from_value(v).ok()).flatten();
    assert_eq!(content2, Some("modified reopened text".to_string()));
}

#[tokio::test]
async fn test_double_did_close() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///double_close.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "content").await;

    // First didClose -> INFO
    test_client
        .send_notification::<tower_lsp::lsp_types::notification::DidCloseTextDocument>(
            tower_lsp::lsp_types::DidCloseTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
            },
        )
        .await;
    let log1 = test_client.read_notification::<LogMessage>().await.unwrap();
    assert_eq!(log1.typ, MessageType::INFO);
    assert!(
        log1.message
            .contains("textDocument/didClose: file:///double_close.zsh")
    );

    // Second didClose -> WARNING
    test_client
        .send_notification::<tower_lsp::lsp_types::notification::DidCloseTextDocument>(
            tower_lsp::lsp_types::DidCloseTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
            },
        )
        .await;
    let log2 = test_client.read_notification::<LogMessage>().await.unwrap();
    assert_eq!(log2.typ, MessageType::WARNING);
    assert!(log2.message.contains("document not found"));
}

#[tokio::test]
async fn test_did_change_on_closed_document() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///change_after_close.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "content").await;

    // Close
    test_client
        .send_notification::<tower_lsp::lsp_types::notification::DidCloseTextDocument>(
            tower_lsp::lsp_types::DidCloseTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
            },
        )
        .await;
    test_client.read_notification::<LogMessage>().await;

    // Attempt to change
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 2),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "new content".to_string(),
            }],
        })
        .await;

    let log = test_client.read_notification::<LogMessage>().await.unwrap();
    assert_eq!(log.typ, MessageType::WARNING);
    assert!(log.message.contains("document not found"));
}

#[tokio::test]
async fn test_rapid_open_change_close_reopen_cycle() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///rapid_cycle.zsh").unwrap();

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

    for cycle in 1..=10 {
        // Open
        test_client
            .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: doc_uri.clone(),
                    language_id: "zsh".to_string(),
                    version: 1,
                    text: format!("cycle_{cycle}_initial"),
                },
            })
            .await;
        test_client.read_notification::<LogMessage>().await.unwrap();

        // Change
        test_client
            .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 2),
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: format!("cycle_{cycle}_modified"),
                }],
            })
            .await;
        test_client.read_notification::<LogMessage>().await.unwrap();

        // Verify content
        let res = test_client
            .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
                command: "zshcs/getDocumentContent".to_string(),
                arguments: vec![serde_json::to_value(&doc_uri).unwrap()],
                ..Default::default()
            })
            .await
            .unwrap();
        let content: Option<String> = res.and_then(|v| serde_json::from_value(v).ok()).flatten();
        assert_eq!(content, Some(format!("cycle_{cycle}_modified")));

        // Close
        test_client
            .send_notification::<tower_lsp::lsp_types::notification::DidCloseTextDocument>(
                tower_lsp::lsp_types::DidCloseTextDocumentParams {
                    text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                        uri: doc_uri.clone(),
                    },
                },
            )
            .await;
        test_client.read_notification::<LogMessage>().await.unwrap();

        // Verify content is None
        let res_closed = test_client
            .send_request::<request::ExecuteCommand>(ExecuteCommandParams {
                command: "zshcs/getDocumentContent".to_string(),
                arguments: vec![serde_json::to_value(&doc_uri).unwrap()],
                ..Default::default()
            })
            .await
            .unwrap();
        let content_closed: Option<String> = res_closed
            .and_then(|v| serde_json::from_value(v).ok())
            .flatten();
        assert_eq!(content_closed, None);
    }
}

#[tokio::test]
async fn test_close_and_immediate_completion_race() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///race_close.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "git ").await;

    // Send didClose immediately followed by completion request
    test_client
        .send_notification::<tower_lsp::lsp_types::notification::DidCloseTextDocument>(
            tower_lsp::lsp_types::DidCloseTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
            },
        )
        .await;

    let res = test_client
        .send_request::<request::Completion>(tower_lsp::lsp_types::CompletionParams {
            text_document_position: tower_lsp::lsp_types::TextDocumentPositionParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(0, 4),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap();

    // Since document was closed, completion must safely return None
    assert!(res.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_async_server_initialization_handshake() {
    use tower_lsp::{LspService, Server};
    use zshcs::Backend;

    let (client_stream, server_stream) = tokio::io::duplex(4096);
    let (service, client_socket) = LspService::new(|client| {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                Backend::new_async(client)
                    .await
                    .expect("Failed to create async backend")
            })
        })
    });

    let _server_handle = tokio::spawn(async move {
        let (server_read, server_write) = tokio::io::split(server_stream);
        Server::new(server_read, server_write, client_socket)
            .serve(service)
            .await;
    });

    let mut client_stream_mut = client_stream;
    let mut test_client = TestClient::new(&mut client_stream_mut);

    let init_result: InitializeResult = test_client
        .send_request::<Initialize>(InitializeParams::default())
        .await
        .unwrap();

    assert_eq!(
        init_result.server_info.as_ref().unwrap().name,
        "zshcs-language-server"
    );

    test_client
        .send_notification::<Initialized>(InitializedParams {})
        .await;

    let log1: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;
    let log2: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;
    assert!(log1.is_some());
    assert!(log2.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_async_with_scripts_server_initialization() {
    use tower_lsp::{LspService, Server};
    use zshcs::Backend;

    let (client_stream, server_stream) = tokio::io::duplex(4096);
    let (service, client_socket) = LspService::new(|client| {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                Backend::new_with_scripts_async(
                    client,
                    common::MOCK_DUMMY_CAPTURE,
                    common::MOCK_DUMMY_ZPTYRC,
                )
                .await
                .expect("Failed to create async backend with scripts")
            })
        })
    });

    let _server_handle = tokio::spawn(async move {
        let (server_read, server_write) = tokio::io::split(server_stream);
        Server::new(server_read, server_write, client_socket)
            .serve(service)
            .await;
    });

    let mut client_stream_mut = client_stream;
    let mut test_client = TestClient::new(&mut client_stream_mut);

    let init_result: InitializeResult = test_client
        .send_request::<Initialize>(InitializeParams::default())
        .await
        .unwrap();

    assert_eq!(
        init_result.server_info.as_ref().unwrap().name,
        "zshcs-language-server"
    );
}

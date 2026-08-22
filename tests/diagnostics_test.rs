mod common;

use std::time::Duration;

use common::{TestClient, setup_server_mock as setup_server};
use serde_json::json;
use tower_lsp::lsp_types::{
    ClientCapabilities, DiagnosticSeverity, DidChangeConfigurationParams,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, InitializeParams, InitializedParams, LogMessageParams,
    PublishDiagnosticsParams, TextDocumentContentChangeEvent, TextDocumentIdentifier,
    TextDocumentItem, Url, VersionedTextDocumentIdentifier,
    notification::{
        DidChangeConfiguration, DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument,
        DidSaveTextDocument, Initialized, LogMessage, PublishDiagnostics,
    },
    request::Initialize,
};

#[tokio::test]
async fn test_diagnostics_disabled_by_default() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    // Initialize with default params (diagnostics disabled)
    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
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

    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // Open document with obvious syntax error
    let doc_uri = Url::parse("file:///syntax_error.zsh").unwrap();
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "if [[ ; then\n  echo bad\nfi\n".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // Verify NO PublishDiagnostics is sent within 300ms
    let diag_result = tokio::time::timeout(
        Duration::from_millis(300),
        test_client.read_notification::<PublishDiagnostics>(),
    )
    .await;

    assert!(
        diag_result.is_err(),
        "Expected no diagnostics when experimental_diagnostics is disabled"
    );
}

#[tokio::test]
async fn test_diagnostics_enabled_via_initialization_options() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    // Initialize with experimental diagnostics enabled
    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
        capabilities: ClientCapabilities::default(),
        initialization_options: Some(json!({
            "zshcs": {
                "experimental": {
                    "diagnostics": true
                }
            }
        })),
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

    // Open document with syntax error
    let doc_uri = Url::parse("file:///syntax_error.zsh").unwrap();
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "if [[ ; then\n  echo bad\nfi\n".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // Should receive PublishDiagnostics with 1 error
    let diag_params: Option<PublishDiagnosticsParams> =
        test_client.read_notification::<PublishDiagnostics>().await;
    assert!(
        diag_params.is_some(),
        "Expected diagnostics notification on syntax error"
    );
    let diags = diag_params.unwrap();
    assert_eq!(diags.uri, doc_uri);
    assert!(!diags.diagnostics.is_empty());
    let diag = &diags.diagnostics[0];
    assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
    assert_eq!(diag.source, Some("zshcs".to_string()));
    assert!(diag.message.contains("parse error"));

    // Fix syntax via didChange
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 2),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "if true; then\n  echo ok\nfi\n".to_string(),
            }],
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // Should receive empty diagnostics clearing error
    let fixed_diags = test_client
        .read_notification::<PublishDiagnostics>()
        .await
        .expect("Expected empty diagnostics clearing error");
    assert_eq!(fixed_diags.uri, doc_uri);
    assert!(
        fixed_diags.diagnostics.is_empty(),
        "Fixed syntax should yield 0 diagnostics, got: {:?}",
        fixed_diags.diagnostics
    );
}

#[tokio::test]
async fn test_diagnostics_dynamic_enable_and_disable_via_did_change_configuration() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    // Initialize with default (disabled)
    let initialize_params = InitializeParams::default();
    test_client
        .send_request::<Initialize>(initialize_params)
        .await
        .unwrap();
    test_client
        .send_notification::<Initialized>(InitializedParams {})
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // Open document with syntax error
    let doc_uri = Url::parse("file:///dynamic.zsh").unwrap();
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "echo \"unclosed quote".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // Verify no diagnostics while disabled
    let initial_check = tokio::time::timeout(
        Duration::from_millis(200),
        test_client.read_notification::<PublishDiagnostics>(),
    )
    .await;
    assert!(initial_check.is_err());

    // 1. Enable dynamically via workspace/didChangeConfiguration
    test_client
        .send_notification::<DidChangeConfiguration>(DidChangeConfigurationParams {
            settings: json!({
                "zshcs": {
                    "experimental": {
                        "diagnostics": true
                    }
                }
            }),
        })
        .await;

    // Should receive diagnostics for open document
    let enabled_diags = test_client
        .read_notification::<PublishDiagnostics>()
        .await
        .expect("Expected diagnostics after dynamic enabling");
    assert_eq!(enabled_diags.uri, doc_uri);
    assert_eq!(enabled_diags.diagnostics.len(), 1);
    assert!(enabled_diags.diagnostics[0].message.contains("unmatched"));

    // 2. Disable dynamically via workspace/didChangeConfiguration
    test_client
        .send_notification::<DidChangeConfiguration>(DidChangeConfigurationParams {
            settings: json!({
                "zshcs": {
                    "experimental": {
                        "diagnostics": false
                    }
                }
            }),
        })
        .await;

    // Should receive empty diagnostics clearing markers
    let disabled_diags = test_client
        .read_notification::<PublishDiagnostics>()
        .await
        .expect("Expected clear diagnostics after dynamic disabling");
    assert_eq!(disabled_diags.uri, doc_uri);
    assert!(disabled_diags.diagnostics.is_empty());
}

#[tokio::test]
async fn test_diagnostics_cleared_on_did_close() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    // Initialize with diagnostics enabled
    let initialize_params = InitializeParams {
        initialization_options: Some(json!({
            "zshcs": {
                "experimental": {
                    "diagnostics": true
                }
            }
        })),
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

    let doc_uri = Url::parse("file:///closing_doc.zsh").unwrap();
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "if [[ ; then".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // Receive initial error diagnostics
    let diags = test_client
        .read_notification::<PublishDiagnostics>()
        .await
        .expect("Expected diagnostics on open");
    assert_eq!(diags.diagnostics.len(), 1);

    // Close document
    test_client
        .send_notification::<DidCloseTextDocument>(DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier {
                uri: doc_uri.clone(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // Receive empty diagnostics to clear editor state
    let closed_diags = test_client
        .read_notification::<PublishDiagnostics>()
        .await
        .expect("Expected empty diagnostics on close");
    assert_eq!(closed_diags.uri, doc_uri);
    assert!(closed_diags.diagnostics.is_empty());
}

#[tokio::test]
async fn test_diagnostics_did_save_triggers_diagnostics() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams {
        initialization_options: Some(json!({
            "zshcs": {
                "experimental": {
                    "diagnostics": true
                }
            }
        })),
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

    let doc_uri = Url::parse("file:///save_test.zsh").unwrap();
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "echo valid".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;
    let open_diags = test_client
        .read_notification::<PublishDiagnostics>()
        .await
        .unwrap();
    assert!(open_diags.diagnostics.is_empty());

    // Send didSave
    test_client
        .send_notification::<DidSaveTextDocument>(DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier {
                uri: doc_uri.clone(),
            },
            text: None,
        })
        .await;

    let save_diags = test_client
        .read_notification::<PublishDiagnostics>()
        .await
        .unwrap();
    assert_eq!(save_diags.uri, doc_uri);
    assert!(save_diags.diagnostics.is_empty());
}

#[tokio::test]
async fn test_diagnostics_debounce_rapid_changes() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams {
        initialization_options: Some(json!({
            "zshcs": {
                "experimental": {
                    "diagnostics": true
                }
            }
        })),
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

    let doc_uri = Url::parse("file:///rapid_edits.zsh").unwrap();
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "echo start".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;
    let _ = test_client.read_notification::<PublishDiagnostics>().await;

    // Send 5 rapid changes in 10ms intervals
    for ver in 2..=5 {
        test_client
            .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), ver),
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: format!("echo step {ver}"),
                }],
            })
            .await;
        let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Final change has syntax error
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 6),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "if [[ ; then".to_string(),
            }],
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // Wait for debounced diagnostic to arrive
    let final_diag = test_client
        .read_notification::<PublishDiagnostics>()
        .await
        .expect("Expected diagnostic notification for final version");
    assert_eq!(final_diag.version, Some(6));
    assert_eq!(final_diag.diagnostics.len(), 1);
    assert!(final_diag.diagnostics[0].message.contains("parse error"));
}

#[tokio::test]
async fn test_diagnostics_did_save_with_syntax_error() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams {
        initialization_options: Some(json!({
            "zshcs": {
                "experimental": {
                    "diagnostics": true
                }
            }
        })),
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

    let doc_uri = Url::parse("file:///save_err.zsh").unwrap();
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "if [[ ; then".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;
    let _open_diags = test_client
        .read_notification::<PublishDiagnostics>()
        .await
        .unwrap();

    // Trigger save
    test_client
        .send_notification::<DidSaveTextDocument>(DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier {
                uri: doc_uri.clone(),
            },
            text: None,
        })
        .await;

    let save_diags = test_client
        .read_notification::<PublishDiagnostics>()
        .await
        .expect("Expected diagnostic notification on save");
    assert_eq!(save_diags.uri, doc_uri);
    assert_eq!(save_diags.diagnostics.len(), 1);
    assert!(save_diags.diagnostics[0].message.contains("parse error"));
}

#[tokio::test]
async fn test_diagnostics_multiple_documents_dynamic_toggle() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams::default();
    test_client
        .send_request::<Initialize>(initialize_params)
        .await
        .unwrap();
    test_client
        .send_notification::<Initialized>(InitializedParams {})
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    let doc1_uri = Url::parse("file:///doc1.zsh").unwrap();
    let doc2_uri = Url::parse("file:///doc2.zsh").unwrap();

    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc1_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "if [[ ; then".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc2_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "echo \"unclosed quote".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // Dynamically enable
    test_client
        .send_notification::<DidChangeConfiguration>(DidChangeConfigurationParams {
            settings: json!({
                "zshcs": {
                    "experimental": {
                        "diagnostics": true
                    }
                }
            }),
        })
        .await;

    // Both documents should receive diagnostics
    let diag_a = test_client
        .read_notification::<PublishDiagnostics>()
        .await
        .expect("Expected diagnostic notification 1");
    let diag_b = test_client
        .read_notification::<PublishDiagnostics>()
        .await
        .expect("Expected diagnostic notification 2");

    let received_uris = [diag_a.uri, diag_b.uri];
    assert!(received_uris.contains(&doc1_uri));
    assert!(received_uris.contains(&doc2_uri));

    // Dynamically disable
    test_client
        .send_notification::<DidChangeConfiguration>(DidChangeConfigurationParams {
            settings: json!({
                "zshcs": {
                    "experimental": {
                        "diagnostics": false
                    }
                }
            }),
        })
        .await;

    let clear_a = test_client
        .read_notification::<PublishDiagnostics>()
        .await
        .expect("Expected clear notification 1");
    let clear_b = test_client
        .read_notification::<PublishDiagnostics>()
        .await
        .expect("Expected clear notification 2");

    assert!(clear_a.diagnostics.is_empty());
    assert!(clear_b.diagnostics.is_empty());
}

#[tokio::test]
async fn test_diagnostics_flat_initialization_options() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    // Initialize with flat experimental options
    let initialize_params = InitializeParams {
        initialization_options: Some(json!({
            "experimental": {
                "diagnostics": true
            }
        })),
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

    let doc_uri = Url::parse("file:///flat_opt.zsh").unwrap();
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "if [[ ; then".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    let diag = test_client
        .read_notification::<PublishDiagnostics>()
        .await
        .expect("Expected diagnostic notification");
    assert_eq!(diag.uri, doc_uri);
    assert_eq!(diag.diagnostics.len(), 1);
}

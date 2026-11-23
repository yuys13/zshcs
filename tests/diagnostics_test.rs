mod common;

use std::time::Duration;

use common::setup_server_mock as setup_server;
use serde_json::json;
use tower_lsp::lsp_types::{
    DiagnosticSeverity, DidSaveTextDocumentParams, PublishDiagnosticsParams,
    TextDocumentIdentifier, Url,
    notification::{DidSaveTextDocument, PublishDiagnostics},
};

#[tokio::test]
async fn test_diagnostics_disabled_by_default() {
    let mut harness = setup_server();
    let mut test_client = harness.client();

    let doc_uri = Url::parse("file:///syntax_error.zsh").unwrap();
    test_client
        .init_and_open(&doc_uri, "if [[ ; then\n  echo bad\nfi\n")
        .await;

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
    let mut harness = setup_server();
    let mut test_client = harness.client();

    let doc_uri = Url::parse("file:///syntax_error.zsh").unwrap();
    test_client
        .init_with_options_and_open(
            Some(json!({
                "zshcs": {
                    "experimental": {
                        "diagnostics": true
                    }
                }
            })),
            &doc_uri,
            "if [[ ; then\n  echo bad\nfi\n",
        )
        .await;

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
        .change_document_full(&doc_uri, 2, "if true; then\n  echo ok\nfi\n")
        .await;

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
    let mut harness = setup_server();
    let mut test_client = harness.client();

    let doc_uri = Url::parse("file:///dynamic.zsh").unwrap();
    test_client
        .init_and_open(&doc_uri, "echo \"unclosed quote")
        .await;

    // Verify no diagnostics while disabled
    let initial_check = tokio::time::timeout(
        Duration::from_millis(200),
        test_client.read_notification::<PublishDiagnostics>(),
    )
    .await;
    assert!(initial_check.is_err());

    // 1. Enable dynamically via workspace/didChangeConfiguration
    test_client
        .change_configuration(json!({
            "zshcs": {
                "experimental": {
                    "diagnostics": true
                }
            }
        }))
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
        .change_configuration(json!({
            "zshcs": {
                "experimental": {
                    "diagnostics": false
                }
            }
        }))
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
    let mut harness = setup_server();
    let mut test_client = harness.client();

    let doc_uri = Url::parse("file:///closing_doc.zsh").unwrap();
    test_client
        .init_with_options_and_open(
            Some(json!({
                "zshcs": {
                    "experimental": {
                        "diagnostics": true
                    }
                }
            })),
            &doc_uri,
            "if [[ ; then",
        )
        .await;

    // Receive initial error diagnostics
    let diags = test_client
        .read_notification::<PublishDiagnostics>()
        .await
        .expect("Expected diagnostics on open");
    assert_eq!(diags.diagnostics.len(), 1);

    // Close document
    test_client.close_document(&doc_uri).await;

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
    let mut harness = setup_server();
    let mut test_client = harness.client();

    let doc_uri = Url::parse("file:///save_test.zsh").unwrap();
    test_client
        .init_with_options_and_open(
            Some(json!({
                "zshcs": {
                    "experimental": {
                        "diagnostics": true
                    }
                }
            })),
            &doc_uri,
            "echo valid",
        )
        .await;
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
    let mut harness = setup_server();
    let mut test_client = harness.client();

    let doc_uri = Url::parse("file:///rapid_edits.zsh").unwrap();
    test_client
        .init_with_options_and_open(
            Some(json!({
                "zshcs": {
                    "experimental": {
                        "diagnostics": true
                    }
                }
            })),
            &doc_uri,
            "echo start",
        )
        .await;
    let _ = test_client.read_notification::<PublishDiagnostics>().await;

    // Send 5 rapid changes in 10ms intervals
    for ver in 2..=5 {
        test_client
            .change_document_full(&doc_uri, ver, &format!("echo step {ver}"))
            .await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Final change has syntax error
    test_client
        .change_document_full(&doc_uri, 6, "if [[ ; then")
        .await;

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
    let mut harness = setup_server();
    let mut test_client = harness.client();

    let doc_uri = Url::parse("file:///save_err.zsh").unwrap();
    test_client
        .init_with_options_and_open(
            Some(json!({
                "zshcs": {
                    "experimental": {
                        "diagnostics": true
                    }
                }
            })),
            &doc_uri,
            "if [[ ; then",
        )
        .await;
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
    let mut harness = setup_server();
    let mut test_client = harness.client();

    test_client.init().await;

    let doc1_uri = Url::parse("file:///doc1.zsh").unwrap();
    let doc2_uri = Url::parse("file:///doc2.zsh").unwrap();

    test_client.open_document(&doc1_uri, "if [[ ; then").await;
    test_client
        .open_document(&doc2_uri, "echo \"unclosed quote")
        .await;

    // Dynamically enable
    test_client
        .change_configuration(json!({
            "zshcs": {
                "experimental": {
                    "diagnostics": true
                }
            }
        }))
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
        .change_configuration(json!({
            "zshcs": {
                "experimental": {
                    "diagnostics": false
                }
            }
        }))
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

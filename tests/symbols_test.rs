mod common;

use common::{TestClient, setup_server_mock as setup_server};
use serde_json::json;
use tower_lsp::lsp_types::{
    ClientCapabilities, DidChangeConfigurationParams, DidOpenTextDocumentParams,
    DocumentSymbolParams, DocumentSymbolResponse, InitializeParams, InitializedParams,
    LogMessageParams, OneOf, SymbolKind, TextDocumentIdentifier, TextDocumentItem, Url,
    notification::{DidChangeConfiguration, DidOpenTextDocument, Initialized, LogMessage},
    request::{DocumentSymbolRequest, Initialize},
};

#[tokio::test]
async fn test_symbols_capability_registered_on_initialize() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
        capabilities: ClientCapabilities::default(),
        ..Default::default()
    };

    let init_result = test_client
        .send_request::<Initialize>(initialize_params)
        .await
        .unwrap();

    assert_eq!(
        init_result.capabilities.document_symbol_provider,
        Some(OneOf::Left(true))
    );
}

#[tokio::test]
async fn test_symbols_disabled_by_default() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

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

    let doc_uri = Url::parse("file:///default_sym.zsh").unwrap();
    let text = r#"
my_func() {
    local my_var="hello"
}
alias ll='ls -la'
"#;

    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: text.to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    let sym_res = test_client
        .send_request::<DocumentSymbolRequest>(DocumentSymbolParams {
            text_document: TextDocumentIdentifier {
                uri: doc_uri.clone(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(sym_res.is_none());
}

#[tokio::test]
async fn test_symbols_enabled_via_initialization_options() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
        initialization_options: Some(json!({
            "zshcs": {
                "experimental": {
                    "symbols": true
                }
            }
        })),
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

    let doc_uri = Url::parse("file:///enabled_sym.zsh").unwrap();
    let text = r#"
GLOBAL_VAR="test"

hello() {
    local msg="world"
    echo "$msg"
}

alias gs='git status'
"#;

    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: text.to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    let sym_res = test_client
        .send_request::<DocumentSymbolRequest>(DocumentSymbolParams {
            text_document: TextDocumentIdentifier {
                uri: doc_uri.clone(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(sym_res.is_some());
    if let Some(DocumentSymbolResponse::Nested(symbols)) = sym_res {
        assert_eq!(symbols.len(), 3); // GLOBAL_VAR, hello, gs

        assert_eq!(symbols[0].name, "GLOBAL_VAR");
        assert_eq!(symbols[0].kind, SymbolKind::VARIABLE);

        assert_eq!(symbols[1].name, "hello");
        assert_eq!(symbols[1].kind, SymbolKind::FUNCTION);
        let children = symbols[1].children.as_ref().unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "msg");
        assert_eq!(children[0].kind, SymbolKind::VARIABLE);

        assert_eq!(symbols[2].name, "gs");
        assert_eq!(symbols[2].kind, SymbolKind::OPERATOR);
    } else {
        panic!("Expected DocumentSymbolResponse::Nested");
    }
}

#[tokio::test]
async fn test_symbols_hierarchy_nested_functions_and_locals() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
        initialization_options: Some(json!({
            "zshcs": {
                "experimental": {
                    "symbols": true
                }
            }
        })),
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

    let doc_uri = Url::parse("file:///nested_sym.zsh").unwrap();
    let text = r#"
outer() {
    local a=1

    inner() {
        local b=2
    }

    local c=3
}
"#;

    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: text.to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    let sym_res = test_client
        .send_request::<DocumentSymbolRequest>(DocumentSymbolParams {
            text_document: TextDocumentIdentifier {
                uri: doc_uri.clone(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(sym_res.is_some());
    if let Some(DocumentSymbolResponse::Nested(symbols)) = sym_res {
        assert_eq!(symbols.len(), 1);
        let outer = &symbols[0];
        assert_eq!(outer.name, "outer");
        assert_eq!(outer.kind, SymbolKind::FUNCTION);

        let outer_children = outer.children.as_ref().unwrap();
        assert_eq!(outer_children.len(), 3); // a, inner, c

        assert_eq!(outer_children[0].name, "a");
        assert_eq!(outer_children[0].kind, SymbolKind::VARIABLE);

        let inner = &outer_children[1];
        assert_eq!(inner.name, "inner");
        assert_eq!(inner.kind, SymbolKind::FUNCTION);
        let inner_children = inner.children.as_ref().unwrap();
        assert_eq!(inner_children.len(), 1);
        assert_eq!(inner_children[0].name, "b");
        assert_eq!(inner_children[0].kind, SymbolKind::VARIABLE);

        assert_eq!(outer_children[2].name, "c");
        assert_eq!(outer_children[2].kind, SymbolKind::VARIABLE);
    } else {
        panic!("Expected Nested document symbols");
    }
}

#[tokio::test]
async fn test_symbols_dynamic_toggle_via_did_change_configuration() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    // 1. Initialize with default (disabled)
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

    let doc_uri = Url::parse("file:///toggle_sym.zsh").unwrap();
    let text = r#"
test_func() {
    local v=100
}
"#;

    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: text.to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // Disabled initially
    let res_disabled = test_client
        .send_request::<DocumentSymbolRequest>(DocumentSymbolParams {
            text_document: TextDocumentIdentifier {
                uri: doc_uri.clone(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(res_disabled.is_none());

    // 2. Enable dynamically via workspace/didChangeConfiguration
    test_client
        .send_notification::<DidChangeConfiguration>(DidChangeConfigurationParams {
            settings: json!({
                "zshcs": {
                    "experimental": {
                        "symbols": true
                    }
                }
            }),
        })
        .await;

    let res_enabled = test_client
        .send_request::<DocumentSymbolRequest>(DocumentSymbolParams {
            text_document: TextDocumentIdentifier {
                uri: doc_uri.clone(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(res_enabled.is_some());
    if let Some(DocumentSymbolResponse::Nested(syms)) = res_enabled {
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "test_func");
    }

    // 3. Disable again dynamically
    test_client
        .send_notification::<DidChangeConfiguration>(DidChangeConfigurationParams {
            settings: json!({
                "zshcs": {
                    "experimental": {
                        "symbols": false
                    }
                }
            }),
        })
        .await;

    let res_disabled_again = test_client
        .send_request::<DocumentSymbolRequest>(DocumentSymbolParams {
            text_document: TextDocumentIdentifier {
                uri: doc_uri.clone(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(res_disabled_again.is_none());
}

#[tokio::test]
async fn test_symbols_unopened_document_returns_none() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
        initialization_options: Some(json!({
            "zshcs": {
                "experimental": {
                    "symbols": true
                }
            }
        })),
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

    let unopened_uri = Url::parse("file:///nonexistent.zsh").unwrap();
    let res = test_client
        .send_request::<DocumentSymbolRequest>(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: unopened_uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(res.is_none());
}

#[tokio::test]
async fn test_symbols_flat_and_nested_settings_variants() {
    // Flat experimental format
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
        initialization_options: Some(json!({
            "experimental": {
                "symbols": true
            }
        })),
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

    let doc_uri = Url::parse("file:///flat_settings.zsh").unwrap();
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "fn() { :; }".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    let res = test_client
        .send_request::<DocumentSymbolRequest>(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: doc_uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(res.is_some());
}

mod common;

use common::{TestClient, setup_server_mock as setup_server};
use serde_json::json;
use tower_lsp::lsp_types::{
    ClientCapabilities, DidChangeConfigurationParams, DidOpenTextDocumentParams, HoverContents,
    HoverParams, HoverProviderCapability, InitializeParams, InitializedParams, LogMessageParams,
    Position, Range, TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, Url,
    notification::{DidChangeConfiguration, DidOpenTextDocument, Initialized, LogMessage},
    request::{HoverRequest, Initialize},
};

#[tokio::test]
async fn test_hover_capability_registered_on_initialize() {
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
        init_result.capabilities.hover_provider,
        Some(HoverProviderCapability::Simple(true))
    );
}

#[tokio::test]
async fn test_hover_disabled_by_default() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    // Initialize with default options (hover = false)
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

    let doc_uri = Url::parse("file:///default_hover.zsh").unwrap();
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "setopt promptsubst\nif [[ -f foo ]]; then\nls -la\n".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // Hover request on builtin "setopt"
    let hover_res = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(0, 3),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(
        hover_res.is_none(),
        "Expected hover to return None when experimental hover is disabled"
    );

    // Hover request on reserved word "if"
    let hover_if = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(1, 0),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(
        hover_if.is_none(),
        "Expected hover to return None for 'if' when disabled"
    );
}

#[tokio::test]
async fn test_hover_enabled_builtin_and_reserved_word() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    // Initialize with experimental hover enabled
    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
        capabilities: ClientCapabilities::default(),
        initialization_options: Some(json!({
            "zshcs": {
                "experimental": {
                    "hover": true
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

    let doc_uri = Url::parse("file:///enabled_hover.zsh").unwrap();
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "setopt promptsubst\nif [[ -f foo ]]; then\n  autoload -Uz compinit\nfi\n"
                    .to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // 1. Builtin "setopt" at line 0, char 2
    let hover_setopt = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(0, 2),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(hover_setopt.is_some());
    let hover_setopt_val = hover_setopt.unwrap();
    assert_eq!(
        hover_setopt_val.range,
        Some(Range {
            start: Position::new(0, 0),
            end: Position::new(0, 6),
        })
    );
    if let HoverContents::Markup(markup) = hover_setopt_val.contents {
        assert!(markup.value.contains("`setopt` (Zsh Builtin)"));
        assert!(markup.value.contains("```zsh"));
    } else {
        panic!("Expected HoverContents::Markup");
    }

    // 2. Reserved word "if" at line 1, char 0
    let hover_if = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(1, 0),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(hover_if.is_some());
    let hover_if_val = hover_if.unwrap();
    assert_eq!(
        hover_if_val.range,
        Some(Range {
            start: Position::new(1, 0),
            end: Position::new(1, 2),
        })
    );
    if let HoverContents::Markup(markup) = hover_if_val.contents {
        assert!(markup.value.contains("`if` (Zsh Reserved Word)"));
    } else {
        panic!("Expected HoverContents::Markup");
    }

    // 3. Builtin "autoload" at line 2, char 4
    let hover_autoload = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(2, 4),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(hover_autoload.is_some());
    let hover_autoload_val = hover_autoload.unwrap();
    assert_eq!(
        hover_autoload_val.range,
        Some(Range {
            start: Position::new(2, 2),
            end: Position::new(2, 10),
        })
    );
    if let HoverContents::Markup(markup) = hover_autoload_val.contents {
        assert!(markup.value.contains("`autoload` (Zsh Builtin)"));
    } else {
        panic!("Expected HoverContents::Markup");
    }
}

#[tokio::test]
async fn test_hover_enabled_man_page_fallback() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
        capabilities: ClientCapabilities::default(),
        initialization_options: Some(json!({
            "zshcs": {
                "experimental": {
                    "hover": true
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

    let doc_uri = Url::parse("file:///man_hover.zsh").unwrap();
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "ls -la /tmp\n".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // Hover on 'ls' (external command with man page)
    let hover_ls = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(0, 1),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(hover_ls.is_some());
    let hover_ls_val = hover_ls.unwrap();
    assert_eq!(
        hover_ls_val.range,
        Some(Range {
            start: Position::new(0, 0),
            end: Position::new(0, 2),
        })
    );
    if let HoverContents::Markup(markup) = hover_ls_val.contents {
        assert!(markup.value.starts_with("```text\n"));
        assert!(markup.value.ends_with("\n```"));
        assert!(
            markup.value.to_lowercase().contains("list directory")
                || markup.value.to_lowercase().contains("ls")
        );
    } else {
        panic!("Expected HoverContents::Markup");
    }
}

#[tokio::test]
async fn test_hover_whitespace_and_boundary_conditions() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
        capabilities: ClientCapabilities::default(),
        initialization_options: Some(json!({
            "experimental": {
                "hover": true
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

    let doc_uri = Url::parse("file:///boundaries.zsh").unwrap();
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "   \n   cd   /tmp\n".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // 1. Hover on blank line (line 0, char 1)
    let hover_blank = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(0, 1),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(hover_blank.is_none());

    // 2. Hover on leading space of line 1 (line 1, char 0)
    let hover_space = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(1, 0),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(hover_space.is_none());

    // 3. Hover on space immediately after 'cd' (line 1, char 5)
    let hover_after_word = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(1, 5),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(
        hover_after_word.is_none(),
        "Hover on space immediately following a word must return None"
    );

    // 4. Hover on gap between cd and /tmp (line 1, char 6)
    let hover_gap = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(1, 6),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(hover_gap.is_none());

    // 5. Hover out of line bounds (line 10, char 0)
    let hover_oob_line = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(10, 0),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(hover_oob_line.is_none());

    // 6. Hover on unopened document
    let unopened_uri = Url::parse("file:///not_opened.zsh").unwrap();
    let hover_unopened = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: unopened_uri },
                position: Position::new(0, 0),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(hover_unopened.is_none());
}

#[tokio::test]
async fn test_hover_dynamic_toggle_via_did_change_configuration() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    // Initialize with default (disabled)
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

    let doc_uri = Url::parse("file:///dynamic_toggle.zsh").unwrap();
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "echo 'hello'\nsetopt promptsubst\n".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // Hover is disabled initially
    let hover_init = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(1, 2),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(hover_init.is_none());

    // Dynamically enable hover
    test_client
        .send_notification::<DidChangeConfiguration>(DidChangeConfigurationParams {
            settings: json!({
                "settings": {
                    "zshcs": {
                        "experimental": {
                            "hover": true
                        }
                    }
                }
            }),
        })
        .await;

    // Small yield to let configuration update propagate
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Hover should now succeed
    let hover_enabled = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(1, 2),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(hover_enabled.is_some());

    // Dynamically disable hover
    test_client
        .send_notification::<DidChangeConfiguration>(DidChangeConfigurationParams {
            settings: json!({
                "zshcs": {
                    "experimental": {
                        "hover": false
                    }
                }
            }),
        })
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Hover should now be disabled again
    let hover_disabled = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(1, 2),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(hover_disabled.is_none());
}

#[tokio::test]
async fn test_hover_path_qualified_and_special_builtins() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
        capabilities: ClientCapabilities::default(),
        initialization_options: Some(json!({
            "zshcs": {
                "experimental": {
                    "hover": true
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

    let doc_uri = Url::parse("file:///special_builtins.zsh").unwrap();
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "/bin/echo 'hello'\n/bin/ls -la\n: 'noop'\n. ./script.sh\nnonexistent_xyz123 --flag\n".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // 1. Hover on `/bin/echo` (line 0, char 6 on 'echo') -> resolves to `echo` builtin doc
    let hover_echo = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(0, 6),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(hover_echo.is_some());
    let hover_echo_val = hover_echo.unwrap();
    assert_eq!(
        hover_echo_val.range,
        Some(Range {
            start: Position::new(0, 0),
            end: Position::new(0, 9),
        })
    );
    if let HoverContents::Markup(markup) = hover_echo_val.contents {
        assert!(markup.value.contains("`echo` (Zsh Builtin)"));
    } else {
        panic!("Expected HoverContents::Markup");
    }

    // 2. Hover on `/bin/ls` (line 1, char 6 on 'ls') -> resolves to `ls` man page
    let hover_ls = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(1, 6),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(hover_ls.is_some());
    let hover_ls_val = hover_ls.unwrap();
    assert_eq!(
        hover_ls_val.range,
        Some(Range {
            start: Position::new(1, 0),
            end: Position::new(1, 7),
        })
    );
    if let HoverContents::Markup(markup) = hover_ls_val.contents {
        assert!(markup.value.starts_with("```text\n"));
    } else {
        panic!("Expected HoverContents::Markup");
    }

    // 3. Hover on `:` builtin (line 2, char 0)
    let hover_colon = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(2, 0),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(hover_colon.is_some());
    let hover_colon_val = hover_colon.unwrap();
    assert_eq!(
        hover_colon_val.range,
        Some(Range {
            start: Position::new(2, 0),
            end: Position::new(2, 1),
        })
    );
    if let HoverContents::Markup(markup) = hover_colon_val.contents {
        assert!(markup.value.contains("`:` (Zsh Builtin)"));
    } else {
        panic!("Expected HoverContents::Markup");
    }

    // 4. Hover on `.` builtin (line 3, char 0)
    let hover_dot = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(3, 0),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(hover_dot.is_some());
    let hover_dot_val = hover_dot.unwrap();
    assert_eq!(
        hover_dot_val.range,
        Some(Range {
            start: Position::new(3, 0),
            end: Position::new(3, 1),
        })
    );
    if let HoverContents::Markup(markup) = hover_dot_val.contents {
        assert!(markup.value.contains("`.` (Zsh Builtin)"));
    } else {
        panic!("Expected HoverContents::Markup");
    }

    // 5. Hover on nonexistent command (line 4, char 2) -> None
    let hover_nonexistent = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(4, 2),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(hover_nonexistent.is_none());

    // 6. Hover on flag `--flag` (line 4, char 20) -> None
    let hover_flag = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(4, 20),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(hover_flag.is_none());
}

#[tokio::test]
async fn test_hover_unicode_and_multibyte_document() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
        capabilities: ClientCapabilities::default(),
        initialization_options: Some(json!({
            "zshcs": {
                "experimental": {
                    "hover": true
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

    let doc_uri = Url::parse("file:///unicode_doc.zsh").unwrap();
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "echo 'こんにちは' # テスト 𩸽 🎉\nprint '成功'\n".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // Hover on 'print' at line 1, char 2
    let hover_print = test_client
        .send_request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(1, 2),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(hover_print.is_some());
    let hover_print_val = hover_print.unwrap();
    assert_eq!(
        hover_print_val.range,
        Some(Range {
            start: Position::new(1, 0),
            end: Position::new(1, 5),
        })
    );
    if let HoverContents::Markup(markup) = hover_print_val.contents {
        assert!(markup.value.contains("`print` (Zsh Builtin)"));
    } else {
        panic!("Expected HoverContents::Markup");
    }
}

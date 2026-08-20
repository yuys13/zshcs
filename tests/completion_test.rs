mod common;

use common::{get_completion_items, setup_server, setup_server_with_scripts};
use tower_lsp::lsp_types::{
    CompletionParams, DidChangeTextDocumentParams, Position, Range, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentPositionParams, Url, VersionedTextDocumentIdentifier,
    notification::{DidChangeTextDocument, LogMessage},
    request,
};

#[tokio::test]
async fn test_completion() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///test.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "git s").await;

    let completion_params = CompletionParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: doc_uri.clone(),
            },
            position: Position::new(0, 5),
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: None,
    };

    let response = test_client
        .send_request::<tower_lsp::lsp_types::request::Completion>(completion_params)
        .await
        .unwrap();

    let response = response.expect("Expected completion response");
    let items = get_completion_items(response);
    assert!(!items.is_empty());
    let has_status = items.iter().any(|item| item.label == "status");
    assert!(has_status, "Expected 'status' in completion items");
}

#[tokio::test]
async fn test_completion_consecutive() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = common::TestClient::new(&mut client_stream);

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

    let items1 = get_completion_items(res1);
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

    let items2 = get_completion_items(res2);
    assert!(items2.iter().any(|i| i.label == "status"));
}

#[tokio::test]
async fn test_completion_after_change() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = common::TestClient::new(&mut client_stream);

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
    test_client.read_notification::<LogMessage>().await;

    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(0, 7),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items = get_completion_items(res);
    assert!(items.iter().any(|i| i.label == "status"));
}

#[tokio::test]
async fn test_completion_with_description() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///description.zsh").unwrap();
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

    let items = get_completion_items(res);

    let has_detail = items.iter().any(|i| i.detail.is_some());
    assert!(
        has_detail,
        "Expected at least one completion item to have a description in detail field. Items: {items:?}"
    );
}

#[tokio::test]
async fn test_daemon_crash_tolerance() {
    // Mock capture script that exits immediately
    let (mut client_stream, _server_handle) =
        setup_server_with_scripts("#!/usr/bin/env zsh\nexit 1\n", "");
    let mut test_client = common::TestClient::new(&mut client_stream);
    let doc_uri = Url::parse("file:///crash.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "git ").await;

    let res = test_client
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
        .await;

    assert!(res.is_ok());
    assert!(res.unwrap().is_none());
}

#[tokio::test]
async fn test_daemon_timeout() {
    // Mock capture script that sleeps, preventing an immediate EOC response
    let (mut client_stream, _server_handle) = setup_server_with_scripts(
        "#!/usr/bin/env zsh\nwhile read -r p; do sleep 5; done\n",
        "",
    );
    let mut test_client = common::TestClient::new(&mut client_stream);
    let doc_uri = Url::parse("file:///timeout.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "git ").await;

    let start = std::time::Instant::now();
    let res = test_client
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
        .await;

    let elapsed = start.elapsed().as_millis();
    assert!(elapsed >= 3000, "Completion should wait for the timeout");

    assert!(res.is_ok());
    assert!(res.unwrap().is_none());
}

#[tokio::test]
async fn test_completion_mock_empty_candidates() {
    let mock_script = r#"#!/usr/bin/env zsh
while IFS= read -r line; do
    printf "\x01EOC\x01\n"
done
"#;
    let (mut client_stream, _server_handle) = setup_server_with_scripts(mock_script, "");
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///empty_cand.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "some_cmd ").await;

    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: doc_uri },
                position: Position::new(0, 9),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items = get_completion_items(res);
    assert!(items.is_empty(), "Expected empty candidates list");
}

#[tokio::test]
async fn test_completion_mock_mixed_formats() {
    let mock_script = r#"#!/usr/bin/env zsh
while IFS= read -r line; do
    printf "status\tShow working tree status\n"
    printf "add\n"
    printf "commit\tRecord changes to repository\n"
    printf "diff\n"
    printf "\x01EOC\x01\n"
done
"#;
    let (mut client_stream, _server_handle) = setup_server_with_scripts(mock_script, "");
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///mixed_formats.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "git ").await;

    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: doc_uri },
                position: Position::new(0, 4),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items = get_completion_items(res);
    assert_eq!(items.len(), 4);
    assert_eq!(items[0].label, "status");
    assert_eq!(items[0].detail.as_deref(), Some("Show working tree status"));
    assert_eq!(items[1].label, "add");
    assert_eq!(items[1].detail, None);
    assert_eq!(items[2].label, "commit");
    assert_eq!(
        items[2].detail.as_deref(),
        Some("Record changes to repository")
    );
    assert_eq!(items[3].label, "diff");
    assert_eq!(items[3].detail, None);
}

#[tokio::test]
async fn test_completion_mock_inline_eoc() {
    let mock_script = r#"#!/usr/bin/env zsh
while IFS= read -r line; do
    printf "status\tShow status\n"
    printf "branch\tList branches\x01EOC\x01\n"
done
"#;
    let (mut client_stream, _server_handle) = setup_server_with_scripts(mock_script, "");
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///inline_eoc.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "git ").await;

    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: doc_uri },
                position: Position::new(0, 4),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items = get_completion_items(res);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].label, "status");
    assert_eq!(items[0].detail.as_deref(), Some("Show status"));
    assert_eq!(items[1].label, "branch");
    assert_eq!(items[1].detail.as_deref(), Some("List branches"));
}

#[tokio::test]
async fn test_completion_mock_blank_lines_and_crlf() {
    let mock_script = r#"#!/usr/bin/env zsh
while IFS= read -r line; do
    printf "\r\n"
    printf "status\tShow status\r\n"
    printf "\r\n\r\n"
    printf "log\tShow commit logs\r\n"
    printf "\r\n"
    printf "\x01EOC\x01\r\n"
done
"#;
    let (mut client_stream, _server_handle) = setup_server_with_scripts(mock_script, "");
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///crlf_mock.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "git ").await;

    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: doc_uri },
                position: Position::new(0, 4),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items = get_completion_items(res);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].label, "status");
    assert_eq!(items[0].detail.as_deref(), Some("Show status"));
    assert_eq!(items[1].label, "log");
    assert_eq!(items[1].detail.as_deref(), Some("Show commit logs"));
}

#[tokio::test]
async fn test_completion_mock_unicode_and_emoji() {
    let mock_script = r#"#!/usr/bin/env zsh
while IFS= read -r line; do
    printf "補完候補1\t説明文1\n"
    printf "🎉celebrate\tparty 🎈\n"
    printf "🚀deploy\tデプロイ実行\n"
    printf "\x01EOC\x01\n"
done
"#;
    let (mut client_stream, _server_handle) = setup_server_with_scripts(mock_script, "");
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///unicode_mock.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "test ").await;

    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: doc_uri },
                position: Position::new(0, 5),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items = get_completion_items(res);
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].label, "補完候補1");
    assert_eq!(items[0].detail.as_deref(), Some("説明文1"));
    assert_eq!(items[1].label, "🎉celebrate");
    assert_eq!(items[1].detail.as_deref(), Some("party 🎈"));
    assert_eq!(items[2].label, "🚀deploy");
    assert_eq!(items[2].detail.as_deref(), Some("デプロイ実行"));
}

#[tokio::test]
async fn test_completion_mock_special_chars() {
    let mock_script = r#"#!/usr/bin/env zsh
while IFS= read -r line; do
    printf "%s\t%s\n" "arg'with\"quotes" "desc'with\"quotes"
    printf "%s\t%s\n" 'path\with\backslashes' 'desc\with\backslashes'
    printf "%s\t%s\n" '$VAR' 'env var'
    printf "\x01EOC\x01\n"
done
"#;
    let (mut client_stream, _server_handle) = setup_server_with_scripts(mock_script, "");
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///special_mock.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "test ").await;

    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: doc_uri },
                position: Position::new(0, 5),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items = get_completion_items(res);
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].label, "arg'with\"quotes");
    assert_eq!(items[0].detail.as_deref(), Some("desc'with\"quotes"));
    assert_eq!(items[1].label, "path\\with\\backslashes");
    assert_eq!(items[1].detail.as_deref(), Some("desc\\with\\backslashes"));
    assert_eq!(items[2].label, "$VAR");
    assert_eq!(items[2].detail.as_deref(), Some("env var"));
}

#[tokio::test]
async fn test_completion_mock_stderr_logging() {
    let mock_script = r#"#!/usr/bin/env zsh
while IFS= read -r line; do
    echo "warning: daemon debug warning" >&2
    printf "status\tShow status\n"
    printf "\x01EOC\x01\n"
done
"#;
    let (mut client_stream, _server_handle) = setup_server_with_scripts(mock_script, "");
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///stderr_mock.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "git ").await;

    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: doc_uri },
                position: Position::new(0, 4),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items = get_completion_items(res);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, "status");

    // Check stderr notification was received
    let log_msg = test_client.read_notification::<LogMessage>().await.unwrap();
    assert_eq!(log_msg.typ, tower_lsp::lsp_types::MessageType::WARNING);
    assert!(
        log_msg
            .message
            .contains("capture.zsh stderr: warning: daemon debug warning"),
        "Expected stderr warning log, got: {}",
        log_msg.message
    );
}

#[tokio::test]
async fn test_completion_mock_crash_mid_stream() {
    let mock_script = r#"#!/usr/bin/env zsh
read -r line
printf "candidate1\tdesc1\n"
exit 1
"#;
    let (mut client_stream, _server_handle) = setup_server_with_scripts(mock_script, "");
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///crash_mid.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "git ").await;

    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: doc_uri },
                position: Position::new(0, 4),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await;

    assert!(res.is_ok());
    assert!(res.unwrap().is_none());
}

#[tokio::test]
async fn test_completion_mock_large_candidate_list() {
    let mock_script = r#"#!/usr/bin/env zsh
while IFS= read -r line; do
    for i in {1..500}; do
        printf "cand%d\tdescription %d\n" "$i" "$i"
    done
    printf "\x01EOC\x01\n"
done
"#;
    let (mut client_stream, _server_handle) = setup_server_with_scripts(mock_script, "");
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///large_cand.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "test ").await;

    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: doc_uri },
                position: Position::new(0, 5),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items = get_completion_items(res);
    assert_eq!(items.len(), 500);
    assert_eq!(items[0].label, "cand1");
    assert_eq!(items[0].detail.as_deref(), Some("description 1"));
    assert_eq!(items[499].label, "cand500");
    assert_eq!(items[499].detail.as_deref(), Some("description 500"));
}

#[tokio::test]
async fn test_completion_unopened_document() {
    let (mut client_stream, _server_handle) = setup_server_with_scripts(
        "#!/bin/zsh\nwhile read -r line; do\n  printf \"\\x01EOC\\x01\\n\"\ndone\n",
        "",
    );
    let mut test_client = common::TestClient::new(&mut client_stream);

    let initialize_params = tower_lsp::lsp_types::InitializeParams::default();
    test_client
        .send_request::<tower_lsp::lsp_types::request::Initialize>(initialize_params)
        .await
        .unwrap();
    test_client
        .send_notification::<tower_lsp::lsp_types::notification::Initialized>(
            tower_lsp::lsp_types::InitializedParams {},
        )
        .await;

    let doc_uri = Url::parse("file:///unopened.zsh").unwrap();
    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: doc_uri },
                position: Position::new(0, 0),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await;

    assert!(res.is_ok());
    assert!(res.unwrap().is_none());
}

#[tokio::test]
async fn test_completion_out_of_bounds_position() {
    let (mut client_stream, _server_handle) = setup_server_with_scripts(
        "#!/bin/zsh\nwhile read -r line; do\n  printf \"\\x01EOC\\x01\\n\"\ndone\n",
        "",
    );
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///out_of_bounds.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "git ").await;

    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: doc_uri },
                position: Position::new(100, 0),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await;

    assert!(res.is_ok());
    assert!(res.unwrap().is_none());
}

#[tokio::test]
async fn test_completion_multiline_document() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///multiline.zsh").unwrap();
    let doc_content = "#!/usr/bin/env zsh\ngit s\nls -";
    test_client.init_and_open(&doc_uri, doc_content).await;

    // Line 1, char 5: "git s" -> completion on "git s"
    let res1 = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(1, 5),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items1 = get_completion_items(res1);
    assert!(items1.iter().any(|i| i.label == "status"));

    // Line 2, char 4: "ls -" -> completion on "ls -"
    let res2 = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(2, 4),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items2 = get_completion_items(res2);
    assert!(items2.iter().any(|i| i.label.starts_with('-')));

    // Line 1, char 0: line start -> prefix ""
    let res3 = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(1, 0),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await;
    assert!(res3.is_ok());
}

#[tokio::test]
async fn test_completion_multibyte_prefix() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///multibyte_prefix.zsh").unwrap();
    // Japanese comment on line 0, "git s" on line 1
    let doc_content = "# 日本語コメント\ngit s";
    test_client.init_and_open(&doc_uri, doc_content).await;

    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(1, 5),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items = get_completion_items(res);
    assert!(items.iter().any(|i| i.label == "status"));

    // Multi-byte character on the same line before command
    // "echo 'こんにちは' && git s"
    // UTF-16 code units: 6 ("echo '") + 5 ("こんにちは") + 11 ("' && git s") = 22
    test_client
        .send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier::new(doc_uri.clone(), 2),
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "echo 'こんにちは' && git s".to_string(),
            }],
        })
        .await;
    test_client.read_notification::<LogMessage>().await;

    let res2 = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: doc_uri },
                position: Position::new(0, 22),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items2 = get_completion_items(res2);
    assert!(items2.iter().any(|i| i.label == "status"));
}

#[tokio::test]
async fn test_completion_middle_of_word() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///middle_word.zsh").unwrap();
    test_client
        .init_and_open(&doc_uri, "git status --short")
        .await;

    // Position (0, 6) -> prefix is "git st"
    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: doc_uri },
                position: Position::new(0, 6),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items = get_completion_items(res);
    assert!(items.iter().any(|i| i.label == "status"));
}

#[tokio::test]
async fn test_completion_crlf_document() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///crlf_doc.zsh").unwrap();
    let doc_content = "echo 1\r\ngit s\r\necho 2";
    test_client.init_and_open(&doc_uri, doc_content).await;

    // Line 1, char 5: "git s"
    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: doc_uri },
                position: Position::new(1, 5),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items = get_completion_items(res);
    assert!(items.iter().any(|i| i.label == "status"));
}

#[tokio::test]
async fn test_completion_concurrent_requests() {
    let mock_capture = r#"#!/bin/zsh
while read -r line; do
  printf "status\tshow working tree status\x01EOC\x01\n"
done
"#;
    let handles: Vec<_> = (0..5)
        .map(|i| {
            tokio::spawn(async move {
                let (mut client_stream, _server_handle) =
                    setup_server_with_scripts(mock_capture, "");
                let mut test_client = common::TestClient::new(&mut client_stream);
                let doc_uri = Url::parse(&format!("file:///concurrent_{i}.zsh")).unwrap();
                test_client.init_and_open(&doc_uri, "git s").await;

                let res = test_client
                    .send_request::<request::Completion>(CompletionParams {
                        text_document_position: TextDocumentPositionParams {
                            text_document: TextDocumentIdentifier { uri: doc_uri },
                            position: Position::new(0, 5),
                        },
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                        context: None,
                    })
                    .await
                    .unwrap()
                    .unwrap();

                let items = get_completion_items(res);
                assert!(items.iter().any(|item| item.label == "status"));
            })
        })
        .collect();

    for handle in handles {
        handle.await.unwrap();
    }
}

#[tokio::test]
async fn test_daemon_crash_recovery_on_next_request() {
    // Mock script that succeeds once, then exits on next request, and succeeds on fresh spawn
    let mock_script = r#"#!/usr/bin/env zsh
count=0
while IFS= read -r line; do
    if [[ $line == input:* ]]; then
        count=$((count + 1))
        if [[ $count -eq 1 ]]; then
            printf "recovered_status\tShow status\x01EOC\x01\n"
        else
            exit 1
        fi
    fi
done
"#;
    let (mut client_stream, _server_handle) = setup_server_with_scripts(mock_script, "");
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///recovery.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "git ").await;

    // 1. First request succeeds
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
        .unwrap();

    let items1 = get_completion_items(res1.expect("First request should succeed"));
    assert_eq!(items1[0].label, "recovered_status");

    // 2. Second request causes the daemon to exit 1 (crash)
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
        .await;

    // Res2 is None (or handled error)
    assert!(res2.is_ok());
    assert!(res2.unwrap().is_none());

    // 3. Third request: Supervisor should automatically restart the daemon and succeed
    let res3 = test_client
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
        .unwrap();

    let items3 =
        get_completion_items(res3.expect("Third request should succeed after auto-restart"));
    assert_eq!(items3[0].label, "recovered_status");
}

#[tokio::test]
async fn test_daemon_crash_between_requests_auto_restart() {
    // Mock script that exits immediately after sending first completion response
    let mock_script = r#"#!/usr/bin/env zsh
while IFS= read -r line; do
    if [[ $line == input:* ]]; then
        printf "resp_item\tdesc\x01EOC\x01\n"
        exit 0
    fi
done
"#;
    let (mut client_stream, _server_handle) = setup_server_with_scripts(mock_script, "");
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///exit_between.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "git ").await;

    // 1. First request
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
        .unwrap();

    let items1 = get_completion_items(res1.expect("First request should succeed"));
    assert_eq!(items1[0].label, "resp_item");

    // Wait a brief moment to ensure process exit has occurred
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // 2. Second request: Process already exited between requests. Supervisor must detect and restart!
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
        .unwrap();

    let items2 =
        get_completion_items(res2.expect("Second request should succeed via supervisor restart"));
    assert_eq!(items2[0].label, "resp_item");
}

#[tokio::test]
async fn test_completion_working_directory_sync_protocol() {
    // Mock script that echoes received commands and candidates based on chdir
    let mock_script = r#"#!/usr/bin/env zsh
current_dir="none"
while IFS= read -r line; do
    if [[ $line == chdir:* ]]; then
        current_dir="${line#chdir:}"
    elif [[ $line == input:* ]]; then
        printf "%s_item\tdir: %s\x01EOC\x01\n" "$current_dir" "$current_dir"
    fi
done
"#;
    let (mut client_stream, _server_handle) = setup_server_with_scripts(mock_script, "");
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri_1 = Url::parse("file:///project/dir_one/file1.zsh").unwrap();
    let doc_uri_2 = Url::parse("file:///project/dir_two/file2.zsh").unwrap();

    test_client.init_and_open(&doc_uri_1, "ls ").await;

    // 1. First request for dir_one -> should receive chdir:/project/dir_one
    let res1 = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri_1.clone(),
                },
                position: Position::new(0, 3),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items1 = get_completion_items(res1);
    assert_eq!(items1[0].label, "/project/dir_one_item");

    // 2. Second request for the same document in dir_one -> cwd hasn't changed, still dir_one
    let res2 = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri_1.clone(),
                },
                position: Position::new(0, 3),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items2 = get_completion_items(res2);
    assert_eq!(items2[0].label, "/project/dir_one_item");

    // 3. Third request for document in dir_two -> should receive chdir:/project/dir_two
    test_client
        .send_notification::<tower_lsp::lsp_types::notification::DidOpenTextDocument>(
            tower_lsp::lsp_types::DidOpenTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentItem {
                    uri: doc_uri_2.clone(),
                    language_id: "zsh".to_string(),
                    version: 1,
                    text: "ls ".to_string(),
                },
            },
        )
        .await;
    test_client
        .read_notification::<tower_lsp::lsp_types::notification::LogMessage>()
        .await;

    let res3 = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri_2.clone(),
                },
                position: Position::new(0, 3),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items3 = get_completion_items(res3);
    assert_eq!(items3[0].label, "/project/dir_two_item");
}

#[tokio::test]
async fn test_completion_relative_path_and_working_directory_real() {
    let temp_dir_a = tempfile::tempdir().unwrap();
    let temp_dir_b = tempfile::tempdir().unwrap();

    // Create unique files in each directory
    let file_a_name = "alpha_unique_target.txt";
    let file_b_name = "beta_unique_target.txt";
    std::fs::write(temp_dir_a.path().join(file_a_name), "hello a").unwrap();
    std::fs::write(temp_dir_b.path().join(file_b_name), "hello b").unwrap();

    let script_a = temp_dir_a.path().join("script_a.zsh");
    let script_b = temp_dir_b.path().join("script_b.zsh");
    let uri_a = Url::from_file_path(&script_a).unwrap();
    let uri_b = Url::from_file_path(&script_b).unwrap();

    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = common::TestClient::new(&mut client_stream);

    // 1. Open script in dir A and complete `cat alp`
    test_client.init_and_open(&uri_a, "cat alp").await;

    let res_a = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri_a.clone() },
                position: Position::new(0, 7),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items_a = get_completion_items(res_a);
    let has_alpha = items_a
        .iter()
        .any(|i| i.label.contains("alpha_unique_target"));
    let has_beta_in_a = items_a
        .iter()
        .any(|i| i.label.contains("beta_unique_target"));
    assert!(
        has_alpha,
        "Expected alpha file in completions for dir A. Got: {items_a:?}"
    );
    assert!(
        !has_beta_in_a,
        "Beta file should NOT be in completions for dir A"
    );

    // 2. Open script in dir B and complete `cat bet`
    test_client
        .send_notification::<tower_lsp::lsp_types::notification::DidOpenTextDocument>(
            tower_lsp::lsp_types::DidOpenTextDocumentParams {
                text_document: tower_lsp::lsp_types::TextDocumentItem {
                    uri: uri_b.clone(),
                    language_id: "zsh".to_string(),
                    version: 1,
                    text: "cat bet".to_string(),
                },
            },
        )
        .await;
    test_client
        .read_notification::<tower_lsp::lsp_types::notification::LogMessage>()
        .await;

    let res_b = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri_b.clone() },
                position: Position::new(0, 7),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items_b = get_completion_items(res_b);
    let has_beta = items_b
        .iter()
        .any(|i| i.label.contains("beta_unique_target"));
    let has_alpha_in_b = items_b
        .iter()
        .any(|i| i.label.contains("alpha_unique_target"));
    assert!(
        has_beta,
        "Expected beta file in completions for dir B. Got: {items_b:?}"
    );
    assert!(
        !has_alpha_in_b,
        "Alpha file should NOT be in completions for dir B"
    );
}

#[tokio::test]
async fn test_completion_chdir_spaces_and_unicode_path() {
    let temp_parent = tempfile::tempdir().unwrap();
    let special_dir = temp_parent.path().join("フォルダ with space and 特殊文字");
    std::fs::create_dir_all(&special_dir).unwrap();

    let target_file = "unicode_target_123.txt";
    std::fs::write(special_dir.join(target_file), "content").unwrap();

    let script_file = special_dir.join("main.zsh");
    let script_uri = Url::from_file_path(&script_file).unwrap();

    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = common::TestClient::new(&mut client_stream);

    test_client.init_and_open(&script_uri, "cat uni").await;

    let res = test_client
        .send_request::<request::Completion>(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: script_uri.clone(),
                },
                position: Position::new(0, 7),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .unwrap()
        .unwrap();

    let items = get_completion_items(res);
    let has_target = items.iter().any(|i| i.label.contains("unicode_target_123"));
    assert!(
        has_target,
        "Expected unicode target file in completion in special directory. Got: {items:?}"
    );
}

#[tokio::test]
async fn test_completion_chdir_non_file_uri() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("untitled:untitled-1").unwrap();
    test_client.init_and_open(&doc_uri, "git s").await;

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

    let items = get_completion_items(res);
    assert!(items.iter().any(|i| i.label == "status"));
}

#[tokio::test]
async fn test_completion_chdir_resync_after_daemon_restart() {
    // Mock script that requires chdir before input; fails if chdir wasn't received in this process
    let mock_script = r#"#!/usr/bin/env zsh
has_chdir=0
count=0
while IFS= read -r line; do
    if [[ $line == chdir:* ]]; then
        has_chdir=1
    elif [[ $line == input:* ]]; then
        if [[ $has_chdir -eq 1 ]]; then
            count=$((count + 1))
            if [[ $count -eq 1 ]]; then
                printf "item_ok\tdesc\x01EOC\x01\n"
            else
                exit 1
            fi
        else
            printf "item_no_chdir\tdesc\x01EOC\x01\n"
        fi
    fi
done
"#;
    let (mut client_stream, _server_handle) = setup_server_with_scripts(mock_script, "");
    let mut test_client = common::TestClient::new(&mut client_stream);

    let doc_uri = Url::parse("file:///workspace/project/script.zsh").unwrap();
    test_client.init_and_open(&doc_uri, "git ").await;

    // 1. First request receives chdir and succeeds
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

    let items1 = get_completion_items(res1);
    assert_eq!(items1[0].label, "item_ok");

    // 2. Second request causes crash
    let _ = test_client
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
        .await;

    // 3. Third request after supervisor restart: new daemon must receive chdir again
    let res3 = test_client
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

    let items3 = get_completion_items(res3);
    assert_eq!(items3[0].label, "item_ok");
}

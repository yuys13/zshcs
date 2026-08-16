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

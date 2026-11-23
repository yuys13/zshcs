mod common;

use common::setup_server_mock as setup_server;
use serde_json::json;
use tower_lsp::lsp_types::{
    ClientCapabilities, DidOpenTextDocumentParams, HoverContents, HoverProviderCapability,
    InitializeParams, LogMessageParams, Position, Range, TextDocumentItem, Url,
    notification::{DidOpenTextDocument, LogMessage},
    request::Initialize,
};

#[tokio::test]
async fn test_hover_capability_registered_on_initialize() {
    let mut harness = setup_server();
    let mut test_client = harness.client();

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
    let mut harness = setup_server();
    let mut test_client = harness.client();

    // Initialize with default options (hover = false)
    test_client.init().await;

    let doc_uri = Url::parse("file:///default_hover.zsh").unwrap();
    test_client
        .open_document(
            &doc_uri,
            "setopt promptsubst\nif [[ -f foo ]]; then\nls -la\n",
        )
        .await;

    // Hover request on builtin "setopt"
    let hover_res = test_client.hover(&doc_uri, 0, 3).await.unwrap();

    assert!(
        hover_res.is_none(),
        "Expected hover to return None when experimental hover is disabled"
    );

    // Hover request on reserved word "if"
    let hover_if = test_client.hover(&doc_uri, 1, 0).await.unwrap();

    assert!(
        hover_if.is_none(),
        "Expected hover to return None for 'if' when disabled"
    );
}

#[tokio::test]
async fn test_hover_enabled_builtin_and_reserved_word() {
    let mut harness = setup_server();
    let mut test_client = harness.client();

    // Initialize with experimental hover enabled
    test_client
        .init_with_options(Some(json!({
            "zshcs": {
                "experimental": {
                    "hover": true
                }
            }
        })))
        .await;

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
    let hover_setopt = test_client.hover(&doc_uri, 0, 2).await.unwrap();

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
    let hover_if = test_client.hover(&doc_uri, 1, 0).await.unwrap();

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
    let hover_autoload = test_client.hover(&doc_uri, 2, 4).await.unwrap();

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
    let mut harness = setup_server();
    let mut test_client = harness.client();

    test_client
        .init_with_options(Some(json!({
            "zshcs": {
                "experimental": {
                    "hover": true
                }
            }
        })))
        .await;

    let doc_uri = Url::parse("file:///man_hover.zsh").unwrap();
    test_client.open_document(&doc_uri, "git status\n").await;

    // Hover on 'git' (external command with man page)
    let hover_git = test_client.hover(&doc_uri, 0, 1).await.unwrap();

    assert!(
        hover_git.is_some(),
        "Expected hover info for 'git' via man page"
    );
    let hover_git_val = hover_git.unwrap();
    assert_eq!(
        hover_git_val.range,
        Some(Range {
            start: Position::new(0, 0),
            end: Position::new(0, 3),
        })
    );
    if let HoverContents::Markup(markup) = hover_git_val.contents {
        assert!(markup.value.starts_with("```text\n"));
        assert!(markup.value.ends_with("\n```"));
        assert!(
            markup.value.to_lowercase().contains("git")
                || markup.value.to_lowercase().contains("repository")
        );
    } else {
        panic!("Expected HoverContents::Markup");
    }
}

#[tokio::test]
async fn test_hover_local_function_variable_and_alias() {
    let mut harness = setup_server();
    let mut test_client = harness.client();

    test_client
        .init_with_options(Some(json!({
            "zshcs": {
                "experimental": {
                    "hover": true
                }
            }
        })))
        .await;

    let doc_uri = Url::parse("file:///local_symbols_hover.zsh").unwrap();
    let text = concat!(
        "# Deploys the app to target env.\n",
        "deploy_app() {\n",
        "  echo \"$1\"\n",
        "}\n",
        "# Target deployment environment\n",
        "DEPLOY_ENV=\"prod\"\n",
        "alias dlog='docker logs -f'\n",
        "deploy_app \"$DEPLOY_ENV\"\n",
        "dlog\n",
    );
    test_client.open_document(&doc_uri, text).await;

    // 1. Hover on local function call `deploy_app` (line 7, char 2)
    let hover_fn = test_client
        .hover(&doc_uri, 7, 2)
        .await
        .unwrap()
        .expect("Expected hover for local function deploy_app");
    if let HoverContents::Markup(markup) = hover_fn.contents {
        assert!(markup.value.contains("(Local Function)"));
        assert!(markup.value.contains("Deploys the app to target env."));
    } else {
        panic!("Expected HoverContents::Markup");
    }

    // 2. Hover on local variable reference `$DEPLOY_ENV` (line 7, char 14)
    let hover_var = test_client
        .hover(&doc_uri, 7, 14)
        .await
        .unwrap()
        .expect("Expected hover for local variable DEPLOY_ENV");
    if let HoverContents::Markup(markup) = hover_var.contents {
        assert!(markup.value.contains("(Local Variable)"));
        assert!(markup.value.contains("DEPLOY_ENV=\"prod\""));
        assert!(markup.value.contains("Target deployment environment"));
    } else {
        panic!("Expected HoverContents::Markup");
    }

    // 3. Hover on local alias `dlog` (line 8, char 1)
    let hover_alias = test_client
        .hover(&doc_uri, 8, 1)
        .await
        .unwrap()
        .expect("Expected hover for local alias dlog");
    if let HoverContents::Markup(markup) = hover_alias.contents {
        assert!(markup.value.contains("(Local Alias)"));
        assert!(markup.value.contains("alias dlog='docker logs -f'"));
    } else {
        panic!("Expected HoverContents::Markup");
    }
}

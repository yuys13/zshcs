mod common;

use common::setup_server_mock as setup_server;
use serde_json::json;
use tempfile::tempdir;
use tower_lsp::lsp_types::{
    ClientCapabilities, GotoDefinitionResponse, InitializeParams, Location, OneOf, Position, Range,
    Url, request::Initialize,
};

#[tokio::test]
async fn test_definition_capability_registered_on_initialize() {
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
        init_result.capabilities.definition_provider,
        Some(OneOf::Left(true))
    );
}

#[tokio::test]
async fn test_definition_disabled_by_default() {
    let mut harness = setup_server();
    let mut test_client = harness.client();

    // Initialize with default options (definition = false)
    test_client.init().await;

    let doc_uri = Url::parse("file:///default_def.zsh").unwrap();
    let text = r#"
my_func() {
    MY_VAR="hello"
}

my_func
echo "$MY_VAR"
source ./helper.zsh
"#;

    test_client.open_document(&doc_uri, text).await;

    // 1. Definition request on function invocation "my_func" at line 5, char 2
    let def_fn = test_client.goto_definition(&doc_uri, 5, 2).await.unwrap();

    assert!(
        def_fn.is_none(),
        "Expected definition to return None when experimental definition is disabled"
    );

    // 2. Definition request on variable "$MY_VAR" at line 6, char 8
    let def_var = test_client.goto_definition(&doc_uri, 6, 8).await.unwrap();

    assert!(
        def_var.is_none(),
        "Expected definition to return None for variable when disabled"
    );

    // 3. Definition request on source statement at line 7, char 5
    let def_src = test_client.goto_definition(&doc_uri, 7, 5).await.unwrap();

    assert!(
        def_src.is_none(),
        "Expected definition to return None for source when disabled"
    );
}

#[tokio::test]
async fn test_definition_enabled_function_jump() {
    let mut harness = setup_server();
    let mut test_client = harness.client();

    test_client
        .init_with_options(Some(json!({
            "zshcs": {
                "experimental": {
                    "definition": true
                }
            }
        })))
        .await;

    let doc_uri = Url::parse("file:///func_def.zsh").unwrap();
    let text = r#"
# Function definitions
greet_user() {
    echo "Hello, $1"
}

function process_data {
    echo "Processing data..."
}

# Invocations
greet_user "Alice"
process_data
"#;

    test_client.open_document(&doc_uri, text).await;

    // 1. Jump from invocation `greet_user "Alice"` (line 11, char 4)
    let def_greet = test_client.goto_definition(&doc_uri, 11, 4).await.unwrap();

    assert!(def_greet.is_some(), "Expected definition for greet_user");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_greet {
        assert_eq!(loc.uri, doc_uri);
        assert_eq!(loc.range.start, Position::new(2, 0));
        assert_eq!(loc.range.end, Position::new(2, 10));
    } else {
        panic!("Expected Scalar GotoDefinitionResponse");
    }

    // 2. Jump from invocation `process_data` (line 12, char 3)
    let def_process = test_client.goto_definition(&doc_uri, 12, 3).await.unwrap();

    assert!(
        def_process.is_some(),
        "Expected definition for process_data"
    );
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_process {
        assert_eq!(loc.uri, doc_uri);
        assert_eq!(loc.range.start, Position::new(6, 9));
        assert_eq!(loc.range.end, Position::new(6, 21));
    } else {
        panic!("Expected Scalar GotoDefinitionResponse");
    }
}

#[tokio::test]
async fn test_definition_enabled_variable_jump() {
    let mut harness = setup_server();
    let mut test_client = harness.client();

    test_client
        .init_with_options(Some(json!({
            "zshcs": {
                "experimental": {
                    "definition": true
                }
            }
        })))
        .await;

    let doc_uri = Url::parse("file:///var_def.zsh").unwrap();
    let text = r#"
# Variable declarations
PORT=8080
export API_KEY="secret_key"
typeset -g LOG_LEVEL="info"
local CACHE_SIZE=1024

echo "Server on $PORT"
echo "Key is ${API_KEY}"
echo "Level: $LOG_LEVEL"
echo "Cache: $CACHE_SIZE"
"#;

    test_client.open_document(&doc_uri, text).await;

    // 1. Jump to PORT from `$PORT` (line 7, char 16)
    let def_port = test_client.goto_definition(&doc_uri, 7, 16).await.unwrap();

    assert!(def_port.is_some(), "Expected definition for PORT");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_port {
        assert_eq!(loc.uri, doc_uri);
        assert_eq!(loc.range.start, Position::new(2, 0));
        assert_eq!(loc.range.end, Position::new(2, 4));
    } else {
        panic!("Expected Scalar GotoDefinitionResponse");
    }

    // 2. Jump to API_KEY from `${API_KEY}` (line 8, char 14)
    let def_key = test_client.goto_definition(&doc_uri, 8, 14).await.unwrap();

    assert!(def_key.is_some(), "Expected definition for API_KEY");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_key {
        assert_eq!(loc.uri, doc_uri);
        assert_eq!(loc.range.start, Position::new(3, 7));
        assert_eq!(loc.range.end, Position::new(3, 14));
    } else {
        panic!("Expected Scalar GotoDefinitionResponse");
    }

    // 3. Jump to LOG_LEVEL from `$LOG_LEVEL` (line 9, char 14)
    let def_level = test_client.goto_definition(&doc_uri, 9, 14).await.unwrap();

    assert!(def_level.is_some(), "Expected definition for LOG_LEVEL");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_level {
        assert_eq!(loc.uri, doc_uri);
        assert_eq!(loc.range.start, Position::new(4, 11));
        assert_eq!(loc.range.end, Position::new(4, 20));
    } else {
        panic!("Expected Scalar GotoDefinitionResponse");
    }

    // 4. Jump to CACHE_SIZE from `$CACHE_SIZE` (line 10, char 14)
    let def_cache = test_client.goto_definition(&doc_uri, 10, 14).await.unwrap();

    assert!(def_cache.is_some(), "Expected definition for CACHE_SIZE");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_cache {
        assert_eq!(loc.uri, doc_uri);
        assert_eq!(loc.range.start, Position::new(5, 6));
        assert_eq!(loc.range.end, Position::new(5, 16));
    } else {
        panic!("Expected Scalar GotoDefinitionResponse");
    }
}

#[tokio::test]
async fn test_definition_enabled_source_jump() {
    let mut harness = setup_server();
    let mut test_client = harness.client();

    let temp_dir = tempdir().unwrap();
    let helper_path = temp_dir.path().join("helper.zsh");
    std::fs::write(&helper_path, "# Helper script\nhelper_func() { : }\n").unwrap();

    let main_path = temp_dir.path().join("main.zsh");
    std::fs::write(&main_path, "source ./helper.zsh\n. \"./helper.zsh\"\n").unwrap();

    let main_uri = Url::from_file_path(&main_path).unwrap();
    let helper_uri = Url::from_file_path(helper_path.canonicalize().unwrap()).unwrap();

    test_client
        .init_with_options(Some(json!({
            "zshcs": {
                "experimental": {
                    "definition": true
                }
            }
        })))
        .await;

    test_client
        .open_document(&main_uri, "source ./helper.zsh\n. \"./helper.zsh\"\n")
        .await;

    // 1. Jump from line 0 `source ./helper.zsh` (char 10)
    let def_source = test_client.goto_definition(&main_uri, 0, 10).await.unwrap();

    assert!(def_source.is_some(), "Expected source file definition");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_source {
        assert_eq!(loc.uri, helper_uri);
        assert_eq!(
            loc.range,
            Range::new(Position::new(0, 0), Position::new(0, 0))
        );
    } else {
        panic!("Expected Scalar GotoDefinitionResponse");
    }

    // 2. Jump from line 1 `. "./helper.zsh"` (char 0 on command)
    let def_dot = test_client.goto_definition(&main_uri, 1, 0).await.unwrap();

    assert!(def_dot.is_some(), "Expected . source file definition");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_dot {
        assert_eq!(loc.uri, helper_uri);
        assert_eq!(
            loc.range,
            Range::new(Position::new(0, 0), Position::new(0, 0))
        );
    } else {
        panic!("Expected Scalar GotoDefinitionResponse");
    }
}

#[tokio::test]
async fn test_definition_unopened_document_and_edge_cases() {
    let mut harness = setup_server();
    let mut test_client = harness.client();

    test_client
        .init_with_options(Some(json!({
            "experimental": {
                "definition": true
            }
        })))
        .await;

    // 1. Definition on unopened document
    let unopened_uri = Url::parse("file:///unopened.zsh").unwrap();
    let def_unopened = test_client
        .goto_definition(&unopened_uri, 0, 0)
        .await
        .unwrap();
    assert!(def_unopened.is_none());

    // 2. Definition on opened document with whitespace / unknown command
    let doc_uri = Url::parse("file:///edge_cases.zsh").unwrap();
    test_client
        .open_document(&doc_uri, "   \necho 'hello'\nsource ./nonexistent.zsh\n")
        .await;

    // Cursor on whitespace
    let def_space = test_client.goto_definition(&doc_uri, 0, 1).await.unwrap();
    assert!(def_space.is_none());

    // Cursor on builtin "echo"
    let def_echo = test_client.goto_definition(&doc_uri, 1, 2).await.unwrap();
    assert!(def_echo.is_none());

    // Cursor on nonexistent source file
    let def_nonexistent_src = test_client.goto_definition(&doc_uri, 2, 10).await.unwrap();
    assert!(def_nonexistent_src.is_none());
}

#[tokio::test]
async fn test_definition_alias_and_local_variable_scoping() {
    let mut harness = setup_server();
    let mut test_client = harness.client();

    test_client
        .init_with_options(Some(json!({
            "zshcs": {
                "experimental": {
                    "definition": true
                }
            }
        })))
        .await;

    let doc_uri = Url::parse("file:///alias_and_scope.zsh").unwrap();
    let text = concat!(
        "alias dlog='docker logs -f'\n",
        "MODE=\"global\"\n",
        "worker() {\n",
        "  local MODE=\"local\"\n",
        "  echo \"$MODE\"\n",
        "}\n",
        "dlog\n",
        "echo \"$MODE\"\n",
    );

    test_client.open_document(&doc_uri, text).await;

    // 1. Jump to alias `dlog` (line 6, char 1) -> line 0, chars 6..10
    let def_alias = test_client.goto_definition(&doc_uri, 6, 1).await.unwrap();
    assert_eq!(
        def_alias,
        Some(GotoDefinitionResponse::Scalar(Location {
            uri: doc_uri.clone(),
            range: Range::new(Position::new(0, 6), Position::new(0, 10)),
        }))
    );

    // 2. Jump to local `$MODE` inside `worker()` (line 4, char 10) -> line 3, chars 8..12
    let def_local = test_client.goto_definition(&doc_uri, 4, 10).await.unwrap();
    assert_eq!(
        def_local,
        Some(GotoDefinitionResponse::Scalar(Location {
            uri: doc_uri.clone(),
            range: Range::new(Position::new(3, 8), Position::new(3, 12)),
        }))
    );

    // 3. Jump to global `$MODE` at top-level (line 7, char 9) -> line 1, chars 0..4
    let def_global = test_client.goto_definition(&doc_uri, 7, 9).await.unwrap();
    assert_eq!(
        def_global,
        Some(GotoDefinitionResponse::Scalar(Location {
            uri: Url::parse("file:///alias_and_scope.zsh").unwrap(),
            range: Range::new(Position::new(1, 0), Position::new(1, 4)),
        }))
    );
}

#[tokio::test]
async fn test_definition_across_sourced_scripts_and_open_buffers() {
    let mut harness = setup_server();
    let mut test_client = harness.client();

    let dir = tempdir().unwrap();
    let helper_path = dir.path().join("helper.zsh");
    let main_path = dir.path().join("main.zsh");
    std::fs::write(
        &helper_path,
        concat!(
            "helper_fn() { : }\n",
            "HELPER_VAR=\"hello\"\n",
            "alias helper_alias='echo hi'\n",
        ),
    )
    .unwrap();

    let helper_canon_uri = Url::from_file_path(helper_path.canonicalize().unwrap()).unwrap();
    let helper_uri = Url::from_file_path(&helper_path).unwrap();
    let main_uri = Url::from_file_path(&main_path).unwrap();

    test_client
        .init_with_options(Some(json!({
            "zshcs": {
                "experimental": {
                    "definition": true
                }
            }
        })))
        .await;

    let main_text = concat!(
        "source ./helper.zsh\n",
        "helper_fn\n",
        "echo \"$HELPER_VAR\"\n",
        "helper_alias\n",
        "unsaved_helper_fn\n",
    );
    test_client.open_document(&main_uri, main_text).await;

    // 1. Jump to function in sourced helper.zsh on disk
    let def_fn = test_client.goto_definition(&main_uri, 1, 2).await.unwrap();
    assert_eq!(
        def_fn,
        Some(GotoDefinitionResponse::Scalar(Location {
            uri: helper_canon_uri.clone(),
            range: Range::new(Position::new(0, 0), Position::new(0, 9)),
        }))
    );

    // 2. Jump to variable in sourced helper.zsh on disk
    let def_var = test_client.goto_definition(&main_uri, 2, 10).await.unwrap();
    assert_eq!(
        def_var,
        Some(GotoDefinitionResponse::Scalar(Location {
            uri: helper_canon_uri.clone(),
            range: Range::new(Position::new(1, 0), Position::new(1, 10)),
        }))
    );

    // 3. Jump to alias in sourced helper.zsh on disk
    let def_alias = test_client.goto_definition(&main_uri, 3, 2).await.unwrap();
    assert_eq!(
        def_alias,
        Some(GotoDefinitionResponse::Scalar(Location {
            uri: helper_canon_uri.clone(),
            range: Range::new(Position::new(2, 6), Position::new(2, 18)),
        }))
    );

    // 4. Open helper.zsh with an unsaved function and verify DocumentManager buffer is used
    test_client
        .open_document(&helper_uri, "unsaved_helper_fn() { : }\n")
        .await;

    let def_unsaved = test_client.goto_definition(&main_uri, 4, 2).await.unwrap();
    assert_eq!(
        def_unsaved,
        Some(GotoDefinitionResponse::Scalar(Location {
            uri: helper_canon_uri,
            range: Range::new(Position::new(0, 0), Position::new(0, 17)),
        }))
    );
}

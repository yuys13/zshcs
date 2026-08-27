mod common;

use common::{TestClient, setup_server_mock as setup_server};
use serde_json::json;
use tempfile::tempdir;
use tower_lsp::lsp_types::{
    ClientCapabilities, DidChangeConfigurationParams, DidOpenTextDocumentParams,
    GotoDefinitionParams, GotoDefinitionResponse, InitializeParams, InitializedParams,
    LogMessageParams, OneOf, Position, Range, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, Url,
    notification::{DidChangeConfiguration, DidOpenTextDocument, Initialized, LogMessage},
    request::{GotoDefinition, Initialize},
};

#[tokio::test]
async fn test_definition_capability_registered_on_initialize() {
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
        init_result.capabilities.definition_provider,
        Some(OneOf::Left(true))
    );
}

#[tokio::test]
async fn test_definition_disabled_by_default() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    // Initialize with default options (definition = false)
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

    let doc_uri = Url::parse("file:///default_def.zsh").unwrap();
    let text = r#"
my_func() {
    MY_VAR="hello"
}

my_func
echo "$MY_VAR"
source ./helper.zsh
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

    // 1. Definition request on function invocation "my_func" at line 5, char 2
    let def_fn = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(5, 2),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(
        def_fn.is_none(),
        "Expected definition to return None when experimental definition is disabled"
    );

    // 2. Definition request on variable "$MY_VAR" at line 6, char 8
    let def_var = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(6, 8),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(
        def_var.is_none(),
        "Expected definition to return None for variable when disabled"
    );

    // 3. Definition request on source statement at line 7, char 5
    let def_src = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(7, 5),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(
        def_src.is_none(),
        "Expected definition to return None for source when disabled"
    );
}

#[tokio::test]
async fn test_definition_enabled_function_jump() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
        capabilities: ClientCapabilities::default(),
        initialization_options: Some(json!({
            "zshcs": {
                "experimental": {
                    "definition": true
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

    // 1. Jump from invocation `greet_user "Alice"` (line 11, char 4)
    let def_greet = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(11, 4),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(def_greet.is_some(), "Expected definition for greet_user");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_greet {
        assert_eq!(loc.uri, doc_uri);
        assert_eq!(loc.range.start, Position::new(2, 0));
        assert_eq!(loc.range.end, Position::new(2, 10));
    } else {
        panic!("Expected Scalar GotoDefinitionResponse");
    }

    // 2. Jump from invocation `process_data` (line 12, char 3)
    let def_process = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(12, 3),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

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
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
        capabilities: ClientCapabilities::default(),
        initialization_options: Some(json!({
            "zshcs": {
                "experimental": {
                    "definition": true
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

    // 1. Jump to PORT from `$PORT` (line 7, char 16)
    let def_port = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(7, 16),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(def_port.is_some(), "Expected definition for PORT");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_port {
        assert_eq!(loc.uri, doc_uri);
        assert_eq!(loc.range.start, Position::new(2, 0));
        assert_eq!(loc.range.end, Position::new(2, 4));
    } else {
        panic!("Expected Scalar GotoDefinitionResponse");
    }

    // 2. Jump to API_KEY from `${API_KEY}` (line 8, char 14)
    let def_key = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(8, 14),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(def_key.is_some(), "Expected definition for API_KEY");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_key {
        assert_eq!(loc.uri, doc_uri);
        assert_eq!(loc.range.start, Position::new(3, 7));
        assert_eq!(loc.range.end, Position::new(3, 14));
    } else {
        panic!("Expected Scalar GotoDefinitionResponse");
    }

    // 3. Jump to LOG_LEVEL from `$LOG_LEVEL` (line 9, char 14)
    let def_level = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(9, 14),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(def_level.is_some(), "Expected definition for LOG_LEVEL");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_level {
        assert_eq!(loc.uri, doc_uri);
        assert_eq!(loc.range.start, Position::new(4, 11));
        assert_eq!(loc.range.end, Position::new(4, 20));
    } else {
        panic!("Expected Scalar GotoDefinitionResponse");
    }

    // 4. Jump to CACHE_SIZE from `$CACHE_SIZE` (line 10, char 14)
    let def_cache = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(10, 14),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

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
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let temp_dir = tempdir().unwrap();
    let helper_path = temp_dir.path().join("helper.zsh");
    std::fs::write(&helper_path, "# Helper script\nhelper_func() { : }\n").unwrap();

    let main_path = temp_dir.path().join("main.zsh");
    std::fs::write(&main_path, "source ./helper.zsh\n. \"./helper.zsh\"\n").unwrap();

    let main_uri = Url::from_file_path(&main_path).unwrap();
    let helper_uri = Url::from_file_path(helper_path.canonicalize().unwrap()).unwrap();

    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
        capabilities: ClientCapabilities::default(),
        initialization_options: Some(json!({
            "zshcs": {
                "experimental": {
                    "definition": true
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

    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: main_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "source ./helper.zsh\n. \"./helper.zsh\"\n".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // 1. Jump from line 0 `source ./helper.zsh` (char 10)
    let def_source = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: main_uri.clone(),
                },
                position: Position::new(0, 10),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

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
    let def_dot = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: main_uri.clone(),
                },
                position: Position::new(1, 0),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

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
async fn test_definition_dynamic_toggle_via_did_change_configuration() {
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

    let doc_uri = Url::parse("file:///dynamic_toggle_def.zsh").unwrap();
    let text = "func_toggle() { : }\nfunc_toggle\n";

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

    // Definition is disabled initially -> None
    let def_init = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(1, 3),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(def_init.is_none());

    // Dynamically enable definition
    test_client
        .send_notification::<DidChangeConfiguration>(DidChangeConfigurationParams {
            settings: json!({
                "settings": {
                    "zshcs": {
                        "experimental": {
                            "definition": true
                        }
                    }
                }
            }),
        })
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Definition should now succeed
    let def_enabled = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(1, 3),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(def_enabled.is_some());

    // Dynamically disable definition
    test_client
        .send_notification::<DidChangeConfiguration>(DidChangeConfigurationParams {
            settings: json!({
                "zshcs": {
                    "experimental": {
                        "definition": false
                    }
                }
            }),
        })
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Definition should now be disabled again -> None
    let def_disabled = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(1, 3),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(def_disabled.is_none());
}

#[tokio::test]
async fn test_definition_unopened_document_and_edge_cases() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
        capabilities: ClientCapabilities::default(),
        initialization_options: Some(json!({
            "experimental": {
                "definition": true
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

    // 1. Definition on unopened document
    let unopened_uri = Url::parse("file:///unopened.zsh").unwrap();
    let def_unopened = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: unopened_uri },
                position: Position::new(0, 0),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(def_unopened.is_none());

    // 2. Definition on opened document with whitespace / unknown command
    let doc_uri = Url::parse("file:///edge_cases.zsh").unwrap();
    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: doc_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: "   \necho 'hello'\nsource ./nonexistent.zsh\n".to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // Cursor on whitespace
    let def_space = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(0, 1),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(def_space.is_none());

    // Cursor on builtin "echo"
    let def_echo = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(1, 2),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(def_echo.is_none());

    // Cursor on nonexistent source file
    let def_nonexistent_src = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(2, 10),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(def_nonexistent_src.is_none());
}

#[tokio::test]
async fn test_definition_surrogate_and_multibyte_integration() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
        capabilities: ClientCapabilities::default(),
        initialization_options: Some(json!({
            "zshcs": {
                "experimental": {
                    "definition": true
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

    let doc_uri = Url::parse("file:///multibyte_def.zsh").unwrap();
    let text = "# 🍣 sushi line\nこんにちは() { : }\n🍣_var=\"delicious\"\n\nこんにちは\necho \"$🍣_var\"\n";

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

    // Jump to こんにちは on line 4, char 2
    let def_cjk = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(4, 2),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(def_cjk.is_some(), "Expected definition for こんにちは");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_cjk {
        assert_eq!(loc.uri, doc_uri);
        assert_eq!(loc.range.start, Position::new(1, 0));
        assert_eq!(loc.range.end, Position::new(1, 5));
    } else {
        panic!("Expected Scalar GotoDefinitionResponse");
    }

    // Jump to 🍣_var on line 5, char 8
    let def_emoji = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(5, 8),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(def_emoji.is_some(), "Expected definition for 🍣_var");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_emoji {
        assert_eq!(loc.uri, doc_uri);
        assert_eq!(loc.range.start, Position::new(2, 0));
        assert_eq!(loc.range.end, Position::new(2, 6));
    } else {
        panic!("Expected Scalar GotoDefinitionResponse");
    }
}

#[tokio::test]
async fn test_definition_advanced_syntax_and_multi_statement() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
        capabilities: ClientCapabilities::default(),
        initialization_options: Some(json!({
            "zshcs": {
                "experimental": {
                    "definition": true
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

    let doc_uri = Url::parse("file:///advanced_def.zsh").unwrap();
    let text = r#"
# Advanced variable and function definitions
BASE_DIR="/opt/app"; APP_PORT=9000
local -r USER_NAME="admin"

echo "Path: $BASE_DIR/config.json"
echo "URL: http://localhost:$APP_PORT/api"
echo "Greeting: ${USER_NAME:-guest}"
echo "Upper: ${(U)USER_NAME}"
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

    // 1. Jump to BASE_DIR from `$BASE_DIR/config.json` on line 5, char 14
    let def_dir = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(5, 14),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(def_dir.is_some(), "Expected definition for BASE_DIR");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_dir {
        assert_eq!(loc.uri, doc_uri);
        assert_eq!(loc.range.start, Position::new(2, 0));
        assert_eq!(loc.range.end, Position::new(2, 8));
    } else {
        panic!("Expected Scalar response for BASE_DIR");
    }

    // 2. Jump to APP_PORT from `$APP_PORT/api` on line 6, char 30 (on APP_PORT)
    let def_port = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(6, 30),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(def_port.is_some(), "Expected definition for APP_PORT");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_port {
        assert_eq!(loc.uri, doc_uri);
        assert_eq!(loc.range.start, Position::new(2, 21));
        assert_eq!(loc.range.end, Position::new(2, 29));
    } else {
        panic!("Expected Scalar response for APP_PORT");
    }

    // 3. Jump to USER_NAME from `${USER_NAME:-guest}` on line 7, char 20
    let def_user = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(7, 20),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(def_user.is_some(), "Expected definition for USER_NAME");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_user {
        assert_eq!(loc.uri, doc_uri);
        assert_eq!(loc.range.start, Position::new(3, 9));
        assert_eq!(loc.range.end, Position::new(3, 18));
    } else {
        panic!("Expected Scalar response for USER_NAME");
    }

    // 4. Jump to USER_NAME from ${(U)USER_NAME} on line 8, char 20
    let def_upper = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: doc_uri.clone(),
                },
                position: Position::new(8, 20),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(
        def_upper.is_some(),
        "Expected definition for ${{(U)USER_NAME}}"
    );
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_upper {
        assert_eq!(loc.uri, doc_uri);
        assert_eq!(loc.range.start, Position::new(3, 9));
        assert_eq!(loc.range.end, Position::new(3, 18));
    } else {
        panic!("Expected Scalar response for ${{(U)USER_NAME}}");
    }
}

#[tokio::test]
async fn test_definition_nested_expansions_loops_and_compound_source() {
    let (mut client_stream, _server_handle) = setup_server();
    let mut test_client = TestClient::new(&mut client_stream);

    let temp_dir = tempdir().unwrap();
    let lib_path = temp_dir.path().join("sublib.zsh");
    std::fs::write(&lib_path, "# Sublib\n").unwrap();
    let lib_uri = Url::from_file_path(lib_path.canonicalize().unwrap()).unwrap();

    let main_path = temp_dir.path().join("main_script.zsh");
    let main_uri = Url::from_file_path(&main_path).unwrap();

    let initialize_params = InitializeParams {
        process_id: Some(123),
        root_uri: None,
        capabilities: ClientCapabilities::default(),
        initialization_options: Some(json!({
            "zshcs": {
                "experimental": {
                    "definition": true
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

    let text = r#"
# Script setup
DEFAULT_USER="guest"
ACTIVE_USER="admin"

echo "User: ${ACTIVE_USER:-${DEFAULT_USER}}"

for entry in one two three; do
    echo "$entry"
done

[ -f ./sublib.zsh ] && source ./sublib.zsh
"#;

    test_client
        .send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: main_uri.clone(),
                language_id: "zsh".to_string(),
                version: 1,
                text: text.to_string(),
            },
        })
        .await;
    let _: Option<LogMessageParams> = test_client.read_notification::<LogMessage>().await;

    // 1. Jump to DEFAULT_USER inside nested expansion `${ACTIVE_USER:-${DEFAULT_USER}}` (line 5, char 32)
    let def_def_user = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: main_uri.clone(),
                },
                position: Position::new(5, 32),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(def_def_user.is_some());
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_def_user {
        assert_eq!(loc.uri, main_uri);
        assert_eq!(loc.range.start, Position::new(2, 0));
        assert_eq!(loc.range.end, Position::new(2, 12));
    } else {
        panic!("Expected Scalar response for DEFAULT_USER");
    }

    // 2. Jump to for-loop variable `entry` (line 8, char 12)
    let def_entry = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: main_uri.clone(),
                },
                position: Position::new(8, 12),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(def_entry.is_some());
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_entry {
        assert_eq!(loc.uri, main_uri);
        assert_eq!(loc.range.start, Position::new(7, 4));
        assert_eq!(loc.range.end, Position::new(7, 9));
    } else {
        panic!("Expected Scalar response for loop variable entry");
    }

    // 3. Jump to compound `source` statement (line 11, char 28)
    let def_src = test_client
        .send_request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: main_uri.clone(),
                },
                position: Position::new(11, 28),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    assert!(def_src.is_some());
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_src {
        assert_eq!(loc.uri, lib_uri);
        assert_eq!(loc.range.start, Position::new(0, 0));
    } else {
        panic!("Expected Scalar response for compound source statement");
    }
}

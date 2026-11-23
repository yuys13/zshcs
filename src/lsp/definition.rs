use tower_lsp::lsp_types::{GotoDefinitionResponse, Location, Position, Range, Url};

use crate::analysis::model::DocumentIndex;
use crate::analysis::parser::parse_document;
use crate::analysis::scope::{
    CursorTarget, extract_cursor_target, resolve_alias_defs, resolve_function_defs,
    resolve_variable_defs,
};
pub use crate::analysis::syntax::{
    DeclarationToken, StatementSpan, byte_to_utf16_col, is_func_ident_char, is_ident_char,
    is_var_ident_char, split_declaration_tokens, split_line_statements, utf16_col_to_byte,
};
use crate::analysis::workspace::find_in_sourced_documents;
pub use crate::analysis::workspace::{resolve_open_sourced_uri, resolve_source_path};
use crate::document::DocumentManager;

/// Target identified under the cursor for definition search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefinitionTarget {
    /// Explicit variable reference (e.g. `$VAR`, `${VAR}`).
    Variable(String),
    /// Identifier that could be either a function or a variable (e.g. `my_func`, `my_var`).
    FunctionOrVariable(String),
}

/// Extracts a source / `.` script path if the cursor is positioned on a `source` or `.` statement.
pub fn extract_source_path_at_position(text: &str, position: Position) -> Option<String> {
    let index = parse_document(text);
    match extract_cursor_target(text, &index, position) {
        Some(CursorTarget::SourcePath { path, .. }) => Some(path),
        _ => None,
    }
}

/// Extracts the definition target under the cursor along with its UTF-16 range.
pub fn extract_word_and_target_at_position(
    text: &str,
    position: Position,
) -> Option<(DefinitionTarget, Range)> {
    let index = parse_document(text);
    match extract_cursor_target(text, &index, position)? {
        CursorTarget::VariableRef { name, range } => {
            Some((DefinitionTarget::Variable(name), range))
        }
        CursorTarget::Word { text, range } => {
            Some((DefinitionTarget::FunctionOrVariable(text), range))
        }
        CursorTarget::SourcePath { .. } => None,
    }
}

/// Scans the document for shell function definitions matching `func_name`.
pub fn scan_function_definitions(text: &str, uri: &Url, func_name: &str) -> Vec<Location> {
    let index = parse_document(text);
    resolve_function_defs(&index, func_name)
        .into_iter()
        .map(|f| Location {
            uri: uri.clone(),
            range: f.name_range,
        })
        .collect()
}

/// Scans the document for shell variable definitions and assignments matching `var_name`.
pub fn scan_variable_definitions(text: &str, uri: &Url, var_name: &str) -> Vec<Location> {
    let index = parse_document(text);
    index
        .variables
        .iter()
        .filter(|v| v.name == var_name)
        .map(|v| Location {
            uri: uri.clone(),
            range: v.name_range,
        })
        .collect()
}

/// Scans the document for `alias` definitions matching `alias_name`.
pub fn scan_alias_definitions(text: &str, uri: &Url, alias_name: &str) -> Vec<Location> {
    let index = parse_document(text);
    resolve_alias_defs(&index, alias_name)
        .into_iter()
        .map(|a| Location {
            uri: uri.clone(),
            range: a.name_range,
        })
        .collect()
}

fn locations_to_response(locs: Vec<Location>) -> Option<GotoDefinitionResponse> {
    if locs.is_empty() {
        None
    } else if locs.len() == 1 {
        Some(GotoDefinitionResponse::Scalar(
            locs.into_iter().next().unwrap(),
        ))
    } else {
        Some(GotoDefinitionResponse::Array(locs))
    }
}

fn resolve_target_in_index(
    index: &DocumentIndex,
    uri: &Url,
    target: &DefinitionTarget,
    cursor_pos: Option<Position>,
) -> Vec<Location> {
    match target {
        DefinitionTarget::Variable(var_name) => resolve_variable_defs(index, var_name, cursor_pos)
            .into_iter()
            .map(|v| Location {
                uri: uri.clone(),
                range: v.name_range,
            })
            .collect(),
        DefinitionTarget::FunctionOrVariable(ident) => {
            let fn_locs: Vec<Location> = resolve_function_defs(index, ident)
                .into_iter()
                .map(|f| Location {
                    uri: uri.clone(),
                    range: f.name_range,
                })
                .collect();
            if !fn_locs.is_empty() {
                fn_locs
            } else {
                let alias_locs: Vec<Location> = resolve_alias_defs(index, ident)
                    .into_iter()
                    .map(|a| Location {
                        uri: uri.clone(),
                        range: a.name_range,
                    })
                    .collect();
                if !alias_locs.is_empty() {
                    alias_locs
                } else {
                    resolve_variable_defs(index, ident, cursor_pos)
                        .into_iter()
                        .map(|v| Location {
                            uri: uri.clone(),
                            range: v.name_range,
                        })
                        .collect()
                }
            }
        }
    }
}

/// Resolves the definition location(s) for the token at `position` within `text`.
pub fn find_definition(
    text: &str,
    uri: &Url,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    find_definition_with_manager(text, uri, position, None)
}

/// Resolves the definition location(s) for the token at `position` within `text`,
/// searching sourced scripts (`source` / `.`) using open buffers in `doc_manager`
/// or bounded disk reads when no definition is found in the current document.
pub fn find_definition_with_manager(
    text: &str,
    uri: &Url,
    position: Position,
    doc_manager: Option<&DocumentManager>,
) -> Option<GotoDefinitionResponse> {
    let index = parse_document(text);
    find_definition_with_index(text, &index, uri, position, doc_manager)
}

/// Resolves the definition location(s) for the token at `position` using a pre-parsed `DocumentIndex`.
pub fn find_definition_with_index(
    text: &str,
    index: &DocumentIndex,
    uri: &Url,
    position: Position,
    doc_manager: Option<&DocumentManager>,
) -> Option<GotoDefinitionResponse> {
    // 1. Check if on `source` or `.` statement line
    let cursor_target = extract_cursor_target(text, index, position)?;
    let target = match cursor_target {
        CursorTarget::SourcePath { path, .. } => {
            if let Some(target_url) = resolve_source_path(&path, uri)
                .or_else(|| resolve_open_sourced_uri(&path, uri, doc_manager))
            {
                return Some(GotoDefinitionResponse::Scalar(Location {
                    uri: target_url,
                    range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                }));
            }
            return None;
        }
        CursorTarget::VariableRef { name, .. } => DefinitionTarget::Variable(name),
        CursorTarget::Word { text, .. } => DefinitionTarget::FunctionOrVariable(text),
    };

    // 2. Search within the current document first
    let current_locs = resolve_target_in_index(index, uri, &target, Some(position));
    if !current_locs.is_empty() {
        return locations_to_response(current_locs);
    }

    // 3. Search across sourced scripts (short-circuiting on first match)
    find_in_sourced_documents(index, uri, position, doc_manager, true, |sourced| {
        let sourced_locs = resolve_target_in_index(&sourced.index, &sourced.uri, &target, None);
        locations_to_response(sourced_locs)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::workspace::prioritized_source_paths;
    use rstest::rstest;
    use tempfile::tempdir;

    #[rstest]
    #[case("hello world", 0, 0)]
    #[case("hello world", 5, 5)]
    #[case("こんにちは", 0, 0)]
    #[case("こんにちは", 3, 1)]
    #[case("こんにちは", 6, 2)]
    #[case("こんにちは", 15, 5)]
    #[case("🍣🍺 emoji", 0, 0)]
    #[case("🍣🍺 emoji", 4, 2)] // 🍣 is 2 utf16 units
    #[case("🍣🍺 emoji", 8, 4)] // 🍺 is 2 utf16 units
    #[case("🍣🍺 emoji", 9, 5)] // space
    fn test_byte_and_utf16_conversions(
        #[case] line: &str,
        #[case] byte_idx: usize,
        #[case] expected_u16: u32,
    ) {
        assert_eq!(byte_to_utf16_col(line, byte_idx), expected_u16);
        assert_eq!(utf16_col_to_byte(line, expected_u16), byte_idx);
    }

    #[test]
    fn test_split_declaration_tokens() {
        let input = "-r -g FOO=123 BAR=\"hello \\\"world\\\"\" BAZ=(a b c) # comment";
        let tokens = split_declaration_tokens(input);
        let token_texts: Vec<&str> = tokens.iter().map(|t| t.text).collect();
        assert_eq!(
            token_texts,
            vec![
                "-r",
                "-g",
                "FOO=123",
                "BAR=\"hello \\\"world\\\"\"",
                "BAZ=(a b c)"
            ]
        );
    }

    #[test]
    fn test_split_line_statements() {
        let line = "export A=1; B=2 && local C=3; echo \"hello; world\" # trailing comment";
        let stmts = split_line_statements(line);
        let texts: Vec<&str> = stmts.iter().map(|s| s.text).collect();
        assert_eq!(
            texts,
            vec![
                "export A=1",
                " B=2 ",
                " local C=3",
                " echo \"hello; world\" "
            ]
        );
    }

    #[test]
    fn test_function_definition_scan_patterns() {
        let text = r#"
# Commented function
# ignored_func() { : }

my_func() {
    echo "inside func"
}

function other_func {
    echo "inside other"
}

function func_with_parens() {
    echo "inside func_with_parens"
}

my-kebab-func () {
    return 0
}

spaced_parens ( ) {
    return 0
}

function spaced_fn_parens ( ) {
    return 0
}

func_a() { : }; func_b() { : }
"#;
        let uri = Url::parse("file:///test.zsh").unwrap();

        // 1. my_func
        let res1 = scan_function_definitions(text, &uri, "my_func");
        assert_eq!(res1.len(), 1);
        assert_eq!(res1[0].range.start, Position::new(4, 0));
        assert_eq!(res1[0].range.end, Position::new(4, 7));

        // 2. other_func
        let res2 = scan_function_definitions(text, &uri, "other_func");
        assert_eq!(res2.len(), 1);
        assert_eq!(res2[0].range.start, Position::new(8, 9));
        assert_eq!(res2[0].range.end, Position::new(8, 19));

        // 3. func_with_parens
        let res3 = scan_function_definitions(text, &uri, "func_with_parens");
        assert_eq!(res3.len(), 1);
        assert_eq!(res3[0].range.start, Position::new(12, 9));
        assert_eq!(res3[0].range.end, Position::new(12, 25));

        // 4. my-kebab-func
        let res4 = scan_function_definitions(text, &uri, "my-kebab-func");
        assert_eq!(res4.len(), 1);
        assert_eq!(res4[0].range.start, Position::new(16, 0));
        assert_eq!(res4[0].range.end, Position::new(16, 13));

        // 5. spaced_parens ( )
        let res5 = scan_function_definitions(text, &uri, "spaced_parens");
        assert_eq!(res5.len(), 1);
        assert_eq!(res5[0].range.start, Position::new(20, 0));
        assert_eq!(res5[0].range.end, Position::new(20, 13));

        // 6. spaced_fn_parens ( )
        let res6 = scan_function_definitions(text, &uri, "spaced_fn_parens");
        assert_eq!(res6.len(), 1);
        assert_eq!(res6[0].range.start, Position::new(24, 9));
        assert_eq!(res6[0].range.end, Position::new(24, 25));

        // 7. func_b on multi-statement line
        let res7 = scan_function_definitions(text, &uri, "func_b");
        assert_eq!(res7.len(), 1);
        assert_eq!(res7[0].range.start, Position::new(28, 16));
        assert_eq!(res7[0].range.end, Position::new(28, 22));

        // 8. ignored_func (comment)
        let res8 = scan_function_definitions(text, &uri, "ignored_func");
        assert!(res8.is_empty());
    }

    #[test]
    fn test_variable_definition_scan_patterns() {
        let text = r#"
# Commented variable
# IGNORED=1

SIMPLE_VAR="hello"
export EXPORTED_VAR=123
typeset -g GLOBAL_VAR="foo"
local LOCAL_VAR=456
readonly CONSTANT_VAR="readonly_val"
typeset -A MAP_VAR
local V1 V2=10 V3="val3"
APPEND_VAR+=("extra")
MAP_VAR[key]="val"
MULTI_A=1; MULTI_B=2; export MULTI_C=3
local ESCAPED="foo \"quoted\"" FOO_NEXT=99
"#;
        let uri = Url::parse("file:///test.zsh").unwrap();

        // 1. SIMPLE_VAR
        let res1 = scan_variable_definitions(text, &uri, "SIMPLE_VAR");
        assert_eq!(res1.len(), 1);
        assert_eq!(res1[0].range.start, Position::new(4, 0));
        assert_eq!(res1[0].range.end, Position::new(4, 10));

        // 2. EXPORTED_VAR
        let res2 = scan_variable_definitions(text, &uri, "EXPORTED_VAR");
        assert_eq!(res2.len(), 1);
        assert_eq!(res2[0].range.start, Position::new(5, 7));
        assert_eq!(res2[0].range.end, Position::new(5, 19));

        // 3. GLOBAL_VAR
        let res3 = scan_variable_definitions(text, &uri, "GLOBAL_VAR");
        assert_eq!(res3.len(), 1);
        assert_eq!(res3[0].range.start, Position::new(6, 11));
        assert_eq!(res3[0].range.end, Position::new(6, 21));

        // 4. MAP_VAR (both declaration and element assignment)
        let res4 = scan_variable_definitions(text, &uri, "MAP_VAR");
        assert_eq!(res4.len(), 2);
        assert_eq!(res4[0].range.start, Position::new(9, 11));
        assert_eq!(res4[0].range.end, Position::new(9, 18));
        assert_eq!(res4[1].range.start, Position::new(12, 0));
        assert_eq!(res4[1].range.end, Position::new(12, 7));

        // 5. V2 from multi-declaration
        let res5 = scan_variable_definitions(text, &uri, "V2");
        assert_eq!(res5.len(), 1);
        assert_eq!(res5[0].range.start, Position::new(10, 9));
        assert_eq!(res5[0].range.end, Position::new(10, 11));

        // 6. APPEND_VAR
        let res6 = scan_variable_definitions(text, &uri, "APPEND_VAR");
        assert_eq!(res6.len(), 1);
        assert_eq!(res6[0].range.start, Position::new(11, 0));
        assert_eq!(res6[0].range.end, Position::new(11, 10));

        // 7. MULTI_B and MULTI_C on same line
        let res7 = scan_variable_definitions(text, &uri, "MULTI_B");
        assert_eq!(res7.len(), 1);
        assert_eq!(res7[0].range.start, Position::new(13, 11));
        assert_eq!(res7[0].range.end, Position::new(13, 18));

        let res8 = scan_variable_definitions(text, &uri, "MULTI_C");
        assert_eq!(res8.len(), 1);
        assert_eq!(res8[0].range.start, Position::new(13, 29));
        assert_eq!(res8[0].range.end, Position::new(13, 36));

        // 8. FOO_NEXT after escaped quote in declaration
        let res9 = scan_variable_definitions(text, &uri, "FOO_NEXT");
        assert_eq!(res9.len(), 1);
        assert_eq!(res9[0].range.start, Position::new(14, 31));
        assert_eq!(res9[0].range.end, Position::new(14, 39));

        // 9. IGNORED (comment)
        let res10 = scan_variable_definitions(text, &uri, "IGNORED");
        assert!(res10.is_empty());
    }

    #[test]
    fn test_extract_source_path_and_resolution() {
        let temp = tempdir().unwrap();
        let helper_path = temp.path().join("helper.zsh");
        std::fs::write(&helper_path, "echo 'helper'\n").unwrap();

        let doc_path = temp.path().join("main.zsh");
        std::fs::write(&doc_path, "source ./helper.zsh\n").unwrap();
        let doc_uri = Url::from_file_path(&doc_path).unwrap();

        let text = "source ./helper.zsh\n. \"./helper.zsh\"\n   source 'helper.zsh'\n";

        // Line 0: source ./helper.zsh
        let p0 = extract_source_path_at_position(text, Position::new(0, 10));
        assert_eq!(p0, Some("./helper.zsh".to_string()));

        let resolved = resolve_source_path("./helper.zsh", &doc_uri);
        assert!(resolved.is_some());
        assert_eq!(
            resolved.unwrap(),
            Url::from_file_path(helper_path.canonicalize().unwrap()).unwrap()
        );

        // Line 1: . "./helper.zsh"
        let p1 = extract_source_path_at_position(text, Position::new(1, 0));
        assert_eq!(p1, Some("./helper.zsh".to_string()));

        // Line 2:    source 'helper.zsh'
        let p2 = extract_source_path_at_position(text, Position::new(2, 5));
        assert_eq!(p2, Some("helper.zsh".to_string()));

        // Nonexistent file resolution
        assert_eq!(resolve_source_path("./nonexistent.zsh", &doc_uri), None);
    }

    #[test]
    fn test_find_definition_full_flow() {
        let text = r#"
setup_env() {
    MY_PORT=8080
}

start_server() {
    setup_env
    echo "Listening on $MY_PORT or ${MY_PORT}"
    echo "Host: $MY_PORT/api and $MY_PORT:8080"
}
"#;
        let uri = Url::parse("file:///server.zsh").unwrap();

        // 1. Jump to setup_env from invocation on line 6
        let def_fn = find_definition(text, &uri, Position::new(6, 6)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def_fn {
            assert_eq!(loc.range.start, Position::new(1, 0));
            assert_eq!(loc.range.end, Position::new(1, 9));
        } else {
            panic!("Expected Scalar response");
        }

        // 2. Jump to MY_PORT from `$MY_PORT` on line 7
        let def_var1 = find_definition(text, &uri, Position::new(7, 25)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def_var1 {
            assert_eq!(loc.range.start, Position::new(2, 4));
            assert_eq!(loc.range.end, Position::new(2, 11));
        } else {
            panic!("Expected Scalar response");
        }

        // 3. Jump to MY_PORT from `${MY_PORT}` on line 7
        let def_var2 = find_definition(text, &uri, Position::new(7, 38)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def_var2 {
            assert_eq!(loc.range.start, Position::new(2, 4));
            assert_eq!(loc.range.end, Position::new(2, 11));
        } else {
            panic!("Expected Scalar response");
        }

        // 4. Jump to MY_PORT from `$MY_PORT/api` on line 8 (char 18 is on MY_PORT)
        let def_var3 = find_definition(text, &uri, Position::new(8, 18)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def_var3 {
            assert_eq!(loc.range.start, Position::new(2, 4));
            assert_eq!(loc.range.end, Position::new(2, 11));
        } else {
            panic!("Expected Scalar response for $MY_PORT in path");
        }

        // 5. Jump to MY_PORT from `$MY_PORT:8080` on line 8 (char 35 is on MY_PORT)
        let def_var4 = find_definition(text, &uri, Position::new(8, 35)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def_var4 {
            assert_eq!(loc.range.start, Position::new(2, 4));
            assert_eq!(loc.range.end, Position::new(2, 11));
        } else {
            panic!("Expected Scalar response for $MY_PORT with colon");
        }

        // 6. Cursor on empty space -> None
        assert!(find_definition(text, &uri, Position::new(0, 0)).is_none());
        assert!(find_definition(text, &uri, Position::new(7, 0)).is_none());

        // 7. Unknown function/variable -> None
        assert!(find_definition(text, &uri, Position::new(7, 10)).is_none());
    }

    #[test]
    fn test_find_definition_parameter_expansion_variants() {
        let text = r#"
NAME="alice"
PORT=3000
ITEMS=(1 2 3)

echo "${NAME:-bob}"
echo "${(U)NAME}"
echo "${#ITEMS}"
echo "$NAME-suffix"
echo "$NAME.txt"
"#;
        let uri = Url::parse("file:///params.zsh").unwrap();

        // 1. ${NAME:-bob}
        let d1 = find_definition(text, &uri, Position::new(5, 8)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = d1 {
            assert_eq!(loc.range.start, Position::new(1, 0));
        } else {
            panic!("Expected scalar for ${{NAME:-bob}}");
        }

        // 2. ${(U)NAME}
        let d2 = find_definition(text, &uri, Position::new(6, 11)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = d2 {
            assert_eq!(loc.range.start, Position::new(1, 0));
        } else {
            panic!("Expected scalar for ${{(U)NAME}}");
        }

        // 3. ${#ITEMS}
        let d3 = find_definition(text, &uri, Position::new(7, 8)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = d3 {
            assert_eq!(loc.range.start, Position::new(3, 0));
        } else {
            panic!("Expected scalar for ${{#ITEMS}}");
        }

        // 4. $NAME-suffix
        let d4 = find_definition(text, &uri, Position::new(8, 7)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = d4 {
            assert_eq!(loc.range.start, Position::new(1, 0));
        } else {
            panic!("Expected scalar for $NAME-suffix");
        }

        // 5. $NAME.txt
        let d5 = find_definition(text, &uri, Position::new(9, 7)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = d5 {
            assert_eq!(loc.range.start, Position::new(1, 0));
        } else {
            panic!("Expected scalar for $NAME.txt");
        }
    }

    #[test]
    fn test_find_definition_unicode_multibyte() {
        let text = "こんにちは() {\n    echo \"hello\"\n}\n\nこんにちは\n";
        let uri = Url::parse("file:///cjk.zsh").unwrap();

        // Jump to function with Japanese name
        let def = find_definition(text, &uri, Position::new(4, 2)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def {
            assert_eq!(loc.range.start, Position::new(0, 0));
            assert_eq!(loc.range.end, Position::new(0, 5)); // 5 UTF-16 units for こんにちは
        } else {
            panic!("Expected Scalar response");
        }
    }

    #[test]
    fn test_find_definition_multiple_definitions_returns_array() {
        let text = r#"
VAR="initial"
VAR="reassigned"
VAR="final"
echo "$VAR"
"#;
        let uri = Url::parse("file:///multi.zsh").unwrap();

        let def = find_definition(text, &uri, Position::new(4, 7)).unwrap();
        if let GotoDefinitionResponse::Array(locs) = def {
            assert_eq!(locs.len(), 3);
            assert_eq!(locs[0].range.start, Position::new(1, 0));
            assert_eq!(locs[1].range.start, Position::new(2, 0));
            assert_eq!(locs[2].range.start, Position::new(3, 0));
        } else {
            panic!("Expected Array response for multiple definitions");
        }
    }

    #[test]
    fn test_find_definition_boundary_edge_cases() {
        let uri = Url::parse("file:///edge.zsh").unwrap();

        // 1. Empty document
        assert!(find_definition("", &uri, Position::new(0, 0)).is_none());
        assert!(find_definition("", &uri, Position::new(10, 10)).is_none());

        // 2. Out of bounds positions and boundary matching
        let sample = "foo() { : }\nfoo\n";
        assert!(find_definition(sample, &uri, Position::new(10, 0)).is_none());
        assert!(find_definition(sample, &uri, Position::new(1, 100)).is_none());
        // Cursor at column 0, 1, 2, and 3 (end boundary of foo)
        assert!(find_definition(sample, &uri, Position::new(1, 0)).is_some());
        assert!(find_definition(sample, &uri, Position::new(1, 1)).is_some());
        assert!(find_definition(sample, &uri, Position::new(1, 2)).is_some());
        assert!(find_definition(sample, &uri, Position::new(1, 3)).is_some());

        // 3. CRLF line endings
        let crlf = "bar() {\r\n    :\r\n}\r\nbar\r\n";
        let def_crlf = find_definition(crlf, &uri, Position::new(3, 1)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def_crlf {
            assert_eq!(loc.range.start, Position::new(0, 0));
            assert_eq!(loc.range.end, Position::new(0, 3));
        } else {
            panic!("Expected Scalar response for CRLF");
        }
    }

    #[test]
    fn test_find_definition_surrogate_pairs() {
        // '🎉' is 4 bytes UTF-8, 2 UTF-16 code units
        let text = "🎉_emoji_var=\"celebrate\"\necho $🎉_emoji_var/test\n";
        let uri = Url::parse("file:///emoji.zsh").unwrap();

        let def = find_definition(text, &uri, Position::new(1, 8)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def {
            assert_eq!(loc.range.start, Position::new(0, 0));
            // 🎉 is 2 units + "_emoji_var" is 10 units = 12 UTF-16 code units
            assert_eq!(loc.range.end, Position::new(0, 12));
        } else {
            panic!("Expected Scalar response for emoji variable");
        }
    }

    #[test]
    fn test_find_definition_tilde_source_path() {
        let home_env = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let temp_home = tempdir().unwrap();
        let target_file = temp_home.path().join("tilde_test.zsh");
        std::fs::write(&target_file, "echo 'tilde'\n").unwrap();

        // Temporarily override HOME for this test
        unsafe {
            std::env::set_var("HOME", temp_home.path());
        }

        let doc_uri = Url::parse("file:///main.zsh").unwrap();
        let text = "source ~/tilde_test.zsh\n";

        let def = find_definition(text, &doc_uri, Position::new(0, 10)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def {
            assert_eq!(
                loc.uri,
                Url::from_file_path(target_file.canonicalize().unwrap()).unwrap()
            );
        } else {
            panic!("Expected Scalar response for tilde source path");
        }

        // Restore HOME
        unsafe {
            std::env::set_var("HOME", home_env);
        }
    }

    #[test]
    fn test_find_definition_nested_parameter_expansions() {
        let text = r#"
FOO="foo_val"
BAR="bar_val"
OTHER="other_val"

echo "${FOO:-${BAR}}"
echo "${FOO:-$OTHER}"
"#;
        let uri = Url::parse("file:///nested_params.zsh").unwrap();

        // 1. Cursor on FOO inside `${FOO:-${BAR}}` (line 5, char 8) -> FOO (line 1)
        let def_foo = find_definition(text, &uri, Position::new(5, 8)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def_foo {
            assert_eq!(loc.range.start, Position::new(1, 0));
            assert_eq!(loc.range.end, Position::new(1, 3));
        } else {
            panic!("Expected Scalar response for FOO in nested expansion");
        }

        // 2. Cursor on BAR inside `${FOO:-${BAR}}` (line 5, char 16) -> BAR (line 2)
        let def_bar = find_definition(text, &uri, Position::new(5, 16)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def_bar {
            assert_eq!(loc.range.start, Position::new(2, 0));
            assert_eq!(loc.range.end, Position::new(2, 3));
        } else {
            panic!("Expected Scalar response for BAR in nested expansion");
        }

        // 3. Cursor on OTHER inside `${FOO:-$OTHER}` (line 6, char 16) -> OTHER (line 3)
        let def_other = find_definition(text, &uri, Position::new(6, 16)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def_other {
            assert_eq!(loc.range.start, Position::new(3, 0));
            assert_eq!(loc.range.end, Position::new(3, 5));
        } else {
            panic!("Expected Scalar response for $OTHER in nested expansion");
        }
    }

    #[test]
    fn test_loop_and_read_variable_definitions() {
        let text = r#"
for item in alpha beta gamma; do
    echo "Item: $item"
done

for f (*.txt); do
    cat "$f"
done

for (( idx=0; idx < 10; idx++ )); do
    echo "$idx"
done

select choice in "yes" "no"; do
    echo "$choice"
    break
done

while IFS= read -r input_line; do
    echo "$input_line"
done
"#;
        let uri = Url::parse("file:///loops.zsh").unwrap();

        // 1. Jump to `item` from `$item` on line 2, char 18
        let def_item = find_definition(text, &uri, Position::new(2, 18)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def_item {
            assert_eq!(loc.range.start, Position::new(1, 4));
            assert_eq!(loc.range.end, Position::new(1, 8));
        } else {
            panic!("Expected Scalar response for for-loop variable `item`");
        }

        // 2. Jump to `f` from `$f` on line 6, char 10
        let def_f = find_definition(text, &uri, Position::new(6, 10)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def_f {
            assert_eq!(loc.range.start, Position::new(5, 4));
            assert_eq!(loc.range.end, Position::new(5, 5));
        } else {
            panic!("Expected Scalar response for short for-loop variable `f`");
        }

        // 3. Jump to `idx` from `$idx` on line 10, char 12
        let def_idx = find_definition(text, &uri, Position::new(10, 12)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def_idx {
            assert_eq!(loc.range.start, Position::new(9, 7));
            assert_eq!(loc.range.end, Position::new(9, 10));
        } else {
            panic!("Expected Scalar response for C-style for-loop variable `idx`");
        }

        // 4. Jump to `choice` from `$choice` on line 14, char 12
        let def_choice = find_definition(text, &uri, Position::new(14, 12)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def_choice {
            assert_eq!(loc.range.start, Position::new(13, 7));
            assert_eq!(loc.range.end, Position::new(13, 13));
        } else {
            panic!("Expected Scalar response for select variable `choice`");
        }

        // 5. Jump to `input_line` from `$input_line` on line 19, char 12
        let def_read = find_definition(text, &uri, Position::new(19, 12)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def_read {
            assert_eq!(loc.range.start, Position::new(18, 19));
            assert_eq!(loc.range.end, Position::new(18, 29));
        } else {
            panic!("Expected Scalar response for read variable `input_line`");
        }
    }

    #[test]
    fn test_single_line_function_with_variable_definitions() {
        let text = "inline_func() { INLINE_VAR=42; echo \"$INLINE_VAR\"; }\ninline_func\n";
        let uri = Url::parse("file:///inline.zsh").unwrap();

        // 1. Jump to inline_func from line 1
        let def_fn = find_definition(text, &uri, Position::new(1, 4)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def_fn {
            assert_eq!(loc.range.start, Position::new(0, 0));
            assert_eq!(loc.range.end, Position::new(0, 11));
        } else {
            panic!("Expected Scalar response for inline_func");
        }

        // 2. Jump to INLINE_VAR from line 0, char 40 ($INLINE_VAR)
        let def_var = find_definition(text, &uri, Position::new(0, 40)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def_var {
            assert_eq!(loc.range.start, Position::new(0, 16));
            assert_eq!(loc.range.end, Position::new(0, 26));
        } else {
            panic!("Expected Scalar response for INLINE_VAR");
        }
    }

    #[test]
    fn test_compound_and_multi_statement_source_jump() {
        let temp = tempdir().unwrap();
        let lib_a = temp.path().join("lib_a.zsh");
        std::fs::write(&lib_a, "echo 'A'\n").unwrap();
        let lib_b = temp.path().join("lib_b.zsh");
        std::fs::write(&lib_b, "echo 'B'\n").unwrap();

        let doc_path = temp.path().join("main.zsh");
        let doc_uri = Url::from_file_path(&doc_path).unwrap();

        let text = "[[ -f ./lib_a.zsh ]] && source ./lib_a.zsh; source ./lib_b.zsh\n";

        // 1. Jump to lib_a from char 26 (on `source ./lib_a.zsh`)
        let def_a = find_definition(text, &doc_uri, Position::new(0, 26)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def_a {
            assert_eq!(
                loc.uri,
                Url::from_file_path(lib_a.canonicalize().unwrap()).unwrap()
            );
        } else {
            panic!("Expected Scalar response for compound source lib_a");
        }

        // 2. Jump to lib_b from char 50 (on `source ./lib_b.zsh`)
        let def_b = find_definition(text, &doc_uri, Position::new(0, 50)).unwrap();
        if let GotoDefinitionResponse::Scalar(loc) = def_b {
            assert_eq!(
                loc.uri,
                Url::from_file_path(lib_b.canonicalize().unwrap()).unwrap()
            );
        } else {
            panic!("Expected Scalar response for second source lib_b on multi-statement line");
        }
    }

    #[test]
    fn test_scan_definitions_ignores_heredoc_body() {
        let uri = Url::parse("file:///heredoc_def.zsh").unwrap();
        let text = concat!(
            "OUT=$(cat <<EOF\n",
            "FAKE_VAR=1\n",
            "local FAKE_LOCAL=2\n",
            "fake_func() { : }\n",
            "EOF\n",
            ")\n",
            "cat <<-INDENT_END\n",
            "\tINDENT_VAR=3\n",
            "\tfunction indent_func { : }\n",
            "\tINDENT_END\n",
            "REAL_VAR=42\n",
            "real_func() { : }\n",
        );

        assert!(scan_variable_definitions(text, &uri, "FAKE_VAR").is_empty());
        assert!(scan_variable_definitions(text, &uri, "FAKE_LOCAL").is_empty());
        assert!(scan_variable_definitions(text, &uri, "INDENT_VAR").is_empty());
        assert!(scan_function_definitions(text, &uri, "fake_func").is_empty());
        assert!(scan_function_definitions(text, &uri, "indent_func").is_empty());

        let out_locs = scan_variable_definitions(text, &uri, "OUT");
        assert_eq!(out_locs.len(), 1);
        assert_eq!(out_locs[0].range.start, Position::new(0, 0));

        let real_var_locs = scan_variable_definitions(text, &uri, "REAL_VAR");
        assert_eq!(real_var_locs.len(), 1);
        assert_eq!(real_var_locs[0].range.start, Position::new(10, 0));

        let real_fn_locs = scan_function_definitions(text, &uri, "real_func");
        assert_eq!(real_fn_locs.len(), 1);
        assert_eq!(real_fn_locs[0].range.start, Position::new(11, 0));
    }

    #[test]
    fn test_find_definition_alias() {
        let uri = Url::parse("file:///alias_def.zsh").unwrap();
        let text = concat!(
            "alias ll='ls -la'\n",
            "alias -g G='| grep'\n",
            "alias a1='foo' a2='bar'\n",
            "cat <<'EOF'\n",
            "alias fake_alias='nope'\n",
            "EOF\n",
            "ll\n",
            "echo hi G foo\n",
            "a2\n",
        );

        let ll_def = find_definition(text, &uri, Position::new(6, 1));
        assert_eq!(
            ll_def,
            Some(GotoDefinitionResponse::Scalar(Location {
                uri: uri.clone(),
                range: Range::new(Position::new(0, 6), Position::new(0, 8)),
            }))
        );

        let g_def = find_definition(text, &uri, Position::new(7, 8));
        assert_eq!(
            g_def,
            Some(GotoDefinitionResponse::Scalar(Location {
                uri: uri.clone(),
                range: Range::new(Position::new(1, 9), Position::new(1, 10)),
            }))
        );

        let a2_def = find_definition(text, &uri, Position::new(8, 1));
        assert_eq!(
            a2_def,
            Some(GotoDefinitionResponse::Scalar(Location {
                uri: uri.clone(),
                range: Range::new(Position::new(2, 15), Position::new(2, 17)),
            }))
        );

        assert!(scan_alias_definitions(text, &uri, "fake_alias").is_empty());
    }

    #[test]
    fn test_find_definition_function_local_variable_scoping() {
        let uri = Url::parse("file:///scoping.zsh").unwrap();
        let text = concat!(
            "TARGET=\"global\"\n",
            "fn_a() {\n",
            "  local TARGET=\"local_a\"\n",
            "  local ONLY_IN_A=\"secret\"\n",
            "  echo \"$TARGET\" \"$ONLY_IN_A\"\n",
            "}\n",
            "fn_b() {\n",
            "  typeset TARGET=\"local_b\"\n",
            "  echo \"$TARGET\" \"$ONLY_IN_A\"\n",
            "}\n",
            "echo \"$TARGET\" \"$ONLY_IN_A\"\n",
        );

        // Inside fn_a: jumps only to fn_a's `local TARGET` on line 2
        let def_in_a = find_definition(text, &uri, Position::new(4, 10));
        assert_eq!(
            def_in_a,
            Some(GotoDefinitionResponse::Scalar(Location {
                uri: uri.clone(),
                range: Range::new(Position::new(2, 8), Position::new(2, 14)),
            }))
        );

        // Inside fn_a: jumps to fn_a's `local ONLY_IN_A` on line 3
        let def_only_a = find_definition(text, &uri, Position::new(4, 20));
        assert_eq!(
            def_only_a,
            Some(GotoDefinitionResponse::Scalar(Location {
                uri: uri.clone(),
                range: Range::new(Position::new(3, 8), Position::new(3, 17)),
            }))
        );

        // Inside fn_b: jumps only to fn_b's `typeset TARGET` on line 7
        let def_in_b = find_definition(text, &uri, Position::new(8, 10));
        assert_eq!(
            def_in_b,
            Some(GotoDefinitionResponse::Scalar(Location {
                uri: uri.clone(),
                range: Range::new(Position::new(7, 10), Position::new(7, 16)),
            }))
        );

        // Inside fn_b and at top-level: ONLY_IN_A is not visible
        assert_eq!(find_definition(text, &uri, Position::new(8, 20)), None);
        assert_eq!(find_definition(text, &uri, Position::new(10, 18)), None);

        // At top-level: jumps only to global `TARGET` on line 0
        let def_global = find_definition(text, &uri, Position::new(10, 8));
        assert_eq!(
            def_global,
            Some(GotoDefinitionResponse::Scalar(Location {
                uri,
                range: Range::new(Position::new(0, 0), Position::new(0, 6)),
            }))
        );
    }

    #[test]
    fn test_find_definition_across_sourced_files_with_cycles_and_scoping() {
        let dir = tempdir().unwrap();
        let a_path = dir.path().join("helper_a.zsh");
        let b_path = dir.path().join("helper_b.zsh");
        let main_path = dir.path().join("main.zsh");

        std::fs::write(
            &a_path,
            concat!(
                "source ./helper_b.zsh\n",
                "a_func() {\n",
                "  local PRIVATE_IN_A=\"secret\"\n",
                "}\n",
                "export A_VAR=\"val\"\n",
                "alias a_alias='echo a'\n",
            ),
        )
        .unwrap();
        std::fs::write(
            &b_path,
            concat!(". \"./helper_a.zsh\"\n", "function b_func { : }\n"),
        )
        .unwrap();

        let a_uri = Url::from_file_path(a_path.canonicalize().unwrap()).unwrap();
        let b_uri = Url::from_file_path(b_path.canonicalize().unwrap()).unwrap();
        let main_uri = Url::from_file_path(&main_path).unwrap();
        let main_text = concat!(
            "source ./helper_a.zsh\n",
            "a_func\n",
            "echo \"$A_VAR\"\n",
            "a_alias\n",
            "b_func\n",
            "echo \"$PRIVATE_IN_A\"\n",
        );

        assert_eq!(
            find_definition(main_text, &main_uri, Position::new(1, 2)),
            Some(GotoDefinitionResponse::Scalar(Location {
                uri: a_uri.clone(),
                range: Range::new(Position::new(1, 0), Position::new(1, 6)),
            }))
        );
        assert_eq!(
            find_definition(main_text, &main_uri, Position::new(2, 9)),
            Some(GotoDefinitionResponse::Scalar(Location {
                uri: a_uri.clone(),
                range: Range::new(Position::new(4, 7), Position::new(4, 12)),
            }))
        );
        assert_eq!(
            find_definition(main_text, &main_uri, Position::new(3, 2)),
            Some(GotoDefinitionResponse::Scalar(Location {
                uri: a_uri,
                range: Range::new(Position::new(5, 6), Position::new(5, 13)),
            }))
        );
        assert_eq!(
            find_definition(main_text, &main_uri, Position::new(4, 2)),
            Some(GotoDefinitionResponse::Scalar(Location {
                uri: b_uri,
                range: Range::new(Position::new(1, 9), Position::new(1, 15)),
            }))
        );
        assert_eq!(
            find_definition(main_text, &main_uri, Position::new(5, 10)),
            None,
            "Function-local variable inside sourced script must not be resolved cross-file"
        );
    }

    #[test]
    fn test_extract_sourced_paths_prioritized_single_pass_dedup() {
        let text = concat!(
            "source ./first.zsh\n",
            "source ./dup.zsh\n",
            "echo $FOO\n",
            "source ./dup.zsh\n",
            "source ./after.zsh\n",
            "source ./first.zsh\n",
        );
        let index = parse_document(text);
        let paths = prioritized_source_paths(&index, Position::new(2, 5), true);
        assert_eq!(
            paths,
            vec![
                "./first.zsh".to_string(),
                "./dup.zsh".to_string(),
                "./after.zsh".to_string(),
            ]
        );
    }

    #[test]
    fn test_find_definition_source_line_resolves_unsaved_open_buffer_without_disk_file() {
        let dir = tempdir().unwrap();
        let main_path = dir.path().join("main.zsh");
        let unsaved_path = dir.path().join("unsaved_only.zsh");
        let main_uri = Url::from_file_path(&main_path).unwrap();
        let unsaved_uri = Url::from_file_path(&unsaved_path).unwrap();

        let mgr = DocumentManager::new();
        mgr.open(unsaved_uri.clone(), 1, "echo 'unsaved'\n".to_string());

        let def = find_definition_with_manager(
            "source ./unsaved_only.zsh\n",
            &main_uri,
            Position::new(0, 10),
            Some(&mgr),
        );
        assert_eq!(
            def,
            Some(GotoDefinitionResponse::Scalar(Location {
                uri: unsaved_uri,
                range: Range::new(Position::new(0, 0), Position::new(0, 0)),
            }))
        );
    }

    #[test]
    fn test_find_definition_unified_multiline_flagged_fn_and_multi_assign() {
        let uri = Url::parse("file:///test.zsh").unwrap();
        let text = r#"
function -T flagged_fn() {
    echo "ok"
}
local \
    CONT_A=1 \
    CONT_B=2
MULTI_A=1 MULTI_B=2 env
flagged_fn
echo "$CONT_B" "$MULTI_B"
"#;
        // 1. Flagged function definition
        let fn_def = find_definition(text, &uri, Position::new(8, 3));
        assert!(matches!(
            fn_def,
            Some(GotoDefinitionResponse::Scalar(loc)) if loc.range.start.line == 1
        ));

        // 2. Multiline continued local variable
        let cont_def = find_definition(text, &uri, Position::new(9, 8));
        assert!(matches!(
            cont_def,
            Some(GotoDefinitionResponse::Scalar(loc)) if loc.range.start.line == 6
        ));

        // 3. Multi-assignment before command
        let multi_def = find_definition(text, &uri, Position::new(9, 18));
        assert!(matches!(
            multi_def,
            Some(GotoDefinitionResponse::Scalar(loc)) if loc.range.start.line == 7
        ));
    }

    #[test]
    fn test_find_definition_short_circuits_on_first_matching_sourced_file() {
        let dir = tempdir().unwrap();
        let main_path = dir.path().join("main.zsh");
        let a_path = dir.path().join("a.zsh");
        let b_path = dir.path().join("b.zsh");
        let nested_path = dir.path().join("nested.zsh");

        std::fs::write(&nested_path, "nested_fn() { : }\n").unwrap();
        std::fs::write(&a_path, "source ./nested.zsh\ntarget_fn() { : }\n").unwrap();
        std::fs::write(&b_path, "other_fn() { : }\n").unwrap();

        let main_uri = Url::from_file_path(&main_path).unwrap();
        let a_uri = Url::from_file_path(a_path.canonicalize().unwrap()).unwrap();
        let main_text = "source ./a.zsh\nsource ./b.zsh\ntarget_fn\n";

        crate::analysis::workspace::tests::reset_load_count();
        let def = find_definition(main_text, &main_uri, Position::new(2, 2)).unwrap();
        assert_eq!(
            def,
            GotoDefinitionResponse::Scalar(Location {
                uri: a_uri,
                range: Range::new(Position::new(1, 0), Position::new(1, 9)),
            })
        );
        assert_eq!(
            crate::analysis::workspace::tests::get_load_count(),
            1,
            "Should short-circuit after finding target_fn in a.zsh without loading nested.zsh or b.zsh"
        );
    }
}

use std::collections::HashSet;
use tower_lsp::lsp_types::{Position, Range};

use crate::analysis::model::{AliasDef, DocumentIndex, FunctionDef, VariableDef};
use crate::analysis::syntax::{
    byte_to_utf16_col, is_func_ident_char, is_var_ident_char, range_contains,
};

/// Target extracted at a cursor position within a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorTarget {
    /// Cursor is on a `source` or `.` directive.
    SourcePath { path: String, range: Range },
    /// Cursor is on an explicit variable reference (`$VAR` or `${VAR}`).
    VariableRef { name: String, range: Range },
    /// Cursor is on a bare word / identifier (`my_func`, `my_var`, command, etc.).
    Word { text: String, range: Range },
}

/// Kind of a locally visible symbol at a cursor position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VisibleSymbolKind {
    Variable,
    Function,
    Alias,
}

/// A symbol visible at a specific cursor position.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VisibleSymbol {
    pub name: String,
    pub kind: VisibleSymbolKind,
}

#[inline]
fn pos_le(a: Position, b: Position) -> bool {
    a.line < b.line || (a.line == b.line && a.character <= b.character)
}

/// Finds the innermost enclosing function index containing `pos`.
pub fn find_innermost_enclosing_function(index: &DocumentIndex, pos: Position) -> Option<usize> {
    let target = Range::new(pos, pos);
    index
        .functions
        .iter()
        .enumerate()
        .filter(|(_, f)| range_contains(&f.full_range, &target))
        .max_by(|(_, a), (_, b)| {
            a.full_range
                .start
                .line
                .cmp(&b.full_range.start.line)
                .then(
                    a.full_range
                        .start
                        .character
                        .cmp(&b.full_range.start.character),
                )
                .then(b.full_range.end.line.cmp(&a.full_range.end.line))
                .then(b.full_range.end.character.cmp(&a.full_range.end.character))
        })
        .map(|(idx, _)| idx)
}

/// Extracts the cursor target (`SourcePath`, `VariableRef`, or `Word`) at `position`.
pub fn extract_cursor_target(
    text: &str,
    index: &DocumentIndex,
    position: Position,
) -> Option<CursorTarget> {
    let pos_range = Range::new(position, position);
    for src in &index.sources {
        if range_contains(&src.stmt_range, &pos_range) {
            return Some(CursorTarget::SourcePath {
                path: src.path.clone(),
                range: src.stmt_range,
            });
        }
    }

    let mut line_start = 0;
    for _ in 0..position.line {
        line_start = text[line_start..].find('\n')? + line_start + 1;
    }
    let mut line_end = text[line_start..]
        .find('\n')
        .map(|idx| line_start + idx)
        .unwrap_or(text.len());
    if line_end > line_start && text.as_bytes()[line_end - 1] == b'\r' {
        line_end -= 1;
    }

    let line_str = &text[line_start..line_end];
    let target_u16 = position.character;

    // 1. Collect all variable references (${...} and $VAR)
    let mut var_spans: Vec<(usize, usize, String)> = Vec::new();
    let bytes = line_str.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] == b'$' && i + 1 < len && bytes[i + 1] == b'{' {
            let start_idx = i;
            let mut depth = 1;
            let mut j = i + 2;
            let mut in_q: Option<u8> = None;

            while j < len && depth > 0 {
                let b = bytes[j];
                if b == b'\\' && in_q != Some(b'\'') && j + 1 < len {
                    j += 2;
                    continue;
                }
                if let Some(q) = in_q {
                    if b == q {
                        in_q = None;
                    }
                } else if b == b'"' || b == b'\'' || b == b'`' {
                    in_q = Some(b);
                } else if b == b'{' {
                    depth += 1;
                } else if b == b'}' {
                    depth -= 1;
                }
                j += 1;
            }

            if depth == 0 {
                let close_idx = j - 1;
                let mut inside = &line_str[start_idx + 2..close_idx];
                if inside.starts_with('(')
                    && let Some(close_paren) = inside.find(')')
                {
                    inside = &inside[close_paren + 1..];
                }
                if (inside.starts_with('#')
                    || inside.starts_with('!')
                    || inside.starts_with('^')
                    || inside.starts_with('=')
                    || inside.starts_with('~'))
                    && inside.len() > 1
                    && is_var_ident_char(inside.chars().nth(1).unwrap_or(' '))
                {
                    inside = &inside[1..];
                }
                let var_name: String = inside
                    .chars()
                    .take_while(|c| is_var_ident_char(*c))
                    .collect();
                if !var_name.is_empty() {
                    var_spans.push((start_idx, close_idx + 1, var_name));
                }
            }
        } else if bytes[i] == b'$' && (i + 1 == len || bytes[i + 1] != b'{') {
            let dollar_idx = i;
            let after_dollar = &line_str[dollar_idx + 1..];
            let var_name: String = after_dollar
                .chars()
                .take_while(|c| is_var_ident_char(*c))
                .collect();
            if !var_name.is_empty() {
                var_spans.push((dollar_idx, dollar_idx + 1 + var_name.len(), var_name));
            }
        }
        i += 1;
    }

    var_spans.sort_by_key(|(start, end, _)| end - start);
    for (start_byte, end_byte, var_name) in &var_spans {
        let start_u16 = byte_to_utf16_col(line_str, *start_byte);
        let end_u16 = byte_to_utf16_col(line_str, *end_byte);
        if target_u16 >= start_u16 && target_u16 <= end_u16 {
            return Some(CursorTarget::VariableRef {
                name: var_name.clone(),
                range: Range::new(
                    Position::new(position.line, start_u16),
                    Position::new(position.line, end_u16),
                ),
            });
        }
    }

    // 2. Extract word token spanning cursor
    struct TokenSpan {
        start_byte: usize,
        end_byte: usize,
        start_u16: u32,
        end_u16: u32,
    }

    let mut tokens = Vec::new();
    let mut in_token = false;
    let mut token_start_byte = 0;
    let mut token_start_u16 = 0;
    let mut current_u16 = 0;

    for (byte_idx, c) in line_str.char_indices() {
        let char_u16_len = c.len_utf16() as u32;
        if is_func_ident_char(c) {
            if !in_token {
                in_token = true;
                token_start_byte = byte_idx;
                token_start_u16 = current_u16;
            }
        } else if in_token {
            in_token = false;
            tokens.push(TokenSpan {
                start_byte: token_start_byte,
                end_byte: byte_idx,
                start_u16: token_start_u16,
                end_u16: current_u16,
            });
        }
        current_u16 += char_u16_len;
    }
    if in_token {
        tokens.push(TokenSpan {
            start_byte: token_start_byte,
            end_byte: line_str.len(),
            start_u16: token_start_u16,
            end_u16: current_u16,
        });
    }

    if target_u16 > current_u16 && !tokens.is_empty() {
        return None;
    }

    for token in tokens {
        if target_u16 >= token.start_u16 && target_u16 <= token.end_u16 {
            let raw_word = &line_str[token.start_byte..token.end_byte];
            return Some(CursorTarget::Word {
                text: raw_word.to_string(),
                range: Range::new(
                    Position::new(position.line, token.start_u16),
                    Position::new(position.line, token.end_u16),
                ),
            });
        }
    }

    None
}

/// Resolves function definitions matching `name`.
pub fn resolve_function_defs<'a>(index: &'a DocumentIndex, name: &str) -> Vec<&'a FunctionDef> {
    index.functions.iter().filter(|f| f.name == name).collect()
}

/// Resolves alias definitions matching `name`.
pub fn resolve_alias_defs<'a>(index: &'a DocumentIndex, name: &str) -> Vec<&'a AliasDef> {
    index.aliases.iter().filter(|a| a.name == name).collect()
}

/// Resolves variable definitions matching `name` respecting function-local scoping at `cursor_pos`.
pub fn resolve_variable_defs<'a>(
    index: &'a DocumentIndex,
    name: &str,
    cursor_pos: Option<Position>,
) -> Vec<&'a VariableDef> {
    let matches: Vec<&VariableDef> = index.variables.iter().filter(|v| v.name == name).collect();
    if matches.is_empty() {
        return matches;
    }

    let local_fns: HashSet<usize> = matches
        .iter()
        .filter(|v| v.is_function_local)
        .filter_map(|v| v.enclosing_fn)
        .collect();

    let cursor_fn = cursor_pos.and_then(|pos| find_innermost_enclosing_function(index, pos));

    if let Some(cur_fn) = cursor_fn
        && local_fns.contains(&cur_fn)
    {
        let scoped: Vec<&VariableDef> = matches
            .iter()
            .copied()
            .filter(|v| v.enclosing_fn == Some(cur_fn))
            .collect();
        if !scoped.is_empty() {
            return scoped;
        }
    }

    matches
        .into_iter()
        .filter(|v| {
            !(v.enclosing_fn != cursor_fn
                && v.enclosing_fn
                    .is_some_and(|enc_fn| local_fns.contains(&enc_fn)))
        })
        .collect()
}

/// Extracts all locally defined symbols visible at `cursor_pos`, preserving document order and deduplicating.
pub fn visible_symbols_at(index: &DocumentIndex, cursor_pos: Position) -> Vec<VisibleSymbol> {
    let mut entries: Vec<(Position, VisibleSymbol)> = Vec::new();
    let mut out_of_scope_locals: HashSet<(usize, &str)> = HashSet::new();

    for f in &index.functions {
        if pos_le(f.name_range.end, cursor_pos) {
            entries.push((
                f.name_range.start,
                VisibleSymbol {
                    name: f.name.clone(),
                    kind: VisibleSymbolKind::Function,
                },
            ));
        }
    }

    for a in &index.aliases {
        if pos_le(a.name_range.end, cursor_pos) {
            entries.push((
                a.name_range.start,
                VisibleSymbol {
                    name: a.name.clone(),
                    kind: VisibleSymbolKind::Alias,
                },
            ));
        }
    }

    let cursor_range = Range::new(cursor_pos, cursor_pos);
    for v in &index.variables {
        if !pos_le(v.name_range.end, cursor_pos) {
            continue;
        }
        if v.is_function_local
            && let Some(enc_fn) = v.enclosing_fn
            && !range_contains(&index.functions[enc_fn].full_range, &cursor_range)
        {
            out_of_scope_locals.insert((enc_fn, v.name.as_str()));
            continue;
        }
        if let Some(enc_fn) = v.enclosing_fn
            && out_of_scope_locals.contains(&(enc_fn, v.name.as_str()))
        {
            continue;
        }
        entries.push((
            v.name_range.start,
            VisibleSymbol {
                name: v.name.clone(),
                kind: VisibleSymbolKind::Variable,
            },
        ));
    }

    entries.sort_by(|(pos_a, _), (pos_b, _)| {
        pos_a
            .line
            .cmp(&pos_b.line)
            .then(pos_a.character.cmp(&pos_b.character))
    });

    let mut seen = HashSet::new();
    let mut result = Vec::with_capacity(entries.len());
    for (_, sym) in entries {
        if seen.insert((sym.name.clone(), sym.kind)) {
            result.push(sym);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::parser::parse_document;

    #[test]
    fn test_resolve_variable_defs_scoping_and_shadowing() {
        let text = r#"
X="global"
fn_a() {
    local X="local_a"
    local Y="only_a"
    echo "$X"
}
fn_b() {
    echo "$X"
}
"#;
        let index = parse_document(text);

        // Inside fn_a: X resolves only to local X in fn_a
        let in_a = resolve_variable_defs(&index, "X", Some(Position::new(5, 11)));
        assert_eq!(in_a.len(), 1);
        assert_eq!(in_a[0].name_range.start.line, 3);

        // Inside fn_b: X resolves to global X (local X in fn_a is hidden)
        let in_b = resolve_variable_defs(&index, "X", Some(Position::new(8, 11)));
        assert_eq!(in_b.len(), 1);
        assert_eq!(in_b[0].name_range.start.line, 1);

        // Inside fn_b: Y is not visible
        let y_in_b = resolve_variable_defs(&index, "Y", Some(Position::new(8, 11)));
        assert!(y_in_b.is_empty());
    }

    #[test]
    fn test_resolve_variable_defs_local_reassignment_does_not_leak() {
        let text = r#"
X="global"
fn_a() {
    local X="local_a"
    X="reassigned_a"
    local Y="only_a"
    Y="reassigned_y"
    typeset -g Z="global_from_a"
    export E="exported_from_a"
    G="implicit_global_from_a"
    echo "$X"
}
fn_b() {
    echo "$X"
    echo "$Y"
    echo "$Z"
    echo "$E"
    echo "$G"
}
echo "$X"
echo "$Y"
"#;
        let index = parse_document(text);

        // Inside fn_a: X resolves to both local declaration and reassignment in fn_a
        let x_in_a = resolve_variable_defs(&index, "X", Some(Position::new(10, 11)));
        let x_in_a_lines: Vec<u32> = x_in_a.iter().map(|v| v.name_range.start.line).collect();
        assert_eq!(x_in_a_lines, vec![3, 4]);

        // Inside fn_b: X resolves only to global X (neither local X nor reassignment in fn_a leaks)
        let x_in_b = resolve_variable_defs(&index, "X", Some(Position::new(13, 11)));
        let x_in_b_lines: Vec<u32> = x_in_b.iter().map(|v| v.name_range.start.line).collect();
        assert_eq!(x_in_b_lines, vec![1]);

        // Inside fn_b: Y is not visible at all (neither local Y nor reassignment in fn_a leaks)
        let y_in_b = resolve_variable_defs(&index, "Y", Some(Position::new(14, 11)));
        assert!(y_in_b.is_empty());

        // Top-level and None cursor_pos: local X/Y and their reassignments in fn_a do not leak
        let x_top = resolve_variable_defs(&index, "X", Some(Position::new(19, 7)));
        let x_top_lines: Vec<u32> = x_top.iter().map(|v| v.name_range.start.line).collect();
        assert_eq!(x_top_lines, vec![1]);

        let y_top = resolve_variable_defs(&index, "Y", Some(Position::new(20, 7)));
        assert!(y_top.is_empty());

        let y_none = resolve_variable_defs(&index, "Y", None);
        assert!(y_none.is_empty());

        // Global declarations/assignments without local declaration remain visible across scopes
        let z_in_b = resolve_variable_defs(&index, "Z", Some(Position::new(15, 11)));
        assert_eq!(z_in_b.len(), 1);
        assert_eq!(z_in_b[0].name_range.start.line, 7);

        let e_in_b = resolve_variable_defs(&index, "E", Some(Position::new(16, 11)));
        assert_eq!(e_in_b.len(), 1);
        assert_eq!(e_in_b[0].name_range.start.line, 8);

        let g_in_b = resolve_variable_defs(&index, "G", Some(Position::new(17, 11)));
        assert_eq!(g_in_b.len(), 1);
        assert_eq!(g_in_b[0].name_range.start.line, 9);
    }

    #[test]
    fn test_visible_symbols_at_filters_by_position_and_scope() {
        let text = r#"
GLOBAL=1
fn_a() {
    local SECRET=2
    SECRET=3
}
AFTER=4
"#;
        let index = parse_document(text);
        let syms_outside = visible_symbols_at(&index, Position::new(6, 0));
        let names: Vec<&str> = syms_outside.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["GLOBAL", "fn_a"]);

        let syms_inside = visible_symbols_at(&index, Position::new(4, 12));
        let inside_names: Vec<&str> = syms_inside.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(inside_names, vec!["GLOBAL", "fn_a", "SECRET"]);
    }

    #[test]
    fn test_extract_cursor_target_variants() {
        let text = "source ./lib.zsh\necho ${MY_VAR}\nmy_func\n";
        let index = parse_document(text);

        assert!(matches!(
            extract_cursor_target(text, &index, Position::new(0, 10)),
            Some(CursorTarget::SourcePath { path, .. }) if path == "./lib.zsh"
        ));
        assert!(matches!(
            extract_cursor_target(text, &index, Position::new(1, 9)),
            Some(CursorTarget::VariableRef { name, .. }) if name == "MY_VAR"
        ));
        assert!(matches!(
            extract_cursor_target(text, &index, Position::new(2, 3)),
            Some(CursorTarget::Word { text, .. }) if text == "my_func"
        ));
    }
}

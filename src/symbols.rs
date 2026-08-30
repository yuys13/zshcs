use tower_lsp::lsp_types::{DocumentSymbol, Position, Range, SymbolKind, Url};

use crate::definition::{
    byte_to_utf16_col, is_func_ident_char, is_var_ident_char, split_declaration_tokens,
    split_line_statements,
};

/// Keywords that should not be treated as function names or variable assignments.
const RESERVED_KEYWORDS: &[&str] = &[
    "if",
    "then",
    "elif",
    "else",
    "fi",
    "for",
    "while",
    "until",
    "do",
    "done",
    "case",
    "esac",
    "select",
    "time",
    "repeat",
    "nocorrect",
    "coproc",
    "return",
    "exit",
    "local",
    "export",
    "typeset",
    "declare",
    "readonly",
    "integer",
    "float",
    "alias",
    "echo",
    "read",
    "source",
    "set",
    "shift",
    "test",
    "trap",
    "unset",
    "function",
];

/// Checks if a word is a reserved shell keyword.
fn is_reserved_keyword(word: &str) -> bool {
    RESERVED_KEYWORDS.contains(&word)
}

/// Skips flags (e.g. `-T`, `-u`, `+x`) in a string and returns the remaining slice.
fn skip_flags(input: &str) -> &str {
    let mut rest = input.trim_start();
    while rest.starts_with('-') || rest.starts_with('+') {
        let flag_len = rest
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(rest.len());
        rest = rest[flag_len..].trim_start();
    }
    rest
}

/// Finds the closing `}` matching the opening `{` starting at `(open_line, open_byte_col)`.
fn find_matching_closing_brace(
    lines: &[&str],
    open_line: usize,
    open_byte_col: usize,
) -> (usize, u32) {
    let mut depth: usize = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut in_backtick = false;

    for (line_idx, line) in lines.iter().enumerate().skip(open_line) {
        let bytes = line.as_bytes();
        let len = bytes.len();
        let start_col = if line_idx == open_line {
            open_byte_col
        } else {
            0
        };

        let mut i = start_col;
        while i < len {
            let b = bytes[i];

            if b == b'\\' && !in_single_quote && i + 1 < len {
                i += 2;
                continue;
            }

            if in_single_quote {
                if b == b'\'' {
                    in_single_quote = false;
                }
            } else if in_double_quote {
                if b == b'"' {
                    in_double_quote = false;
                }
            } else if in_backtick {
                if b == b'`' {
                    in_backtick = false;
                }
            } else {
                match b {
                    b'\'' => in_single_quote = true,
                    b'"' => in_double_quote = true,
                    b'`' => in_backtick = true,
                    b'#' => {
                        // Comment starts; skip rest of line
                        break;
                    }
                    b'{' => {
                        depth += 1;
                    }
                    b'}' if depth > 0 => {
                        depth -= 1;
                        if depth == 0 {
                            let end_u16 = byte_to_utf16_col(line, i + 1);
                            return (line_idx, end_u16);
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        }
    }

    // If unclosed, return end of last line
    let last_line_idx = lines.len().saturating_sub(1);
    let last_line = lines.get(last_line_idx).copied().unwrap_or("");
    let end_u16 = byte_to_utf16_col(last_line, last_line.len());
    (last_line_idx, end_u16)
}

/// Searches forward for the first opening `{` starting from `start_line` and `start_byte_col`.
fn find_opening_brace(
    lines: &[&str],
    start_line: usize,
    start_byte_col: usize,
) -> Option<(usize, usize)> {
    for (line_idx, line) in lines.iter().enumerate().skip(start_line) {
        let bytes = line.as_bytes();
        let len = bytes.len();
        let col = if line_idx == start_line {
            start_byte_col
        } else {
            0
        };

        let mut in_single_quote = false;
        let mut in_double_quote = false;
        let mut in_backtick = false;

        let mut i = col;
        while i < len {
            let b = bytes[i];

            if b == b'\\' && !in_single_quote && i + 1 < len {
                i += 2;
                continue;
            }

            if in_single_quote {
                if b == b'\'' {
                    in_single_quote = false;
                }
            } else if in_double_quote {
                if b == b'"' {
                    in_double_quote = false;
                }
            } else if in_backtick {
                if b == b'`' {
                    in_backtick = false;
                }
            } else if b == b'#' {
                break;
            } else if b == b'{' {
                return Some((line_idx, i));
            } else if b == b';' && line_idx == start_line {
                // If statement terminates before `{`, check if next tokens or next line has `{`
            }
            i += 1;
        }

        // If we crossed beyond the start line and encountered non-empty non-comment code without `{`, stop searching
        if line_idx > start_line {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') && !trimmed.starts_with('{') {
                break;
            }
        }
    }

    None
}

/// Checks if a parent range strictly contains a child range.
fn range_contains(parent: &Range, child: &Range) -> bool {
    let start_ok = (parent.start.line < child.start.line)
        || (parent.start.line == child.start.line
            && parent.start.character <= child.start.character);
    let end_ok = (parent.end.line > child.end.line)
        || (parent.end.line == child.end.line && parent.end.character >= child.end.character);
    start_ok && end_ok
}

/// Builds a hierarchical `DocumentSymbol` tree from a flat list of extracted symbols.
fn build_symbol_tree(mut symbols: Vec<DocumentSymbol>) -> Vec<DocumentSymbol> {
    // Sort symbols:
    // 1. By start line ascending
    // 2. By start character ascending
    // 3. By end line descending (enclosing symbols first)
    // 4. By end character descending
    symbols.sort_by(|a, b| {
        let cmp_start_line = a.range.start.line.cmp(&b.range.start.line);
        if cmp_start_line != std::cmp::Ordering::Equal {
            return cmp_start_line;
        }
        let cmp_start_char = a.range.start.character.cmp(&b.range.start.character);
        if cmp_start_char != std::cmp::Ordering::Equal {
            return cmp_start_char;
        }
        let cmp_end_line = b.range.end.line.cmp(&a.range.end.line);
        if cmp_end_line != std::cmp::Ordering::Equal {
            return cmp_end_line;
        }
        b.range.end.character.cmp(&a.range.end.character)
    });

    let mut root: Vec<DocumentSymbol> = Vec::new();
    let mut stack: Vec<DocumentSymbol> = Vec::new();

    for sym in symbols {
        while let Some(top) = stack.last() {
            if top.kind == SymbolKind::FUNCTION && range_contains(&top.range, &sym.range) {
                break;
            }
            let popped = stack.pop().unwrap();
            if let Some(parent) = stack.last_mut() {
                parent.children.get_or_insert_with(Vec::new).push(popped);
            } else {
                root.push(popped);
            }
        }
        stack.push(sym);
    }

    while let Some(popped) = stack.pop() {
        if let Some(parent) = stack.last_mut() {
            parent.children.get_or_insert_with(Vec::new).push(popped);
        } else {
            root.push(popped);
        }
    }

    root
}

/// Extracts document symbols (functions, variables, aliases) in a hierarchical tree.
pub fn extract_document_symbols(text: &str, _uri: &Url) -> Vec<DocumentSymbol> {
    let lines: Vec<&str> = text.lines().collect();
    let mut raw_symbols = Vec::new();

    let decl_keywords = [
        "export", "typeset", "local", "declare", "readonly", "integer", "float",
    ];

    for (line_idx, line) in lines.iter().enumerate() {
        for stmt in split_line_statements(line) {
            let trimmed = stmt.text.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let stmt_indent = stmt.text.len() - trimmed.len();
            let stmt_offset = stmt.start_byte + stmt_indent;

            // 1. Check for Shell Functions
            let mut detected_function: Option<(String, Range, Range)> = None;

            // 1.1 Keyword style: `function func ...` or `function func() ...`
            if trimmed.starts_with("function ")
                || trimmed.starts_with("function\t")
                || trimmed == "function"
            {
                let after_kw = if trimmed.len() > 8 { &trimmed[8..] } else { "" };
                let after_kw_trimmed = after_kw.trim_start();
                let kw_spaces = after_kw.len() - after_kw_trimmed.len();
                let rest = skip_flags(after_kw_trimmed);
                let flag_len = after_kw_trimmed.len() - rest.len();

                let name: String = rest
                    .chars()
                    .take_while(|c| is_func_ident_char(*c) && *c != '(' && *c != '{')
                    .collect();

                if !name.is_empty() && !is_reserved_keyword(&name) {
                    let after_name = &rest[name.len()..].trim_start();
                    let after_parens = if let Some(stripped) = after_name.strip_prefix("()") {
                        stripped.trim_start()
                    } else if after_name.starts_with('(')
                        && let Some(close) = after_name.find(')')
                    {
                        after_name[close + 1..].trim_start()
                    } else {
                        after_name
                    };

                    if after_parens.is_empty()
                        || after_parens.starts_with('{')
                        || after_parens.starts_with(';')
                        || after_parens.starts_with('#')
                    {
                        let name_start_byte = stmt_offset + 8 + kw_spaces + flag_len;
                        let name_end_byte = name_start_byte + name.len();
                        let sel_start_u16 = byte_to_utf16_col(line, name_start_byte);
                        let sel_end_u16 = byte_to_utf16_col(line, name_end_byte);
                        let sel_range = Range::new(
                            Position::new(line_idx as u32, sel_start_u16),
                            Position::new(line_idx as u32, sel_end_u16),
                        );

                        // Find opening brace and matching closing brace
                        let func_start_u16 = byte_to_utf16_col(line, stmt_offset);
                        let search_start_byte = name_end_byte;
                        let (end_line, end_u16) = if let Some((open_line, open_byte)) =
                            find_opening_brace(&lines, line_idx, search_start_byte)
                        {
                            find_matching_closing_brace(&lines, open_line, open_byte)
                        } else {
                            (
                                line_idx,
                                byte_to_utf16_col(line, stmt.start_byte + stmt.text.len()),
                            )
                        };

                        let full_range = Range::new(
                            Position::new(line_idx as u32, func_start_u16),
                            Position::new(end_line as u32, end_u16),
                        );

                        detected_function = Some((name, full_range, sel_range));
                    }
                }
            }

            // 1.2 POSIX style: `func() { ... }` or `func () { ... }`
            if detected_function.is_none()
                && let Some(open_paren_idx) = trimmed.find('(')
            {
                let name_part = trimmed[..open_paren_idx].trim_end();
                if !name_part.is_empty()
                    && !name_part.starts_with('#')
                    && !name_part.chars().next().unwrap().is_ascii_digit()
                    && name_part.chars().all(is_func_ident_char)
                    && !is_reserved_keyword(name_part)
                {
                    let after_open = trimmed[open_paren_idx + 1..].trim_start();
                    if let Some(stripped_close) = after_open.strip_prefix(')') {
                        let after_parens = stripped_close.trim_start();
                        if after_parens.is_empty()
                            || after_parens.starts_with('{')
                            || after_parens.starts_with(';')
                            || after_parens.starts_with('#')
                        {
                            let name_start_byte =
                                stmt_offset + trimmed.find(name_part).unwrap_or(0);
                            let name_end_byte = name_start_byte + name_part.len();
                            let sel_start_u16 = byte_to_utf16_col(line, name_start_byte);
                            let sel_end_u16 = byte_to_utf16_col(line, name_end_byte);
                            let sel_range = Range::new(
                                Position::new(line_idx as u32, sel_start_u16),
                                Position::new(line_idx as u32, sel_end_u16),
                            );

                            let func_start_u16 = byte_to_utf16_col(line, stmt_offset);
                            let search_start_byte = name_end_byte;
                            let (end_line, end_u16) = if let Some((open_line, open_byte)) =
                                find_opening_brace(&lines, line_idx, search_start_byte)
                            {
                                find_matching_closing_brace(&lines, open_line, open_byte)
                            } else {
                                (
                                    line_idx,
                                    byte_to_utf16_col(line, stmt.start_byte + stmt.text.len()),
                                )
                            };

                            let full_range = Range::new(
                                Position::new(line_idx as u32, func_start_u16),
                                Position::new(end_line as u32, end_u16),
                            );

                            detected_function =
                                Some((name_part.to_string(), full_range, sel_range));
                        }
                    }
                }
            }

            if let Some((name, range, selection_range)) = detected_function {
                raw_symbols.push(DocumentSymbol {
                    name,
                    detail: Some("()".to_string()),
                    kind: SymbolKind::FUNCTION,
                    tags: None,
                    #[allow(deprecated)]
                    deprecated: None,
                    range,
                    selection_range,
                    children: None,
                });
                continue;
            }

            // 2. Check for Alias Declarations: `alias ...`
            if trimmed.starts_with("alias ") || trimmed.starts_with("alias\t") || trimmed == "alias"
            {
                let after_alias = if trimmed.len() > 5 { &trimmed[5..] } else { "" };
                let alias_offset = stmt_offset + 5;

                for token in split_declaration_tokens(after_alias) {
                    if token.text.starts_with('-') || token.text.starts_with('+') {
                        continue;
                    }

                    let token_str = token.text;
                    let token_start_in_line = alias_offset + token.byte_offset;
                    let (name_raw, val_opt) = if let Some(eq) = token_str.find('=') {
                        (&token_str[..eq], Some(&token_str[eq + 1..]))
                    } else {
                        (token_str, None)
                    };

                    let alias_name = name_raw.trim();
                    if !alias_name.is_empty()
                        && alias_name
                            .chars()
                            .all(|c| is_func_ident_char(c) || c == '-')
                        && !is_reserved_keyword(alias_name)
                    {
                        let name_start_byte =
                            token_start_in_line + token_str.find(alias_name).unwrap_or(0);
                        let name_end_byte = name_start_byte + alias_name.len();
                        let sel_start_u16 = byte_to_utf16_col(line, name_start_byte);
                        let sel_end_u16 = byte_to_utf16_col(line, name_end_byte);
                        let selection_range = Range::new(
                            Position::new(line_idx as u32, sel_start_u16),
                            Position::new(line_idx as u32, sel_end_u16),
                        );

                        let tok_start_u16 = byte_to_utf16_col(line, token_start_in_line);
                        let tok_end_u16 =
                            byte_to_utf16_col(line, token_start_in_line + token_str.len());
                        let range = Range::new(
                            Position::new(line_idx as u32, tok_start_u16),
                            Position::new(line_idx as u32, tok_end_u16),
                        );

                        let detail = val_opt
                            .map(|v| v.trim_matches('\'').trim_matches('"').to_string())
                            .or_else(|| Some("alias".to_string()));

                        raw_symbols.push(DocumentSymbol {
                            name: alias_name.to_string(),
                            detail,
                            kind: SymbolKind::OPERATOR,
                            tags: None,
                            #[allow(deprecated)]
                            deprecated: None,
                            range,
                            selection_range,
                            children: None,
                        });
                    }
                }
                continue;
            }

            // 3. Check for Keyword Variable Declarations: `local`, `export`, `typeset`, etc.
            let mut matched_decl_kw = false;
            for kw in decl_keywords {
                if trimmed.starts_with(kw)
                    && (trimmed[kw.len()..].starts_with(' ')
                        || trimmed[kw.len()..].starts_with('\t')
                        || trimmed.len() == kw.len())
                {
                    matched_decl_kw = true;
                    let after_kw = &trimmed[kw.len()..];
                    let kw_offset = stmt_offset + kw.len();

                    for token in split_declaration_tokens(after_kw) {
                        if token.text.starts_with('-') || token.text.starts_with('+') {
                            continue;
                        }

                        let token_str = token.text;
                        let token_start_in_line = kw_offset + token.byte_offset;

                        let var_name_raw = if let Some(eq) = token_str.find('=') {
                            let before_eq = &token_str[..eq];
                            if let Some(stripped) = before_eq.strip_suffix('+') {
                                stripped
                            } else if let Some(br) = before_eq.find('[') {
                                &before_eq[..br]
                            } else {
                                before_eq
                            }
                        } else if let Some(br) = token_str.find('[') {
                            &token_str[..br]
                        } else {
                            token_str
                        };

                        let var_name = var_name_raw.trim();
                        if !var_name.is_empty()
                            && !var_name.chars().next().unwrap().is_ascii_digit()
                            && var_name.chars().all(is_var_ident_char)
                            && !is_reserved_keyword(var_name)
                        {
                            let name_start_byte =
                                token_start_in_line + token_str.find(var_name).unwrap_or(0);
                            let name_end_byte = name_start_byte + var_name.len();
                            let sel_start_u16 = byte_to_utf16_col(line, name_start_byte);
                            let sel_end_u16 = byte_to_utf16_col(line, name_end_byte);
                            let selection_range = Range::new(
                                Position::new(line_idx as u32, sel_start_u16),
                                Position::new(line_idx as u32, sel_end_u16),
                            );

                            let tok_start_u16 = byte_to_utf16_col(line, token_start_in_line);
                            let tok_end_u16 =
                                byte_to_utf16_col(line, token_start_in_line + token_str.len());
                            let range = Range::new(
                                Position::new(line_idx as u32, tok_start_u16),
                                Position::new(line_idx as u32, tok_end_u16),
                            );

                            raw_symbols.push(DocumentSymbol {
                                name: var_name.to_string(),
                                detail: Some(kw.to_string()),
                                kind: SymbolKind::VARIABLE,
                                tags: None,
                                #[allow(deprecated)]
                                deprecated: None,
                                range,
                                selection_range,
                                children: None,
                            });
                        }
                    }
                    break;
                }
            }

            if matched_decl_kw {
                continue;
            }

            // 4. Check for Direct Variable Assignments: `VAR=...`, `VAR+=...`, `VAR[k]=...`
            if let Some(eq_idx) = trimmed.find('=')
                && eq_idx > 0
                && !trimmed.starts_with("==")
                && !trimmed[eq_idx..].starts_with("==")
            {
                let prev_char = trimmed.as_bytes().get(eq_idx.saturating_sub(1)).copied();
                if !matches!(prev_char, Some(b'!' | b'<' | b'>' | b'=')) {
                    let left_raw = &trimmed[..eq_idx];
                    let left_no_plus = if let Some(stripped) = left_raw.strip_suffix('+') {
                        stripped
                    } else {
                        left_raw
                    };
                    let left_no_bracket = if let Some(br) = left_no_plus.find('[') {
                        &left_no_plus[..br]
                    } else {
                        left_no_plus
                    };

                    let var_name = left_no_bracket.trim();
                    if !var_name.is_empty()
                        && !var_name.chars().next().unwrap().is_ascii_digit()
                        && var_name.chars().all(is_var_ident_char)
                        && !is_reserved_keyword(var_name)
                    {
                        let name_start_byte = stmt_offset + trimmed.find(var_name).unwrap_or(0);
                        let name_end_byte = name_start_byte + var_name.len();
                        let sel_start_u16 = byte_to_utf16_col(line, name_start_byte);
                        let sel_end_u16 = byte_to_utf16_col(line, name_end_byte);
                        let selection_range = Range::new(
                            Position::new(line_idx as u32, sel_start_u16),
                            Position::new(line_idx as u32, sel_end_u16),
                        );

                        let stmt_start_u16 = byte_to_utf16_col(line, stmt_offset);
                        let stmt_end_u16 =
                            byte_to_utf16_col(line, stmt.start_byte + stmt.text.trim_end().len());
                        let range = Range::new(
                            Position::new(line_idx as u32, stmt_start_u16),
                            Position::new(line_idx as u32, stmt_end_u16),
                        );

                        raw_symbols.push(DocumentSymbol {
                            name: var_name.to_string(),
                            detail: None,
                            kind: SymbolKind::VARIABLE,
                            tags: None,
                            #[allow(deprecated)]
                            deprecated: None,
                            range,
                            selection_range,
                            children: None,
                        });
                    }
                }
            }
        }
    }

    build_symbol_tree(raw_symbols)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_uri() -> Url {
        Url::parse("file:///test.zsh").unwrap()
    }

    #[test]
    fn test_extract_symbols_empty_document() {
        let uri = dummy_uri();
        let symbols = extract_document_symbols("", &uri);
        assert!(symbols.is_empty());
    }

    #[test]
    fn test_extract_single_function_posix() {
        let uri = dummy_uri();
        let text = r#"
my_func() {
    echo "hello"
}
"#;
        let symbols = extract_document_symbols(text, &uri);
        assert_eq!(symbols.len(), 1);
        let sym = &symbols[0];
        assert_eq!(sym.name, "my_func");
        assert_eq!(sym.kind, SymbolKind::FUNCTION);
        assert_eq!(sym.selection_range.start.line, 1);
        assert_eq!(sym.range.start.line, 1);
        assert_eq!(sym.range.end.line, 3);
    }

    #[test]
    fn test_extract_single_function_keyword() {
        let uri = dummy_uri();
        let text = r#"
function fn_one {
    echo 1
}

function fn_two() {
    echo 2
}
"#;
        let symbols = extract_document_symbols(text, &uri);
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].name, "fn_one");
        assert_eq!(symbols[0].kind, SymbolKind::FUNCTION);
        assert_eq!(symbols[1].name, "fn_two");
        assert_eq!(symbols[1].kind, SymbolKind::FUNCTION);
    }

    #[test]
    fn test_extract_function_with_brace_on_next_line() {
        let uri = dummy_uri();
        let text = r#"
func_split()
{
    local x=10
}
"#;
        let symbols = extract_document_symbols(text, &uri);
        assert_eq!(symbols.len(), 1);
        let func = &symbols[0];
        assert_eq!(func.name, "func_split");
        assert_eq!(func.range.start.line, 1);
        assert_eq!(func.range.end.line, 4);
        let children = func.children.as_ref().unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "x");
        assert_eq!(children[0].kind, SymbolKind::VARIABLE);
    }

    #[test]
    fn test_extract_single_line_function() {
        let uri = dummy_uri();
        let text = r#"hello() { local msg="hi"; echo "$msg"; }"#;
        let symbols = extract_document_symbols(text, &uri);
        assert_eq!(symbols.len(), 1);
        let func = &symbols[0];
        assert_eq!(func.name, "hello");
        assert_eq!(func.kind, SymbolKind::FUNCTION);
        assert_eq!(func.range.start.line, 0);
        assert_eq!(func.range.end.line, 0);
        let children = func.children.as_ref().unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "msg");
        assert_eq!(children[0].kind, SymbolKind::VARIABLE);
    }

    #[test]
    fn test_extract_nested_functions_and_variables_hierarchy() {
        let uri = dummy_uri();
        let text = r#"
GLOBAL_VAR="root"

outer() {
    local outer_var=1

    inner() {
        local inner_var=2
    }

    alias inside_alias='ls -la'
    outer_var+=1
}

alias global_alias='git status'
"#;
        let symbols = extract_document_symbols(text, &uri);
        assert_eq!(symbols.len(), 3); // GLOBAL_VAR, outer, global_alias

        // 1. GLOBAL_VAR
        assert_eq!(symbols[0].name, "GLOBAL_VAR");
        assert_eq!(symbols[0].kind, SymbolKind::VARIABLE);

        // 2. outer function
        let outer = &symbols[1];
        assert_eq!(outer.name, "outer");
        assert_eq!(outer.kind, SymbolKind::FUNCTION);
        let outer_children = outer.children.as_ref().unwrap();
        assert_eq!(outer_children.len(), 4); // outer_var, inner, inside_alias, outer_var
        assert_eq!(outer_children[0].name, "outer_var");
        assert_eq!(outer_children[0].kind, SymbolKind::VARIABLE);

        let inner = &outer_children[1];
        assert_eq!(inner.name, "inner");
        assert_eq!(inner.kind, SymbolKind::FUNCTION);
        let inner_children = inner.children.as_ref().unwrap();
        assert_eq!(inner_children.len(), 1);
        assert_eq!(inner_children[0].name, "inner_var");
        assert_eq!(inner_children[0].kind, SymbolKind::VARIABLE);

        assert_eq!(outer_children[2].name, "inside_alias");
        assert_eq!(outer_children[2].kind, SymbolKind::OPERATOR);

        // 3. global_alias
        assert_eq!(symbols[2].name, "global_alias");
        assert_eq!(symbols[2].kind, SymbolKind::OPERATOR);
    }

    #[test]
    fn test_extract_variable_declaration_keywords() {
        let uri = dummy_uri();
        let text = r#"
export EXP_VAR="exported"
typeset -g TYPESET_VAR="global"
local LOC_VAR="local"
declare -r CONST_VAR=42
readonly RO_VAR="readonly"
integer INT_VAR=100
float FLOAT_VAR=3.14
local MULTI_A=1 MULTI_B=2
"#;
        let symbols = extract_document_symbols(text, &uri);
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "EXP_VAR",
                "TYPESET_VAR",
                "LOC_VAR",
                "CONST_VAR",
                "RO_VAR",
                "INT_VAR",
                "FLOAT_VAR",
                "MULTI_A",
                "MULTI_B"
            ]
        );
        for sym in &symbols {
            assert_eq!(sym.kind, SymbolKind::VARIABLE);
        }
    }

    #[test]
    fn test_extract_alias_simple_and_global() {
        let uri = dummy_uri();
        let text = r#"
alias ll='ls -la'
alias -g G='| grep'
alias gs='git status' gd='git diff'
"#;
        let symbols = extract_document_symbols(text, &uri);
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["ll", "G", "gs", "gd"]);
        for sym in &symbols {
            assert_eq!(sym.kind, SymbolKind::OPERATOR);
        }
    }

    #[test]
    fn test_extract_multibyte_and_emoji_coordinates() {
        let uri = dummy_uri();
        let text = r#"
# Multibyte Kanji and surrogate pair emoji
日本語関数() {
    local 変数="値"
    local 🍣="sushi"
}
"#;
        let symbols = extract_document_symbols(text, &uri);
        assert_eq!(symbols.len(), 1);
        let func = &symbols[0];
        assert_eq!(func.name, "日本語関数");
        assert_eq!(func.kind, SymbolKind::FUNCTION);

        let children = func.children.as_ref().unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].name, "変数");
        assert_eq!(children[0].kind, SymbolKind::VARIABLE);
        assert_eq!(children[1].name, "🍣");
        assert_eq!(children[1].kind, SymbolKind::VARIABLE);

        // UTF-16 coordinate assertion for 🍣 (2 UTF-16 code units)
        assert_eq!(
            children[1].selection_range.end.character - children[1].selection_range.start.character,
            2
        );
    }

    #[test]
    fn test_unclosed_function_tolerance() {
        let uri = dummy_uri();
        let text = r#"
unclosed_func() {
    local a=1
"#;
        let symbols = extract_document_symbols(text, &uri);
        assert_eq!(symbols.len(), 1);
        let func = &symbols[0];
        assert_eq!(func.name, "unclosed_func");
        let children = func.children.as_ref().unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "a");
    }

    #[test]
    fn test_comments_and_strings_ignored() {
        let uri = dummy_uri();
        let text = r#"
# ignored_func() { echo "fake"; }
# export FAKE_VAR=1
echo "func_in_string() { echo 1; }"
echo "VAR=123"
REAL_VAR="value"
"#;
        let symbols = extract_document_symbols(text, &uri);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "REAL_VAR");
    }

    #[test]
    fn test_quoted_braces_and_comments_in_function_body() {
        let uri = dummy_uri();
        let text = r#"
complex_fn() {
    echo "closing } inside quote"
    echo 'another } quote'
    # comment with } brace
    local inner_var="ok"
}
"#;
        let symbols = extract_document_symbols(text, &uri);
        assert_eq!(symbols.len(), 1);
        let func = &symbols[0];
        assert_eq!(func.name, "complex_fn");
        assert_eq!(func.range.start.line, 1);
        assert_eq!(func.range.end.line, 6);
        let children = func.children.as_ref().unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "inner_var");
    }

    #[test]
    fn test_deep_nesting_three_levels() {
        let uri = dummy_uri();
        let text = r#"
level1() {
    local v1=1
    level2() {
        local v2=2
        level3() {
            local v3=3
        }
    }
}
"#;
        let symbols = extract_document_symbols(text, &uri);
        assert_eq!(symbols.len(), 1);
        let l1 = &symbols[0];
        assert_eq!(l1.name, "level1");
        let l1_children = l1.children.as_ref().unwrap();
        assert_eq!(l1_children.len(), 2); // v1, level2
        assert_eq!(l1_children[0].name, "v1");

        let l2 = &l1_children[1];
        assert_eq!(l2.name, "level2");
        let l2_children = l2.children.as_ref().unwrap();
        assert_eq!(l2_children.len(), 2); // v2, level3
        assert_eq!(l2_children[0].name, "v2");

        let l3 = &l2_children[1];
        assert_eq!(l3.name, "level3");
        let l3_children = l3.children.as_ref().unwrap();
        assert_eq!(l3_children.len(), 1);
        assert_eq!(l3_children[0].name, "v3");
    }

    #[test]
    fn test_array_and_indexed_assignments() {
        let uri = dummy_uri();
        let text = r#"
ARR=(1 2 3)
ARR+=(4 5)
MAP[key]=value
VAR+=suffix
"#;
        let symbols = extract_document_symbols(text, &uri);
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["ARR", "ARR", "MAP", "VAR"]);
        for sym in &symbols {
            assert_eq!(sym.kind, SymbolKind::VARIABLE);
        }
    }

    #[test]
    fn test_suffix_and_global_aliases() {
        let uri = dummy_uri();
        let text = r#"
alias -s txt=nvim
alias -g G='| grep'
alias -r locked='echo locked'
"#;
        let symbols = extract_document_symbols(text, &uri);
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["txt", "G", "locked"]);
        for sym in &symbols {
            assert_eq!(sym.kind, SymbolKind::OPERATOR);
        }
    }

    #[test]
    fn test_special_function_names() {
        let uri = dummy_uri();
        let text = r#"
my-kebab-func() { :; }
prompt_pure_setup() { :; }
ns::method() { :; }
my.func() { :; }
"#;
        let symbols = extract_document_symbols(text, &uri);
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "my-kebab-func",
                "prompt_pure_setup",
                "ns::method",
                "my.func"
            ]
        );
        for sym in &symbols {
            assert_eq!(sym.kind, SymbolKind::FUNCTION);
        }
    }

    #[test]
    fn test_multi_statement_single_line() {
        let uri = dummy_uri();
        let text = r#"a=1; b=2; fn() { c=3; }; d=4"#;
        let symbols = extract_document_symbols(text, &uri);
        assert_eq!(symbols.len(), 4); // a, b, fn, d
        assert_eq!(symbols[0].name, "a");
        assert_eq!(symbols[0].kind, SymbolKind::VARIABLE);
        assert_eq!(symbols[1].name, "b");
        assert_eq!(symbols[1].kind, SymbolKind::VARIABLE);

        assert_eq!(symbols[2].name, "fn");
        assert_eq!(symbols[2].kind, SymbolKind::FUNCTION);
        let fn_children = symbols[2].children.as_ref().unwrap();
        assert_eq!(fn_children.len(), 1);
        assert_eq!(fn_children[0].name, "c");
        assert_eq!(fn_children[0].kind, SymbolKind::VARIABLE);

        assert_eq!(symbols[3].name, "d");
        assert_eq!(symbols[3].kind, SymbolKind::VARIABLE);
    }
}

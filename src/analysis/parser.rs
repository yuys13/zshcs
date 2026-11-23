use tower_lsp::lsp_types::{Position, Range};

use crate::analysis::model::{
    AliasDef, DocumentIndex, FunctionDef, SourceDirective, VariableDeclKind, VariableDef,
};
use crate::analysis::syntax::{
    byte_to_utf16_col, detect_heredoc_delimiter, ends_with_line_continuation,
    find_matching_closing_brace, find_opening_brace, find_read_command, is_func_ident_char,
    is_reserved_keyword, is_var_ident_char, range_contains, skip_flags, split_declaration_tokens,
    split_line_statements, split_words_with_offsets,
};

const DECL_KEYWORDS: &[&str] = &[
    "export", "typeset", "local", "declare", "readonly", "integer", "float",
];

/// Extracts contiguous `#` comment lines immediately preceding `def_line_idx` as Markdown text.
fn extract_preceding_comment_block(lines: &[&str], def_line_idx: usize) -> Option<String> {
    let mut collected = Vec::new();
    let mut idx = def_line_idx;
    while idx > 0 {
        let trimmed = lines[idx - 1].trim();
        if !trimmed.starts_with('#') || trimmed.starts_with("#!") {
            break;
        }
        let body = trimmed.trim_start_matches('#');
        let body = body.strip_prefix(' ').unwrap_or(body);
        collected.push(body);
        idx -= 1;
    }
    if collected.is_empty() {
        return None;
    }
    collected.reverse();
    let doc = collected.join("\n").trim().to_string();
    if doc.is_empty() { None } else { Some(doc) }
}

/// Finds the innermost enclosing function index for `target` range.
fn find_enclosing_fn_index(
    functions: &[FunctionDef],
    target: &Range,
    exclude_idx: Option<usize>,
) -> Option<usize> {
    functions
        .iter()
        .enumerate()
        .filter(|(idx, f)| Some(*idx) != exclude_idx && range_contains(&f.full_range, target))
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

/// Parses a Zsh document in a single pass into a unified `DocumentIndex`.
pub fn parse_document(text: &str) -> DocumentIndex {
    let lines: Vec<&str> = text.lines().collect();
    let mut functions = Vec::new();
    let mut raw_variables: Vec<(VariableDef, bool)> = Vec::new();
    let mut aliases = Vec::new();
    let mut sources = Vec::new();

    let mut heredoc_delimiter: Option<String> = None;
    let mut pending_decl_kw: Option<&'static str> = None;
    let mut pending_decl_has_global_flag = false;
    let mut pending_function_kw = false;
    let mut pending_for_or_select_kw: Option<VariableDeclKind> = None;
    let mut pending_read_kw = false;
    let mut pending_alias_kw = false;
    let mut pending_source_kw = false;

    for (line_idx, &line) in lines.iter().enumerate() {
        if let Some(ref delim) = heredoc_delimiter {
            if line.trim_start_matches('\t').trim_end() == delim {
                heredoc_delimiter = None;
            }
            pending_decl_kw = None;
            pending_decl_has_global_flag = false;
            pending_function_kw = false;
            pending_for_or_select_kw = None;
            pending_read_kw = false;
            pending_alias_kw = false;
            pending_source_kw = false;
            continue;
        }

        if let Some(delim) = detect_heredoc_delimiter(line) {
            heredoc_delimiter = Some(delim);
            pending_decl_kw = None;
            pending_decl_has_global_flag = false;
            pending_function_kw = false;
            pending_for_or_select_kw = None;
            pending_read_kw = false;
            pending_alias_kw = false;
            pending_source_kw = false;
        }

        let has_line_continuation = ends_with_line_continuation(line);
        let mut current_pending_decl_kw = pending_decl_kw.take();
        let mut current_pending_decl_global = std::mem::take(&mut pending_decl_has_global_flag);
        let mut current_pending_function_kw = std::mem::take(&mut pending_function_kw);
        let mut current_pending_for_or_select_kw = pending_for_or_select_kw.take();
        let mut current_pending_read_kw = std::mem::take(&mut pending_read_kw);
        let mut current_pending_alias_kw = std::mem::take(&mut pending_alias_kw);
        let mut current_pending_source_kw = std::mem::take(&mut pending_source_kw);

        let mut next_pending_decl_kw: Option<&'static str> = None;
        let mut next_pending_decl_global = false;
        let mut next_pending_function_kw = false;
        let mut next_pending_for_or_select_kw: Option<VariableDeclKind> = None;
        let mut next_pending_read_kw = false;
        let mut next_pending_alias_kw = false;
        let mut next_pending_source_kw = false;

        let stmts = split_line_statements(line);
        if stmts.is_empty() {
            if has_line_continuation {
                pending_decl_kw = current_pending_decl_kw;
                pending_decl_has_global_flag = current_pending_decl_global;
                pending_function_kw = current_pending_function_kw;
                pending_for_or_select_kw = current_pending_for_or_select_kw;
                pending_read_kw = current_pending_read_kw;
                pending_alias_kw = current_pending_alias_kw;
                pending_source_kw = current_pending_source_kw;
            }
            continue;
        }

        let num_stmts = stmts.len();
        for (stmt_idx, stmt) in stmts.into_iter().enumerate() {
            let is_first_stmt = stmt_idx == 0;
            let is_last_stmt = stmt_idx + 1 == num_stmts;

            let pending_decl_for_this_stmt = if is_first_stmt {
                current_pending_decl_kw.take()
            } else {
                None
            };
            let pending_decl_global_for_this_stmt = if is_first_stmt {
                std::mem::take(&mut current_pending_decl_global)
            } else {
                false
            };
            let pending_function_for_this_stmt = if is_first_stmt {
                std::mem::take(&mut current_pending_function_kw)
            } else {
                false
            };
            let pending_for_or_select_for_this_stmt = if is_first_stmt {
                current_pending_for_or_select_kw.take()
            } else {
                None
            };
            let pending_read_for_this_stmt = if is_first_stmt {
                std::mem::take(&mut current_pending_read_kw)
            } else {
                false
            };
            let pending_alias_for_this_stmt = if is_first_stmt {
                std::mem::take(&mut current_pending_alias_kw)
            } else {
                false
            };
            let pending_source_for_this_stmt = if is_first_stmt {
                std::mem::take(&mut current_pending_source_kw)
            } else {
                false
            };

            let trimmed = stmt.text.trim_start();
            let trimmed_end = trimmed.trim_end();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                if is_last_stmt && has_line_continuation {
                    if next_pending_decl_kw.is_none() {
                        next_pending_decl_kw = pending_decl_for_this_stmt;
                        next_pending_decl_global = pending_decl_global_for_this_stmt;
                    }
                    if !next_pending_function_kw {
                        next_pending_function_kw = pending_function_for_this_stmt;
                    }
                    if next_pending_for_or_select_kw.is_none() {
                        next_pending_for_or_select_kw = pending_for_or_select_for_this_stmt;
                    }
                    if !next_pending_read_kw {
                        next_pending_read_kw = pending_read_for_this_stmt;
                    }
                    if !next_pending_alias_kw {
                        next_pending_alias_kw = pending_alias_for_this_stmt;
                    }
                    if !next_pending_source_kw {
                        next_pending_source_kw = pending_source_for_this_stmt;
                    }
                }
                continue;
            }

            let stmt_indent = stmt.text.len() - trimmed.len();
            let stmt_offset = stmt.start_byte + stmt_indent;

            // 1. Source / `.` directives
            if pending_source_for_this_stmt
                || trimmed == "source"
                || trimmed == "source\\"
                || trimmed.starts_with("source ")
                || trimmed.starts_with("source\t")
                || trimmed == "."
                || trimmed == ".\\"
                || trimmed.starts_with(". ")
                || trimmed.starts_with(".\t")
            {
                let parsed_source = if pending_source_for_this_stmt {
                    Some((trimmed, stmt_offset, stmt_offset))
                } else if trimmed == "source"
                    || trimmed == "source\\"
                    || trimmed == "."
                    || trimmed == ".\\"
                {
                    if is_last_stmt && has_line_continuation {
                        next_pending_source_kw = true;
                    }
                    None
                } else if trimmed.starts_with("source ") || trimmed.starts_with("source\t") {
                    Some((&trimmed[6..], stmt_offset + 6, stmt_offset))
                } else {
                    Some((&trimmed[1..], stmt_offset + 1, stmt_offset))
                };

                if let Some((after_cmd, cmd_end_byte, cmd_start_byte)) = parsed_source {
                    let after_trimmed = after_cmd.trim_start();
                    if after_trimmed.is_empty() || after_trimmed == "\\" {
                        if is_last_stmt && has_line_continuation {
                            next_pending_source_kw = true;
                        }
                    } else {
                        let arg_trim_len = after_cmd.len() - after_trimmed.len();
                        let arg_start_byte = cmd_end_byte + arg_trim_len;

                        let (unquoted, arg_end_byte) =
                            if let Some(stripped) = after_trimmed.strip_prefix('"') {
                                if let Some(close_idx) = stripped.find('"') {
                                    (&stripped[..close_idx], arg_start_byte + 1 + close_idx + 1)
                                } else {
                                    (stripped, arg_start_byte + after_trimmed.len())
                                }
                            } else if let Some(stripped) = after_trimmed.strip_prefix('\'') {
                                if let Some(close_idx) = stripped.find('\'') {
                                    (&stripped[..close_idx], arg_start_byte + 1 + close_idx + 1)
                                } else {
                                    (stripped, arg_start_byte + after_trimmed.len())
                                }
                            } else {
                                let token_len = after_trimmed
                                    .find(|c: char| {
                                        c.is_whitespace() || c == '#' || c == ';' || c == '\\'
                                    })
                                    .unwrap_or(after_trimmed.len());
                                (&after_trimmed[..token_len], arg_start_byte + token_len)
                            };

                        if !unquoted.is_empty() {
                            let cmd_start_u16 = byte_to_utf16_col(line, cmd_start_byte);
                            let arg_end_u16 = byte_to_utf16_col(line, arg_end_byte);
                            let stmt_range = Range::new(
                                Position::new(line_idx as u32, cmd_start_u16),
                                Position::new(line_idx as u32, arg_end_u16),
                            );
                            sources.push(SourceDirective {
                                path: unquoted.to_string(),
                                stmt_range,
                                arg_end: Position::new(line_idx as u32, arg_end_u16),
                            });
                        }
                    }
                }
                continue;
            }

            // 2. Shell Functions
            // 2.1 Keyword style: `function [flags] func [()] ...`
            if trimmed.starts_with("function ")
                || trimmed.starts_with("function\t")
                || trimmed == "function"
                || trimmed == "function\\"
                || pending_function_for_this_stmt
            {
                let (after_kw, kw_prefix_len) = if pending_function_for_this_stmt {
                    (trimmed, 0)
                } else if trimmed.len() > 8 {
                    (&trimmed[8..], 8)
                } else {
                    ("", trimmed.len())
                };
                let after_kw_trimmed = after_kw.trim_start();
                let kw_spaces = after_kw.len() - after_kw_trimmed.len();
                let rest = skip_flags(after_kw_trimmed);
                let flag_len = after_kw_trimmed.len() - rest.len();

                let name: String = rest
                    .chars()
                    .take_while(|c| is_func_ident_char(*c) && *c != '(' && *c != '{')
                    .collect();

                if name.is_empty()
                    && is_last_stmt
                    && has_line_continuation
                    && !pending_function_for_this_stmt
                {
                    next_pending_function_kw = true;
                    continue;
                }

                if !name.is_empty() && !is_reserved_keyword(&name) {
                    let after_name = rest[name.len()..].trim_start();
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
                        || after_parens.starts_with('\\')
                    {
                        let name_start_byte = stmt_offset + kw_prefix_len + kw_spaces + flag_len;
                        let name_end_byte = name_start_byte + name.len();
                        let sel_start_u16 = byte_to_utf16_col(line, name_start_byte);
                        let sel_end_u16 = byte_to_utf16_col(line, name_end_byte);
                        let name_range = Range::new(
                            Position::new(line_idx as u32, sel_start_u16),
                            Position::new(line_idx as u32, sel_end_u16),
                        );

                        let func_start_u16 = byte_to_utf16_col(line, stmt_offset);
                        let (end_line, end_u16) = if let Some((open_line, open_byte)) =
                            find_opening_brace(&lines, line_idx, name_end_byte)
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

                        functions.push(FunctionDef {
                            name,
                            name_range,
                            full_range,
                            signature: trimmed_end.to_string(),
                            doc_comment: extract_preceding_comment_block(&lines, line_idx),
                            parent_fn: None,
                        });
                        continue;
                    }
                }
            }

            // 2.2 POSIX style: `func() { ... }` or `func () { ... }`
            if let Some(open_paren_idx) = trimmed.find('(') {
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
                            || after_parens.starts_with('\\')
                        {
                            let name_start_byte =
                                stmt_offset + trimmed.find(name_part).unwrap_or(0);
                            let name_end_byte = name_start_byte + name_part.len();
                            let sel_start_u16 = byte_to_utf16_col(line, name_start_byte);
                            let sel_end_u16 = byte_to_utf16_col(line, name_end_byte);
                            let name_range = Range::new(
                                Position::new(line_idx as u32, sel_start_u16),
                                Position::new(line_idx as u32, sel_end_u16),
                            );

                            let func_start_u16 = byte_to_utf16_col(line, stmt_offset);
                            let (end_line, end_u16) = if let Some((open_line, open_byte)) =
                                find_opening_brace(&lines, line_idx, name_end_byte)
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

                            functions.push(FunctionDef {
                                name: name_part.to_string(),
                                name_range,
                                full_range,
                                signature: trimmed_end.to_string(),
                                doc_comment: extract_preceding_comment_block(&lines, line_idx),
                                parent_fn: None,
                            });
                            continue;
                        }
                    }
                }
            }

            // 3. Alias Declarations: `alias ll='ls -la'`, `alias -g G='| grep'`
            if trimmed.starts_with("alias ")
                || trimmed.starts_with("alias\t")
                || trimmed == "alias"
                || trimmed == "alias\\"
                || pending_alias_for_this_stmt
            {
                if is_last_stmt && has_line_continuation {
                    next_pending_alias_kw = true;
                }
                let (after_alias, alias_offset) =
                    if trimmed.starts_with("alias ") || trimmed.starts_with("alias\t") {
                        (&trimmed[5..], stmt_offset + 5)
                    } else if trimmed == "alias" || trimmed == "alias\\" {
                        ("", stmt_offset + 5)
                    } else {
                        (trimmed, stmt_offset)
                    };

                let doc_comment = extract_preceding_comment_block(&lines, line_idx);
                for token in split_declaration_tokens(after_alias) {
                    if token.text.starts_with('-')
                        || token.text.starts_with('+')
                        || token.text == "\\"
                    {
                        continue;
                    }

                    let token_str = token.text;
                    let token_start_in_line = alias_offset + token.byte_offset;
                    if let Some(eq) = token_str.find('=')
                        && eq > 0
                    {
                        let alias_name = token_str[..eq].trim().trim_end_matches('\\').trim();
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
                            let name_range = Range::new(
                                Position::new(line_idx as u32, sel_start_u16),
                                Position::new(line_idx as u32, sel_end_u16),
                            );

                            let tok_start_u16 = byte_to_utf16_col(line, token_start_in_line);
                            let tok_end_u16 =
                                byte_to_utf16_col(line, token_start_in_line + token_str.len());
                            let full_range = Range::new(
                                Position::new(line_idx as u32, tok_start_u16),
                                Position::new(line_idx as u32, tok_end_u16),
                            );

                            let val = &token_str[eq + 1..];
                            let expansion =
                                Some(val.trim_matches('\'').trim_matches('"').to_string());

                            aliases.push(AliasDef {
                                name: alias_name.to_string(),
                                name_range,
                                full_range,
                                expansion,
                                signature: format!("alias {}", token_str.trim()),
                                doc_comment: doc_comment.clone(),
                                enclosing_fn: None,
                            });
                        }
                    }
                }
                continue;
            }

            // 4. Keyword Variable Declarations: `local`, `export`, `typeset`, etc.
            let mut matched_decl_kw = false;
            let mut active_kw: Option<&'static str> = None;
            let mut after_kw_slice = "";
            let mut kw_offset = stmt_offset;

            for &kw in DECL_KEYWORDS {
                if trimmed.starts_with(kw)
                    && (trimmed[kw.len()..].starts_with(' ')
                        || trimmed[kw.len()..].starts_with('\t')
                        || trimmed.len() == kw.len()
                        || trimmed[kw.len()..].starts_with('\\'))
                {
                    matched_decl_kw = true;
                    active_kw = Some(kw);
                    after_kw_slice = &trimmed[kw.len()..];
                    kw_offset = stmt_offset + kw.len();
                    break;
                }
            }

            if !matched_decl_kw && let Some(kw) = pending_decl_for_this_stmt {
                active_kw = Some(kw);
                after_kw_slice = trimmed;
                kw_offset = stmt_offset;
            }

            if let Some(kw) = active_kw {
                let mut has_global_flag = kw == "export" || pending_decl_global_for_this_stmt;
                let doc_comment = extract_preceding_comment_block(&lines, line_idx);
                let tokens = split_declaration_tokens(after_kw_slice);
                for token in &tokens {
                    if token.text.starts_with('-') || token.text.starts_with('+') {
                        if token.text.contains('g') {
                            has_global_flag = true;
                        }
                        continue;
                    }
                    if token.text == "\\" {
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

                    let var_name = var_name_raw.trim().trim_end_matches('\\').trim();
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
                        let name_range = Range::new(
                            Position::new(line_idx as u32, sel_start_u16),
                            Position::new(line_idx as u32, sel_end_u16),
                        );

                        let tok_start_u16 = byte_to_utf16_col(line, token_start_in_line);
                        let tok_end_u16 =
                            byte_to_utf16_col(line, token_start_in_line + token_str.len());
                        let full_range = Range::new(
                            Position::new(line_idx as u32, tok_start_u16),
                            Position::new(line_idx as u32, tok_end_u16),
                        );

                        raw_variables.push((
                            VariableDef {
                                name: var_name.to_string(),
                                name_range,
                                full_range,
                                kind: VariableDeclKind::Keyword(kw),
                                is_function_local: false,
                                signature: trimmed_end.to_string(),
                                doc_comment: doc_comment.clone(),
                                enclosing_fn: None,
                            },
                            has_global_flag,
                        ));
                    }
                }
                if is_last_stmt && has_line_continuation {
                    next_pending_decl_kw = Some(kw);
                    next_pending_decl_global = has_global_flag;
                }
                continue;
            }

            // 5. Loop Variables
            // 5.1 C-style for loop: `for (( VAR = 0; ... ))`
            if trimmed.starts_with("for ((") || trimmed.starts_with("for((") {
                let after_for = if let Some(stripped) = trimmed.strip_prefix("for ((") {
                    stripped
                } else {
                    trimmed.strip_prefix("for((").unwrap_or("")
                };
                let kw_offset = stmt.text.len() - after_for.len();
                let init_part = if let Some(close_idx) = after_for.find("))") {
                    &after_for[..close_idx]
                } else {
                    after_for
                };
                let first_semi = init_part.find(';').unwrap_or(init_part.len());
                let clause = &init_part[..first_semi];
                let doc_comment = extract_preceding_comment_block(&lines, line_idx);
                let mut comma_offset = 0;
                for sub_clause in clause.split(',') {
                    for token in split_declaration_tokens(sub_clause) {
                        let token_text = token.text;
                        if token_text == "\\" {
                            continue;
                        }
                        let var_name_raw = if let Some(eq) = token_text.find('=') {
                            &token_text[..eq]
                        } else {
                            token_text
                        };
                        let var_name = var_name_raw
                            .strip_suffix('+')
                            .unwrap_or(var_name_raw)
                            .trim()
                            .trim_end_matches('\\')
                            .trim();
                        if !var_name.is_empty()
                            && !var_name.chars().next().unwrap().is_ascii_digit()
                            && var_name.chars().all(is_var_ident_char)
                            && !is_reserved_keyword(var_name)
                        {
                            let name_start_byte = stmt.start_byte
                                + kw_offset
                                + comma_offset
                                + token.byte_offset
                                + token_text.find(var_name).unwrap_or(0);
                            let name_end_byte = name_start_byte + var_name.len();
                            let sel_start_u16 = byte_to_utf16_col(line, name_start_byte);
                            let sel_end_u16 = byte_to_utf16_col(line, name_end_byte);
                            let name_range = Range::new(
                                Position::new(line_idx as u32, sel_start_u16),
                                Position::new(line_idx as u32, sel_end_u16),
                            );

                            raw_variables.push((
                                VariableDef {
                                    name: var_name.to_string(),
                                    name_range,
                                    full_range: name_range,
                                    kind: VariableDeclKind::CForLoop,
                                    is_function_local: false,
                                    signature: trimmed_end.to_string(),
                                    doc_comment: doc_comment.clone(),
                                    enclosing_fn: None,
                                },
                                false,
                            ));
                        }
                    }
                    comma_offset += sub_clause.len() + 1;
                }
                continue;
            }

            // 5.2 `for VAR in ...`, `for VAR (...)`, `for VAR ; do`, `select VAR in ...`
            let for_or_select_kind = if trimmed.starts_with("for ") || trimmed.starts_with("for\t")
            {
                Some((4, VariableDeclKind::ForLoop))
            } else if trimmed.starts_with("select ") || trimmed.starts_with("select\t") {
                Some((7, VariableDeclKind::SelectLoop))
            } else {
                pending_for_or_select_for_this_stmt.map(|k| (0, k))
            };

            if let Some((kw_len, loop_kind)) = for_or_select_kind {
                let after_kw = if pending_for_or_select_for_this_stmt.is_some() {
                    trimmed
                } else {
                    &trimmed[kw_len..]
                };
                let doc_comment = extract_preceding_comment_block(&lines, line_idx);
                let mut extracted_any = false;
                for (word_offset, word) in split_words_with_offsets(after_kw) {
                    if word == "in"
                        || word == "do"
                        || word.starts_with(';')
                        || word.starts_with('(')
                    {
                        break;
                    }
                    let clean_word = word.trim_end_matches(';').trim_end_matches('\\');
                    if !clean_word.is_empty()
                        && !clean_word.chars().next().unwrap().is_ascii_digit()
                        && clean_word.chars().all(is_var_ident_char)
                        && !is_reserved_keyword(clean_word)
                    {
                        let name_start_byte =
                            stmt_offset + kw_len + word_offset + word.find(clean_word).unwrap_or(0);
                        let name_end_byte = name_start_byte + clean_word.len();
                        let sel_start_u16 = byte_to_utf16_col(line, name_start_byte);
                        let sel_end_u16 = byte_to_utf16_col(line, name_end_byte);
                        let name_range = Range::new(
                            Position::new(line_idx as u32, sel_start_u16),
                            Position::new(line_idx as u32, sel_end_u16),
                        );
                        raw_variables.push((
                            VariableDef {
                                name: clean_word.to_string(),
                                name_range,
                                full_range: name_range,
                                kind: loop_kind,
                                is_function_local: false,
                                signature: trimmed_end.to_string(),
                                doc_comment: doc_comment.clone(),
                                enclosing_fn: None,
                            },
                            false,
                        ));
                        extracted_any = true;
                    }
                    if word.ends_with(';') {
                        break;
                    }
                }
                if is_last_stmt
                    && has_line_continuation
                    && (!extracted_any || pending_for_or_select_for_this_stmt.is_some())
                {
                    next_pending_for_or_select_kw = Some(loop_kind);
                }
                continue;
            }

            // 6. `read` statements: `read VAR`, `read -r line`, `while read ...`
            let matched_read = find_read_command(trimmed);
            if matched_read.is_some() || pending_read_for_this_stmt {
                if is_last_stmt && has_line_continuation {
                    next_pending_read_kw = true;
                }
                let (read_kw_offset, after_read) = if let Some((read_start, after)) = matched_read {
                    (stmt_offset + read_start + 5, after)
                } else {
                    (stmt_offset, trimmed)
                };
                let doc_comment = extract_preceding_comment_block(&lines, line_idx);

                for token in split_declaration_tokens(after_read) {
                    if token.text.starts_with('-')
                        || token.text.starts_with('+')
                        || token.text == "\\"
                    {
                        continue;
                    }
                    let token_str = token.text;
                    let var_name_raw = token_str
                        .split(['=', '?'])
                        .next()
                        .unwrap_or(token_str)
                        .trim()
                        .trim_end_matches('\\')
                        .trim();
                    if !var_name_raw.is_empty()
                        && !var_name_raw.chars().next().unwrap().is_ascii_digit()
                        && var_name_raw.chars().all(is_var_ident_char)
                        && !is_reserved_keyword(var_name_raw)
                    {
                        let name_start_byte = read_kw_offset
                            + token.byte_offset
                            + token_str.find(var_name_raw).unwrap_or(0);
                        let name_end_byte = name_start_byte + var_name_raw.len();
                        let sel_start_u16 = byte_to_utf16_col(line, name_start_byte);
                        let sel_end_u16 = byte_to_utf16_col(line, name_end_byte);
                        let name_range = Range::new(
                            Position::new(line_idx as u32, sel_start_u16),
                            Position::new(line_idx as u32, sel_end_u16),
                        );

                        raw_variables.push((
                            VariableDef {
                                name: var_name_raw.to_string(),
                                name_range,
                                full_range: name_range,
                                kind: VariableDeclKind::Read,
                                is_function_local: false,
                                signature: trimmed_end.to_string(),
                                doc_comment: doc_comment.clone(),
                                enclosing_fn: None,
                            },
                            false,
                        ));
                    }
                }
                continue;
            }

            // 7. Direct Variable Assignments: `VAR=...`, `VAR+=...`, `VAR[k]=...`
            let doc_comment = extract_preceding_comment_block(&lines, line_idx);
            for token in split_declaration_tokens(trimmed) {
                let token_text = token.text;
                if token_text == "\\" {
                    continue;
                }
                if let Some(eq_idx) = token_text.find('=')
                    && eq_idx > 0
                    && !token_text.starts_with("==")
                    && !token_text[eq_idx..].starts_with("==")
                {
                    let prev_char = token_text.as_bytes().get(eq_idx.saturating_sub(1)).copied();
                    if !matches!(prev_char, Some(b'!' | b'<' | b'>' | b'=')) {
                        let left_raw = &token_text[..eq_idx];
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

                        let var_name = left_no_bracket.trim().trim_end_matches('\\').trim();
                        if !var_name.is_empty()
                            && !var_name.chars().next().unwrap().is_ascii_digit()
                            && var_name.chars().all(is_var_ident_char)
                            && !is_reserved_keyword(var_name)
                        {
                            let name_start_byte = stmt_offset
                                + token.byte_offset
                                + token_text.find(var_name).unwrap_or(0);
                            let name_end_byte = name_start_byte + var_name.len();
                            let sel_start_u16 = byte_to_utf16_col(line, name_start_byte);
                            let sel_end_u16 = byte_to_utf16_col(line, name_end_byte);
                            let name_range = Range::new(
                                Position::new(line_idx as u32, sel_start_u16),
                                Position::new(line_idx as u32, sel_end_u16),
                            );

                            let stmt_start_u16 = byte_to_utf16_col(line, stmt_offset);
                            let stmt_end_u16 = byte_to_utf16_col(
                                line,
                                stmt_offset + token.byte_offset + token_text.len(),
                            );
                            let full_range = Range::new(
                                Position::new(line_idx as u32, stmt_start_u16),
                                Position::new(line_idx as u32, stmt_end_u16),
                            );

                            raw_variables.push((
                                VariableDef {
                                    name: var_name.to_string(),
                                    name_range,
                                    full_range,
                                    kind: VariableDeclKind::Direct,
                                    is_function_local: false,
                                    signature: trimmed_end.to_string(),
                                    doc_comment: doc_comment.clone(),
                                    enclosing_fn: None,
                                },
                                false,
                            ));
                            continue;
                        }
                    }
                }
                // Stop once a token is not a valid assignment (e.g. `cmd` in `VAR=1 cmd arg=2`)
                break;
            }
        }

        pending_decl_kw = next_pending_decl_kw;
        pending_decl_has_global_flag = next_pending_decl_global;
        pending_function_kw = next_pending_function_kw;
        pending_for_or_select_kw = next_pending_for_or_select_kw;
        pending_read_kw = next_pending_read_kw;
        pending_alias_kw = next_pending_alias_kw;
        pending_source_kw = next_pending_source_kw;
    }

    // Post-pass: assign enclosing_fn / parent_fn and compute is_function_local
    let parent_fns: Vec<Option<usize>> = functions
        .iter()
        .enumerate()
        .map(|(idx, f)| find_enclosing_fn_index(&functions, &f.full_range, Some(idx)))
        .collect();
    for (f, parent) in functions.iter_mut().zip(parent_fns) {
        f.parent_fn = parent;
    }

    let variables = raw_variables
        .into_iter()
        .map(|(mut var, has_global_flag)| {
            let enc = find_enclosing_fn_index(&functions, &var.name_range, None);
            var.enclosing_fn = enc;
            var.is_function_local =
                enc.is_some() && var.kind.is_local_by_default() && !has_global_flag;
            var
        })
        .collect();

    for alias in &mut aliases {
        alias.enclosing_fn = find_enclosing_fn_index(&functions, &alias.name_range, None);
    }

    DocumentIndex {
        functions,
        variables,
        aliases,
        sources,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_functions_posix_keyword_flagged_and_nested() {
        let text = r#"#!/bin/zsh
# Greet user
# Takes name as $1
function -T greet() {
    local msg="hi"
    inner() {
        echo "$msg"
    }
}
"#;
        let index = parse_document(text);
        assert_eq!(index.functions.len(), 2);

        let greet = &index.functions[0];
        assert_eq!(greet.name, "greet");
        assert_eq!(greet.parent_fn, None);
        assert_eq!(
            greet.doc_comment.as_deref(),
            Some("Greet user\nTakes name as $1")
        );
        assert_eq!(greet.name_range.start, Position::new(3, 12));
        assert_eq!(greet.name_range.end, Position::new(3, 17));
        assert_eq!(greet.full_range.end.line, 8);

        let inner = &index.functions[1];
        assert_eq!(inner.name, "inner");
        assert_eq!(inner.parent_fn, Some(0));

        assert_eq!(index.variables.len(), 1);
        let msg = &index.variables[0];
        assert_eq!(msg.name, "msg");
        assert!(msg.is_function_local);
        assert_eq!(msg.enclosing_fn, Some(0));
    }

    #[test]
    fn test_parse_variables_all_forms_and_multiline_continuation() {
        let text = r#"
local \
  A=1 \
  B=2
export -a ARR=(x y)
typeset -g GLOBAL_IN_FN=1
X=10 Y=20 cmd_name arg=99
for item in a b; do
  echo "$item"
done
select choice in one two; do
  break
done
for (( idx = 0; idx < 3; idx++ )); do
  echo $idx
done
while read -r line; do
  echo "$line"
done
"#;
        let index = parse_document(text);
        let names: Vec<&str> = index.variables.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "A",
                "B",
                "ARR",
                "GLOBAL_IN_FN",
                "X",
                "Y",
                "item",
                "choice",
                "idx",
                "line"
            ]
        );
        assert_eq!(index.variables[0].kind, VariableDeclKind::Keyword("local"));
        assert_eq!(index.variables[4].kind, VariableDeclKind::Direct);
        assert_eq!(index.variables[6].kind, VariableDeclKind::ForLoop);
        assert_eq!(index.variables[7].kind, VariableDeclKind::SelectLoop);
        assert_eq!(index.variables[8].kind, VariableDeclKind::CForLoop);
        assert_eq!(index.variables[9].kind, VariableDeclKind::Read);
    }

    #[test]
    fn test_parse_skips_heredoc_and_extracts_aliases_and_sources() {
        let text = r#"
cat <<EOF
IGNORED_VAR=1
ignored_fn() { : }
alias ignored_alias='x'
source ./ignored.zsh
EOF

alias -g G='| grep' ll="ls -la"
source "./lib/helper.zsh"
. ~/env.zsh
"#;
        let index = parse_document(text);
        assert!(index.functions.is_empty());
        assert!(index.variables.is_empty());
        assert_eq!(index.aliases.len(), 2);
        assert_eq!(index.aliases[0].name, "G");
        assert_eq!(index.aliases[0].expansion.as_deref(), Some("| grep"));
        assert_eq!(index.aliases[1].name, "ll");
        assert_eq!(index.aliases[1].expansion.as_deref(), Some("ls -la"));

        assert_eq!(index.sources.len(), 2);
        assert_eq!(index.sources[0].path, "./lib/helper.zsh");
        assert_eq!(index.sources[1].path, "~/env.zsh");
    }

    #[test]
    fn test_parse_global_vs_local_in_function() {
        let text = r#"
fn_a() {
    local loc_var=1
    typeset -g glob_var=2
    export exp_var=3
    direct_var=4
}
"#;
        let index = parse_document(text);
        assert_eq!(index.variables.len(), 4);
        assert!(index.variables[0].is_function_local);
        assert!(!index.variables[1].is_function_local);
        assert!(!index.variables[2].is_function_local);
        assert!(!index.variables[3].is_function_local);
        for v in &index.variables {
            assert_eq!(v.enclosing_fn, Some(0));
        }
    }

    #[test]
    fn test_parse_statements_after_mid_word_hash() {
        let text = concat!(
            "echo foo#bar; AFTER_VAR=1\n",
            "TAG=v1#rc; after_fn() { : }\n",
            "echo x#y; source ./after.zsh\n",
        );
        let index = parse_document(text);

        let var_names: Vec<&str> = index.variables.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(var_names, vec!["AFTER_VAR", "TAG"]);

        let fn_names: Vec<&str> = index.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(fn_names, vec!["after_fn"]);

        let source_paths: Vec<&str> = index.sources.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(source_paths, vec!["./after.zsh"]);
    }

    #[test]
    fn test_parse_source_unclosed_quote_and_backslash_escaped_paths() {
        let text = concat!(
            "source \"./unclosed.zsh\n",
            "source ./dir\\\\file.zsh\n",
            ". './quoted\\ path.zsh'\n",
        );
        let index = parse_document(text);
        let paths: Vec<&str> = index.sources.iter().map(|s| s.path.as_str()).collect();
        assert!(
            paths.contains(&"./unclosed.zsh"),
            "Expected unclosed quoted source path, got: {paths:?}"
        );
        assert!(
            paths.contains(&"./dir"),
            "Expected unquoted path stopped at backslash in current parser, got: {paths:?}"
        );
        assert!(
            paths
                .iter()
                .any(|p| p.contains("quoted") && p.contains("path.zsh")),
            "Expected single-quoted path, got: {paths:?}"
        );
    }
}

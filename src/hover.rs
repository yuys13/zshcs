use std::time::Duration;
use tower_lsp::lsp_types::{HoverContents, MarkupContent, MarkupKind, Position, Range};

pub use crate::analysis::builtins::get_builtin_or_reserved_doc;
pub use crate::analysis::syntax::is_word_char;
use crate::analysis::syntax::{
    byte_to_utf16_col, detect_heredoc_delimiter, find_read_command, is_func_ident_char,
    is_reserved_keyword, is_var_ident_char, skip_flags, split_declaration_tokens,
    split_line_statements, utf16_col_to_byte,
};
use crate::definition::{find_innermost_enclosing_function, scan_function_body_ranges};
pub use crate::runtime::man::{
    DEFAULT_HOVER_MAN_TIMEOUT, ManPageResult, clean_man_text, format_man_markdown, get_man_page,
    get_man_page_result,
};
use crate::runtime::man::{
    ManCache, is_eligible_for_external_command, resolve_man_markdown_with_fetcher,
};

/// Extracts the word under the cursor position and its Range in UTF-16 coordinates.
///
/// Safely handles multi-byte UTF-8 sequences and UTF-16 surrogate pairs.
/// Returns `None` if the cursor is not positioned on a valid word or if the position is out of bounds.
pub fn extract_word_at_position(text: &str, position: Position) -> Option<(&str, Range)> {
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
    let target_char = position.character as usize;

    // Find token spans in line (byte offsets and UTF-16 code unit offsets)
    struct TokenSpan {
        start_byte: usize,
        end_byte: usize,
        start_u16: usize,
        end_u16: usize,
    }

    let mut tokens: Vec<TokenSpan> = Vec::new();
    let mut in_word = false;
    let mut word_start_byte = 0;
    let mut word_start_u16 = 0;
    let mut current_u16 = 0;

    for (byte_idx, c) in line_str.char_indices() {
        let char_u16_len = c.len_utf16();
        if is_word_char(c) {
            if !in_word {
                in_word = true;
                word_start_byte = line_start + byte_idx;
                word_start_u16 = current_u16;
            }
        } else if in_word {
            in_word = false;
            tokens.push(TokenSpan {
                start_byte: word_start_byte,
                end_byte: line_start + byte_idx,
                start_u16: word_start_u16,
                end_u16: current_u16,
            });
        }
        current_u16 += char_u16_len;
    }

    if in_word {
        tokens.push(TokenSpan {
            start_byte: word_start_byte,
            end_byte: line_end,
            start_u16: word_start_u16,
            end_u16: current_u16,
        });
    }

    if target_char >= current_u16 {
        return None;
    }

    for token in tokens {
        if target_char >= token.start_u16 && target_char < token.end_u16 {
            let word_str = &text[token.start_byte..token.end_byte];
            let range = Range {
                start: Position::new(position.line, token.start_u16 as u32),
                end: Position::new(position.line, token.end_u16 as u32),
            };
            return Some((word_str, range));
        }
    }

    None
}

/// Generates hover information for a word asynchronously with default timeout.
pub async fn get_hover_info(word: &str) -> Option<HoverContents> {
    get_hover_info_with_timeout(word, DEFAULT_HOVER_MAN_TIMEOUT).await
}

/// Generates hover information for a word asynchronously with a custom timeout.
///
/// Priority:
/// 1. Zsh builtins & reserved words static Markdown dictionary.
/// 2. External command `man` page full text in a code block.
pub async fn get_hover_info_with_timeout(
    word: &str,
    timeout_dur: Duration,
) -> Option<HoverContents> {
    // 1. Check static dictionary for word or basename
    if let Some(doc) = get_builtin_or_reserved_doc(word) {
        return Some(HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: doc.to_string(),
        }));
    }

    let target = if let Some(slash_idx) = word.rfind('/') {
        &word[slash_idx + 1..]
    } else {
        word
    };

    if !target.is_empty()
        && target != word
        && let Some(doc) = get_builtin_or_reserved_doc(target)
    {
        return Some(HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: doc.to_string(),
        }));
    }

    // 2. Query man page asynchronously
    let man_text = get_man_page(word, timeout_dur).await?;
    if man_text.is_empty() {
        return None;
    }

    let markdown = format_man_markdown(&man_text);
    Some(HoverContents::Markup(MarkupContent {
        kind: MarkupKind::Markdown,
        value: markdown,
    }))
}

/// Generates hover information for a word asynchronously using a shared `ManCache` and default timeout.
pub async fn get_hover_info_cached(word: &str, cache: &ManCache) -> Option<HoverContents> {
    get_hover_info_cached_with_timeout(word, cache, DEFAULT_HOVER_MAN_TIMEOUT).await
}

/// Generates hover information for a word asynchronously using a shared `ManCache` and custom timeout.
///
/// Priority:
/// 1. Zsh builtins & reserved words static Markdown dictionary.
/// 2. External command `man` page resolved via shared cache and single-flight deduplication.
pub async fn get_hover_info_cached_with_timeout(
    word: &str,
    cache: &ManCache,
    timeout_dur: Duration,
) -> Option<HoverContents> {
    // 1. Check static dictionary for word or basename
    if let Some(doc) = get_builtin_or_reserved_doc(word) {
        return Some(HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: doc.to_string(),
        }));
    }

    let target = if let Some(slash_idx) = word.rfind('/') {
        &word[slash_idx + 1..]
    } else {
        word
    };

    if !target.is_empty()
        && target != word
        && let Some(doc) = get_builtin_or_reserved_doc(target)
    {
        return Some(HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: doc.to_string(),
        }));
    }

    if !is_eligible_for_external_command(None, target) {
        return None;
    }

    // 2. Query man page asynchronously with shared cache and single-flight coalescing
    let markdown =
        resolve_man_markdown_with_fetcher(target, cache, timeout_dur, |cmd, dur| async move {
            get_man_page_result(&cmd, dur).await
        })
        .await?;

    Some(HoverContents::Markup(MarkupContent {
        kind: MarkupKind::Markdown,
        value: markdown,
    }))
}

/// Returns `true` if the word at `range` is directly preceded by a shell variable sigil
/// such as `$VAR`, `${VAR}`, `${#VAR}`, or `${(U)VAR}`.
pub fn is_variable_reference_at(text: &str, range: Range) -> bool {
    let Some(line) = text.lines().nth(range.start.line as usize) else {
        return false;
    };
    let start_byte = utf16_col_to_byte(line, range.start.character);
    let prefix = &line[..start_byte];
    if prefix.ends_with('$') || prefix.ends_with("${") {
        return true;
    }
    if let Some(brace_idx) = prefix.rfind("${") {
        let between = &prefix[brace_idx + 2..];
        if !between.contains('}')
            && !between.contains(' ')
            && !between.contains('\t')
            && (between
                .chars()
                .all(|c| matches!(c, '#' | '!' | '^' | '=' | '~'))
                || (between.starts_with('(') && between.ends_with(')')))
        {
            return true;
        }
    }
    false
}

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

fn format_local_symbol_markdown(
    lines: &[&str],
    def_line_idx: usize,
    name: &str,
    kind_label: &str,
    signature: &str,
) -> String {
    let mut md = format!("### `{name}` ({kind_label})\n\n```zsh\n{signature}\n```");
    if let Some(doc) = extract_preceding_comment_block(lines, def_line_idx) {
        md.push_str("\n\n");
        md.push_str(&doc);
    }
    md
}

fn find_local_function_hover(lines: &[&str], word: &str) -> Option<String> {
    if word.is_empty()
        || word.chars().next()?.is_ascii_digit()
        || is_reserved_keyword(word)
        || !word.chars().all(is_func_ident_char)
    {
        return None;
    }

    let mut heredoc_delimiter: Option<String> = None;
    for (line_idx, &line) in lines.iter().enumerate() {
        if let Some(ref delim) = heredoc_delimiter {
            if line.trim_start_matches('\t').trim_end() == delim {
                heredoc_delimiter = None;
            }
            continue;
        }
        if let Some(delim) = detect_heredoc_delimiter(line) {
            heredoc_delimiter = Some(delim);
        }

        for stmt in split_line_statements(line) {
            let trimmed = stmt.text.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            // POSIX style: `word() ...` or `word () ...`
            if let Some(rest) = trimmed.strip_prefix(word) {
                let rest_t = rest.trim_start();
                if let Some(after_open) = rest_t.strip_prefix('(')
                    && let Some(after_close) = after_open.trim_start().strip_prefix(')')
                {
                    let after_parens = after_close.trim_start();
                    if after_parens.is_empty()
                        || after_parens.starts_with('{')
                        || after_parens.starts_with('#')
                        || after_parens.starts_with(';')
                    {
                        return Some(format_local_symbol_markdown(
                            lines,
                            line_idx,
                            word,
                            "Local Function",
                            trimmed,
                        ));
                    }
                }
            }

            // Keyword style: `function [flags...] word ...`
            if trimmed.starts_with("function ") || trimmed.starts_with("function\t") {
                let after_fn = skip_flags(trimmed[8..].trim_start());
                if let Some(rest) = after_fn.strip_prefix(word) {
                    let rest_t = rest.trim_start();
                    let is_header_end = rest_t.is_empty()
                        || rest_t.starts_with('{')
                        || rest_t.starts_with('#')
                        || rest_t.starts_with(';')
                        || (rest_t.starts_with('(') && rest_t.contains(')'));
                    if is_header_end {
                        return Some(format_local_symbol_markdown(
                            lines,
                            line_idx,
                            word,
                            "Local Function",
                            trimmed,
                        ));
                    }
                }
            }
        }
    }

    None
}

fn find_local_alias_hover(lines: &[&str], word: &str) -> Option<String> {
    if word.is_empty()
        || is_reserved_keyword(word)
        || !word.chars().all(|c| is_func_ident_char(c) || c == '-')
    {
        return None;
    }

    let mut heredoc_delimiter: Option<String> = None;
    for (line_idx, &line) in lines.iter().enumerate() {
        if let Some(ref delim) = heredoc_delimiter {
            if line.trim_start_matches('\t').trim_end() == delim {
                heredoc_delimiter = None;
            }
            continue;
        }
        if let Some(delim) = detect_heredoc_delimiter(line) {
            heredoc_delimiter = Some(delim);
        }

        for stmt in split_line_statements(line) {
            let trimmed = stmt.text.trim();
            if !(trimmed.starts_with("alias ") || trimmed.starts_with("alias\t")) {
                continue;
            }
            for token in split_declaration_tokens(&trimmed[5..]) {
                if token.text.starts_with('-') || token.text.starts_with('+') {
                    continue;
                }
                if let Some(eq) = token.text.find('=')
                    && token.text[..eq].trim() == word
                {
                    let sig = format!("alias {}", token.text.trim());
                    return Some(format_local_symbol_markdown(
                        lines,
                        line_idx,
                        word,
                        "Local Alias",
                        &sig,
                    ));
                }
            }
        }
    }

    None
}

fn token_defines_variable(token_text: &str, var_name: &str) -> bool {
    let Some(rest) = token_text.strip_prefix(var_name) else {
        return false;
    };
    rest.is_empty()
        || rest.starts_with('=')
        || rest.starts_with("+=")
        || (rest.starts_with('[')
            && (rest.find(']').is_some_and(|close| {
                let after = &rest[close + 1..];
                after.is_empty() || after.starts_with('=') || after.starts_with("+=")
            }) || rest.ends_with(']')))
}

fn find_local_variable_hover(
    text: &str,
    lines: &[&str],
    word: &str,
    cursor_pos: Option<Position>,
) -> Option<String> {
    if word.is_empty()
        || word.chars().next()?.is_ascii_digit()
        || is_reserved_keyword(word)
        || !word.chars().all(is_var_ident_char)
    {
        return None;
    }

    let decl_keywords = [
        "export", "typeset", "local", "declare", "readonly", "integer", "float",
    ];
    let mut heredoc_delimiter: Option<String> = None;
    let mut candidates: Vec<(usize, Position, &str, bool)> = Vec::new();

    for (line_idx, &line) in lines.iter().enumerate() {
        if let Some(ref delim) = heredoc_delimiter {
            if line.trim_start_matches('\t').trim_end() == delim {
                heredoc_delimiter = None;
            }
            continue;
        }
        if let Some(delim) = detect_heredoc_delimiter(line) {
            heredoc_delimiter = Some(delim);
        }

        for stmt in split_line_statements(line) {
            let trimmed_start = stmt.text.trim_start();
            let trimmed = trimmed_start.trim_end();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let stmt_offset = stmt.start_byte + (stmt.text.len() - trimmed_start.len());
            let stmt_pos = Position::new(line_idx as u32, byte_to_utf16_col(line, stmt_offset));

            // 1. Keyword variable declarations (`export`, `typeset`, `local`, etc.)
            if let Some(kw) = decl_keywords.iter().find(|&&kw| {
                trimmed.starts_with(kw)
                    && (trimmed[kw.len()..].starts_with(' ')
                        || trimmed[kw.len()..].starts_with('\t'))
            }) {
                let tokens = split_declaration_tokens(&trimmed[kw.len()..]);
                let has_global_flag = *kw == "export"
                    || tokens
                        .iter()
                        .take_while(|t| t.text.starts_with('-') || t.text.starts_with('+'))
                        .any(|t| t.text.contains('g'));
                let matched = tokens.iter().any(|t| {
                    !t.text.starts_with('-')
                        && !t.text.starts_with('+')
                        && token_defines_variable(t.text, word)
                });
                if matched {
                    candidates.push((line_idx, stmt_pos, trimmed, !has_global_flag));
                    continue;
                }
            }

            // 2. Direct assignments (`VAR=...`, `VAR+=...`, `VAR[k]=...`)
            let mut matched_direct = false;
            for token in split_declaration_tokens(trimmed) {
                if let Some(rest) = token.text.strip_prefix(word) {
                    let is_direct = rest.starts_with('=')
                        || rest.starts_with("+=")
                        || (rest.starts_with('[')
                            && rest.find(']').is_some_and(|close| {
                                let after = &rest[close + 1..];
                                after.starts_with('=') || after.starts_with("+=")
                            }));
                    if is_direct {
                        candidates.push((line_idx, stmt_pos, trimmed, false));
                        matched_direct = true;
                        break;
                    }
                }
                if !token.text.contains('=') {
                    break;
                }
            }
            if matched_direct {
                continue;
            }

            // 3. Loop variables (`for VAR ...`, `select VAR ...`) and `for (( VAR = ... ))`
            let for_kw_len = if trimmed.starts_with("for ") || trimmed.starts_with("for\t") {
                Some(4)
            } else if trimmed.starts_with("select ") || trimmed.starts_with("select\t") {
                Some(7)
            } else {
                None
            };
            if let Some(kw_len) = for_kw_len {
                let after_kw = trimmed[kw_len..].trim_start();
                if let Some(rest) = after_kw.strip_prefix(word)
                    && (rest.is_empty()
                        || rest.starts_with(' ')
                        || rest.starts_with('\t')
                        || rest.starts_with(';')
                        || rest.starts_with('('))
                {
                    candidates.push((line_idx, stmt_pos, trimmed, false));
                    continue;
                }
            }
            if trimmed.starts_with("for ((") || trimmed.starts_with("for((") {
                let after_for = trimmed
                    .strip_prefix("for ((")
                    .or_else(|| trimmed.strip_prefix("for(("))
                    .unwrap_or("");
                if let Some(close_idx) = after_for.find("))") {
                    let clause = after_for[..close_idx].split(';').next().unwrap_or("");
                    let matched = split_declaration_tokens(clause).into_iter().any(|t| {
                        t.text.strip_prefix(word).is_some_and(|r| {
                            r.is_empty()
                                || r.starts_with('=')
                                || r.starts_with("+=")
                                || r.starts_with("++")
                                || r.starts_with("--")
                        })
                    });
                    if matched {
                        candidates.push((line_idx, stmt_pos, trimmed, false));
                        continue;
                    }
                }
            }

            // 4. `read` statements (`read VAR`, `read -r VAR`, `while read -r line`)
            if let Some((_, after_read)) = find_read_command(trimmed) {
                let matched = split_declaration_tokens(after_read).into_iter().any(|t| {
                    !t.text.starts_with('-')
                        && !t.text.starts_with('+')
                        && t.text
                            .strip_prefix(word)
                            .is_some_and(|r| r.is_empty() || r.starts_with('='))
                });
                if matched {
                    candidates.push((line_idx, stmt_pos, trimmed, false));
                }
            }
        }
    }

    if candidates.is_empty() {
        return None;
    }

    let fn_ranges = scan_function_body_ranges(text);
    let cursor_fn = cursor_pos.and_then(|pos| find_innermost_enclosing_function(&fn_ranges, pos));

    if let Some(cur_fn) = cursor_fn
        && let Some(&(line_idx, _, sig, _)) = candidates.iter().find(|&&(_, pos, _, is_local)| {
            is_local && find_innermost_enclosing_function(&fn_ranges, pos) == Some(cur_fn)
        })
    {
        return Some(format_local_symbol_markdown(
            lines,
            line_idx,
            word,
            "Local Variable",
            sig,
        ));
    }

    let chosen = candidates.iter().find(|&&(_, pos, _, is_local)| {
        let enc = find_innermost_enclosing_function(&fn_ranges, pos);
        let scoped_local = enc.is_some() && is_local;
        !(scoped_local && enc != cursor_fn)
    });

    let &(line_idx, _, sig, _) = match chosen {
        Some(c) => c,
        None if cursor_pos.is_none() => &candidates[0],
        None => return None,
    };

    Some(format_local_symbol_markdown(
        lines,
        line_idx,
        word,
        "Local Variable",
        sig,
    ))
}

/// Resolves Markdown hover documentation for script-local functions, aliases, and variables.
pub fn get_local_symbol_hover_doc(text: &str, word: &str, is_variable_ref: bool) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    if is_variable_ref {
        find_local_variable_hover(text, &lines, word, None)
    } else {
        find_local_function_hover(&lines, word)
            .or_else(|| find_local_alias_hover(&lines, word))
            .or_else(|| find_local_variable_hover(text, &lines, word, None))
    }
}

/// Resolves Markdown hover documentation for script-local functions, aliases, and variables
/// taking into account function-local variable scoping at `position`.
pub fn get_local_symbol_hover_doc_at(
    text: &str,
    word: &str,
    position: Position,
    is_variable_ref: bool,
) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    if is_variable_ref {
        find_local_variable_hover(text, &lines, word, Some(position))
    } else {
        find_local_function_hover(&lines, word)
            .or_else(|| find_local_alias_hover(&lines, word))
            .or_else(|| find_local_variable_hover(text, &lines, word, Some(position)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    // 1. Basic word extraction on single line
    #[case("echo hello world", 0, 0, Some(("echo", 0, 4)))]
    #[case("echo hello world", 0, 2, Some(("echo", 0, 4)))]
    #[case("echo hello world", 0, 3, Some(("echo", 0, 4)))]
    #[case("echo hello world", 0, 4, None)] // space after echo
    #[case("echo hello world", 0, 5, Some(("hello", 5, 10)))]
    #[case("echo hello world", 0, 8, Some(("hello", 5, 10)))]
    #[case("echo hello world", 0, 9, Some(("hello", 5, 10)))]
    #[case("echo hello world", 0, 10, None)] // space after hello
    #[case("echo hello world", 0, 11, Some(("world", 11, 16)))]
    #[case("echo hello world", 0, 15, Some(("world", 11, 16)))]
    #[case("echo hello world", 0, 16, None)] // end of line
    #[case("echo hello world", 0, 17, None)]
    // 2. Spaces and gaps
    #[case("   cd   /tmp   ", 0, 0, None)]
    #[case("   cd   /tmp   ", 0, 2, None)]
    #[case("   cd   /tmp   ", 0, 3, Some(("cd", 3, 5)))]
    #[case("   cd   /tmp   ", 0, 4, Some(("cd", 3, 5)))]
    #[case("   cd   /tmp   ", 0, 5, None)] // space after cd
    #[case("   cd   /tmp   ", 0, 6, None)]
    #[case("   cd   /tmp   ", 0, 8, Some(("/tmp", 8, 12)))]
    // 3. Multi-line extraction
    #[case("line1\nsetopt promptsubst\nline3", 1, 0, Some(("setopt", 0, 6)))]
    #[case("line1\nsetopt promptsubst\nline3", 1, 3, Some(("setopt", 0, 6)))]
    #[case("line1\nsetopt promptsubst\nline3", 1, 5, Some(("setopt", 0, 6)))]
    #[case("line1\nsetopt promptsubst\nline3", 1, 6, None)] // space after setopt
    #[case("line1\nsetopt promptsubst\nline3", 1, 7, Some(("promptsubst", 7, 18)))]
    #[case("line1\nsetopt promptsubst\nline3", 1, 17, Some(("promptsubst", 7, 18)))]
    #[case("line1\nsetopt promptsubst\nline3", 1, 18, None)]
    #[case("line1\nsetopt promptsubst\nline3", 2, 2, Some(("line3", 0, 5)))]
    // 4. Out of bounds lines
    #[case("single line", 1, 0, None)]
    #[case("single line", 10, 0, None)]
    #[case("first\n", 1, 0, None)] // empty trailing line
    fn test_extract_word_at_position_basic(
        #[case] text: &str,
        #[case] line: u32,
        #[case] character: u32,
        #[case] expected: Option<(&str, u32, u32)>,
    ) {
        let pos = Position::new(line, character);
        let result = extract_word_at_position(text, pos);
        match expected {
            Some((word, start, end)) => {
                let (res_word, range) = result.expect("expected word match");
                assert_eq!(res_word, word);
                assert_eq!(range.start, Position::new(line, start));
                assert_eq!(range.end, Position::new(line, end));
            }
            None => {
                assert!(result.is_none());
            }
        }
    }

    #[rstest]
    // 1. Delimiters and operators: pipes, redirects, quotes, semicolons
    #[case("cat file | grep foo", 0, 9, None)] // on '|'
    #[case("cat file|grep foo", 0, 8, None)] // on '|' without space
    #[case("cat file|grep foo", 0, 7, Some(("file", 4, 8)))]
    #[case("cat file|grep foo", 0, 9, Some(("grep", 9, 13)))]
    #[case("echo \"hello\"; ls", 0, 5, None)] // on '"'
    #[case("echo \"hello\"; ls", 0, 6, Some(("hello", 6, 11)))]
    #[case("echo \"hello\"; ls", 0, 11, None)] // on closing '"'
    #[case("echo \"hello\"; ls", 0, 12, None)] // on ';'
    #[case("echo \"hello\"; ls", 0, 14, Some(("ls", 14, 16)))]
    #[case("echo;ls", 0, 4, None)] // on ';' without space
    #[case("echo;ls", 0, 3, Some(("echo", 0, 4)))]
    #[case("echo;ls", 0, 5, Some(("ls", 5, 7)))]
    #[case("(autoload -Uz compinit)", 0, 0, None)] // on '('
    #[case("(autoload -Uz compinit)", 0, 1, Some(("autoload", 1, 9)))]
    #[case("(autoload -Uz compinit)", 0, 9, None)] // space
    #[case("(autoload -Uz compinit)", 0, 10, Some(("-Uz", 10, 13)))]
    #[case("(autoload -Uz compinit)", 0, 21, Some(("compinit", 14, 22)))] // last char of 'compinit'
    #[case("(autoload -Uz compinit)", 0, 22, None)] // on ')'
    // 2. Words with hyphens and underscores
    #[case("zsh-lovers --all-targets test_func", 0, 3, Some(("zsh-lovers", 0, 10)))]
    #[case("zsh-lovers --all-targets test_func", 0, 15, Some(("--all-targets", 11, 24)))]
    #[case("zsh-lovers --all-targets test_func", 0, 28, Some(("test_func", 25, 34)))]
    // 3. Variables with '$'
    #[case("echo $VAR_NAME", 0, 5, None)] // on '$'
    #[case("echo $VAR_NAME", 0, 6, Some(("VAR_NAME", 6, 14)))]
    fn test_extract_word_at_position_delimiters(
        #[case] text: &str,
        #[case] line: u32,
        #[case] character: u32,
        #[case] expected: Option<(&str, u32, u32)>,
    ) {
        let pos = Position::new(line, character);
        let result = extract_word_at_position(text, pos);
        match expected {
            Some((word, start, end)) => {
                let (res_word, range) = result.expect("expected word match");
                assert_eq!(res_word, word);
                assert_eq!(range.start, Position::new(line, start));
                assert_eq!(range.end, Position::new(line, end));
            }
            None => {
                assert!(result.is_none());
            }
        }
    }

    #[rstest]
    // 1. Multibyte Japanese characters ("日本語" is 3 chars, 3 UTF-16 units)
    #[case("echo 日本語 echo", 0, 5, Some(("日本語", 5, 8)))]
    #[case("echo 日本語 echo", 0, 7, Some(("日本語", 5, 8)))]
    #[case("echo 日本語 echo", 0, 8, None)] // space after 日本語
    #[case("echo 日本語 echo", 0, 9, Some(("echo", 9, 13)))]
    #[case("echo 日本語 echo", 0, 12, Some(("echo", 9, 13)))]
    #[case("echo 日本語 echo", 0, 13, None)]
    // 2. Surrogate pairs SIP/SMP ('𩸽' is 1 char, 2 UTF-16 code units)
    #[case("echo 𩸽 echo", 0, 5, Some(("𩸽", 5, 7)))]
    #[case("echo 𩸽 echo", 0, 6, Some(("𩸽", 5, 7)))]
    #[case("echo 𩸽 echo", 0, 7, None)] // space after 𩸽
    #[case("echo 𩸽 echo", 0, 8, Some(("echo", 8, 12)))]
    // 3. Emojis
    #[case("echo 🎉🎊 test", 0, 5, Some(("🎉🎊", 5, 9)))]
    #[case("echo 🎉🎊 test", 0, 8, Some(("🎉🎊", 5, 9)))]
    #[case("echo 🎉🎊 test", 0, 9, None)] // space after emojis
    fn test_extract_word_at_position_unicode(
        #[case] text: &str,
        #[case] line: u32,
        #[case] character: u32,
        #[case] expected: Option<(&str, u32, u32)>,
    ) {
        let pos = Position::new(line, character);
        let result = extract_word_at_position(text, pos);
        match expected {
            Some((word, start, end)) => {
                let (res_word, range) = result.expect("expected word match");
                assert_eq!(res_word, word);
                assert_eq!(range.start, Position::new(line, start));
                assert_eq!(range.end, Position::new(line, end));
            }
            None => {
                assert!(result.is_none());
            }
        }
    }

    #[test]
    fn test_extract_word_empty_and_edge_cases() {
        assert!(extract_word_at_position("", Position::new(0, 0)).is_none());
        assert!(extract_word_at_position("   ", Position::new(0, 1)).is_none());
        assert!(extract_word_at_position("echo\n", Position::new(0, 100)).is_none());
        assert!(extract_word_at_position("echo\n", Position::new(1, 0)).is_none());
        assert!(extract_word_at_position("echo\r\n", Position::new(0, 3)).is_some());
    }

    #[test]
    fn test_clean_man_text_overstrikes_and_escapes() {
        // Bold overstrike: N\bN A\bA M\bM E\bE
        let raw_bold = "N\x08NA\x08AM\x08ME\x08E";
        assert_eq!(clean_man_text(raw_bold), "NAME");

        // Triple overstrike: c\bc\bc
        let raw_triple = "c\x08c\x08c";
        assert_eq!(clean_man_text(raw_triple), "c");

        // Underline overstrike: _\bf _\bo _\bo
        let raw_underline = "_\x08f_\x08o_\x08o";
        assert_eq!(clean_man_text(raw_underline), "foo");

        // Underlined bold: _\bc\bc
        let raw_under_bold = "_\x08c\x08c";
        assert_eq!(clean_man_text(raw_under_bold), "c");

        // ANSI color escape: \x1b[32mhello\x1b[0m
        let raw_ansi = "\x1b[32mhello\x1b[0m world";
        assert_eq!(clean_man_text(raw_ansi), "hello world");

        // Unicode minus normalization
        let raw_unicode = "grep \u{2212}v pattern";
        assert_eq!(clean_man_text(raw_unicode), "grep -v pattern");

        // Plain text
        let raw_plain = "Simple line\nSecond line\n";
        assert_eq!(clean_man_text(raw_plain), "Simple line\nSecond line");
    }

    #[test]
    fn test_builtin_and_reserved_word_dictionary() {
        // Verify key builtins
        let builtins = [
            "cd",
            "echo",
            "export",
            "set",
            "setopt",
            "unsetopt",
            "autoload",
            "compadd",
            "typeset",
            "print",
            "printf",
            "source",
            ".",
            "eval",
            "alias",
            "unalias",
            "read",
            "return",
            "exit",
            "shift",
            "test",
            "trap",
            "unset",
            "local",
            "declare",
            "which",
            "where",
            "whence",
            "type",
            "bindkey",
            "zstyle",
            "zmodload",
            "zpty",
            "zparseopts",
            "pushd",
            "popd",
            "dirs",
            "pwd",
            "history",
            "fc",
            "bg",
            "fg",
            "jobs",
            "kill",
            "wait",
            "disown",
            "exec",
            "hash",
            "rehash",
            "umask",
            "true",
            "false",
            ":",
        ];
        for b in builtins {
            let doc = get_builtin_or_reserved_doc(b);
            assert!(doc.is_some(), "Builtin '{b}' should have documentation");
            let doc_str = doc.unwrap();
            assert!(
                doc_str.contains("Builtin") || doc_str.contains("Module"),
                "Doc for '{b}' should mention Builtin or Module"
            );
            assert!(
                doc_str.contains("```zsh"),
                "Doc for '{b}' should contain zsh syntax block"
            );
        }

        // Verify key reserved words
        let reserved = [
            "if",
            "then",
            "elif",
            "else",
            "fi",
            "for",
            "do",
            "done",
            "while",
            "until",
            "case",
            "esac",
            "select",
            "function",
            "repeat",
            "time",
            "coproc",
            "nocorrect",
            "foreach",
            "end",
        ];
        for r in reserved {
            let doc = get_builtin_or_reserved_doc(r);
            assert!(
                doc.is_some(),
                "Reserved word '{r}' should have documentation"
            );
            let doc_str = doc.unwrap();
            assert!(
                doc_str.contains("Reserved Word"),
                "Doc for '{r}' should mention Reserved Word"
            );
            assert!(
                doc_str.contains("```zsh"),
                "Doc for '{r}' should contain zsh syntax block"
            );
        }

        // Non-existent keyword
        assert!(get_builtin_or_reserved_doc("non_existent_command_xyz").is_none());
    }

    #[tokio::test]
    async fn test_get_hover_info_builtin() {
        let hover = get_hover_info("setopt").await;
        assert!(hover.is_some());
        if let Some(HoverContents::Markup(markup)) = hover {
            assert_eq!(markup.kind, MarkupKind::Markdown);
            assert!(markup.value.contains("`setopt` (Zsh Builtin)"));
        } else {
            panic!("Expected HoverContents::Markup");
        }
    }

    #[tokio::test]
    async fn test_get_hover_info_reserved_word() {
        let hover = get_hover_info("while").await;
        assert!(hover.is_some());
        if let Some(HoverContents::Markup(markup)) = hover {
            assert_eq!(markup.kind, MarkupKind::Markdown);
            assert!(markup.value.contains("`while` (Zsh Reserved Word)"));
        } else {
            panic!("Expected HoverContents::Markup");
        }
    }

    #[tokio::test]
    async fn test_get_hover_info_man_page_fallback() {
        // 'git' is a standard tool included in nativeBuildInputs with a man page
        let hover = get_hover_info("git").await;
        assert!(
            hover.is_some(),
            "Expected hover info for 'git' via man page"
        );
        if let Some(HoverContents::Markup(markup)) = hover {
            assert_eq!(markup.kind, MarkupKind::Markdown);
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
    async fn test_get_hover_info_path_command() {
        // '/usr/bin/git' should resolve to 'git' man page rather than opening binary
        let hover = get_hover_info("/usr/bin/git").await;
        assert!(
            hover.is_some(),
            "Expected hover info for '/usr/bin/git' via man page"
        );
        if let Some(HoverContents::Markup(markup)) = hover {
            assert_eq!(markup.kind, MarkupKind::Markdown);
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
    async fn test_get_hover_info_path_builtin() {
        // '/bin/echo' should resolve to echo builtin documentation
        let hover = get_hover_info("/bin/echo").await;
        assert!(hover.is_some());
        if let Some(HoverContents::Markup(markup)) = hover {
            assert_eq!(markup.kind, MarkupKind::Markdown);
            assert!(markup.value.contains("`echo` (Zsh Builtin)"));
        } else {
            panic!("Expected HoverContents::Markup");
        }
    }

    #[tokio::test]
    async fn test_get_hover_info_nonexistent_command() {
        let hover = get_hover_info("nonexistent_command_xyz123_456789").await;
        assert!(hover.is_none());
    }

    #[tokio::test]
    async fn test_get_hover_info_invalid_command_characters() {
        assert!(get_hover_info("").await.is_none());
        assert!(get_hover_info("--help").await.is_none());
        assert!(get_hover_info("ls; rm").await.is_none());
    }

    #[tokio::test]
    async fn test_get_hover_info_timeout_handling() {
        // Zero timeout should expire immediately
        let hover = get_hover_info_with_timeout("ls", Duration::from_nanos(1)).await;
        // Either times out and returns None, or if cached returns Some
        // We verify that it does not panic or hang
        let _ = hover;
    }

    #[tokio::test]
    async fn test_get_man_page_edge_cases() {
        // Empty target
        assert!(get_man_page("", Duration::from_secs(1)).await.is_none());
        // Leading hyphen (option-like argument)
        assert!(get_man_page("-la", Duration::from_secs(1)).await.is_none());
        // Command injection attempt or invalid characters
        assert!(
            get_man_page("ls; rm -rf /", Duration::from_secs(1))
                .await
                .is_none()
        );
        assert!(
            get_man_page("cat $(whoami)", Duration::from_secs(1))
                .await
                .is_none()
        );
        // Excessively long target name (> 256 characters)
        let long_name = "a".repeat(300);
        assert!(
            get_man_page(&long_name, Duration::from_secs(1))
                .await
                .is_none()
        );
        // Non-existent command
        assert!(
            get_man_page("nonexistent_command_xyz123_456789", Duration::from_secs(1))
                .await
                .is_none()
        );
    }

    #[test]
    fn test_clean_man_text_tabs_and_cr() {
        // Tab stop expansion (every 8 columns)
        let raw_tabs = "a\tb";
        assert_eq!(clean_man_text(raw_tabs), "a       b");

        // Carriage return reset column
        let raw_cr = "hello\rworld";
        assert_eq!(clean_man_text(raw_cr), "world");
    }

    #[test]
    fn test_clean_man_text_multibyte_and_form_feed() {
        // Form feed reset column
        let raw_ff = "hello\x0cworld";
        assert_eq!(clean_man_text(raw_ff), "world");

        // Multibyte character overstrike with underline
        let raw_under_mb = "_\x08あ";
        assert_eq!(clean_man_text(raw_under_mb), "あ");

        let raw_mb_under = "あ\x08_";
        assert_eq!(clean_man_text(raw_mb_under), "あ");

        // Multibyte overwrite
        let raw_overwrite = "あ\x08い";
        assert_eq!(clean_man_text(raw_overwrite), "い");
    }

    #[test]
    fn test_format_man_markdown_escaping() {
        let plain = "NAME\n    git - fast version control";
        assert_eq!(
            format_man_markdown(plain),
            "```text\nNAME\n    git - fast version control\n```"
        );

        let with_backticks = "Run ```command``` to execute.";
        assert_eq!(
            format_man_markdown(with_backticks),
            "````text\nRun ```command``` to execute.\n````"
        );
    }

    #[tokio::test]
    async fn test_get_hover_info_cached_populates_and_reuses_cache() {
        let cache = crate::completion::ManCache::new();
        assert!(!cache.contains_key("git"));

        // First call should resolve man page and populate cache
        let first = get_hover_info_cached("git", &cache).await;
        assert!(first.is_some(), "Expected hover info for 'git'");
        assert!(cache.contains_key("git"));
        assert!(cache.get("git").unwrap().is_some());

        // Replace cache value with sentinel to verify second call reads from cache
        cache.insert(
            "git".to_string(),
            Some("```text\ncached sentinel\n```".to_string()),
        );
        let second = get_hover_info_cached("git", &cache).await;
        match second {
            Some(HoverContents::Markup(markup)) => {
                assert_eq!(markup.value, "```text\ncached sentinel\n```");
            }
            other => panic!("Expected cached MarkupContent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_get_hover_info_cached_negative_cache() {
        let cache = crate::completion::ManCache::new();
        let dummy = "nonexistent_hover_cmd_xyz123";

        let first = get_hover_info_cached(dummy, &cache).await;
        assert!(first.is_none());
        assert!(cache.contains_key(dummy));
        assert_eq!(*cache.get(dummy).unwrap(), None);

        // Even if we don't spawn man again, negative cache returns None
        let second = get_hover_info_cached(dummy, &cache).await;
        assert!(second.is_none());
    }

    #[test]
    fn test_get_local_symbol_hover_doc_function_variable_and_alias() {
        let text = concat!(
            "#!/usr/bin/env zsh\n",
            "\n",
            "# Deploys the application.\n",
            "# Arguments: $1 - target environment\n",
            "deploy_app() {\n",
            "  echo \"$1\"\n",
            "}\n",
            "\n",
            "# API token for service\n",
            "export API_KEY=\"secret\"\n",
            "PORT=8080\n",
            "alias ll='ls -la'\n",
            "\n",
            "cat <<'EOF'\n",
            "# fake comment\n",
            "heredoc_func() { : }\n",
            "EOF\n",
            "\n",
            "echo $PORT ${API_KEY}\n",
        );

        let fn_doc = get_local_symbol_hover_doc(text, "deploy_app", false)
            .expect("Expected function hover doc");
        assert!(fn_doc.contains("### `deploy_app` (Local Function)"));
        assert!(fn_doc.contains("```zsh\ndeploy_app()\n```"));
        assert!(fn_doc.contains("Deploys the application.\nArguments: $1 - target environment"));
        assert!(!fn_doc.contains("/usr/bin/env"));

        let var_doc =
            get_local_symbol_hover_doc(text, "API_KEY", true).expect("Expected variable hover doc");
        assert!(var_doc.contains("### `API_KEY` (Local Variable)"));
        assert!(var_doc.contains("export API_KEY=\"secret\""));
        assert!(var_doc.contains("API token for service"));

        let port_range = extract_word_at_position(text, Position::new(18, 6))
            .unwrap()
            .1;
        assert!(is_variable_reference_at(text, port_range));
        let port_doc = get_local_symbol_hover_doc(text, "PORT", true)
            .expect("Expected PORT variable hover doc");
        assert!(port_doc.contains("PORT=8080"));

        let alias_doc =
            get_local_symbol_hover_doc(text, "ll", false).expect("Expected alias hover doc");
        assert!(alias_doc.contains("### `ll` (Local Alias)"));
        assert!(alias_doc.contains("alias ll='ls -la'"));

        assert!(
            get_local_symbol_hover_doc(text, "heredoc_func", false).is_none(),
            "Heredoc body must be ignored"
        );
    }

    #[test]
    fn test_get_local_symbol_hover_doc_at_respects_function_local_variable_scoping() {
        let text = concat!(
            "# Global mode\n",
            "MODE=\"global\"\n",
            "fn_a() {\n",
            "  # Local mode A\n",
            "  local MODE=\"local_a\"\n",
            "  # Private to A\n",
            "  local ONLY_A=\"secret\"\n",
            "  echo \"$MODE\" \"$ONLY_A\"\n",
            "}\n",
            "fn_b() {\n",
            "  # Local mode B\n",
            "  typeset MODE=\"local_b\"\n",
            "  echo \"$MODE\"\n",
            "}\n",
            "echo \"$MODE\"\n",
        );

        let doc_a = get_local_symbol_hover_doc_at(text, "MODE", Position::new(7, 10), true)
            .expect("Expected MODE hover inside fn_a");
        assert!(doc_a.contains("local MODE=\"local_a\""));
        assert!(doc_a.contains("Local mode A"));

        let doc_b = get_local_symbol_hover_doc_at(text, "MODE", Position::new(12, 10), true)
            .expect("Expected MODE hover inside fn_b");
        assert!(doc_b.contains("typeset MODE=\"local_b\""));
        assert!(doc_b.contains("Local mode B"));

        let doc_global = get_local_symbol_hover_doc_at(text, "MODE", Position::new(14, 8), true)
            .expect("Expected MODE hover at top-level");
        assert!(doc_global.contains("MODE=\"global\""));
        assert!(doc_global.contains("Global mode"));

        assert!(
            get_local_symbol_hover_doc_at(text, "ONLY_A", Position::new(7, 18), true).is_some()
        );
        assert!(
            get_local_symbol_hover_doc_at(text, "ONLY_A", Position::new(12, 10), true).is_none()
        );
        assert!(
            get_local_symbol_hover_doc_at(text, "ONLY_A", Position::new(14, 8), true).is_none()
        );
    }
}

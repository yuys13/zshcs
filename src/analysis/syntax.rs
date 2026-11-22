use tower_lsp::lsp_types::Range;

/// Token parsed from a declaration line (`export`, `local`, `typeset`, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationToken<'a> {
    pub text: &'a str,
    pub byte_offset: usize,
}

/// Span of a statement within a single line (separated by `;`, `&&`, `||`, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementSpan<'a> {
    pub text: &'a str,
    pub start_byte: usize,
}

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
pub fn is_reserved_keyword(word: &str) -> bool {
    RESERVED_KEYWORDS.contains(&word)
}

/// Computes the UTF-16 code unit offset of a byte offset within a line.
pub fn byte_to_utf16_col(line: &str, byte_offset: usize) -> u32 {
    let mut u16_offset = 0;
    for (idx, ch) in line.char_indices() {
        if idx >= byte_offset {
            break;
        }
        u16_offset += ch.len_utf16() as u32;
    }
    u16_offset
}

/// Computes the byte offset corresponding to a UTF-16 code unit offset within a line.
pub fn utf16_col_to_byte(line: &str, target_u16: u32) -> usize {
    let mut current_u16 = 0;
    for (idx, ch) in line.char_indices() {
        if current_u16 >= target_u16 as usize {
            return idx;
        }
        current_u16 += ch.len_utf16();
    }
    line.len()
}

/// Checks if a parent range contains a child range (inclusive on boundaries).
pub fn range_contains(parent: &Range, child: &Range) -> bool {
    let start_ok = (parent.start.line < child.start.line)
        || (parent.start.line == child.start.line
            && parent.start.character <= child.start.character);
    let end_ok = (parent.end.line > child.end.line)
        || (parent.end.line == child.end.line && parent.end.character >= child.end.character);
    start_ok && end_ok
}

/// Determines whether a character is a valid identifier character in a shell variable name.
#[inline]
pub fn is_var_ident_char(c: char) -> bool {
    c.is_alphanumeric()
        || c == '_'
        || (!c.is_ascii()
            && !c.is_whitespace()
            && !matches!(
                c,
                '{' | '}'
                    | '['
                    | ']'
                    | '('
                    | ')'
                    | '"'
                    | '\''
                    | '`'
                    | '\\'
                    | '$'
                    | '#'
                    | ';'
                    | ','
                    | '='
                    | ':'
                    | '/'
                    | '.'
                    | '-'
                    | '+'
                    | '*'
                    | '?'
                    | '!'
                    | '~'
                    | '^'
                    | '|'
                    | '&'
                    | '<'
                    | '>'
                    | '\0'
            ))
}

/// Determines whether a character is part of a shell function name or bare identifier word.
#[inline]
pub fn is_func_ident_char(c: char) -> bool {
    if c.is_whitespace() {
        return false;
    }
    !matches!(
        c,
        ';' | '|'
            | '&'
            | '('
            | ')'
            | '<'
            | '>'
            | '"'
            | '\''
            | '`'
            | '\\'
            | '{'
            | '}'
            | '['
            | ']'
            | ','
            | '#'
            | '='
            | '!'
            | '~'
            | '^'
            | '*'
            | '?'
            | '$'
            | '/'
            | '\0'
    )
}

/// Backward compatibility alias for identifier characters.
#[inline]
pub fn is_ident_char(c: char) -> bool {
    is_func_ident_char(c)
}

/// Determines whether a character is part of a word/token for hover inspection.
///
/// Shell delimiters, operators, quotes, and whitespace are excluded.
#[inline]
pub fn is_word_char(c: char) -> bool {
    if c.is_whitespace() {
        return false;
    }
    !matches!(
        c,
        ';' | '|'
            | '&'
            | '('
            | ')'
            | '<'
            | '>'
            | '"'
            | '\''
            | '`'
            | '\\'
            | '{'
            | '}'
            | '['
            | ']'
            | '$'
            | ','
            | '#'
            | '='
            | '!'
            | '~'
            | '^'
            | '*'
            | '?'
            | '\0'
    )
}

/// Skips flags (e.g. `-T`, `-u`, `+x`) in a string and returns the remaining slice.
pub fn skip_flags(input: &str) -> &str {
    let mut rest = input.trim_start();
    while rest.starts_with('-') || rest.starts_with('+') {
        let flag_len = rest
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(rest.len());
        rest = rest[flag_len..].trim_start();
    }
    rest
}

/// Checks whether a line ends with an unescaped line continuation backslash.
pub fn ends_with_line_continuation(line: &str) -> bool {
    let trimmed = line.trim_end();
    let backslash_count = trimmed.chars().rev().take_while(|c| *c == '\\').count();
    backslash_count % 2 == 1
}

/// Splits words with their start byte offsets in a string.
pub fn split_words_with_offsets(input: &str) -> Vec<(usize, &str)> {
    let mut words = Vec::new();
    let mut in_word = false;
    let mut start = 0;

    for (i, c) in input.char_indices() {
        if c.is_whitespace() {
            if in_word {
                words.push((start, &input[start..i]));
                in_word = false;
            }
        } else if !in_word {
            start = i;
            in_word = true;
        }
    }
    if in_word {
        words.push((start, &input[start..]));
    }
    words
}

/// Splits a line into distinct statements separated by `;`, `&&`, `||`, `&`, `|`
/// as well as standalone command block braces `{` and `}`, while respecting single quotes,
/// double quotes, backticks, parentheses, and parameter expansions (`${...}`).
pub fn split_line_statements(line: &str) -> Vec<StatementSpan<'_>> {
    let mut statements = Vec::new();
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut stmt_start = 0;

    let mut in_quote: Option<u8> = None;
    let mut paren_depth = 0;
    let mut param_brace_depth = 0;

    while i < len {
        let b = bytes[i];

        if b == b'\\' && in_quote != Some(b'\'') && i + 1 < len {
            i += 2;
            continue;
        }

        if let Some(q) = in_quote {
            if b == q {
                in_quote = None;
            }
        } else if b == b'"' || b == b'\'' || b == b'`' {
            in_quote = Some(b);
        } else if b == b'(' {
            paren_depth += 1;
        } else if b == b')' {
            if paren_depth > 0 {
                paren_depth -= 1;
            }
        } else if b == b'{' && i > 0 && bytes[i - 1] == b'$' {
            param_brace_depth += 1;
        } else if b == b'}' && param_brace_depth > 0 {
            param_brace_depth -= 1;
        } else if paren_depth == 0 && param_brace_depth == 0 {
            if b == b'#' {
                // Comment starts; capture current statement and stop for the line
                let candidate = &line[stmt_start..i];
                if !candidate.trim().is_empty() {
                    statements.push(StatementSpan {
                        text: candidate,
                        start_byte: stmt_start,
                    });
                }
                stmt_start = len;
                break;
            } else if b == b';' || b == b'&' || b == b'|' {
                let candidate = &line[stmt_start..i];
                if !candidate.trim().is_empty() {
                    statements.push(StatementSpan {
                        text: candidate,
                        start_byte: stmt_start,
                    });
                }
                if (b == b'&' && i + 1 < len && bytes[i + 1] == b'&')
                    || (b == b'|' && i + 1 < len && bytes[i + 1] == b'|')
                {
                    i += 1;
                }
                stmt_start = i + 1;
            } else if b == b'{' || b == b'}' {
                let candidate = &line[stmt_start..i];
                if !candidate.trim().is_empty() {
                    statements.push(StatementSpan {
                        text: candidate,
                        start_byte: stmt_start,
                    });
                }
                stmt_start = i + 1;
            }
        }
        i += 1;
    }

    if stmt_start < len {
        let candidate = &line[stmt_start..len];
        if !candidate.trim().is_empty() {
            statements.push(StatementSpan {
                text: candidate,
                start_byte: stmt_start,
            });
        }
    }

    statements
}

/// Splits tokens in a declaration statement (`local`, `export`, `typeset`, etc.)
/// respecting quotes and parentheses.
pub fn split_declaration_tokens(input: &str) -> Vec<DeclarationToken<'_>> {
    let mut tokens = Vec::new();
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len || bytes[i] == b'#' || bytes[i] == b';' {
            break;
        }

        let start = i;
        let mut in_quote: Option<u8> = None;
        let mut paren_depth = 0;

        while i < len {
            let b = bytes[i];

            if b == b'\\' && in_quote != Some(b'\'') && i + 1 < len {
                i += 2;
                continue;
            }

            if let Some(q) = in_quote {
                if b == q {
                    in_quote = None;
                }
            } else if b == b'"' || b == b'\'' || b == b'`' {
                in_quote = Some(b);
            } else if b == b'(' {
                paren_depth += 1;
            } else if b == b')' {
                if paren_depth > 0 {
                    paren_depth -= 1;
                }
            } else if paren_depth == 0 && (b.is_ascii_whitespace() || b == b';' || b == b'#') {
                break;
            }
            i += 1;
        }

        if start < i {
            tokens.push(DeclarationToken {
                text: &input[start..i],
                byte_offset: start,
            });
        }
    }

    tokens
}

/// Extracts a `read` command from a statement (supporting `while read ...`, `IFS= read ...`, etc.).
pub fn find_read_command(stmt_text: &str) -> Option<(usize, &str)> {
    if stmt_text.starts_with("read ") || stmt_text.starts_with("read\t") {
        return Some((0, &stmt_text[5..]));
    }

    let markers = [" read ", "\tread ", " read\t", "\tread\t"];
    for marker in markers {
        if let Some(pos) = stmt_text.find(marker) {
            let read_start = pos + 1;
            return Some((read_start, &stmt_text[read_start + 5..]));
        }
    }

    None
}

/// Extracts the heredoc delimiter from a line if one is introduced, e.g. `cat <<EOF` -> `"EOF"`.
/// Skips comments and string literals so that `<<` inside `# ...` or `"..."` is ignored.
pub fn detect_heredoc_delimiter(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_quote: Option<u8> = None;

    let mut param_brace_depth: usize = 0;

    while i < len {
        let b = bytes[i];

        if b == b'\\' && in_quote != Some(b'\'') && i + 1 < len {
            i += 2;
            continue;
        }

        if let Some(q) = in_quote {
            if b == q {
                in_quote = None;
            }
            i += 1;
            continue;
        }

        if b == b'"' || b == b'\'' || b == b'`' {
            in_quote = Some(b);
            i += 1;
            continue;
        }

        if b == b'{' && i > 0 && bytes[i - 1] == b'$' {
            param_brace_depth += 1;
        } else if b == b'}' && param_brace_depth > 0 {
            param_brace_depth -= 1;
        } else if param_brace_depth == 0 && b == b'#' {
            let is_comment_start = i == 0
                || bytes[i - 1].is_ascii_whitespace()
                || bytes[i - 1] == b';'
                || bytes[i - 1] == b'&'
                || bytes[i - 1] == b'|'
                || bytes[i - 1] == b'('
                || bytes[i - 1] == b'`';
            if is_comment_start {
                break;
            }
        }

        if b == b'<' && i + 1 < len && bytes[i + 1] == b'<' {
            // Exclude here-strings (<<<) or sequences preceded by <
            if (i > 0 && bytes[i - 1] == b'<') || (i + 2 < len && bytes[i + 2] == b'<') {
                i += 2;
                continue;
            }
            let mut rest = &line[i + 2..];
            if let Some(stripped) = rest.strip_prefix('-') {
                rest = stripped;
            }
            let trimmed = rest.trim_start();
            let delim_token = if let Some(stripped) = trimmed.strip_prefix('\'') {
                stripped.split('\'').next()
            } else if let Some(stripped) = trimmed.strip_prefix('"') {
                stripped.split('"').next()
            } else if let Some(stripped) = trimmed.strip_prefix('\\') {
                stripped.split_whitespace().next()
            } else {
                trimmed.split_whitespace().next()
            };
            if let Some(delim) = delim_token {
                let clean_delim = delim.trim_matches(|c: char| {
                    c == ';' || c == '|' || c == '&' || c == ')' || c == '(' || c == '>'
                });
                if !clean_delim.is_empty()
                    && clean_delim
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                {
                    return Some(clean_delim.to_string());
                }
            }
            i += 2;
            continue;
        }
        i += 1;
    }
    None
}

/// Finds the closing `}` matching the opening `{` starting at `(open_line, open_byte_col)`.
pub fn find_matching_closing_brace(
    lines: &[&str],
    open_line: usize,
    open_byte_col: usize,
) -> (usize, u32) {
    let mut depth: usize = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut in_backtick = false;
    let mut heredoc_delimiter: Option<String> = None;

    for (line_idx, line) in lines.iter().enumerate().skip(open_line) {
        if let Some(ref delim) = heredoc_delimiter {
            if line.trim_start_matches('\t').trim_end() == delim {
                heredoc_delimiter = None;
            }
            continue;
        }

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

        if depth > 0
            && !in_single_quote
            && !in_double_quote
            && !in_backtick
            && let Some(delim) = detect_heredoc_delimiter(&line[start_col..])
        {
            heredoc_delimiter = Some(delim);
        }
    }

    // If unclosed, return end of last line
    let last_line_idx = lines.len().saturating_sub(1);
    let last_line = lines.get(last_line_idx).copied().unwrap_or("");
    let end_u16 = byte_to_utf16_col(last_line, last_line.len());
    (last_line_idx, end_u16)
}

/// Searches forward for the first opening `{` starting from `start_line` and `start_byte_col`.
pub fn find_opening_brace(
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

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("hello world", 0, 0)]
    #[case("hello world", 5, 5)]
    #[case("こんにちは", 0, 0)]
    #[case("こんにちは", 3, 1)]
    #[case("こんにちは", 6, 2)]
    #[case("こんにちは", 15, 5)]
    #[case("🍣🍺 emoji", 0, 0)]
    #[case("🍣🍺 emoji", 4, 2)]
    #[case("🍣🍺 emoji", 8, 4)]
    #[case("🍣🍺 emoji", 9, 5)]
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
    fn test_detect_heredoc_delimiter_with_length_expansion_and_hash_words() {
        let line1 = "cat <<EOF # comment";
        assert_eq!(detect_heredoc_delimiter(line1), Some("EOF".to_string()));

        let line2 = "echo ${#foo} <<'END_DOC'";
        assert_eq!(detect_heredoc_delimiter(line2), Some("END_DOC".to_string()));

        let line3 = "cat <<< 'here-string'";
        assert_eq!(detect_heredoc_delimiter(line3), None);
    }
}

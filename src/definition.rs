use std::path::{Path, PathBuf};
use tower_lsp::lsp_types::{GotoDefinitionResponse, Location, Position, Range, Url};

/// Target identified under the cursor for definition search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefinitionTarget {
    /// Explicit variable reference (e.g. `$VAR`, `${VAR}`).
    Variable(String),
    /// Identifier that could be either a function or a variable (e.g. `my_func`, `my_var`).
    FunctionOrVariable(String),
}

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

/// Splits a line into distinct statements separated by `;`, `&&`, `||`, `&`, `|`
/// while respecting single quotes, double quotes, backticks, and parentheses.
pub fn split_line_statements(line: &str) -> Vec<StatementSpan<'_>> {
    let mut statements = Vec::new();
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut stmt_start = 0;

    let mut in_quote: Option<u8> = None;
    let mut paren_depth = 0;
    let mut brace_depth = 0;

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
        } else if b == b'{' {
            brace_depth += 1;
        } else if b == b'}' {
            if brace_depth > 0 {
                brace_depth -= 1;
            }
        } else if paren_depth == 0 && brace_depth == 0 {
            if b == b'#' {
                // Comment starts; capture current statement and stop for the line
                if stmt_start < i {
                    statements.push(StatementSpan {
                        text: &line[stmt_start..i],
                        start_byte: stmt_start,
                    });
                }
                stmt_start = len;
                break;
            } else if b == b';' || b == b'&' || b == b'|' {
                if stmt_start < i {
                    statements.push(StatementSpan {
                        text: &line[stmt_start..i],
                        start_byte: stmt_start,
                    });
                }
                if (b == b'&' && i + 1 < len && bytes[i + 1] == b'&')
                    || (b == b'|' && i + 1 < len && bytes[i + 1] == b'|')
                {
                    i += 1;
                }
                stmt_start = i + 1;
            }
        }
        i += 1;
    }

    if stmt_start < len {
        statements.push(StatementSpan {
            text: &line[stmt_start..len],
            start_byte: stmt_start,
        });
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

/// Extracts a source / `.` script path if the cursor is positioned on a `source` or `.` statement.
pub fn extract_source_path_at_position(text: &str, position: Position) -> Option<String> {
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
    let trimmed = line_str.trim_start();
    let indent_bytes = line_str.len() - trimmed.len();

    if trimmed.starts_with('#') {
        return None;
    }

    let cmd_len = if trimmed.starts_with("source ") || trimmed.starts_with("source\t") {
        6
    } else if trimmed.starts_with(". ") || trimmed.starts_with(".\t") {
        1
    } else {
        return None;
    };

    let after_cmd = &trimmed[cmd_len..];
    let arg_trim_len = after_cmd.len() - after_cmd.trim_start().len();
    let arg_start_byte = indent_bytes + cmd_len + arg_trim_len;
    if arg_start_byte >= line_str.len() {
        return None;
    }

    let arg_content = &line_str[arg_start_byte..];
    let (unquoted, arg_end_byte) = if let Some(stripped) = arg_content.strip_prefix('"') {
        if let Some(close_idx) = stripped.find('"') {
            (&stripped[..close_idx], arg_start_byte + 1 + close_idx + 1)
        } else {
            (stripped, line_str.len())
        }
    } else if let Some(stripped) = arg_content.strip_prefix('\'') {
        if let Some(close_idx) = stripped.find('\'') {
            (&stripped[..close_idx], arg_start_byte + 1 + close_idx + 1)
        } else {
            (stripped, line_str.len())
        }
    } else {
        let token_len = arg_content
            .find(|c: char| c.is_whitespace() || c == '#' || c == ';')
            .unwrap_or(arg_content.len());
        (&arg_content[..token_len], arg_start_byte + token_len)
    };

    let cmd_start_u16 = byte_to_utf16_col(line_str, indent_bytes);
    let arg_end_u16 = byte_to_utf16_col(line_str, arg_end_byte);

    if position.character >= cmd_start_u16 && position.character <= arg_end_u16 {
        Some(unquoted.to_string())
    } else {
        None
    }
}

/// Resolves a source path relative to the current document's directory or as an absolute / tilde path.
pub fn resolve_source_path(path_str: &str, base_uri: &Url) -> Option<Url> {
    if path_str.is_empty() {
        return None;
    }

    let mut expanded_str = path_str.to_string();
    if let Ok(home) = std::env::var("HOME") {
        expanded_str = expanded_str
            .replace("$HOME", &home)
            .replace("${HOME}", &home);
    }

    // Tilde expansion (~ or ~/...)
    if (expanded_str == "~" || expanded_str.starts_with("~/"))
        && let Ok(home) = std::env::var("HOME")
    {
        let sub = if expanded_str == "~" {
            ""
        } else {
            &expanded_str[2..]
        };
        let candidate = PathBuf::from(home).join(sub);
        if candidate.exists() {
            if let Ok(canon) = candidate.canonicalize() {
                return Url::from_file_path(canon).ok();
            }
            return Url::from_file_path(candidate).ok();
        }
    }

    let path_obj = Path::new(&expanded_str);
    if path_obj.is_absolute() {
        if path_obj.exists() {
            if let Ok(canon) = path_obj.canonicalize() {
                return Url::from_file_path(canon).ok();
            }
            return Url::from_file_path(path_obj).ok();
        }
        return None;
    }

    // Relative path resolution
    if let Ok(base_path) = base_uri.to_file_path()
        && let Some(parent) = base_path.parent()
    {
        let candidate = parent.join(&expanded_str);
        if candidate.exists() {
            if let Ok(canon) = candidate.canonicalize() {
                return Url::from_file_path(canon).ok();
            }
            return Url::from_file_path(candidate).ok();
        }
    }

    None
}

/// Extracts the definition target under the cursor along with its UTF-16 range.
pub fn extract_word_and_target_at_position(
    text: &str,
    position: Position,
) -> Option<(DefinitionTarget, Range)> {
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

    // 1. Check for parameter expansion `${VAR}` spanning cursor
    for (start_idx, _) in line_str.match_indices("${") {
        if let Some(close_rel) = line_str[start_idx + 2..].find('}') {
            let close_idx = start_idx + 2 + close_rel;
            let start_u16 = byte_to_utf16_col(line_str, start_idx);
            let end_u16 = byte_to_utf16_col(line_str, close_idx + 1);

            if target_u16 >= start_u16 && target_u16 <= end_u16 {
                let mut inside = &line_str[start_idx + 2..close_idx];
                // Strip Zsh parameter expansion flags e.g. `(U)`, `(q)`, `(f)`
                if inside.starts_with('(')
                    && let Some(close_paren) = inside.find(')')
                {
                    inside = &inside[close_paren + 1..];
                }
                // Strip leading length `#` or indirect `!` if followed by identifier
                if (inside.starts_with('#') || inside.starts_with('!'))
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
                    let range = Range {
                        start: Position::new(position.line, start_u16),
                        end: Position::new(position.line, end_u16),
                    };
                    return Some((DefinitionTarget::Variable(var_name), range));
                }
            }
        }
    }

    // 2. Check for `$VAR` variable reference spanning cursor
    for (dollar_idx, _) in line_str.match_indices('$') {
        let after_dollar = &line_str[dollar_idx + 1..];
        let var_name: String = after_dollar
            .chars()
            .take_while(|c| is_var_ident_char(*c))
            .collect();
        if !var_name.is_empty() {
            let start_u16 = byte_to_utf16_col(line_str, dollar_idx);
            let end_u16 = byte_to_utf16_col(line_str, dollar_idx + 1 + var_name.len());
            if target_u16 >= start_u16 && target_u16 <= end_u16 {
                let range = Range {
                    start: Position::new(position.line, start_u16),
                    end: Position::new(position.line, end_u16),
                };
                return Some((DefinitionTarget::Variable(var_name), range));
            }
        }
    }

    // 3. Extract word token spanning cursor (functions, bare variables, assignments)
    struct TokenSpan {
        start_byte: usize,
        end_byte: usize,
        start_u16: u32,
        end_u16: u32,
    }

    let mut tokens: Vec<TokenSpan> = Vec::new();
    let mut in_token = false;
    let mut token_start_byte = 0;
    let mut token_start_u16 = 0;
    let mut current_u16 = 0;

    for (byte_idx, c) in line_str.char_indices() {
        let char_u16_len = c.len_utf16() as u32;
        let is_token = is_func_ident_char(c);

        if is_token {
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

    if target_u16 >= current_u16 && !tokens.is_empty() {
        // Cursor beyond end of line
        return None;
    }

    for token in tokens {
        if target_u16 >= token.start_u16 && target_u16 <= token.end_u16 {
            let raw_word = &line_str[token.start_byte..token.end_byte];
            let range = Range {
                start: Position::new(position.line, token.start_u16),
                end: Position::new(position.line, token.end_u16),
            };

            return Some((
                DefinitionTarget::FunctionOrVariable(raw_word.to_string()),
                range,
            ));
        }
    }

    None
}

/// Scans the document for shell function definitions matching `func_name`.
pub fn scan_function_definitions(text: &str, uri: &Url, func_name: &str) -> Vec<Location> {
    let mut locations = Vec::new();

    for (line_idx, line) in text.lines().enumerate() {
        for stmt in split_line_statements(line) {
            let trimmed = stmt.text.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let stmt_indent = stmt.text.len() - trimmed.len();
            let stmt_offset = stmt.start_byte + stmt_indent;

            // Pattern 1: `func_name() ...` or `func_name () ...` or `func_name ( ) ...`
            if let Some(rest) = trimmed.strip_prefix(func_name) {
                let rest_t = rest.trim_start();
                if let Some(after_open_raw) = rest_t.strip_prefix('(') {
                    let after_open = after_open_raw.trim_start();
                    if let Some(after_close) = after_open.strip_prefix(')') {
                        let after_parens = after_close.trim_start();
                        if after_parens.is_empty()
                            || after_parens.starts_with('{')
                            || after_parens.starts_with('#')
                            || after_parens.starts_with(';')
                        {
                            let start_byte = stmt_offset;
                            let end_byte = start_byte + func_name.len();
                            let start_u16 = byte_to_utf16_col(line, start_byte);
                            let end_u16 = byte_to_utf16_col(line, end_byte);
                            locations.push(Location {
                                uri: uri.clone(),
                                range: Range::new(
                                    Position::new(line_idx as u32, start_u16),
                                    Position::new(line_idx as u32, end_u16),
                                ),
                            });
                            continue;
                        }
                    }
                }
            }

            // Pattern 2: `function func_name ...`
            if trimmed.starts_with("function ") || trimmed.starts_with("function\t") {
                let after_fn = trimmed[8..].trim_start();
                if let Some(rest) = after_fn.strip_prefix(func_name) {
                    let rest_t = rest.trim_start();
                    let is_header_end = rest_t.is_empty()
                        || rest_t.starts_with('{')
                        || rest_t.starts_with('#')
                        || rest_t.starts_with(';')
                        || (rest_t.starts_with('(') && rest_t.contains(')'));

                    if is_header_end {
                        let fn_kw_offset = stmt_offset + 8;
                        if let Some(offset) = line[fn_kw_offset..].find(func_name) {
                            let start_byte = fn_kw_offset + offset;
                            let end_byte = start_byte + func_name.len();
                            let start_u16 = byte_to_utf16_col(line, start_byte);
                            let end_u16 = byte_to_utf16_col(line, end_byte);
                            locations.push(Location {
                                uri: uri.clone(),
                                range: Range::new(
                                    Position::new(line_idx as u32, start_u16),
                                    Position::new(line_idx as u32, end_u16),
                                ),
                            });
                        }
                    }
                }
            }
        }
    }

    locations
}

/// Scans the document for shell variable definitions and assignments matching `var_name`.
pub fn scan_variable_definitions(text: &str, uri: &Url, var_name: &str) -> Vec<Location> {
    let mut locations = Vec::new();
    let decl_keywords = [
        "export", "typeset", "local", "declare", "readonly", "integer", "float",
    ];

    for (line_idx, line) in text.lines().enumerate() {
        for stmt in split_line_statements(line) {
            let trimmed = stmt.text.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let stmt_indent = stmt.text.len() - trimmed.len();
            let stmt_offset = stmt.start_byte + stmt_indent;

            // Direct assignment: `VAR=...`, `VAR+=...`, `VAR[1]=...`
            if let Some(rest) = trimmed.strip_prefix(var_name) {
                let is_direct_assignment = rest.starts_with('=')
                    || rest.starts_with("+=")
                    || (rest.starts_with('[')
                        && rest.find(']').is_some_and(|close| {
                            let after_bracket = &rest[close + 1..];
                            after_bracket.starts_with('=') || after_bracket.starts_with("+=")
                        }));

                if is_direct_assignment {
                    let start_byte = stmt_offset;
                    let end_byte = start_byte + var_name.len();
                    let start_u16 = byte_to_utf16_col(line, start_byte);
                    let end_u16 = byte_to_utf16_col(line, end_byte);
                    locations.push(Location {
                        uri: uri.clone(),
                        range: Range::new(
                            Position::new(line_idx as u32, start_u16),
                            Position::new(line_idx as u32, end_u16),
                        ),
                    });
                    continue;
                }
            }

            // Declaration statements: `export`, `typeset`, `local`, `declare`, `readonly`, etc.
            for kw in decl_keywords {
                if trimmed.starts_with(kw)
                    && (trimmed[kw.len()..].starts_with(' ')
                        || trimmed[kw.len()..].starts_with('\t'))
                {
                    let kw_byte_offset = stmt_offset + kw.len();
                    let rest_line = &trimmed[kw.len()..];

                    for token in split_declaration_tokens(rest_line) {
                        let token_str = token.text;
                        let token_start_in_line = kw_byte_offset + token.byte_offset;

                        if token_str.starts_with('-') || token_str.starts_with('+') {
                            continue;
                        }

                        if let Some(rest) = token_str.strip_prefix(var_name) {
                            let is_match = rest.is_empty()
                                || rest.starts_with('=')
                                || rest.starts_with("+=")
                                || (rest.starts_with('[')
                                    && (rest.find(']').is_some_and(|close| {
                                        let after_bracket = &rest[close + 1..];
                                        after_bracket.is_empty()
                                            || after_bracket.starts_with('=')
                                            || after_bracket.starts_with("+=")
                                    }) || rest.ends_with(']')));

                            if is_match {
                                let start_byte = token_start_in_line;
                                let end_byte = start_byte + var_name.len();
                                let start_u16 = byte_to_utf16_col(line, start_byte);
                                let end_u16 = byte_to_utf16_col(line, end_byte);
                                locations.push(Location {
                                    uri: uri.clone(),
                                    range: Range::new(
                                        Position::new(line_idx as u32, start_u16),
                                        Position::new(line_idx as u32, end_u16),
                                    ),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    locations
}

/// Resolves the definition location(s) for the token at `position` within `text`.
pub fn find_definition(
    text: &str,
    uri: &Url,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    // 1. Check if on `source` or `.` statement line
    if let Some(source_path) = extract_source_path_at_position(text, position)
        && let Some(target_url) = resolve_source_path(&source_path, uri)
    {
        return Some(GotoDefinitionResponse::Scalar(Location {
            uri: target_url,
            range: Range::new(Position::new(0, 0), Position::new(0, 0)),
        }));
    }

    // 2. Extract word and target under cursor
    let (target, _) = extract_word_and_target_at_position(text, position)?;

    match target {
        DefinitionTarget::Variable(var_name) => {
            let var_locs = scan_variable_definitions(text, uri, &var_name);
            if var_locs.is_empty() {
                None
            } else if var_locs.len() == 1 {
                Some(GotoDefinitionResponse::Scalar(
                    var_locs.into_iter().next().unwrap(),
                ))
            } else {
                Some(GotoDefinitionResponse::Array(var_locs))
            }
        }
        DefinitionTarget::FunctionOrVariable(ident) => {
            // First search for function definitions
            let fn_locs = scan_function_definitions(text, uri, &ident);
            if !fn_locs.is_empty() {
                if fn_locs.len() == 1 {
                    return Some(GotoDefinitionResponse::Scalar(
                        fn_locs.into_iter().next().unwrap(),
                    ));
                } else {
                    return Some(GotoDefinitionResponse::Array(fn_locs));
                }
            }

            // Fallback to variable definitions
            let var_locs = scan_variable_definitions(text, uri, &ident);
            if !var_locs.is_empty() {
                if var_locs.len() == 1 {
                    return Some(GotoDefinitionResponse::Scalar(
                        var_locs.into_iter().next().unwrap(),
                    ));
                } else {
                    return Some(GotoDefinitionResponse::Array(var_locs));
                }
            }

            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

        // 2. Out of bounds positions
        let sample = "foo() { : }\nfoo\n";
        assert!(find_definition(sample, &uri, Position::new(10, 0)).is_none());
        assert!(find_definition(sample, &uri, Position::new(1, 100)).is_none());

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
}

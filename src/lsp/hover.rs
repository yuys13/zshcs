use std::time::Duration;
use tower_lsp::lsp_types::{HoverContents, MarkupContent, MarkupKind, Position, Range, Url};

pub use crate::analysis::builtins::get_builtin_or_reserved_doc;
use crate::analysis::model::DocumentIndex;
use crate::analysis::parser::parse_document;
use crate::analysis::scope::{resolve_alias_defs, resolve_function_defs, resolve_variable_defs};
pub use crate::analysis::syntax::is_word_char;
use crate::analysis::syntax::utf16_col_to_byte;
use crate::analysis::workspace::find_in_sourced_documents;
use crate::document::DocumentManager;
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

fn format_symbol_markdown(
    name: &str,
    kind_label: &str,
    signature: &str,
    doc_comment: Option<&str>,
) -> String {
    let mut md = format!("### `{name}` ({kind_label})\n\n```zsh\n{signature}\n```");
    if let Some(doc) = doc_comment {
        md.push_str("\n\n");
        md.push_str(doc);
    }
    md
}

fn resolve_local_hover_in_index(
    index: &DocumentIndex,
    word: &str,
    cursor_pos: Option<Position>,
    is_variable_ref: bool,
) -> Option<String> {
    if !is_variable_ref {
        if let Some(func) = resolve_function_defs(index, word).into_iter().next() {
            return Some(format_symbol_markdown(
                &func.name,
                "Local Function",
                &func.signature,
                func.doc_comment.as_deref(),
            ));
        }
        if let Some(alias) = resolve_alias_defs(index, word).into_iter().next() {
            return Some(format_symbol_markdown(
                &alias.name,
                "Local Alias",
                &alias.signature,
                alias.doc_comment.as_deref(),
            ));
        }
    }

    let var_defs = resolve_variable_defs(index, word, cursor_pos);
    let chosen = var_defs
        .iter()
        .copied()
        .find(|v| v.is_function_local)
        .or_else(|| var_defs.into_iter().next())?;

    Some(format_symbol_markdown(
        &chosen.name,
        "Local Variable",
        &chosen.signature,
        chosen.doc_comment.as_deref(),
    ))
}

/// Resolves Markdown hover documentation for script-local functions, aliases, and variables.
pub fn get_local_symbol_hover_doc(text: &str, word: &str, is_variable_ref: bool) -> Option<String> {
    let index = parse_document(text);
    resolve_local_hover_in_index(&index, word, None, is_variable_ref)
}

/// Resolves Markdown hover documentation for script-local functions, aliases, and variables
/// taking into account function-local variable scoping at `position`.
pub fn get_local_symbol_hover_doc_at(
    text: &str,
    word: &str,
    position: Position,
    is_variable_ref: bool,
) -> Option<String> {
    let index = parse_document(text);
    resolve_local_hover_in_index(&index, word, Some(position), is_variable_ref)
}

/// Resolves Markdown hover documentation for script-local symbols across the active document
/// and any scripts sourced via `source` or `.` (preferring open buffers in `doc_manager`
/// and falling back to bounded disk reads).
pub fn get_local_symbol_hover_doc_at_with_manager(
    text: &str,
    uri: &Url,
    word: &str,
    position: Position,
    is_variable_ref: bool,
    doc_manager: Option<&DocumentManager>,
) -> Option<String> {
    let index = parse_document(text);
    get_local_symbol_hover_doc_with_index(&index, uri, word, position, is_variable_ref, doc_manager)
}

/// Resolves Markdown hover documentation for script-local symbols using a pre-parsed `DocumentIndex`.
pub fn get_local_symbol_hover_doc_with_index(
    index: &DocumentIndex,
    uri: &Url,
    word: &str,
    position: Position,
    is_variable_ref: bool,
    doc_manager: Option<&DocumentManager>,
) -> Option<String> {
    if let Some(doc) = resolve_local_hover_in_index(index, word, Some(position), is_variable_ref) {
        return Some(doc);
    }

    find_in_sourced_documents(index, uri, position, doc_manager, true, |sourced| {
        resolve_local_hover_in_index(&sourced.index, word, None, is_variable_ref)
    })
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

    #[tokio::test]
    async fn test_get_hover_info_cached_populates_and_reuses_cache() {
        let cache = ManCache::new();
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
        let cache = ManCache::new();
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

    #[test]
    fn test_local_symbol_hover_cross_file_sourced_scripts() {
        let dir = tempfile::tempdir().unwrap();
        let main_path = dir.path().join("main.zsh");
        let lib_path = dir.path().join("lib.zsh");
        let main_uri = Url::from_file_path(&main_path).unwrap();

        std::fs::write(
            &lib_path,
            r#"# Shared helper
helper_fn() {
    local HIDDEN_IN_LIB="secret"
    echo "$HIDDEN_IN_LIB"
}
# Shared var
export SHARED_VAR="ok"
alias ll='ls -la'
"#,
        )
        .unwrap();

        let main_text = "source ./lib.zsh\nhelper_fn\necho $SHARED_VAR\nll\necho $HIDDEN_IN_LIB\n";

        let fn_doc = get_local_symbol_hover_doc_at_with_manager(
            main_text,
            &main_uri,
            "helper_fn",
            Position::new(1, 2),
            false,
            None,
        )
        .expect("Expected hover for sourced function");
        assert!(fn_doc.contains("Shared helper"));
        assert!(fn_doc.contains("Local Function"));

        let var_doc = get_local_symbol_hover_doc_at_with_manager(
            main_text,
            &main_uri,
            "SHARED_VAR",
            Position::new(2, 8),
            true,
            None,
        )
        .expect("Expected hover for sourced variable");
        assert!(var_doc.contains("Shared var"));
        assert!(var_doc.contains("export SHARED_VAR=\"ok\""));

        let alias_doc = get_local_symbol_hover_doc_at_with_manager(
            main_text,
            &main_uri,
            "ll",
            Position::new(3, 1),
            false,
            None,
        )
        .expect("Expected hover for sourced alias");
        assert!(alias_doc.contains("alias ll='ls -la'"));

        // Function-local variable inside sourced script must remain hidden
        assert!(
            get_local_symbol_hover_doc_at_with_manager(
                main_text,
                &main_uri,
                "HIDDEN_IN_LIB",
                Position::new(4, 8),
                true,
                None,
            )
            .is_none()
        );
    }

    #[test]
    fn test_local_symbol_hover_short_circuits_on_first_matching_sourced_file() {
        let dir = tempfile::tempdir().unwrap();
        let main_path = dir.path().join("main.zsh");
        let a_path = dir.path().join("a.zsh");
        let b_path = dir.path().join("b.zsh");
        let nested_path = dir.path().join("nested.zsh");

        std::fs::write(&nested_path, "nested_fn() { : }\n").unwrap();
        std::fs::write(
            &a_path,
            "source ./nested.zsh\n# Target doc\ntarget_fn() { : }\n",
        )
        .unwrap();
        std::fs::write(&b_path, "other_fn() { : }\n").unwrap();

        let main_uri = Url::from_file_path(&main_path).unwrap();
        let main_text = "source ./a.zsh\nsource ./b.zsh\ntarget_fn\n";

        crate::analysis::workspace::tests::reset_load_count();
        let doc = get_local_symbol_hover_doc_at_with_manager(
            main_text,
            &main_uri,
            "target_fn",
            Position::new(2, 2),
            false,
            None,
        )
        .expect("Expected hover doc for target_fn");
        assert!(doc.contains("Target doc"));
        assert_eq!(
            crate::analysis::workspace::tests::get_load_count(),
            1,
            "Should short-circuit after finding target_fn in a.zsh without loading nested.zsh or b.zsh"
        );
    }
}

use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, Documentation, MarkupContent, MarkupKind, Position,
};

use crate::analysis::builtins::get_builtin_or_reserved_doc;
use crate::analysis::parser::parse_document;
use crate::analysis::scope::{VisibleSymbolKind, visible_symbols_at};
use crate::analysis::syntax::{is_func_ident_char, is_var_ident_char};
use crate::analysis::workspace::collect_sourced_documents;
use crate::document::{DocumentManager, DocumentState};
pub use crate::runtime::daemon::{
    CAPTURE_ZSH, CompletionRequest, DAEMON_REQUEST_TIMEOUT, ZPTYRC_ZSH, run_completion_daemon,
};
pub use crate::runtime::man::{
    DEFAULT_RESOLVE_MAN_TIMEOUT, ManCache, is_eligible_for_external_command,
};
use crate::runtime::man::{ManPageResult, get_man_page_result, resolve_man_markdown_with_fetcher};

/// Infers an intelligent `CompletionItemKind` based on candidate prefix and description.
pub fn infer_completion_kind(label: &str, detail: Option<&str>) -> CompletionItemKind {
    if label.starts_with('-') {
        CompletionItemKind::KEYWORD
    } else if label.starts_with('$') {
        CompletionItemKind::VARIABLE
    } else if label == "." || label == ".." || label == "~" || label.ends_with('/') {
        CompletionItemKind::FOLDER
    } else if let Some(d) = detail {
        let d_lower = d.to_ascii_lowercase();
        if d_lower.contains("command")
            || d_lower.contains("builtin")
            || d_lower.contains("function")
            || d_lower.contains("alias")
            || d_lower.contains("executable")
        {
            CompletionItemKind::FUNCTION
        } else if d_lower.contains("option") || d_lower.contains("flag") {
            CompletionItemKind::KEYWORD
        } else if d_lower.contains("variable")
            || d_lower.contains("parameter")
            || d_lower.contains("env")
        {
            CompletionItemKind::VARIABLE
        } else if d_lower.contains("directory") || d_lower.contains("folder") {
            CompletionItemKind::FOLDER
        } else if d_lower.contains("file") || d_lower.contains("archive") || label.contains('.') {
            CompletionItemKind::FILE
        } else if label.contains('/') {
            CompletionItemKind::FOLDER
        } else {
            CompletionItemKind::TEXT
        }
    } else if label.contains('.') {
        CompletionItemKind::FILE
    } else if label.contains('/') {
        CompletionItemKind::FOLDER
    } else {
        CompletionItemKind::TEXT
    }
}

pub fn parse_candidate_line(line: &str, items: &mut Vec<CompletionItem>) {
    // ddc-source-shell_native style outputs `candidate\tdescription`
    let (label, detail) = match line.split_once('\t') {
        Some((lbl, dtl)) => {
            let detail_opt = if !dtl.trim().is_empty() {
                Some(dtl.to_string())
            } else {
                None
            };
            (lbl.to_string(), detail_opt)
        }
        None => (line.to_string(), None),
    };

    let kind = infer_completion_kind(&label, detail.as_deref());

    items.push(CompletionItem {
        label,
        kind: Some(kind),
        insert_text: None,
        detail,
        ..Default::default()
    });
}

/// Deduplicates completion candidates while preserving original candidate order.
///
/// If multiple candidates share the same label:
/// - The first candidate's position in the candidate sequence is preserved.
/// - If the first candidate lacks fields (such as `detail`, specific `kind`, `documentation`,
///   `insert_text`, `sort_text`, etc.) but a subsequent duplicate has them, the first candidate
///   is enriched with those fields.
/// - Redundant duplicate entries are removed in-place with zero string allocations during candidate lookup.
pub fn deduplicate_completion_items(items: &mut Vec<CompletionItem>) {
    if items.len() <= 1 {
        return;
    }

    let mut seen_indices: HashMap<&str, usize> = HashMap::with_capacity(items.len());
    let mut duplicate_pairs: Vec<(usize, usize)> = Vec::new();
    let mut is_duplicate: Vec<bool> = vec![false; items.len()];

    for (idx, item) in items.iter().enumerate() {
        if let Some(&first_idx) = seen_indices.get(item.label.as_str()) {
            is_duplicate[idx] = true;
            duplicate_pairs.push((first_idx, idx));
        } else {
            seen_indices.insert(item.label.as_str(), idx);
        }
    }

    if duplicate_pairs.is_empty() {
        return;
    }

    // Drop seen_indices to release immutable borrow of items
    drop(seen_indices);

    for (first_idx, dup_idx) in duplicate_pairs {
        if first_idx < dup_idx {
            let (left, right) = items.split_at_mut(dup_idx);
            let target = &mut left[first_idx];
            let source = &right[0];
            enrich_completion_item(target, source);
        }
    }

    let mut idx = 0;
    items.retain(|_| {
        let keep = !is_duplicate[idx];
        idx += 1;
        keep
    });
}

fn enrich_completion_item(target: &mut CompletionItem, source: &CompletionItem) {
    if target.detail.is_none() && source.detail.is_some() {
        target.detail = source.detail.clone();
    }

    match (target.kind, source.kind) {
        (None, Some(s_kind)) => {
            target.kind = Some(s_kind);
        }
        (Some(CompletionItemKind::TEXT), Some(s_kind)) if s_kind != CompletionItemKind::TEXT => {
            target.kind = Some(s_kind);
        }
        _ => {}
    }

    if target.documentation.is_none() && source.documentation.is_some() {
        target.documentation = source.documentation.clone();
    }

    if target.insert_text.is_none() && source.insert_text.is_some() {
        target.insert_text = source.insert_text.clone();
    }

    if target.insert_text_format.is_none() && source.insert_text_format.is_some() {
        target.insert_text_format = source.insert_text_format;
    }

    if target.sort_text.is_none() && source.sort_text.is_some() {
        target.sort_text = source.sort_text.clone();
    }

    if target.filter_text.is_none() && source.filter_text.is_some() {
        target.filter_text = source.filter_text.clone();
    }

    if target.text_edit.is_none() && source.text_edit.is_some() {
        target.text_edit = source.text_edit.clone();
    }

    if target.tags.is_none() && source.tags.is_some() {
        target.tags = source.tags.clone();
    }

    if target.deprecated.is_none() && source.deprecated.is_some() {
        target.deprecated = source.deprecated;
    }
}

/// Resolves detailed documentation for completion items on demand.
///
/// Builtin commands and reserved words are looked up in the static documentation dictionary.
/// If matched, Markdown documentation is attached to `item.documentation`.
/// Items that are non-command types (such as files, folders, or variables) or for which no
/// documentation exists are returned unmodified.
pub fn resolve_completion_item(mut item: CompletionItem) -> CompletionItem {
    let has_doc = match &item.documentation {
        Some(Documentation::String(s)) => !s.trim().is_empty(),
        Some(Documentation::MarkupContent(m)) => !m.value.trim().is_empty(),
        None => false,
    };
    if has_doc {
        return item;
    }
    // Clear empty or whitespace documentation placeholder
    item.documentation = None;

    let trimmed_label = item.label.trim();
    if trimmed_label.is_empty() {
        return item;
    }

    if let Some(kind) = item.kind {
        let is_eligible = matches!(
            kind,
            CompletionItemKind::FUNCTION | CompletionItemKind::KEYWORD | CompletionItemKind::TEXT
        ) || (kind == CompletionItemKind::FOLDER && trimmed_label == ".");

        if !is_eligible {
            return item;
        }
    }

    if let Some(doc) = get_builtin_or_reserved_doc(trimmed_label) {
        item.documentation = Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: doc.to_string(),
        }));
    }

    item
}

/// Resolves detailed documentation for a `CompletionItem` asynchronously using an underlying fetcher
/// with single-flight request coalescing, negative caching, and timeout protection.
pub async fn resolve_completion_item_async_with_fetcher<F, Fut>(
    mut item: CompletionItem,
    cache: &ManCache,
    timeout_dur: Duration,
    fetcher: F,
) -> CompletionItem
where
    F: FnOnce(String, Duration) -> Fut,
    Fut: std::future::Future<Output = ManPageResult>,
{
    // 1. Preserve existing documentation if present and non-empty
    let has_doc = match &item.documentation {
        Some(Documentation::String(s)) => !s.trim().is_empty(),
        Some(Documentation::MarkupContent(m)) => !m.value.trim().is_empty(),
        None => false,
    };
    if has_doc {
        return item;
    }
    // Clear empty or whitespace documentation placeholder so fallthrough checks work reliably
    item.documentation = None;

    // 2. Try resolving builtin or reserved word synchronously
    item = resolve_completion_item(item);
    if item.documentation.is_some() {
        return item;
    }

    let trimmed_label = item.label.trim().to_string();
    if trimmed_label.is_empty() {
        return item;
    }

    if !is_eligible_for_external_command(item.kind, &trimmed_label) {
        return item;
    }

    if let Some(markdown) =
        resolve_man_markdown_with_fetcher(&trimmed_label, cache, timeout_dur, fetcher).await
    {
        item.documentation = Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: markdown,
        }));
    }

    item
}

/// Resolves detailed documentation for a `CompletionItem` asynchronously with a custom timeout.
///
/// Documentation lookup precedence:
/// 1. Pre-existing non-empty documentation is preserved.
/// 2. Zsh builtins and reserved words are resolved synchronously from the static dictionary.
/// 3. Eligible external commands are resolved asynchronously via `man` with single-flight deduplication,
///    negative caching, and timeout protection.
pub async fn resolve_completion_item_async_with_timeout(
    item: CompletionItem,
    cache: &ManCache,
    timeout_dur: Duration,
) -> CompletionItem {
    resolve_completion_item_async_with_fetcher(
        item,
        cache,
        timeout_dur,
        |cmd: String, dur: Duration| async move { get_man_page_result(&cmd, dur).await },
    )
    .await
}

/// Resolves detailed documentation for a `CompletionItem` asynchronously with the default 2-second timeout.
pub async fn resolve_completion_item_async(
    item: CompletionItem,
    cache: &ManCache,
) -> CompletionItem {
    resolve_completion_item_async_with_timeout(item, cache, DEFAULT_RESOLVE_MAN_TIMEOUT).await
}

/// Kind of a locally discovered symbol within a script document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocalSymbolKind {
    /// Shell variable or parameter (e.g. `VAR=1`, `local x`, `for item in ...`).
    Variable,
    /// Shell function (e.g. `my_func() { ... }`, `function my_func { ... }`).
    Function,
    /// Shell alias (e.g. `alias ll='ls -la'`).
    Alias,
}

/// A locally defined symbol extracted from document text up to a given position.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LocalSymbol {
    pub name: String,
    pub kind: LocalSymbolKind,
    pub detail: Option<String>,
}

impl LocalSymbol {
    /// Creates a new `LocalSymbol` with standard detail string.
    pub fn new(name: impl Into<String>, kind: LocalSymbolKind) -> Self {
        let name = name.into();
        let detail = match kind {
            LocalSymbolKind::Variable => Some("(local variable)".to_string()),
            LocalSymbolKind::Function => Some("(local function)".to_string()),
            LocalSymbolKind::Alias => Some("(local alias)".to_string()),
        };
        Self { name, kind, detail }
    }

    /// Creates a new `LocalSymbol` with an explicit detail string.
    pub fn with_detail(
        name: impl Into<String>,
        kind: LocalSymbolKind,
        detail: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            kind,
            detail,
        }
    }
}

impl From<VisibleSymbolKind> for LocalSymbolKind {
    fn from(kind: VisibleSymbolKind) -> Self {
        match kind {
            VisibleSymbolKind::Variable => LocalSymbolKind::Variable,
            VisibleSymbolKind::Function => LocalSymbolKind::Function,
            VisibleSymbolKind::Alias => LocalSymbolKind::Alias,
        }
    }
}

/// Extracts locally defined variables, functions, and aliases from an open document state
/// (and any scripts sourced at or before `position`) up to the given `position`.
pub fn extract_local_symbols(doc: &DocumentState, position: Position) -> Vec<LocalSymbol> {
    extract_local_symbols_with_manager(doc, position, None)
}

#[cfg(test)]
static EXTRACT_THREAD_IDS: std::sync::OnceLock<
    dashmap::DashMap<tower_lsp::lsp_types::Url, std::thread::ThreadId>,
> = std::sync::OnceLock::new();

#[cfg(test)]
fn record_extract_thread(uri: &tower_lsp::lsp_types::Url) {
    let map = EXTRACT_THREAD_IDS.get_or_init(dashmap::DashMap::new);
    map.insert(uri.clone(), std::thread::current().id());
}

#[cfg(test)]
pub(crate) fn last_extract_thread_id(
    uri: &tower_lsp::lsp_types::Url,
) -> Option<std::thread::ThreadId> {
    EXTRACT_THREAD_IDS
        .get()
        .and_then(|map| map.get(uri).map(|entry| *entry.value()))
}

/// Extracts locally defined variables, functions, and aliases from an open document state
/// and any scripts sourced at or before `position`, preferring open buffers in `doc_manager`
/// and falling back to disk reads.
pub fn extract_local_symbols_with_manager(
    doc: &DocumentState,
    position: Position,
    doc_manager: Option<&DocumentManager>,
) -> Vec<LocalSymbol> {
    #[cfg(test)]
    record_extract_thread(doc.uri());

    let index = doc.index();
    let mut symbols: Vec<LocalSymbol> = visible_symbols_at(&index, position)
        .into_iter()
        .map(|s| LocalSymbol::new(s.name, s.kind.into()))
        .collect();
    let mut seen: HashSet<(String, LocalSymbolKind)> =
        symbols.iter().map(|s| (s.name.clone(), s.kind)).collect();

    let end_pos = Position::new(u32::MAX, u32::MAX);
    for sourced in collect_sourced_documents(&index, doc.uri(), position, doc_manager, false) {
        for s in visible_symbols_at(&sourced.index, end_pos) {
            let kind: LocalSymbolKind = s.kind.into();
            if seen.insert((s.name.clone(), kind)) {
                symbols.push(LocalSymbol::new(s.name, kind));
            }
        }
    }

    symbols
}

/// Extracts locally defined variables and functions from document text
/// that were defined at or before the given `position`.
pub fn extract_local_symbols_from_text(text: &str, position: Position) -> Vec<LocalSymbol> {
    let index = parse_document(text);
    visible_symbols_at(&index, position)
        .into_iter()
        .map(|s| LocalSymbol::new(s.name, s.kind.into()))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellContext {
    Normal,
    DoubleQuote,
    SingleQuote,
    Subshell,
    Arithmetic,
    Backtick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionContext {
    None,
    Variable,
    Arithmetic,
    CommandOrWord,
}

/// Analyzes `line_prefix` to determine the active shell completion context and token.
pub fn determine_completion_context(line_prefix: &str) -> (CompletionContext, &str) {
    let bytes = line_prefix.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut stack: Vec<ShellContext> = vec![ShellContext::Normal];
    let mut in_comment = false;
    let mut param_brace_depth: usize = 0;

    while i < len {
        let b = bytes[i];
        let current = *stack.last().unwrap_or(&ShellContext::Normal);

        if b == b'\\' && current != ShellContext::SingleQuote && i + 1 < len {
            i += 2;
            continue;
        }

        match current {
            ShellContext::SingleQuote => {
                if b == b'\'' {
                    stack.pop();
                }
            }
            ShellContext::DoubleQuote => {
                if b == b'"' {
                    stack.pop();
                } else if b == b'`' {
                    stack.push(ShellContext::Backtick);
                } else if b == b'$' && i + 2 < len && bytes[i + 1] == b'(' && bytes[i + 2] == b'(' {
                    stack.push(ShellContext::Arithmetic);
                    i += 3;
                    continue;
                } else if b == b'$' && i + 1 < len && bytes[i + 1] == b'(' {
                    stack.push(ShellContext::Subshell);
                    i += 2;
                    continue;
                } else if b == b'{' && i > 0 && bytes[i - 1] == b'$' {
                    param_brace_depth += 1;
                } else if b == b'}' && param_brace_depth > 0 {
                    param_brace_depth -= 1;
                }
            }
            ShellContext::Backtick => {
                if b == b'`' {
                    stack.pop();
                } else if b == b'\'' {
                    stack.push(ShellContext::SingleQuote);
                } else if b == b'"' {
                    stack.push(ShellContext::DoubleQuote);
                }
            }
            ShellContext::Arithmetic => {
                if b == b')' && i + 1 < len && bytes[i + 1] == b')' {
                    stack.pop();
                    i += 2;
                    continue;
                } else if b == b'$' && i + 1 < len && bytes[i + 1] == b'(' {
                    stack.push(ShellContext::Subshell);
                    i += 2;
                    continue;
                } else if b == b'`' {
                    stack.push(ShellContext::Backtick);
                } else if b == b'"' {
                    stack.push(ShellContext::DoubleQuote);
                } else if b == b'\'' {
                    stack.push(ShellContext::SingleQuote);
                }
            }
            ShellContext::Normal | ShellContext::Subshell => {
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
                        in_comment = true;
                        break;
                    }
                } else if b == b'\'' {
                    stack.push(ShellContext::SingleQuote);
                } else if b == b'"' {
                    stack.push(ShellContext::DoubleQuote);
                } else if b == b'`' {
                    stack.push(ShellContext::Backtick);
                } else if b == b'$' && i + 2 < len && bytes[i + 1] == b'(' && bytes[i + 2] == b'(' {
                    stack.push(ShellContext::Arithmetic);
                    i += 3;
                    continue;
                } else if b == b'(' && i + 1 < len && bytes[i + 1] == b'(' {
                    stack.push(ShellContext::Arithmetic);
                    i += 2;
                    continue;
                } else if b == b'$' && i + 1 < len && bytes[i + 1] == b'(' {
                    stack.push(ShellContext::Subshell);
                    i += 2;
                    continue;
                } else if b == b'(' {
                    stack.push(ShellContext::Subshell);
                } else if b == b')' && current == ShellContext::Subshell {
                    stack.pop();
                }
            }
        }
        i += 1;
    }

    let current = *stack.last().unwrap_or(&ShellContext::Normal);
    if in_comment || current == ShellContext::SingleQuote {
        return (CompletionContext::None, "");
    }

    // 1. Check for variable context (`$VAR`, `${VAR}`, `${#VAR}`)
    let mut var_ident_start = line_prefix.len();
    for (idx, c) in line_prefix.char_indices().rev() {
        if is_var_ident_char(c) {
            var_ident_start = idx;
        } else {
            break;
        }
    }

    let before_var_ident = &line_prefix[..var_ident_start];
    let is_dollar_var = if before_var_ident.ends_with("${#") {
        Some(&line_prefix[var_ident_start - 3..])
    } else if before_var_ident.ends_with("${") {
        Some(&line_prefix[var_ident_start - 2..])
    } else if before_var_ident.ends_with('$') && !before_var_ident.ends_with("$$") {
        Some(&line_prefix[var_ident_start - 1..])
    } else {
        None
    };

    if let Some(token) = is_dollar_var {
        return (CompletionContext::Variable, token);
    }

    // 2. Non-dollar contexts
    match current {
        ShellContext::Arithmetic => {
            if line_prefix.ends_with(|c: char| c.is_whitespace()) {
                (CompletionContext::Arithmetic, "")
            } else {
                (
                    CompletionContext::Arithmetic,
                    &line_prefix[var_ident_start..],
                )
            }
        }
        ShellContext::DoubleQuote => {
            // In a double-quoted string without a leading $, words are string literals,
            // NOT executable functions or commands.
            (CompletionContext::None, "")
        }
        ShellContext::Normal | ShellContext::Subshell | ShellContext::Backtick => {
            if line_prefix.ends_with(|c: char| c.is_whitespace()) {
                if is_command_position(line_prefix) {
                    (CompletionContext::CommandOrWord, "")
                } else {
                    (CompletionContext::None, "")
                }
            } else {
                let mut func_ident_start = line_prefix.len();
                for (idx, c) in line_prefix.char_indices().rev() {
                    if is_func_ident_char(c) {
                        func_ident_start = idx;
                    } else {
                        break;
                    }
                }

                let before_func_ident = &line_prefix[..func_ident_start];
                if before_func_ident.ends_with('/')
                    || before_func_ident.ends_with('=')
                    || before_func_ident.ends_with(':')
                    || before_func_ident.trim_end().ends_with(['>', '<'])
                {
                    return (CompletionContext::None, "");
                }

                (
                    CompletionContext::CommandOrWord,
                    &line_prefix[func_ident_start..],
                )
            }
        }
        ShellContext::SingleQuote => (CompletionContext::None, ""),
    }
}

/// Extracts the token immediately preceding the cursor from `line_prefix`.
pub fn extract_completion_token(line_prefix: &str) -> &str {
    let (_, token) = determine_completion_context(line_prefix);
    token
}

/// Checks whether `line_prefix` places the cursor in a command execution position.
fn is_command_position(line_prefix: &str) -> bool {
    let trimmed = line_prefix.trim_end();
    if trimmed.is_empty() {
        return true;
    }
    if trimmed.ends_with("((") {
        return false;
    }
    if trimmed.ends_with(';')
        || trimmed.ends_with('|')
        || trimmed.ends_with('&')
        || trimmed.ends_with('(')
        || trimmed.ends_with('`')
        || trimmed.ends_with('{')
    {
        return true;
    }

    let last_word = trimmed.split_whitespace().next_back().unwrap_or("");
    matches!(last_word, "then" | "do" | "else" | "elif")
}

/// Filters and maps local symbols according to the current cursor context within `line_prefix`.
pub fn filter_local_symbols(
    local_symbols: &[LocalSymbol],
    line_prefix: &str,
) -> Vec<CompletionItem> {
    let (context, token) = determine_completion_context(line_prefix);
    let mut items = Vec::new();

    match context {
        CompletionContext::Variable => {
            let var_prefix = if let Some(stripped) = token.strip_prefix("${#") {
                stripped
            } else if let Some(stripped) = token.strip_prefix("${") {
                stripped
            } else {
                token.strip_prefix('$').unwrap_or("")
            };

            for sym in local_symbols {
                if sym.kind == LocalSymbolKind::Variable && sym.name.starts_with(var_prefix) {
                    items.push(CompletionItem {
                        label: sym.name.clone(),
                        kind: Some(CompletionItemKind::VARIABLE),
                        detail: sym
                            .detail
                            .clone()
                            .or_else(|| Some("(local variable)".to_string())),
                        sort_text: Some(format!("00_{}", sym.name)),
                        ..Default::default()
                    });
                }
            }
        }
        CompletionContext::Arithmetic => {
            let var_prefix = if let Some(stripped) = token.strip_prefix('$') {
                stripped
            } else {
                token
            };

            for sym in local_symbols {
                if sym.kind == LocalSymbolKind::Variable && sym.name.starts_with(var_prefix) {
                    items.push(CompletionItem {
                        label: sym.name.clone(),
                        kind: Some(CompletionItemKind::VARIABLE),
                        detail: sym
                            .detail
                            .clone()
                            .or_else(|| Some("(local variable)".to_string())),
                        sort_text: Some(format!("00_{}", sym.name)),
                        ..Default::default()
                    });
                }
            }
        }
        CompletionContext::CommandOrWord => {
            let func_prefix = token;
            let eligible = !func_prefix.is_empty() || is_command_position(line_prefix);
            if eligible {
                for sym in local_symbols {
                    if matches!(sym.kind, LocalSymbolKind::Function | LocalSymbolKind::Alias)
                        && sym.name.starts_with(func_prefix)
                    {
                        let default_detail = match sym.kind {
                            LocalSymbolKind::Alias => "(local alias)",
                            _ => "(local function)",
                        };
                        items.push(CompletionItem {
                            label: sym.name.clone(),
                            kind: Some(CompletionItemKind::FUNCTION),
                            detail: sym
                                .detail
                                .clone()
                                .or_else(|| Some(default_detail.to_string())),
                            sort_text: Some(format!("00_{}", sym.name)),
                            ..Default::default()
                        });
                    }
                }
            }
        }
        CompletionContext::None => {}
    }

    items
}

/// Determines whether a daemon candidate label matches a local candidate label,
/// accounting for variable prefix variations like `$VAR` or `${VAR}`, as well as
/// colon-separated descriptions like `VAR:description`.
fn daemon_label_matches_local(daemon_label: &str, local_label: &str) -> bool {
    let raw_daemon = daemon_label
        .split_once(':')
        .map(|(name, _)| name)
        .unwrap_or(daemon_label)
        .trim();

    if raw_daemon == local_label {
        return true;
    }
    if let Some(stripped) = raw_daemon.strip_prefix('$')
        && stripped == local_label
    {
        return true;
    }
    if let Some(stripped) = raw_daemon.strip_prefix("${") {
        let inner = stripped.strip_suffix('}').unwrap_or(stripped);
        let unhashed = inner.strip_prefix('#').unwrap_or(inner);
        if unhashed == local_label {
            return true;
        }
    }
    false
}

/// Merges candidates returned from the Zsh completion daemon with local symbols.
///
/// Local definitions are prioritized at the top of the candidate list with their
/// corresponding `(local ...)` detail annotations. If any candidate returned by
/// the daemon matches a local symbol, the duplicate daemon entry is omitted.
pub fn merge_local_completions(
    mut daemon_items: Vec<CompletionItem>,
    local_symbols: &[LocalSymbol],
    line_prefix: &str,
) -> Vec<CompletionItem> {
    deduplicate_completion_items(&mut daemon_items);

    let local_items = filter_local_symbols(local_symbols, line_prefix);
    if local_items.is_empty() {
        return daemon_items;
    }

    let mut merged = Vec::with_capacity(local_items.len() + daemon_items.len());
    let mut seen_labels: HashSet<String> = HashSet::new();

    // 1. Add all filtered local items
    for item in &local_items {
        if seen_labels.insert(item.label.clone()) {
            merged.push(item.clone());
        }
    }

    // 2. Append non-duplicate daemon items
    for daemon_item in daemon_items {
        let is_duplicate = local_items
            .iter()
            .any(|loc| daemon_label_matches_local(&daemon_item.label, &loc.label));
        if !is_duplicate && seen_labels.insert(daemon_item.label.clone()) {
            merged.push(daemon_item);
        }
    }

    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    // 1. Only candidate without tab
    #[case("git status", "git status", None)]
    // 2. Candidate with tab and description
    #[case(
        "status\tshow working tree status",
        "status",
        Some("show working tree status")
    )]
    // 3. Candidate with tab but empty or whitespace description
    #[case("status\t   ", "status", None)]
    // 4. Multiple tabs in description
    #[case("foo\tbar\tbaz", "foo", Some("bar\tbaz"))]
    fn test_parse_candidate_line(
        #[case] input: &str,
        #[case] expected_label: &str,
        #[case] expected_detail: Option<&str>,
    ) {
        let mut items = Vec::new();
        parse_candidate_line(input, &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, expected_label);
        assert_eq!(items[0].detail.as_deref(), expected_detail);
    }

    #[rstest]
    // 1. Empty string
    #[case("", "", None, None, Some(CompletionItemKind::TEXT))]
    // 2. Whitespace only
    #[case("   ", "   ", None, None, Some(CompletionItemKind::TEXT))]
    // 3. Single tab only
    #[case("\t", "", None, None, Some(CompletionItemKind::TEXT))]
    // 4. Tab with whitespace
    #[case("\t   ", "", None, None, Some(CompletionItemKind::TEXT))]
    #[case("   \t   ", "   ", None, None, Some(CompletionItemKind::TEXT))]
    // 5. Consecutive tabs only
    #[case("\t\t", "", None, None, Some(CompletionItemKind::TEXT))]
    #[case("\t\t\t", "", None, None, Some(CompletionItemKind::TEXT))]
    // 6. Empty label with description
    #[case(
        "\tdescription only",
        "",
        Some("description only"),
        None,
        Some(CompletionItemKind::TEXT)
    )]
    fn test_parse_candidate_line_empty_and_whitespace(
        #[case] input: &str,
        #[case] expected_label: &str,
        #[case] expected_detail: Option<&str>,
        #[case] expected_insert_text: Option<&str>,
        #[case] expected_kind: Option<CompletionItemKind>,
    ) {
        let mut items = Vec::new();
        parse_candidate_line(input, &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, expected_label);
        assert_eq!(items[0].detail.as_deref(), expected_detail);
        assert_eq!(items[0].insert_text.as_deref(), expected_insert_text);
        assert_eq!(items[0].kind, expected_kind);
    }

    #[rstest]
    // Leading whitespace in label
    #[case("  status\tdesc", "  status", Some("desc"))]
    // Trailing whitespace in label
    #[case("status  \tdesc", "status  ", Some("desc"))]
    // Both leading and trailing in label
    #[case("  cmd  \tdesc", "  cmd  ", Some("desc"))]
    // Preserving leading and trailing spaces in detail
    #[case(
        "status\t  detailed description  ",
        "status",
        Some("  detailed description  ")
    )]
    // Trailing tab only
    #[case("status\t", "status", None)]
    // Without tab, spaces preserved
    #[case("  leading and trailing  ", "  leading and trailing  ", None)]
    fn test_parse_candidate_line_leading_trailing_spaces(
        #[case] input: &str,
        #[case] expected_label: &str,
        #[case] expected_detail: Option<&str>,
    ) {
        let mut items = Vec::new();
        parse_candidate_line(input, &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, expected_label);
        assert_eq!(items[0].detail.as_deref(), expected_detail);
    }

    #[rstest]
    // Consecutive tabs with description
    #[case("status\t\tdesc", "status", Some("\tdesc"))]
    // Multiple tabs in description
    #[case("part1\tpart2\tpart3", "part1", Some("part2\tpart3"))]
    // Many tabs in description
    #[case("cmd\topt1\topt2\topt3\topt4", "cmd", Some("opt1\topt2\topt3\topt4"))]
    fn test_parse_candidate_line_tab_delimiters(
        #[case] input: &str,
        #[case] expected_label: &str,
        #[case] expected_detail: Option<&str>,
    ) {
        let mut items = Vec::new();
        parse_candidate_line(input, &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, expected_label);
        assert_eq!(items[0].detail.as_deref(), expected_detail);
    }

    #[rstest]
    // Double and single quotes
    #[case(
        "\"double quoted\"\t'single quoted' desc",
        "\"double quoted\"",
        Some("'single quoted' desc")
    )]
    // Backslashes
    #[case("path\\to\\file\tdesc\\path", "path\\to\\file", Some("desc\\path"))]
    // Escaped quotes
    #[case("escaped\\\"quote\\\"\tdesc", "escaped\\\"quote\\\"", Some("desc"))]
    // Single quote label, double quote desc
    #[case("'single'\t\"double\"", "'single'", Some("\"double\""))]
    fn test_parse_candidate_line_quotes_and_escapes(
        #[case] input: &str,
        #[case] expected_label: &str,
        #[case] expected_detail: Option<&str>,
    ) {
        let mut items = Vec::new();
        parse_candidate_line(input, &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, expected_label);
        assert_eq!(items[0].detail.as_deref(), expected_detail);
    }

    #[rstest]
    // Environment variables and parameter expansions
    #[case("$VAR\tenvironment variable", "$VAR", Some("environment variable"))]
    #[case(
        "${VAR:-default}\tparameter expansion",
        "${VAR:-default}",
        Some("parameter expansion")
    )]
    // Globs and braces
    #[case("*.tar.gz\tglob pattern", "*.tar.gz", Some("glob pattern"))]
    #[case("{a,b,c}\tbrace expansion", "{a,b,c}", Some("brace expansion"))]
    #[case("[0-9]*.txt\trange glob", "[0-9]*.txt", Some("range glob"))]
    // Pipes, redirects, and background
    #[case("cmd1 | cmd2\tpipeline", "cmd1 | cmd2", Some("pipeline"))]
    #[case("cmd &\tbackground", "cmd &", Some("background"))]
    #[case("cmd1; cmd2\tseparator", "cmd1; cmd2", Some("separator"))]
    #[case("cmd1 && cmd2\tand operator", "cmd1 && cmd2", Some("and operator"))]
    #[case(">out.log\tredirect stdout", ">out.log", Some("redirect stdout"))]
    #[case("2>&1\tredirect stderr", "2>&1", Some("redirect stderr"))]
    // Subshells and process substitution
    #[case("<(cmd)\tprocess substitution", "<(cmd)", Some("process substitution"))]
    #[case("$(whoami)\tsubshell", "$(whoami)", Some("subshell"))]
    #[case("`pwd`\tbacktick", "`pwd`", Some("backtick"))]
    // Flags and options
    #[case("-o:fmt\tcolon flag", "-o:fmt", Some("colon flag"))]
    #[case("--opt=val\tequals flag", "--opt=val", Some("equals flag"))]
    #[case("~/.zshrc\ttilde path", "~/.zshrc", Some("tilde path"))]
    fn test_parse_candidate_line_shell_metacharacters(
        #[case] input: &str,
        #[case] expected_label: &str,
        #[case] expected_detail: Option<&str>,
    ) {
        let mut items = Vec::new();
        parse_candidate_line(input, &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, expected_label);
        assert_eq!(items[0].detail.as_deref(), expected_detail);
    }

    #[rstest]
    // ANSI color in label
    #[case(
        "\x1b[32m--verbose\x1b[0m\tenable verbose",
        "\x1b[32m--verbose\x1b[0m",
        Some("enable verbose")
    )]
    // ANSI color in detail
    #[case(
        "--color\t\x1b[1mcolored description\x1b[0m",
        "--color",
        Some("\x1b[1mcolored description\x1b[0m")
    )]
    // Protocol control character literal
    #[case("\x01EOC\x01\tmarker in label", "\x01EOC\x01", Some("marker in label"))]
    fn test_parse_candidate_line_ansi_escapes(
        #[case] input: &str,
        #[case] expected_label: &str,
        #[case] expected_detail: Option<&str>,
    ) {
        let mut items = Vec::new();
        parse_candidate_line(input, &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, expected_label);
        assert_eq!(items[0].detail.as_deref(), expected_detail);
    }

    #[rstest]
    // CJK Japanese
    #[case("コミット\t変更内容を記録する", "コミット", Some("変更内容を記録する"))]
    #[case("設定ファイル.zsh\t設定の概要", "設定ファイル.zsh", Some("設定の概要"))]
    // CJK Chinese
    #[case("分支\t显示分支列表", "分支", Some("显示分支列表"))]
    // CJK Korean
    #[case("커밋\t커밋 생성", "커밋", Some("커밋 생성"))]
    // Emojis with ZWJ and skin tones
    #[case("✨ feat\t新機能の追加", "✨ feat", Some("新機能の追加"))]
    #[case("🚀 deploy\tデプロイ実行", "🚀 deploy", Some("デプロイ実行"))]
    #[case("👨‍👩‍👧‍👦 family\t家族絵文字", "👨‍👩‍👧‍👦 family", Some("家族絵文字"))]
    #[case(
        "👍🏽 thumbs_up\tskin tone emoji",
        "👍🏽 thumbs_up",
        Some("skin tone emoji")
    )]
    // Accents
    #[case("café\tFrench cafe", "café", Some("French cafe"))]
    #[case(
        "üñîçødé\tcombining and accents",
        "üñîçødé",
        Some("combining and accents")
    )]
    // RTL
    #[case("مرحبا\tArabic greeting", "مرحبا", Some("Arabic greeting"))]
    #[case("שלום\tHebrew greeting", "שלום", Some("Hebrew greeting"))]
    fn test_parse_candidate_line_multibyte_and_unicode(
        #[case] input: &str,
        #[case] expected_label: &str,
        #[case] expected_detail: Option<&str>,
    ) {
        let mut items = Vec::new();
        parse_candidate_line(input, &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, expected_label);
        assert_eq!(items[0].detail.as_deref(), expected_detail);
    }

    #[rstest]
    #[case(10_000, 10_000)]
    #[case(10_000, 0)]
    #[case(10, 50_000)]
    fn test_parse_candidate_line_extremely_long(
        #[case] label_len: usize,
        #[case] detail_len: usize,
    ) {
        let mut items = Vec::new();
        let long_label = "a".repeat(label_len);
        let line = if detail_len > 0 {
            let long_detail = "b".repeat(detail_len);
            format!("{}\t{}", long_label, long_detail)
        } else {
            long_label.clone()
        };

        parse_candidate_line(&line, &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label.len(), label_len);
        assert_eq!(items[0].label, long_label);
        if detail_len > 0 {
            assert_eq!(items[0].detail.as_ref().map(|s| s.len()), Some(detail_len));
        } else {
            assert_eq!(items[0].detail, None);
        }
    }

    #[rstest]
    #[case(
        "checkout\tswitch branch",
        "checkout",
        Some(CompletionItemKind::TEXT),
        None,
        Some("switch branch")
    )]
    #[case("commit", "commit", Some(CompletionItemKind::TEXT), None, None)]
    #[case(
        "--help\tshow help",
        "--help",
        Some(CompletionItemKind::KEYWORD),
        None,
        Some("show help")
    )]
    fn test_parse_candidate_line_item_properties(
        #[case] input: &str,
        #[case] expected_label: &str,
        #[case] expected_kind: Option<CompletionItemKind>,
        #[case] expected_insert_text: Option<&str>,
        #[case] expected_detail: Option<&str>,
    ) {
        let mut items = Vec::new();
        parse_candidate_line(input, &mut items);
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.label, expected_label);
        assert_eq!(item.kind, expected_kind);
        assert_eq!(item.insert_text.as_deref(), expected_insert_text);
        assert_eq!(item.detail.as_deref(), expected_detail);
    }

    #[rstest]
    // 1. Prefix options / flags -> KEYWORD
    #[case("-v", None, CompletionItemKind::KEYWORD)]
    #[case("--help", Some("show help"), CompletionItemKind::KEYWORD)]
    #[case("-o:fmt", Some("output format"), CompletionItemKind::KEYWORD)]
    #[case("--flag=value", None, CompletionItemKind::KEYWORD)]
    // 2. Prefix variables -> VARIABLE
    #[case("$HOME", None, CompletionItemKind::VARIABLE)]
    #[case("${VAR:-default}", None, CompletionItemKind::VARIABLE)]
    #[case("$PATH", Some("search path"), CompletionItemKind::VARIABLE)]
    #[case("$?", Some("exit code"), CompletionItemKind::VARIABLE)]
    // 3. Folders ending with / or dot directories -> FOLDER
    #[case("src/", None, CompletionItemKind::FOLDER)]
    #[case("path/to/dir/", None, CompletionItemKind::FOLDER)]
    #[case("/etc/", Some("config dir"), CompletionItemKind::FOLDER)]
    #[case("~/", None, CompletionItemKind::FOLDER)]
    #[case(".", None, CompletionItemKind::FOLDER)]
    #[case("..", None, CompletionItemKind::FOLDER)]
    #[case("~", None, CompletionItemKind::FOLDER)]
    // 4. Files containing dot -> FILE
    #[case(".zshrc", None, CompletionItemKind::FILE)]
    #[case("script.sh", None, CompletionItemKind::FILE)]
    #[case("path/to/file.txt", None, CompletionItemKind::FILE)]
    #[case("archive.tar.gz", Some("archive"), CompletionItemKind::FILE)]
    // 5. Paths containing slash but no dot -> FOLDER
    #[case("path/to/subdir", None, CompletionItemKind::FOLDER)]
    #[case("/usr/local/bin", None, CompletionItemKind::FOLDER)]
    // 6. Detail-based inference: commands, builtins, functions, aliases, executables -> FUNCTION
    #[case("echo", Some("builtin command"), CompletionItemKind::FUNCTION)]
    #[case("git", Some("command line tool"), CompletionItemKind::FUNCTION)]
    #[case("my_func", Some("shell function"), CompletionItemKind::FUNCTION)]
    #[case("ls", Some("builtin"), CompletionItemKind::FUNCTION)]
    #[case("ll", Some("alias for ls -la"), CompletionItemKind::FUNCTION)]
    #[case("cargo", Some("executable binary"), CompletionItemKind::FUNCTION)]
    #[case("./my_func", Some("shell function"), CompletionItemKind::FUNCTION)]
    // 7. Detail-based inference: options, flags -> KEYWORD
    #[case("verbose", Some("verbose option"), CompletionItemKind::KEYWORD)]
    #[case("debug", Some("debug flag"), CompletionItemKind::KEYWORD)]
    // 8. Detail-based inference: variables, env -> VARIABLE
    #[case("USER", Some("environment variable"), CompletionItemKind::VARIABLE)]
    #[case("SHELL", Some("shell parameter"), CompletionItemKind::VARIABLE)]
    #[case("PORT", Some("env configuration"), CompletionItemKind::VARIABLE)]
    // 9. Detail-based inference: directory, folder -> FOLDER
    #[case("mydir", Some("project directory"), CompletionItemKind::FOLDER)]
    #[case("docs", Some("documentation folder"), CompletionItemKind::FOLDER)]
    #[case("./mydir", Some("directory"), CompletionItemKind::FOLDER)]
    // 10. Detail-based inference: file, archive -> FILE
    #[case("LICENSE", Some("license file"), CompletionItemKind::FILE)]
    #[case("README", Some("documentation file"), CompletionItemKind::FILE)]
    #[case("bundle.zip", Some("compressed archive"), CompletionItemKind::FILE)]
    // 11. Plain text fallback -> TEXT
    #[case("checkout", Some("switch branch"), CompletionItemKind::TEXT)]
    #[case("status", Some("show working tree status"), CompletionItemKind::TEXT)]
    #[case("plain", None, CompletionItemKind::TEXT)]
    fn test_infer_completion_kind(
        #[case] label: &str,
        #[case] detail: Option<&str>,
        #[case] expected_kind: CompletionItemKind,
    ) {
        let kind = infer_completion_kind(label, detail);
        assert_eq!(kind, expected_kind);
    }

    #[rstest]
    #[case(&[], &[])]
    #[case(&["single\tonly"], &[("single", Some("only"))])]
    #[case(
        &["first\tdesc1", "second\tdesc2", "third"],
        &[("first", Some("desc1")), ("second", Some("desc2")), ("third", None)]
    )]
    #[case(
        &["--flag\toption", "-v\tverbose", "subcmd"],
        &[("--flag", Some("option")), ("-v", Some("verbose")), ("subcmd", None)]
    )]
    fn test_parse_candidate_line_accumulation(
        #[case] inputs: &[&str],
        #[case] expected: &[(&str, Option<&str>)],
    ) {
        let mut items = Vec::new();
        for input in inputs {
            parse_candidate_line(input, &mut items);
        }
        assert_eq!(items.len(), expected.len());
        for (item, &(exp_label, exp_detail)) in items.iter().zip(expected.iter()) {
            assert_eq!(item.label, exp_label);
            assert_eq!(item.detail.as_deref(), exp_detail);
        }
    }

    #[test]
    fn test_resolve_completion_item_builtins() {
        let cases = vec![
            ("cd", "Change the current working directory."),
            ("echo", "Write arguments to the standard output."),
            ("export", "Set export attribute for shell parameters."),
            (
                "pwd",
                "Print the absolute path name of the current working directory.",
            ),
            ("setopt", "Set the specified shell options."),
        ];

        for (label, snippet) in cases {
            let item = CompletionItem {
                label: label.to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                ..Default::default()
            };
            let resolved = resolve_completion_item(item);
            let doc = resolved
                .documentation
                .expect("Expected documentation for builtin");
            match doc {
                Documentation::MarkupContent(markup) => {
                    assert_eq!(markup.kind, MarkupKind::Markdown);
                    assert!(
                        markup.value.contains(snippet),
                        "Expected snippet '{}' in doc for '{}'",
                        snippet,
                        label
                    );
                }
                Documentation::String(_) => panic!("Expected MarkupContent documentation"),
            }
        }
    }

    #[test]
    fn test_resolve_completion_item_reserved_words() {
        let cases = vec![
            (
                "if",
                "Execute command list conditionally based on exit status.",
            ),
            (
                "while",
                "Execute command list repeatedly as long as the test command returns status 0.",
            ),
            (
                "function",
                "Define a shell function with the specified name.",
            ),
            (
                "for",
                "Execute command list for each member in a list or arithmetic iteration.",
            ),
            (
                "case",
                "Execute command list corresponding to the first matching pattern.",
            ),
        ];

        for (label, snippet) in cases {
            let item = CompletionItem {
                label: label.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            };
            let resolved = resolve_completion_item(item);
            let doc = resolved
                .documentation
                .expect("Expected documentation for reserved word");
            match doc {
                Documentation::MarkupContent(markup) => {
                    assert_eq!(markup.kind, MarkupKind::Markdown);
                    assert!(
                        markup.value.contains(snippet),
                        "Expected snippet '{}' in doc for '{}'",
                        snippet,
                        label
                    );
                }
                Documentation::String(_) => panic!("Expected MarkupContent documentation"),
            }
        }
    }

    #[test]
    fn test_resolve_completion_item_special_kinds_and_omitted_kind() {
        for (label, kind) in [
            (".", Some(CompletionItemKind::FOLDER)),
            (":", Some(CompletionItemKind::TEXT)),
            ("cd", Some(CompletionItemKind::TEXT)),
            ("cd", None),
            ("if", Some(CompletionItemKind::TEXT)),
            ("if", None),
        ] {
            let item = CompletionItem {
                label: label.to_string(),
                kind,
                ..Default::default()
            };
            let resolved = resolve_completion_item(item);
            assert!(
                resolved.documentation.is_some(),
                "Expected documentation for {label} ({kind:?})"
            );
        }
    }

    #[test]
    fn test_resolve_completion_item_fallback_unrecognized() {
        let unknown = CompletionItem {
            label: "my_custom_unknown_binary".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some("custom command".to_string()),
            ..Default::default()
        };
        let resolved = resolve_completion_item(unknown.clone());
        assert_eq!(resolved.documentation, None);
        assert_eq!(resolved.label, unknown.label);
        assert_eq!(resolved.kind, unknown.kind);
        assert_eq!(resolved.detail, unknown.detail);
    }

    #[test]
    fn test_resolve_completion_item_file_fallback() {
        // Even if a file shares a name with a builtin (e.g., 'cd' or 'echo'), if kind is FILE,
        // it must not be resolved to builtin documentation.
        let file_item = CompletionItem {
            label: "cd".to_string(),
            kind: Some(CompletionItemKind::FILE),
            ..Default::default()
        };
        let resolved = resolve_completion_item(file_item);
        assert_eq!(resolved.documentation, None);

        // Path candidate fallback
        let path_item = CompletionItem {
            label: "/usr/bin/cd".to_string(),
            kind: Some(CompletionItemKind::FILE),
            ..Default::default()
        };
        let resolved_path = resolve_completion_item(path_item);
        assert_eq!(resolved_path.documentation, None);
    }

    #[test]
    fn test_resolve_completion_item_preserves_existing_documentation() {
        let item = CompletionItem {
            label: "cd".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            documentation: Some(Documentation::String("pre-existing doc".to_string())),
            ..Default::default()
        };
        let resolved = resolve_completion_item(item);
        match resolved.documentation {
            Some(Documentation::String(s)) => assert_eq!(s, "pre-existing doc"),
            other => {
                panic!("Expected pre-existing string documentation preserved, got {other:?}")
            }
        }
    }

    #[test]
    fn test_resolve_completion_item_special_builtins() {
        // '.' builtin
        let dot_item = CompletionItem {
            label: ".".to_string(),
            kind: Some(CompletionItemKind::FOLDER),
            ..Default::default()
        };
        let resolved = resolve_completion_item(dot_item);
        assert!(resolved.documentation.is_some());

        // ':' builtin
        let colon_item = CompletionItem {
            label: ":".to_string(),
            ..Default::default()
        };
        let resolved = resolve_completion_item(colon_item);
        assert!(resolved.documentation.is_some());
    }

    #[test]
    fn test_resolve_completion_item_edge_cases() {
        // Empty label
        let empty = CompletionItem {
            label: String::new(),
            ..Default::default()
        };
        assert_eq!(resolve_completion_item(empty).documentation, None);

        // Whitespace only label
        let ws = CompletionItem {
            label: "   ".to_string(),
            ..Default::default()
        };
        assert_eq!(resolve_completion_item(ws).documentation, None);

        // Variable kind with builtin name
        let var = CompletionItem {
            label: "cd".to_string(),
            kind: Some(CompletionItemKind::VARIABLE),
            ..Default::default()
        };
        assert_eq!(resolve_completion_item(var).documentation, None);

        // Folder kind with non-dot name
        let folder = CompletionItem {
            label: "cd".to_string(),
            kind: Some(CompletionItemKind::FOLDER),
            ..Default::default()
        };
        assert_eq!(resolve_completion_item(folder).documentation, None);

        // Parent folder '..'
        let parent_folder = CompletionItem {
            label: "..".to_string(),
            kind: Some(CompletionItemKind::FOLDER),
            ..Default::default()
        };
        assert_eq!(resolve_completion_item(parent_folder).documentation, None);

        // Snippet kind with builtin name
        let snippet = CompletionItem {
            label: "echo".to_string(),
            kind: Some(CompletionItemKind::SNIPPET),
            ..Default::default()
        };
        assert_eq!(resolve_completion_item(snippet).documentation, None);
    }

    #[test]
    fn test_resolve_completion_item_empty_documentation_is_resolved() {
        // Empty String documentation should not block resolution
        let item_empty_str = CompletionItem {
            label: "cd".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            documentation: Some(Documentation::String("".to_string())),
            ..Default::default()
        };
        let resolved = resolve_completion_item(item_empty_str);
        match resolved.documentation {
            Some(Documentation::MarkupContent(markup)) => {
                assert_eq!(markup.kind, MarkupKind::Markdown);
                assert!(
                    markup
                        .value
                        .contains("Change the current working directory.")
                );
            }
            other => {
                panic!("Expected empty string doc to be resolved to MarkupContent, got {other:?}")
            }
        }

        // Whitespace String documentation should not block resolution
        let item_ws_str = CompletionItem {
            label: "echo".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            documentation: Some(Documentation::String("   \n\t  ".to_string())),
            ..Default::default()
        };
        let resolved_ws = resolve_completion_item(item_ws_str);
        match resolved_ws.documentation {
            Some(Documentation::MarkupContent(markup)) => {
                assert_eq!(markup.kind, MarkupKind::Markdown);
                assert!(
                    markup
                        .value
                        .contains("Write arguments to the standard output.")
                );
            }
            other => panic!("Expected whitespace string doc to be resolved, got {other:?}"),
        }

        // Empty MarkupContent documentation should not block resolution
        let item_empty_markup = CompletionItem {
            label: "export".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            documentation: Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: "".to_string(),
            })),
            ..Default::default()
        };
        let resolved_markup = resolve_completion_item(item_empty_markup);
        match resolved_markup.documentation {
            Some(Documentation::MarkupContent(markup)) => {
                assert_eq!(markup.kind, MarkupKind::Markdown);
                assert!(
                    markup
                        .value
                        .contains("Set export attribute for shell parameters.")
                );
            }
            other => panic!("Expected empty markup doc to be resolved, got {other:?}"),
        }

        // Whitespace-only MarkupContent should not block resolution
        let item_ws_markup = CompletionItem {
            label: "setopt".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            documentation: Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: "   \n\t  ".to_string(),
            })),
            ..Default::default()
        };
        let resolved_ws_markup = resolve_completion_item(item_ws_markup);
        match resolved_ws_markup.documentation {
            Some(Documentation::MarkupContent(markup)) => {
                assert_eq!(markup.kind, MarkupKind::Markdown);
                assert!(markup.value.contains("Set the specified shell options."));
            }
            other => panic!("Expected whitespace markup doc to be resolved, got {other:?}"),
        }

        // Non-empty PlainText MarkupContent should be preserved as-is
        let item_plain_markup = CompletionItem {
            label: "cd".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            documentation: Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::PlainText,
                value: "Custom plain text doc for cd".to_string(),
            })),
            ..Default::default()
        };
        let resolved_plain_markup = resolve_completion_item(item_plain_markup);
        match resolved_plain_markup.documentation {
            Some(Documentation::MarkupContent(markup)) => {
                assert_eq!(markup.kind, MarkupKind::PlainText);
                assert_eq!(markup.value, "Custom plain text doc for cd");
            }
            other => panic!("Expected plain text markup doc to be preserved, got {other:?}"),
        }
    }

    #[test]
    fn test_resolve_completion_item_whitespace_in_label() {
        // Leading and trailing whitespace in label
        let item = CompletionItem {
            label: "  echo  ".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        };
        let resolved = resolve_completion_item(item);
        assert_eq!(resolved.label, "  echo  ");
        assert!(resolved.documentation.is_some());

        // Builtin dot '.' with whitespace
        let dot_item = CompletionItem {
            label: " . ".to_string(),
            kind: Some(CompletionItemKind::FOLDER),
            ..Default::default()
        };
        let resolved_dot = resolve_completion_item(dot_item);
        assert_eq!(resolved_dot.label, " . ");
        assert!(resolved_dot.documentation.is_some());
    }

    #[test]
    fn test_resolve_completion_item_field_preservation() {
        use tower_lsp::lsp_types::{
            Command, CompletionItemLabelDetails, CompletionItemTag, InsertTextFormat, Position,
            Range, TextEdit,
        };

        let original = CompletionItem {
            label: "echo".to_string(),
            label_details: Some(CompletionItemLabelDetails {
                detail: Some(" (builtin)".to_string()),
                description: Some("print args".to_string()),
            }),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some("builtin command".to_string()),
            documentation: None,
            deprecated: Some(false),
            preselect: Some(true),
            sort_text: Some("0001".to_string()),
            filter_text: Some("echo".to_string()),
            insert_text: Some("echo $0".to_string()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            insert_text_mode: None,
            text_edit: Some(tower_lsp::lsp_types::CompletionTextEdit::Edit(TextEdit {
                range: Range::new(Position::new(0, 0), Position::new(0, 4)),
                new_text: "echo ".to_string(),
            })),
            additional_text_edits: Some(vec![TextEdit {
                range: Range::new(Position::new(0, 4), Position::new(0, 4)),
                new_text: "\n".to_string(),
            }]),
            command: Some(Command {
                title: "Trigger Suggest".to_string(),
                command: "editor.action.triggerSuggest".to_string(),
                arguments: None,
            }),
            commit_characters: Some(vec![" ".to_string()]),
            data: Some(serde_json::json!({
                "source": "zshcs",
                "custom_id": 999
            })),
            tags: Some(vec![CompletionItemTag::DEPRECATED]),
        };

        let resolved = resolve_completion_item(original.clone());

        // Verify documentation was added
        assert!(resolved.documentation.is_some());

        // Verify every other single field was preserved exactly
        assert_eq!(resolved.label, original.label);
        assert_eq!(resolved.label_details, original.label_details);
        assert_eq!(resolved.kind, original.kind);
        assert_eq!(resolved.detail, original.detail);
        assert_eq!(resolved.deprecated, original.deprecated);
        assert_eq!(resolved.preselect, original.preselect);
        assert_eq!(resolved.sort_text, original.sort_text);
        assert_eq!(resolved.filter_text, original.filter_text);
        assert_eq!(resolved.insert_text, original.insert_text);
        assert_eq!(resolved.insert_text_format, original.insert_text_format);
        assert_eq!(resolved.text_edit, original.text_edit);
        assert_eq!(
            resolved.additional_text_edits,
            original.additional_text_edits
        );
        assert_eq!(resolved.command, original.command);
        assert_eq!(resolved.commit_characters, original.commit_characters);
        assert_eq!(resolved.data, original.data);
        assert_eq!(resolved.tags, original.tags);
    }

    #[tokio::test]
    async fn test_resolve_completion_item_async_external_command_git() {
        let cache = ManCache::new();
        let item = CompletionItem {
            label: "git".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        };

        let resolved = resolve_completion_item_async(item, &cache).await;
        assert_eq!(resolved.label, "git");
        match resolved.documentation {
            Some(Documentation::MarkupContent(markup)) => {
                assert_eq!(markup.kind, MarkupKind::Markdown);
                assert!(markup.value.starts_with("```text\n"));
                assert!(markup.value.ends_with("\n```"));
                assert!(
                    markup.value.to_lowercase().contains("git")
                        || markup.value.to_lowercase().contains("repository")
                );
            }
            other => panic!("Expected MarkupContent documentation for 'git', got {other:?}"),
        }

        // Cache must have stored the result
        assert!(cache.contains_key("git"));
        assert!(cache.get("git").unwrap().is_some());
    }

    #[tokio::test]
    async fn test_resolve_completion_item_async_cache_hit_and_caching() {
        let cache = ManCache::new();
        let item = CompletionItem {
            label: "git".to_string(),
            kind: Some(CompletionItemKind::TEXT),
            ..Default::default()
        };

        // 1. First resolution populates cache
        let resolved = resolve_completion_item_async(item.clone(), &cache).await;
        assert!(resolved.documentation.is_some());
        assert!(cache.contains_key("git"));

        // 2. Overwrite cache with mock entry to prove 2nd resolution reads strictly from cache
        let mock_md = "```text\nmock git manual page\n```".to_string();
        cache.insert("git".to_string(), Some(mock_md.clone()));

        let resolved2 = resolve_completion_item_async(item, &cache).await;
        match resolved2.documentation {
            Some(Documentation::MarkupContent(markup)) => {
                assert_eq!(markup.value, mock_md);
            }
            other => panic!("Expected mock doc from cache, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_resolve_completion_item_async_negative_cache() {
        let cache = ManCache::new();
        let dummy = "nonexistent_dummy_binary_xyz123_456";
        let item = CompletionItem {
            label: dummy.to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        };

        // 1. Initial resolution fails to find man page
        let resolved = resolve_completion_item_async(item.clone(), &cache).await;
        assert_eq!(resolved.documentation, None);

        // Negative cache must contain None
        assert!(cache.contains_key(dummy));
        assert_eq!(*cache.get(dummy).unwrap(), None);

        // 2. Second resolution should return None immediately from negative cache
        let resolved2 = resolve_completion_item_async(item, &cache).await;
        assert_eq!(resolved2.documentation, None);
    }

    #[tokio::test]
    async fn test_resolve_completion_item_async_ineligible_kinds() {
        let cache = ManCache::new();

        let ineligible_kinds = vec![
            CompletionItemKind::FILE,
            CompletionItemKind::FOLDER,
            CompletionItemKind::VARIABLE,
            CompletionItemKind::SNIPPET,
            CompletionItemKind::KEYWORD,
        ];

        for kind in ineligible_kinds {
            let item = CompletionItem {
                label: "git".to_string(),
                kind: Some(kind),
                ..Default::default()
            };
            let resolved = resolve_completion_item_async(item, &cache).await;
            assert_eq!(
                resolved.documentation, None,
                "Kind {kind:?} should not resolve external command"
            );
            assert!(
                !cache.contains_key("git"),
                "Cache should not be touched for ineligible kind {kind:?}"
            );
        }
    }

    #[tokio::test]
    async fn test_resolve_completion_item_async_preserves_existing_documentation() {
        let cache = ManCache::new();
        let existing = "Custom user documentation";
        let item = CompletionItem {
            label: "git".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            documentation: Some(Documentation::String(existing.to_string())),
            ..Default::default()
        };

        let resolved = resolve_completion_item_async(item, &cache).await;
        assert_eq!(
            resolved.documentation,
            Some(Documentation::String(existing.to_string()))
        );
        assert!(!cache.contains_key("git"));
    }

    #[tokio::test]
    async fn test_resolve_completion_item_async_resolves_builtins_without_caching() {
        let cache = ManCache::new();
        let item = CompletionItem {
            label: "echo".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        };

        let resolved = resolve_completion_item_async(item, &cache).await;
        match resolved.documentation {
            Some(Documentation::MarkupContent(markup)) => {
                assert!(markup.value.contains("`echo` (Zsh Builtin)"));
            }
            other => panic!("Expected builtin doc, got {other:?}"),
        }
        // Builtins must not pollute the external man cache
        assert!(!cache.contains_key("echo"));
    }

    #[tokio::test]
    async fn test_resolve_completion_item_async_concurrency() {
        use std::sync::Arc;
        use tokio::sync::Barrier;

        let cache = Arc::new(ManCache::new());
        let count = 20;
        let barrier = Arc::new(Barrier::new(count));
        let mut handles = Vec::new();

        for _ in 0..count {
            let cache_clone = Arc::clone(&cache);
            let barrier_clone = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier_clone.wait().await;
                let item = CompletionItem {
                    label: "git".to_string(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    ..Default::default()
                };
                resolve_completion_item_async(item, &cache_clone).await
            }));
        }

        for handle in handles {
            let resolved = handle.await.unwrap();
            match resolved.documentation {
                Some(Documentation::MarkupContent(markup)) => {
                    assert!(
                        markup.value.to_lowercase().contains("git")
                            || markup.value.to_lowercase().contains("repository")
                    );
                }
                other => panic!("Expected MarkupContent in concurrent test, got {other:?}"),
            }
        }

        assert!(cache.contains_key("git"));
    }

    #[tokio::test]
    async fn test_resolve_completion_item_async_empty_doc_is_resolved_for_external_command() {
        let cache = ManCache::new();

        // 1. Empty string documentation
        let item_empty_str = CompletionItem {
            label: "git".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            documentation: Some(Documentation::String("".to_string())),
            ..Default::default()
        };
        let resolved = resolve_completion_item_async(item_empty_str, &cache).await;
        match resolved.documentation {
            Some(Documentation::MarkupContent(markup)) => {
                assert!(
                    markup.value.to_lowercase().contains("git")
                        || markup.value.to_lowercase().contains("repository")
                );
            }
            other => panic!(
                "Expected empty string doc on external command to be resolved, got {other:?}"
            ),
        }

        // 2. Whitespace-only string documentation
        let item_ws_str = CompletionItem {
            label: "git".to_string(),
            kind: Some(CompletionItemKind::TEXT),
            documentation: Some(Documentation::String("   \n\t  ".to_string())),
            ..Default::default()
        };
        let resolved_ws = resolve_completion_item_async(item_ws_str, &cache).await;
        match resolved_ws.documentation {
            Some(Documentation::MarkupContent(markup)) => {
                assert!(markup.value.to_lowercase().contains("git"));
            }
            other => panic!(
                "Expected whitespace string doc on external command to be resolved, got {other:?}"
            ),
        }

        // 3. Empty MarkupContent documentation
        let item_empty_markup = CompletionItem {
            label: "git".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            documentation: Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: "".to_string(),
            })),
            ..Default::default()
        };
        let resolved_markup = resolve_completion_item_async(item_empty_markup, &cache).await;
        match resolved_markup.documentation {
            Some(Documentation::MarkupContent(markup)) => {
                assert!(markup.value.to_lowercase().contains("git"));
            }
            other => panic!(
                "Expected empty markup doc on external command to be resolved, got {other:?}"
            ),
        }

        // 4. Nonexistent external command with empty documentation returns None
        let dummy = "nonexistent_dummy_binary_with_empty_doc";
        let dummy_item = CompletionItem {
            label: dummy.to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            documentation: Some(Documentation::String("   ".to_string())),
            ..Default::default()
        };
        let resolved_dummy = resolve_completion_item_async(dummy_item, &cache).await;
        assert_eq!(resolved_dummy.documentation, None);
    }

    #[tokio::test]
    async fn test_resolve_completion_item_async_ineligible_label_edge_cases() {
        let cache = ManCache::new();

        let edge_labels = [
            ".gitignore",
            ".zshrc",
            "..",
            "...",
            "::",
            ":wq",
            "git.",
            "test:",
            "_",
            "__",
            "-v",
            "--flag",
        ];

        for label in edge_labels {
            let item = CompletionItem {
                label: label.to_string(),
                kind: Some(CompletionItemKind::TEXT),
                ..Default::default()
            };
            let resolved = resolve_completion_item_async(item, &cache).await;
            assert_eq!(
                resolved.documentation, None,
                "Label '{label}' must not resolve external man documentation"
            );
            assert!(
                !cache.contains_key(label),
                "Label '{label}' must not be stored in man_cache"
            );
        }
    }

    #[test]
    fn test_extract_local_symbols_empty() {
        let syms = extract_local_symbols_from_text("", Position::new(0, 0));
        assert!(syms.is_empty());
    }

    #[test]
    fn test_extract_local_symbols_variables_and_functions() {
        let text = r#"
MY_VAR="hello"
my_func() {
    echo "inside"
}
function other_func {
    return 0
}
echo "done"
"#;
        // Cursor at line 8 (after everything)
        let syms = extract_local_symbols_from_text(text, Position::new(8, 0));
        assert_eq!(
            syms,
            vec![
                LocalSymbol::new("MY_VAR", LocalSymbolKind::Variable),
                LocalSymbol::new("my_func", LocalSymbolKind::Function),
                LocalSymbol::new("other_func", LocalSymbolKind::Function),
            ]
        );
    }

    #[test]
    fn test_extract_local_symbols_scope_boundary_subsequent_line() {
        let text = r#"
BEFORE_VAR="visible"
before_func() { : }
echo $
AFTER_VAR="hidden"
after_func() { : }
"#;
        // Cursor at line 3: `echo $`
        let syms = extract_local_symbols_from_text(text, Position::new(3, 6));
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"BEFORE_VAR"));
        assert!(names.contains(&"before_func"));
        assert!(!names.contains(&"AFTER_VAR"));
        assert!(!names.contains(&"after_func"));
    }

    #[test]
    fn test_extract_local_symbols_scope_boundary_same_line() {
        let text = "PRIOR_VAR=1; echo $; POST_VAR=2; post_fn() { : }";
        // Cursor at `echo $` (character 18)
        let syms = extract_local_symbols_from_text(text, Position::new(0, 18));
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"PRIOR_VAR"));
        assert!(!names.contains(&"POST_VAR"));
        assert!(!names.contains(&"post_fn"));
    }

    #[test]
    fn test_extract_local_symbols_loop_variables_and_read() {
        let text = r#"
for item in foo bar baz; do
    echo $item
done
for k v in key1 val1; do
    :
done
select choice in apple orange; do
    :
done
read -r single_var
while IFS= read -r line_a line_b; do
    :
done
"#;
        let syms = extract_local_symbols_from_text(text, Position::new(15, 0));
        let var_names: Vec<&str> = syms
            .iter()
            .filter(|s| s.kind == LocalSymbolKind::Variable)
            .map(|s| s.name.as_str())
            .collect();
        assert!(var_names.contains(&"item"));
        assert!(var_names.contains(&"k"));
        assert!(var_names.contains(&"v"));
        assert!(var_names.contains(&"choice"));
        assert!(var_names.contains(&"single_var"));
        assert!(var_names.contains(&"line_a"));
        assert!(var_names.contains(&"line_b"));
    }

    #[test]
    fn test_extract_local_symbols_c_style_for_loop() {
        let text = r#"
for (( idx = 0, jdx = 10; idx < jdx; idx++ )); do
    echo $idx
done
"#;
        let syms = extract_local_symbols_from_text(text, Position::new(4, 0));
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"idx"));
        assert!(names.contains(&"jdx"));
    }

    #[test]
    fn test_extract_local_symbols_declaration_keywords() {
        let text = r#"
local LOCAL_VAR="loc"
typeset -a ARRAY_VAR=(1 2 3)
export EXPORTED_VAR="exp"
declare -i INT_VAR=42
readonly RO_VAR="ro"
"#;
        let syms = extract_local_symbols_from_text(text, Position::new(6, 0));
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"LOCAL_VAR"));
        assert!(names.contains(&"ARRAY_VAR"));
        assert!(names.contains(&"EXPORTED_VAR"));
        assert!(names.contains(&"INT_VAR"));
        assert!(names.contains(&"RO_VAR"));
    }

    #[test]
    fn test_extract_local_symbols_multibyte_and_comments() {
        let text = r#"
# COMMENTED_VAR="invisible"
# commented_fn() { : }
🍣_VAR="sushi"
こんにちは_func() {
    echo "hello"
}
# trailing comment
"#;
        let syms = extract_local_symbols_from_text(text, Position::new(8, 0));
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(!names.contains(&"COMMENTED_VAR"));
        assert!(!names.contains(&"commented_fn"));
        assert!(names.contains(&"🍣_VAR"));
        assert!(names.contains(&"こんにちは_func"));
    }

    #[test]
    fn test_extract_local_symbols_deduplication() {
        let text = r#"
VAR="first"
VAR="second"
my_fn() { : }
my_fn() { : }
"#;
        let syms = extract_local_symbols_from_text(text, Position::new(5, 0));
        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].name, "VAR");
        assert_eq!(syms[0].kind, LocalSymbolKind::Variable);
        assert_eq!(syms[1].name, "my_fn");
        assert_eq!(syms[1].kind, LocalSymbolKind::Function);
    }

    #[test]
    fn test_extract_local_symbols_heredoc_excluded() {
        let text = r#"
REAL_VAR="yes"
cat <<EOF
HEREDOC_VAR="no"
heredoc_func() { :; }
EOF
REAL_FUNC() { :; }
"#;
        let syms = extract_local_symbols_from_text(text, Position::new(8, 0));
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"REAL_VAR"));
        assert!(names.contains(&"REAL_FUNC"));
        assert!(!names.contains(&"HEREDOC_VAR"));
        assert!(!names.contains(&"heredoc_func"));
    }

    #[test]
    fn test_extract_local_symbols_multiple_assignments_single_line() {
        let text = "VAR1=one VAR2=two VAR3=three";
        let syms = extract_local_symbols_from_text(text, Position::new(1, 0));
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"VAR1"));
        assert!(names.contains(&"VAR2"));
        assert!(names.contains(&"VAR3"));
    }

    #[test]
    fn test_extract_local_symbols_command_arguments_not_extracted() {
        let text = "curl --header=foo http://localhost";
        let syms = extract_local_symbols_from_text(text, Position::new(1, 0));
        assert!(syms.is_empty());
    }

    #[test]
    fn test_extract_local_symbols_heredoc_not_triggered_in_comments_or_strings() {
        let text = r#"
# Example: cat <<EOF
MY_COMMENT_PRESERVED_VAR="hello"
echo "Here is <<EOF in a string"
MY_STRING_PRESERVED_VAR="world"
my_preserved_func() { :; }
"#;
        let syms = extract_local_symbols_from_text(text, Position::new(7, 0));
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"MY_COMMENT_PRESERVED_VAR"),
            "Variable after comment containing <<EOF must be extracted"
        );
        assert!(
            names.contains(&"MY_STRING_PRESERVED_VAR"),
            "Variable after string containing <<EOF must be extracted"
        );
        assert!(
            names.contains(&"my_preserved_func"),
            "Function after string containing <<EOF must be extracted"
        );
    }

    #[test]
    fn test_extract_local_symbols_heredoc_with_hyphen_delimiter() {
        let text = r#"
cat <<EOF-DATA
HEREDOC_VAR="inside"
EOF-DATA
OUTSIDE_VAR="outside"
"#;
        let syms = extract_local_symbols_from_text(text, Position::new(6, 0));
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"OUTSIDE_VAR"));
        assert!(
            !names.contains(&"HEREDOC_VAR"),
            "Variable inside <<EOF-DATA must not be extracted"
        );
    }

    #[test]
    fn test_extract_local_symbols_c_style_for_loop_comma_separated_no_spaces() {
        let text = r#"
for ((i=0,j=0; i<10; i++)); do
    :
done
"#;
        let syms = extract_local_symbols_from_text(text, Position::new(4, 0));
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"i"));
        assert!(
            names.contains(&"j"),
            "Second loop variable j in comma-separated clause must be extracted"
        );
    }

    #[test]
    fn test_extract_local_symbols_multiline_backslash_variable_declarations() {
        let text = r#"
local \
    VAR_A \
    VAR_B=10 \
    VAR_C

export \
    EXPORTED_ONE=foo \
    EXPORTED_TWO

typeset -r \
    CONST_X=1 \
    CONST_Y=2
"#;
        let syms = extract_local_symbols_from_text(text, Position::new(14, 0));
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"VAR_A"));
        assert!(names.contains(&"VAR_B"));
        assert!(names.contains(&"VAR_C"));
        assert!(names.contains(&"EXPORTED_ONE"));
        assert!(names.contains(&"EXPORTED_TWO"));
        assert!(names.contains(&"CONST_X"));
        assert!(names.contains(&"CONST_Y"));
    }

    #[test]
    fn test_extract_local_symbols_multiline_backslash_functions() {
        let text = r#"
my_posix_func() \
{
    echo hi
}

function my_ksh_func \
{
    echo hi
}

function \
    my_split_ksh_func {
    echo hi
}
"#;
        let syms = extract_local_symbols_from_text(text, Position::new(16, 0));
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"my_posix_func"));
        assert!(names.contains(&"my_ksh_func"));
        assert!(names.contains(&"my_split_ksh_func"));
    }

    #[test]
    fn test_extract_local_symbols_c_style_for_loop_multiline() {
        let text = r#"
for (( i=0, j=10; \
       i<10; \
       i++, j-- )); do
    :
done
"#;
        let syms = extract_local_symbols_from_text(text, Position::new(6, 0));
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"i"));
        assert!(names.contains(&"j"));
    }

    #[test]
    fn test_extract_local_symbols_utf16_surrogate_pairs() {
        // Line with surrogate pairs (🚀 is 4 bytes UTF-8, 2 UTF-16 code units)
        let text = "MSG=\"🚀\"; BEFORE_VAR=1; AFTER_VAR=2";
        // Position right after BEFORE_VAR=1;
        // In UTF-16: MSG=" (5) + 🚀 (2) + ";  (3) + BEFORE_VAR=1; (13) = 23
        let syms_before = extract_local_symbols_from_text(text, Position::new(0, 23));
        let names_before: Vec<&str> = syms_before.iter().map(|s| s.name.as_str()).collect();
        assert!(names_before.contains(&"MSG"));
        assert!(names_before.contains(&"BEFORE_VAR"));
        assert!(!names_before.contains(&"AFTER_VAR"));

        // Position at the end of line
        let syms_all = extract_local_symbols_from_text(text, Position::new(0, 50));
        let names_all: Vec<&str> = syms_all.iter().map(|s| s.name.as_str()).collect();
        assert!(names_all.contains(&"AFTER_VAR"));
    }

    #[test]
    fn test_extract_local_symbols_multiline_read() {
        let text = r#"
read -r \
    READ_VAR1 \
    READ_VAR2

while IFS= read -r \
    STREAM_LINE; do
    :
done
"#;
        let syms = extract_local_symbols_from_text(text, Position::new(10, 0));
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"READ_VAR1"));
        assert!(names.contains(&"READ_VAR2"));
        assert!(names.contains(&"STREAM_LINE"));
    }

    #[test]
    fn test_extract_local_symbols_multiline_semicolon_isolation() {
        let text = r#"
local VAR_A=1; echo \
    not_a_variable

local VAR_B=2; \
echo also_not_a_variable
"#;
        let syms = extract_local_symbols_from_text(text, Position::new(7, 0));
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"VAR_A"));
        assert!(names.contains(&"VAR_B"));
        assert!(
            !names.contains(&"not_a_variable"),
            "Argument to echo must not be extracted as local variable"
        );
        assert!(
            !names.contains(&"also_not_a_variable"),
            "Command after terminated local must not be extracted as local variable"
        );
    }

    #[test]
    fn test_extract_local_symbols_multiline_with_intermediate_comments() {
        let text = r#"
local \
    VAR_ONE \
    # Explanatory comment line \
    VAR_TWO
"#;
        let syms = extract_local_symbols_from_text(text, Position::new(6, 0));
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"VAR_ONE"));
        assert!(names.contains(&"VAR_TWO"));
    }

    #[rstest]
    #[case("", true)]
    #[case("   ", true)]
    #[case("echo foo; ", true)]
    #[case("echo foo | ", true)]
    #[case("echo foo && ", true)]
    #[case("echo foo || ", true)]
    #[case("echo $( ", true)]
    #[case("if true; then ", true)]
    #[case("for x in 1 2; do ", true)]
    #[case("if true; then :; else ", true)]
    // Negative cases: words ending in "do", "then", "else" must NOT trigger command position
    #[case("echo todo ", false)]
    #[case("echo pseudo ", false)]
    #[case("cat /path/to/something_else ", false)]
    #[case("grep lengthen ", false)]
    fn test_is_command_position_no_false_positives(#[case] prefix: &str, #[case] expected: bool) {
        assert_eq!(is_command_position(prefix), expected);
    }

    #[rstest]
    #[case("echo $MY_", "$MY_")]
    #[case("echo ${MY_", "${MY_")]
    #[case("echo \"$MY_", "$MY_")]
    #[case("echo \"${MY_", "${MY_")]
    #[case("FOO=$MY_", "$MY_")]
    #[case("FOO=${MY_", "${MY_")]
    #[case("PATH=$PATH:$MY_", "$MY_")]
    #[case("echo /path/to/$MY_", "$MY_")]
    #[case("echo -Dprop=$MY_", "$MY_")]
    #[case("arr[$MY_", "$MY_")]
    #[case("echo foo,$MY_", "$MY_")]
    #[case("FOO=$", "$")]
    #[case("FOO=${", "${")]
    #[case("echo ${#MY_", "${#MY_")]
    #[case("echo /path/to/my_", "")]
    #[case("./my_", "")]
    #[case("my_func", "my_func")]
    #[case("cat file.txt | my_", "my_")]
    #[case("foo; my_", "my_")]
    #[case("echo $(my_", "my_")]
    #[case("echo ", "")]
    #[case("# echo $MY_", "")]
    #[case("echo '$MY_", "")]
    fn test_extract_completion_token(#[case] prefix: &str, #[case] expected: &str) {
        assert_eq!(extract_completion_token(prefix), expected);
    }

    #[test]
    fn test_filter_local_symbols_sort_text_priority() {
        let syms = vec![
            LocalSymbol::new("my_var", LocalSymbolKind::Variable),
            LocalSymbol::new("my_func", LocalSymbolKind::Function),
        ];
        let var_items = filter_local_symbols(&syms, "echo $my_");
        assert_eq!(var_items.len(), 1);
        assert_eq!(var_items[0].sort_text.as_deref(), Some("00_my_var"));

        let func_items = filter_local_symbols(&syms, "my_");
        assert_eq!(func_items.len(), 1);
        assert_eq!(func_items[0].sort_text.as_deref(), Some("00_my_func"));
    }

    #[test]
    fn test_filter_local_symbols_variable_context() {
        let symbols = vec![
            LocalSymbol::new("MY_VAR", LocalSymbolKind::Variable),
            LocalSymbol::new("OTHER_VAR", LocalSymbolKind::Variable),
            LocalSymbol::new("my_func", LocalSymbolKind::Function),
        ];

        // 1. $MY_ matches only MY_VAR
        let res = filter_local_symbols(&symbols, "echo $MY_");
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].label, "MY_VAR");
        assert_eq!(res[0].kind, Some(CompletionItemKind::VARIABLE));
        assert_eq!(res[0].detail.as_deref(), Some("(local variable)"));

        // 2. ${MY_ matches only MY_VAR
        let res_braced = filter_local_symbols(&symbols, "echo ${MY_");
        assert_eq!(res_braced.len(), 1);
        assert_eq!(res_braced[0].label, "MY_VAR");

        // 3. $ matches all variables, no functions
        let res_all_vars = filter_local_symbols(&symbols, "echo $");
        let labels: Vec<&str> = res_all_vars.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["MY_VAR", "OTHER_VAR"]);
    }

    #[test]
    fn test_filter_local_symbols_function_context() {
        let symbols = vec![
            LocalSymbol::new("my_func", LocalSymbolKind::Function),
            LocalSymbol::new("other_func", LocalSymbolKind::Function),
            LocalSymbol::new("my_var", LocalSymbolKind::Variable),
        ];

        // 1. my_ matches only my_func
        let res = filter_local_symbols(&symbols, "my_");
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].label, "my_func");
        assert_eq!(res[0].kind, Some(CompletionItemKind::FUNCTION));
        assert_eq!(res[0].detail.as_deref(), Some("(local function)"));

        // 2. Pipe command position
        let res_pipe = filter_local_symbols(&symbols, "echo foo | other_");
        assert_eq!(res_pipe.len(), 1);
        assert_eq!(res_pipe[0].label, "other_func");

        // 3. Normal word input after space that is not command position
        let res_arg = filter_local_symbols(&symbols, "echo ");
        assert!(res_arg.is_empty());
    }

    #[test]
    fn test_merge_local_completions_deduplication() {
        let symbols = vec![
            LocalSymbol::new("LOCAL_VAR", LocalSymbolKind::Variable),
            LocalSymbol::new("SHARED_VAR", LocalSymbolKind::Variable),
            LocalSymbol::new("my_func", LocalSymbolKind::Function),
        ];

        let daemon_items = vec![
            CompletionItem {
                label: "$SHARED_VAR".to_string(),
                kind: Some(CompletionItemKind::VARIABLE),
                detail: Some("global var".to_string()),
                ..Default::default()
            },
            CompletionItem {
                label: "$DAEMON_VAR".to_string(),
                kind: Some(CompletionItemKind::VARIABLE),
                detail: Some("global var".to_string()),
                ..Default::default()
            },
        ];

        // Complete $
        let merged = merge_local_completions(daemon_items, &symbols, "echo $");

        // LOCAL_VAR and SHARED_VAR should come from local items
        // $SHARED_VAR from daemon should be omitted (deduplicated)
        // $DAEMON_VAR should remain
        let labels: Vec<&str> = merged.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["LOCAL_VAR", "SHARED_VAR", "$DAEMON_VAR"]);
        assert_eq!(merged[1].detail.as_deref(), Some("(local variable)"));
    }

    #[test]
    fn test_merge_local_completions_daemon_internal_duplicates() {
        let daemon_items = vec![
            CompletionItem {
                label: "export".to_string(),
                kind: Some(CompletionItemKind::TEXT),
                detail: None,
                ..Default::default()
            },
            CompletionItem {
                label: "export".to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some("shell builtin".to_string()),
                ..Default::default()
            },
            CompletionItem {
                label: "local".to_string(),
                kind: Some(CompletionItemKind::TEXT),
                detail: None,
                ..Default::default()
            },
            CompletionItem {
                label: "local".to_string(),
                kind: Some(CompletionItemKind::TEXT),
                detail: None,
                ..Default::default()
            },
        ];

        // 1. With empty local symbols
        let merged_empty = merge_local_completions(daemon_items.clone(), &[], "exp");
        assert_eq!(merged_empty.len(), 2);
        assert_eq!(merged_empty[0].label, "export");
        assert_eq!(merged_empty[0].detail.as_deref(), Some("shell builtin"));
        assert_eq!(merged_empty[0].kind, Some(CompletionItemKind::FUNCTION));
        assert_eq!(merged_empty[1].label, "local");

        // 2. With local symbol present
        let local_symbols = vec![LocalSymbol::new("custom_cmd", LocalSymbolKind::Function)];
        let merged_local = merge_local_completions(daemon_items, &local_symbols, "");
        assert_eq!(merged_local.len(), 3);
        assert_eq!(merged_local[0].label, "custom_cmd");
        assert_eq!(merged_local[1].label, "export");
        assert_eq!(merged_local[2].label, "local");
    }

    #[test]
    fn test_deduplicate_completion_items_empty_and_single() {
        let mut empty: Vec<CompletionItem> = Vec::new();
        deduplicate_completion_items(&mut empty);
        assert!(empty.is_empty());

        let mut single = vec![CompletionItem {
            label: "single".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        }];
        deduplicate_completion_items(&mut single);
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].label, "single");
    }

    #[test]
    fn test_deduplicate_completion_items_no_duplicates() {
        let mut items = vec![
            CompletionItem {
                label: "alpha".to_string(),
                ..Default::default()
            },
            CompletionItem {
                label: "beta".to_string(),
                ..Default::default()
            },
            CompletionItem {
                label: "gamma".to_string(),
                ..Default::default()
            },
        ];
        deduplicate_completion_items(&mut items);
        assert_eq!(items.len(), 3);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn test_deduplicate_completion_items_preserves_order() {
        let mut items = vec![
            CompletionItem {
                label: "export".to_string(),
                ..Default::default()
            },
            CompletionItem {
                label: "expand".to_string(),
                ..Default::default()
            },
            CompletionItem {
                label: "export".to_string(),
                ..Default::default()
            },
            CompletionItem {
                label: "expr".to_string(),
                ..Default::default()
            },
            CompletionItem {
                label: "expand".to_string(),
                ..Default::default()
            },
        ];
        deduplicate_completion_items(&mut items);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["export", "expand", "expr"]);
    }

    #[test]
    fn test_deduplicate_completion_items_detail_enrichment() {
        // First occurrence has no detail and generic kind
        // Second occurrence has detail and specific kind
        let mut items = vec![
            CompletionItem {
                label: "export".to_string(),
                kind: Some(CompletionItemKind::TEXT),
                detail: None,
                ..Default::default()
            },
            CompletionItem {
                label: "export".to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some("shell builtin".to_string()),
                ..Default::default()
            },
        ];
        deduplicate_completion_items(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "export");
        assert_eq!(items[0].detail.as_deref(), Some("shell builtin"));
        assert_eq!(items[0].kind, Some(CompletionItemKind::FUNCTION));
    }

    #[test]
    fn test_deduplicate_completion_items_detail_preservation() {
        // First occurrence already has detail; second occurrence does not
        let mut items = vec![
            CompletionItem {
                label: "export".to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: Some("reserved word".to_string()),
                ..Default::default()
            },
            CompletionItem {
                label: "export".to_string(),
                kind: Some(CompletionItemKind::TEXT),
                detail: None,
                ..Default::default()
            },
        ];
        deduplicate_completion_items(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "export");
        assert_eq!(items[0].detail.as_deref(), Some("reserved word"));
        assert_eq!(items[0].kind, Some(CompletionItemKind::KEYWORD));
    }

    #[test]
    fn test_deduplicate_completion_items_first_detail_wins_on_conflict() {
        let mut items = vec![
            CompletionItem {
                label: "cmd".to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some("first description".to_string()),
                ..Default::default()
            },
            CompletionItem {
                label: "cmd".to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: Some("second description".to_string()),
                ..Default::default()
            },
        ];
        deduplicate_completion_items(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "cmd");
        assert_eq!(items[0].detail.as_deref(), Some("first description"));
        assert_eq!(items[0].kind, Some(CompletionItemKind::FUNCTION));
    }

    #[test]
    fn test_deduplicate_completion_items_enrich_when_kind_is_none() {
        let mut items = vec![
            CompletionItem {
                label: "export".to_string(),
                kind: None,
                detail: None,
                ..Default::default()
            },
            CompletionItem {
                label: "export".to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some("shell builtin".to_string()),
                ..Default::default()
            },
        ];
        deduplicate_completion_items(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, Some(CompletionItemKind::FUNCTION));
        assert_eq!(items[0].detail.as_deref(), Some("shell builtin"));
    }

    #[test]
    fn test_deduplicate_completion_items_do_not_downgrade_kind_to_none() {
        let mut items = vec![
            CompletionItem {
                label: "export".to_string(),
                kind: Some(CompletionItemKind::TEXT),
                detail: None,
                ..Default::default()
            },
            CompletionItem {
                label: "export".to_string(),
                kind: None,
                detail: Some("shell builtin".to_string()),
                ..Default::default()
            },
        ];
        deduplicate_completion_items(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, Some(CompletionItemKind::TEXT));
        assert_eq!(items[0].detail.as_deref(), Some("shell builtin"));
    }

    #[test]
    fn test_deduplicate_completion_items_enrich_kind_without_detail() {
        let mut items = vec![
            CompletionItem {
                label: "export".to_string(),
                kind: Some(CompletionItemKind::TEXT),
                detail: None,
                ..Default::default()
            },
            CompletionItem {
                label: "export".to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: None,
                ..Default::default()
            },
        ];
        deduplicate_completion_items(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, Some(CompletionItemKind::KEYWORD));
    }

    #[test]
    fn test_deduplicate_completion_items_documentation_and_insert_text_enrichment() {
        let mut items = vec![
            CompletionItem {
                label: "func".to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                ..Default::default()
            },
            CompletionItem {
                label: "func".to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                documentation: Some(Documentation::String("Documentation for func".to_string())),
                insert_text: Some("func()".to_string()),
                sort_text: Some("10_func".to_string()),
                ..Default::default()
            },
        ];
        deduplicate_completion_items(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].documentation,
            Some(Documentation::String("Documentation for func".to_string()))
        );
        assert_eq!(items[0].insert_text.as_deref(), Some("func()"));
        assert_eq!(items[0].sort_text.as_deref(), Some("10_func"));
    }

    #[test]
    fn test_determine_completion_context() {
        assert_eq!(
            determine_completion_context("echo $VAR"),
            (CompletionContext::Variable, "$VAR")
        );
        assert_eq!(
            determine_completion_context("echo \"hello $VAR"),
            (CompletionContext::Variable, "$VAR")
        );
        assert_eq!(
            determine_completion_context("echo \"hello my_"),
            (CompletionContext::None, "")
        );
        assert_eq!(
            determine_completion_context("echo 'hello $VAR"),
            (CompletionContext::None, "")
        );
        assert_eq!(
            determine_completion_context("echo $((VAR"),
            (CompletionContext::Arithmetic, "VAR")
        );
        assert_eq!(
            determine_completion_context("echo $((1 + VAR"),
            (CompletionContext::Arithmetic, "VAR")
        );
        assert_eq!(
            determine_completion_context("((VAR"),
            (CompletionContext::Arithmetic, "VAR")
        );
        assert_eq!(
            determine_completion_context("echo \"$(my_"),
            (CompletionContext::CommandOrWord, "my_")
        );
        assert_eq!(
            determine_completion_context("echo `my_"),
            (CompletionContext::CommandOrWord, "my_")
        );
        assert_eq!(
            determine_completion_context("echo $(my_"),
            (CompletionContext::CommandOrWord, "my_")
        );
    }

    #[test]
    fn test_filter_local_symbols_arithmetic_context() {
        let syms = vec![
            LocalSymbol::new("COUNT", LocalSymbolKind::Variable),
            LocalSymbol::new("TOTAL", LocalSymbolKind::Variable),
            LocalSymbol::new("calculate", LocalSymbolKind::Function),
        ];

        // 1. Inside $(( ... )) without $
        let res = filter_local_symbols(&syms, "echo $((COU");
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].label, "COUNT");
        assert_eq!(res[0].kind, Some(CompletionItemKind::VARIABLE));

        // 2. Inside $(( 1 + ... )) without $
        let res_expr = filter_local_symbols(&syms, "echo $((1 + TOT");
        assert_eq!(res_expr.len(), 1);
        assert_eq!(res_expr[0].label, "TOTAL");

        // 3. Inside (( ... ))
        let res_cmd = filter_local_symbols(&syms, "((COU");
        assert_eq!(res_cmd.len(), 1);
        assert_eq!(res_cmd[0].label, "COUNT");

        // 4. Inside $(( ... )) after space suggests all variables, NO functions
        let res_space = filter_local_symbols(&syms, "echo $(( ");
        let labels: Vec<&str> = res_space.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["COUNT", "TOTAL"]);
    }

    #[test]
    fn test_filter_local_symbols_double_quote_no_function_false_positives() {
        let syms = vec![
            LocalSymbol::new("my_func", LocalSymbolKind::Function),
            LocalSymbol::new("MY_VAR", LocalSymbolKind::Variable),
        ];

        // Normal double-quoted string: function should NOT be suggested
        let res_str = filter_local_symbols(&syms, "echo \"hello my_");
        assert!(
            res_str.is_empty(),
            "Function must not be suggested in string literal"
        );

        // Normal double-quoted string: variable SHOULD be suggested
        let res_var = filter_local_symbols(&syms, "echo \"hello $MY_");
        assert_eq!(res_var.len(), 1);
        assert_eq!(res_var[0].label, "MY_VAR");
    }

    #[test]
    fn test_filter_local_symbols_subshell_and_backtick() {
        let syms = vec![
            LocalSymbol::new("my_func", LocalSymbolKind::Function),
            LocalSymbol::new("MY_VAR", LocalSymbolKind::Variable),
        ];

        // Subshell inside double quotes: function SHOULD be suggested
        let res_subshell = filter_local_symbols(&syms, "echo \"$(my_");
        assert_eq!(res_subshell.len(), 1);
        assert_eq!(res_subshell[0].label, "my_func");

        // Backtick inside double quotes: function SHOULD be suggested
        let res_backtick = filter_local_symbols(&syms, "echo \"`my_");
        assert_eq!(res_backtick.len(), 1);
        assert_eq!(res_backtick[0].label, "my_func");
    }

    #[test]
    fn test_filter_local_symbols_assignment_and_redirection_no_functions() {
        let syms = vec![
            LocalSymbol::new("my_func", LocalSymbolKind::Function),
            LocalSymbol::new("MY_VAR", LocalSymbolKind::Variable),
        ];

        // 1. Direct assignment RHS: functions must not be suggested
        let res_assign = filter_local_symbols(&syms, "TARGET=my_");
        assert!(
            res_assign.is_empty(),
            "Function must not be suggested as assignment value"
        );

        // 2. Redirection target: functions must not be suggested
        let res_redir_out = filter_local_symbols(&syms, "cat > my_");
        assert!(
            res_redir_out.is_empty(),
            "Function must not be suggested as output redirect target"
        );
        let res_redir_in = filter_local_symbols(&syms, "cat < my_");
        assert!(
            res_redir_in.is_empty(),
            "Function must not be suggested as input redirect target"
        );

        // 3. Colon path: functions must not be suggested
        let res_colon = filter_local_symbols(&syms, "export PATH=/bin:my_");
        assert!(
            res_colon.is_empty(),
            "Function must not be suggested in colon separated path"
        );
    }

    #[rstest]
    #[case("MY_VAR", "MY_VAR", true)]
    #[case("$MY_VAR", "MY_VAR", true)]
    #[case("${MY_VAR}", "MY_VAR", true)]
    #[case("${MY_VAR", "MY_VAR", true)]
    #[case("MY_VAR:description", "MY_VAR", true)]
    #[case("$MY_VAR:description", "MY_VAR", true)]
    #[case("${MY_VAR}:description", "MY_VAR", true)]
    #[case("${#MY_VAR}", "MY_VAR", true)]
    #[case("${#MY_VAR", "MY_VAR", true)]
    #[case("${#MY_VAR}:length", "MY_VAR", true)]
    #[case("OTHER_VAR", "MY_VAR", false)]
    fn test_daemon_label_matches_local_variants(
        #[case] label: &str,
        #[case] name: &str,
        #[case] expected: bool,
    ) {
        assert_eq!(daemon_label_matches_local(label, name), expected);
    }

    #[test]
    fn test_filter_local_symbols_arithmetic_nested_subshell() {
        let syms = vec![
            LocalSymbol::new("my_calc_func", LocalSymbolKind::Function),
            LocalSymbol::new("MY_VAR", LocalSymbolKind::Variable),
        ];

        // Inside subshell within arithmetic expansion: function SHOULD be suggested
        let res_subshell = filter_local_symbols(&syms, "echo $(( 1 + $(my_calc_");
        assert_eq!(res_subshell.len(), 1);
        assert_eq!(res_subshell[0].label, "my_calc_func");
        assert_eq!(res_subshell[0].kind, Some(CompletionItemKind::FUNCTION));
    }

    #[test]
    fn test_extract_local_symbols_aliases() {
        let text = concat!(
            "alias ll='ls -la'\n",
            "alias -g G='| grep'\n",
            "alias -s log=less\n",
            "alias a1='echo 1' a2=\"echo 2\"\n",
            "alias \\\n",
            "  cont_alias='pwd'\n",
            "alias query_only\n",
            "alias future_alias='true'\n",
        );

        let syms = extract_local_symbols_from_text(text, Position::new(6, 16));
        let alias_names: Vec<&str> = syms
            .iter()
            .filter(|s| s.kind == LocalSymbolKind::Alias)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(
            alias_names,
            vec!["ll", "G", "log", "a1", "a2", "cont_alias"]
        );

        // Cursor before alias name end on line 0 ("alias ll='ls -la'")
        let syms_early = extract_local_symbols_from_text(text, Position::new(0, 7));
        assert!(
            !syms_early.iter().any(|s| s.name == "ll"),
            "Alias should not be extracted before cursor passes its name"
        );
    }

    #[test]
    fn test_filter_and_merge_local_aliases() {
        let syms = vec![
            LocalSymbol::new("ll", LocalSymbolKind::Alias),
            LocalSymbol::new("my_func", LocalSymbolKind::Function),
            LocalSymbol::new("MY_VAR", LocalSymbolKind::Variable),
        ];

        // 1. CommandOrWord suggests Alias and Function
        let cmd_items = filter_local_symbols(&syms, "l");
        assert_eq!(cmd_items.len(), 1);
        assert_eq!(cmd_items[0].label, "ll");
        assert_eq!(cmd_items[0].kind, Some(CompletionItemKind::FUNCTION));
        assert_eq!(cmd_items[0].detail.as_deref(), Some("(local alias)"));

        // 2. Variable / Arithmetic contexts do NOT suggest Alias
        assert!(filter_local_symbols(&syms, "echo $l").is_empty());
        assert!(filter_local_symbols(&syms, "echo $((l").is_empty());

        // 3. merge_local_completions deduplicates against daemon candidates
        let daemon_items = vec![
            CompletionItem {
                label: "ll".to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some("alias for ls -la".to_string()),
                ..Default::default()
            },
            CompletionItem {
                label: "ls".to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                ..Default::default()
            },
        ];
        let merged = merge_local_completions(daemon_items, &syms, "l");
        let labels: Vec<&str> = merged.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["ll", "ls"]);
        assert_eq!(merged[0].detail.as_deref(), Some("(local alias)"));
    }

    #[test]
    fn test_extract_local_symbols_includes_sourced_files_and_handles_cycles() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a_path = dir.path().join("helper_a.zsh");
        let b_path = dir.path().join("helper_b.zsh");
        let after_path = dir.path().join("after.zsh");

        std::fs::write(
            &a_path,
            concat!(
                "source ./helper_b.zsh\n",
                "a_func() { : }\n",
                "A_VAR=1\n",
                "alias a_alias='echo a'\n",
            ),
        )
        .unwrap();
        std::fs::write(
            &b_path,
            concat!(
                ". \"./helper_a.zsh\"\n",
                "function b_func { : }\n",
                "export B_VAR=2\n",
                "alias b_alias='echo b'\n",
            ),
        )
        .unwrap();
        std::fs::write(&after_path, "after_func() { : }\n").unwrap();

        let main_path = dir.path().join("main.zsh");
        let main_uri = tower_lsp::lsp_types::Url::from_file_path(&main_path).unwrap();
        let main_text = concat!(
            "source ./helper_a.zsh\n",
            "local_func() { : }\n",
            "echo $A_VAR\n",
            "source ./after.zsh\n",
        );
        let doc = DocumentState::new(main_uri, 1, main_text.to_string());

        let syms = extract_local_symbols(&doc, Position::new(2, 11));
        let names: Vec<(&str, LocalSymbolKind)> =
            syms.iter().map(|s| (s.name.as_str(), s.kind)).collect();

        assert!(names.contains(&("local_func", LocalSymbolKind::Function)));
        assert!(names.contains(&("a_func", LocalSymbolKind::Function)));
        assert!(names.contains(&("A_VAR", LocalSymbolKind::Variable)));
        assert!(names.contains(&("a_alias", LocalSymbolKind::Alias)));
        assert!(names.contains(&("b_func", LocalSymbolKind::Function)));
        assert!(names.contains(&("B_VAR", LocalSymbolKind::Variable)));
        assert!(names.contains(&("b_alias", LocalSymbolKind::Alias)));
        assert!(
            !names.iter().any(|(n, _)| *n == "after_func"),
            "Symbols from files sourced after cursor must not be included: {names:?}"
        );
    }

    #[test]
    fn test_extract_local_symbols_respects_function_local_variable_scoping() {
        let text = concat!(
            "GLOBAL_VAR=1\n",
            "fn_a() {\n",
            "  local LOCAL_A=10\n",
            "  LOCAL_A=20\n",
            "  typeset -g GLOBAL_FROM_A=30\n",
            "  export EXPORT_FROM_A=40\n",
            "  echo $\n",
            "}\n",
            "fn_b() {\n",
            "  local LOCAL_B=50\n",
            "  echo $\n",
            "}\n",
            "echo $\n",
        );

        let syms_in_a = extract_local_symbols_from_text(text, Position::new(6, 8));
        let names_in_a: Vec<&str> = syms_in_a.iter().map(|s| s.name.as_str()).collect();
        assert!(names_in_a.contains(&"LOCAL_A"));
        assert!(names_in_a.contains(&"GLOBAL_VAR"));
        assert!(names_in_a.contains(&"GLOBAL_FROM_A"));
        assert!(names_in_a.contains(&"EXPORT_FROM_A"));

        let syms_in_b = extract_local_symbols_from_text(text, Position::new(10, 8));
        let names_in_b: Vec<&str> = syms_in_b.iter().map(|s| s.name.as_str()).collect();
        assert!(names_in_b.contains(&"LOCAL_B"));
        assert!(names_in_b.contains(&"GLOBAL_VAR"));
        assert!(names_in_b.contains(&"GLOBAL_FROM_A"));
        assert!(names_in_b.contains(&"EXPORT_FROM_A"));
        assert!(
            !names_in_b.contains(&"LOCAL_A"),
            "LOCAL_A from fn_a must not leak into fn_b: {names_in_b:?}"
        );

        let syms_top = extract_local_symbols_from_text(text, Position::new(12, 6));
        let names_top: Vec<&str> = syms_top.iter().map(|s| s.name.as_str()).collect();
        assert!(names_top.contains(&"GLOBAL_VAR"));
        assert!(names_top.contains(&"GLOBAL_FROM_A"));
        assert!(names_top.contains(&"EXPORT_FROM_A"));
        assert!(
            !names_top.contains(&"LOCAL_A"),
            "LOCAL_A from fn_a must not leak to top-level: {names_top:?}"
        );
        assert!(
            !names_top.contains(&"LOCAL_B"),
            "LOCAL_B from fn_b must not leak to top-level: {names_top:?}"
        );
    }

    #[rstest]
    #[case(r#"echo \$MY_"#, CompletionContext::Variable)]
    #[case("echo $$", CompletionContext::CommandOrWord)]
    #[case(r#"echo "hello \" $MY_"#, CompletionContext::Variable)]
    #[case("echo 'a' $MY_", CompletionContext::Variable)]
    #[case("$((1 + 2)) cmd", CompletionContext::CommandOrWord)]
    fn test_determine_completion_context_escaping_and_quotes(
        #[case] prefix: &str,
        #[case] expected: CompletionContext,
    ) {
        let (ctx, _) = determine_completion_context(prefix);
        assert_eq!(ctx, expected);
    }
}

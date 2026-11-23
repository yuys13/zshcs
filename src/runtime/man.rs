use std::time::Duration;
use tower_lsp::lsp_types::CompletionItemKind;

/// Default timeout for asynchronous man page retrieval (5000 milliseconds).
pub const DEFAULT_HOVER_MAN_TIMEOUT: Duration = Duration::from_millis(5000);

/// Default timeout for asynchronous man page retrieval during completion resolution (2000 milliseconds).
pub const DEFAULT_RESOLVE_MAN_TIMEOUT: Duration = Duration::from_millis(2000);

pub type ManCache = dashmap::DashMap<String, Option<String>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManPageResult {
    Found(String),
    NotFound,
    Timeout,
    Error(String),
}

/// Cleans raw manual page output by removing backspace overstrikes and ANSI escapes.
pub fn clean_man_text(raw: &str) -> String {
    let mut cleaned = String::with_capacity(raw.len());

    for line in raw.lines() {
        // Strip ANSI escape sequences first
        let mut chars = Vec::with_capacity(line.len());
        let line_chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < line_chars.len() {
            if line_chars[i] == '\x1b' && i + 1 < line_chars.len() && line_chars[i + 1] == '[' {
                i += 2;
                while i < line_chars.len() && !line_chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                if i < line_chars.len() {
                    i += 1;
                }
                continue;
            }
            chars.push(line_chars[i]);
            i += 1;
        }

        // Apply backspace and overstrike resolution (col -b emulation)
        let mut line_buf: Vec<char> = Vec::with_capacity(chars.len());
        let mut col: usize = 0;

        for c in chars {
            if c == '\x08' {
                col = col.saturating_sub(1);
            } else if c == '\t' {
                let tab_stop = (col / 8 + 1) * 8;
                while col < tab_stop {
                    if col < line_buf.len() {
                        line_buf[col] = ' ';
                    } else {
                        line_buf.push(' ');
                    }
                    col += 1;
                }
            } else if c == '\r' || c == '\x0c' {
                col = 0;
            } else {
                let norm_c = match c {
                    '\u{2212}' => '-', // Unicode minus sign
                    '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' => '-',
                    '\u{00a0}' => ' ', // Non-breaking space
                    _ => c,
                };

                if col < line_buf.len() {
                    if line_buf[col] == '_' && norm_c != '_' {
                        line_buf[col] = norm_c;
                    } else if line_buf[col] != '_' && norm_c == '_' {
                        // preserve existing non-underscore character
                    } else {
                        line_buf[col] = norm_c;
                    }
                } else {
                    while line_buf.len() < col {
                        line_buf.push(' ');
                    }
                    line_buf.push(norm_c);
                }
                col += 1;
            }
        }

        let line_string: String = line_buf.into_iter().collect();
        cleaned.push_str(line_string.trim_end());
        cleaned.push('\n');
    }

    cleaned.trim_end().to_string()
}

/// Formats raw manual page text into a Markdown code block with dynamic fence escaping.
pub fn format_man_markdown(man_text: &str) -> String {
    let mut fence_len = 3;
    while man_text.contains(&"`".repeat(fence_len)) {
        fence_len += 1;
    }
    let fence = "`".repeat(fence_len);
    format!("{fence}text\n{man_text}\n{fence}")
}

/// Retrieves the manual page for an external command asynchronously with a timeout, returning a detailed result.
pub async fn get_man_page_result(word: &str, timeout_dur: Duration) -> ManPageResult {
    get_man_page_result_with_command(word, timeout_dur, "man", &["-P", "cat"]).await
}

pub(crate) async fn get_man_page_result_with_command(
    word: &str,
    timeout_dur: Duration,
    program: &str,
    args: &[&str],
) -> ManPageResult {
    let target = if let Some(slash_idx) = word.rfind('/') {
        &word[slash_idx + 1..]
    } else {
        word
    };

    if target.is_empty() || target.len() > 256 || target.starts_with('-') {
        return ManPageResult::NotFound;
    }

    // Only allow alphanumeric and safe command characters
    if !target
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
    {
        return ManPageResult::NotFound;
    }

    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .arg(target)
        .stdin(std::process::Stdio::null())
        .env("MANPAGER", "cat")
        .env("PAGER", "cat")
        .env("TERM", "dumb")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    if let Ok(manpath) = std::env::var("MANPATH") {
        cmd.env("MANPATH", manpath);
    }

    let child_output = tokio::time::timeout(timeout_dur, cmd.output()).await;

    match child_output {
        Ok(Ok(output)) if output.status.success() => {
            let stdout_str = String::from_utf8_lossy(&output.stdout);
            let cleaned = clean_man_text(&stdout_str);
            if cleaned.trim().is_empty() {
                ManPageResult::NotFound
            } else {
                ManPageResult::Found(cleaned)
            }
        }
        Ok(Ok(output)) => {
            tracing::debug!(
                word = %target,
                status = ?output.status.code(),
                stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                "man command exited with non-zero status"
            );
            ManPageResult::NotFound
        }
        Ok(Err(e)) => {
            tracing::debug!(word = %target, error = %e, "Failed to execute man command");
            ManPageResult::Error(e.to_string())
        }
        Err(_) => {
            tracing::debug!(word = %target, "man command timed out");
            ManPageResult::Timeout
        }
    }
}

/// Retrieves the manual page for an external command asynchronously with a timeout.
pub async fn get_man_page(word: &str, timeout_dur: Duration) -> Option<String> {
    match get_man_page_result(word, timeout_dur).await {
        ManPageResult::Found(page) => Some(page),
        _ => None,
    }
}

/// Determines whether a completion item or word is eligible for external command man page lookup.
pub fn is_eligible_for_external_command(kind: Option<CompletionItemKind>, label: &str) -> bool {
    if let Some(k) = kind
        && !matches!(k, CompletionItemKind::FUNCTION | CompletionItemKind::TEXT)
    {
        return false;
    }
    let trimmed = label.trim();
    if trimmed.is_empty()
        || trimmed.len() > 256
        || trimmed.starts_with('-')
        || trimmed.starts_with('.')
        || trimmed.starts_with(':')
        || trimmed.ends_with('.')
        || trimmed.ends_with(':')
        || !trimmed.chars().any(|c| c.is_alphanumeric())
    {
        return false;
    }
    trimmed
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

#[derive(Clone, Debug)]
pub(crate) enum SingleFlightResult {
    Found(String),
    NotFound,
    Timeout,
    Error,
}

static IN_FLIGHT: std::sync::LazyLock<
    dashmap::DashMap<String, tokio::sync::watch::Receiver<Option<SingleFlightResult>>>,
> = std::sync::LazyLock::new(dashmap::DashMap::new);

struct InFlightGuard {
    key: String,
    active: bool,
}

impl InFlightGuard {
    fn new(key: String) -> Self {
        Self { key, active: true }
    }

    fn complete(mut self) {
        self.active = false;
        IN_FLIGHT.remove(&self.key);
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if self.active {
            IN_FLIGHT.remove(&self.key);
        }
    }
}

#[cfg(test)]
pub(crate) fn is_in_flight(key: &str) -> bool {
    IN_FLIGHT.contains_key(key)
}

fn apply_single_flight_markdown_result(
    result: &SingleFlightResult,
    cache: &ManCache,
    trimmed_label: &str,
) -> Option<String> {
    match result {
        SingleFlightResult::Found(md) => {
            cache.insert(trimmed_label.to_string(), Some(md.clone()));
            Some(md.clone())
        }
        SingleFlightResult::NotFound => {
            cache.insert(trimmed_label.to_string(), None);
            None
        }
        SingleFlightResult::Timeout | SingleFlightResult::Error => {
            // Do not cache transient timeouts or execution errors so future attempts can retry
            None
        }
    }
}

/// Resolves formatted Markdown documentation for an external command using `ManCache`,
/// single-flight request coalescing, negative caching, and timeout protection.
pub async fn resolve_man_markdown_with_fetcher<F, Fut>(
    trimmed_label: &str,
    cache: &ManCache,
    timeout_dur: Duration,
    fetcher: F,
) -> Option<String>
where
    F: FnOnce(String, Duration) -> Fut,
    Fut: std::future::Future<Output = ManPageResult>,
{
    // 1. Fast path: check cache
    if let Some(entry) = cache.get(trimmed_label) {
        return entry.value().clone();
    }

    // 2. Single-Flight request deduplication
    enum FlightRole {
        Leader(
            InFlightGuard,
            tokio::sync::watch::Sender<Option<SingleFlightResult>>,
        ),
        Waiter(tokio::sync::watch::Receiver<Option<SingleFlightResult>>),
    }

    let role = match IN_FLIGHT.entry(trimmed_label.to_string()) {
        dashmap::mapref::entry::Entry::Occupied(entry) => FlightRole::Waiter(entry.get().clone()),
        dashmap::mapref::entry::Entry::Vacant(entry) => {
            // Double-check cache in case a previous in-flight request completed just before locking
            if let Some(entry_cache) = cache.get(trimmed_label) {
                return entry_cache.value().clone();
            }
            let (tx, rx) = tokio::sync::watch::channel(None);
            entry.insert(rx);
            let guard = InFlightGuard::new(trimmed_label.to_string());
            FlightRole::Leader(guard, tx)
        }
    };

    match role {
        FlightRole::Leader(guard, tx) => {
            match fetcher(trimmed_label.to_string(), timeout_dur).await {
                ManPageResult::Found(raw_man) => {
                    let md = format_man_markdown(&raw_man);
                    cache.insert(trimmed_label.to_string(), Some(md.clone()));
                    let _ = tx.send(Some(SingleFlightResult::Found(md.clone())));
                    guard.complete();
                    Some(md)
                }
                ManPageResult::NotFound => {
                    cache.insert(trimmed_label.to_string(), None);
                    let _ = tx.send(Some(SingleFlightResult::NotFound));
                    guard.complete();
                    None
                }
                ManPageResult::Timeout => {
                    let _ = tx.send(Some(SingleFlightResult::Timeout));
                    guard.complete();
                    None
                }
                ManPageResult::Error(_) => {
                    let _ = tx.send(Some(SingleFlightResult::Error));
                    guard.complete();
                    None
                }
            }
        }
        FlightRole::Waiter(mut rx) => {
            // Check if the result was already published before subscription
            if let Some(ref res) = *rx.borrow() {
                return apply_single_flight_markdown_result(res, cache, trimmed_label);
            }

            match tokio::time::timeout(timeout_dur, rx.changed()).await {
                Ok(Ok(())) => {
                    if let Some(ref res) = *rx.borrow() {
                        return apply_single_flight_markdown_result(res, cache, trimmed_label);
                    }
                }
                Ok(Err(_)) => {
                    // Leader disconnected or dropped without a new value
                    if let Some(ref res) = *rx.borrow() {
                        return apply_single_flight_markdown_result(res, cache, trimmed_label);
                    }
                    if let Some(entry) = cache.get(trimmed_label) {
                        return entry.value().clone();
                    }
                }
                Err(_) => {
                    // Waiter timed out waiting for the in-flight resolution
                }
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::completion::{
        resolve_completion_item_async_with_fetcher, resolve_completion_item_async_with_timeout,
    };
    use rstest::rstest;
    use tower_lsp::lsp_types::{CompletionItem, Documentation};

    #[test]
    fn test_clean_man_text_overstrikes_and_escapes() {
        let raw_bold = "N\x08NA\x08AM\x08ME\x08E";
        assert_eq!(clean_man_text(raw_bold), "NAME");

        let raw_triple = "c\x08c\x08c";
        assert_eq!(clean_man_text(raw_triple), "c");

        let raw_underline = "_\x08f_\x08o_\x08o";
        assert_eq!(clean_man_text(raw_underline), "foo");

        let raw_under_bold = "_\x08c\x08c";
        assert_eq!(clean_man_text(raw_under_bold), "c");

        let raw_ansi = "\x1b[1mhello\x1b[0m \x1b[4mworld\x1b[0m";
        assert_eq!(clean_man_text(raw_ansi), "hello world");

        let raw_unicode = "grep \u{2212}v pattern";
        assert_eq!(clean_man_text(raw_unicode), "grep -v pattern");

        // Truncated ANSI escape at end of input, NBSP, and Unicode dashes
        assert_eq!(clean_man_text("abc\x1b[1"), "abc");
        assert_eq!(
            clean_man_text("foo\u{00a0}bar\u{2010}a\u{2011}b\u{2012}c\u{2013}d"),
            "foo bar-a-b-c-d"
        );
        // Underscore overstrike boundary
        assert_eq!(clean_man_text("a\x08_ _\x08b"), "a b");

        let raw_plain = "Simple line\nSecond line\n";
        assert_eq!(clean_man_text(raw_plain), "Simple line\nSecond line");
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
    async fn test_resolve_completion_item_async_single_flight_deduplication() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::sync::Barrier;

        let cache = Arc::new(ManCache::new());
        let call_count = Arc::new(AtomicUsize::new(0));
        let count = 20;
        let barrier = Arc::new(Barrier::new(count));

        let mut handles = Vec::new();
        for _ in 0..count {
            let cache_clone = Arc::clone(&cache);
            let barrier_clone = Arc::clone(&barrier);
            let count_clone = Arc::clone(&call_count);
            handles.push(tokio::spawn(async move {
                barrier_clone.wait().await;
                let item = CompletionItem {
                    label: "single_flight_cmd".to_string(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    ..Default::default()
                };
                resolve_completion_item_async_with_fetcher(
                    item,
                    &cache_clone,
                    Duration::from_millis(2000),
                    move |_cmd, _dur| {
                        let cnt = count_clone;
                        async move {
                            cnt.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(30)).await;
                            ManPageResult::Found(
                                "single_flight_cmd(1) - single flight documentation".to_string(),
                            )
                        }
                    },
                )
                .await
            }));
        }

        for handle in handles {
            let resolved = handle.await.unwrap();
            match resolved.documentation {
                Some(Documentation::MarkupContent(markup)) => {
                    assert!(markup.value.contains("single_flight_cmd"));
                }
                other => panic!("Expected MarkupContent, got {other:?}"),
            }
        }

        // Exactly one background fetch task must execute despite 20 concurrent requests
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
        assert!(cache.contains_key("single_flight_cmd"));
        assert!(!is_in_flight("single_flight_cmd"));
    }

    #[tokio::test]
    async fn test_resolve_completion_item_async_single_flight_not_found() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::sync::Barrier;

        let cache = Arc::new(ManCache::new());
        let call_count = Arc::new(AtomicUsize::new(0));
        let count = 10;
        let barrier = Arc::new(Barrier::new(count));

        let mut handles = Vec::new();
        for _ in 0..count {
            let cache_clone = Arc::clone(&cache);
            let barrier_clone = Arc::clone(&barrier);
            let count_clone = Arc::clone(&call_count);
            handles.push(tokio::spawn(async move {
                barrier_clone.wait().await;
                let item = CompletionItem {
                    label: "nonexistent_single_flight_cmd".to_string(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    ..Default::default()
                };
                resolve_completion_item_async_with_fetcher(
                    item,
                    &cache_clone,
                    Duration::from_millis(2000),
                    move |_cmd, _dur| {
                        let cnt = count_clone;
                        async move {
                            cnt.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            ManPageResult::NotFound
                        }
                    },
                )
                .await
            }));
        }

        for handle in handles {
            let resolved = handle.await.unwrap();
            assert_eq!(resolved.documentation, None);
        }

        assert_eq!(call_count.load(Ordering::SeqCst), 1);
        assert!(cache.contains_key("nonexistent_single_flight_cmd"));
        assert_eq!(*cache.get("nonexistent_single_flight_cmd").unwrap(), None);
        assert!(!is_in_flight("nonexistent_single_flight_cmd"));
    }

    #[tokio::test]
    async fn test_resolve_completion_item_async_single_flight_timeout_does_not_poison_cache() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::sync::Barrier;

        let cache = Arc::new(ManCache::new());
        let call_count = Arc::new(AtomicUsize::new(0));
        let count = 10;
        let barrier = Arc::new(Barrier::new(count));

        let mut handles = Vec::new();
        for _ in 0..count {
            let cache_clone = Arc::clone(&cache);
            let barrier_clone = Arc::clone(&barrier);
            let count_clone = Arc::clone(&call_count);
            handles.push(tokio::spawn(async move {
                barrier_clone.wait().await;
                let item = CompletionItem {
                    label: "timeout_single_flight_cmd".to_string(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    ..Default::default()
                };
                resolve_completion_item_async_with_fetcher(
                    item,
                    &cache_clone,
                    Duration::from_millis(2000),
                    move |_cmd, _dur| {
                        let cnt = count_clone;
                        async move {
                            cnt.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            ManPageResult::Timeout
                        }
                    },
                )
                .await
            }));
        }

        for handle in handles {
            let resolved = handle.await.unwrap();
            assert_eq!(resolved.documentation, None);
        }

        assert_eq!(call_count.load(Ordering::SeqCst), 1);
        assert!(!cache.contains_key("timeout_single_flight_cmd"));
        assert!(!is_in_flight("timeout_single_flight_cmd"));
    }

    #[tokio::test]
    async fn test_resolve_completion_item_async_timeout_protection() {
        let cache = ManCache::new();
        let item = CompletionItem {
            label: "timeout_protection_isolated_cmd".to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        };

        // Pass 0 duration timeout
        let resolved =
            resolve_completion_item_async_with_timeout(item, &cache, Duration::from_millis(0))
                .await;
        // Zero timeout aborts immediately without caching transient timeouts, returns unmodified item
        assert_eq!(resolved.documentation, None);
        assert!(!cache.contains_key("timeout_protection_isolated_cmd"));
        assert!(!is_in_flight("timeout_protection_isolated_cmd"));
    }

    #[rstest]
    // Valid commands
    #[case(Some(CompletionItemKind::FUNCTION), "git", true)]
    #[case(Some(CompletionItemKind::TEXT), "cargo", true)]
    #[case(None, "grep", true)]
    #[case(Some(CompletionItemKind::FUNCTION), "python3.11", true)]
    #[case(Some(CompletionItemKind::FUNCTION), "git-commit", true)]
    #[case(Some(CompletionItemKind::FUNCTION), "7z", true)]
    #[case(Some(CompletionItemKind::FUNCTION), "my_cmd_1", true)]
    // Ineligible labels: flags, dots, colons, punctuation-only, path slashes
    #[case(Some(CompletionItemKind::FUNCTION), "-v", false)]
    #[case(Some(CompletionItemKind::FUNCTION), "--help", false)]
    #[case(Some(CompletionItemKind::FUNCTION), ".", false)]
    #[case(Some(CompletionItemKind::FUNCTION), "..", false)]
    #[case(Some(CompletionItemKind::FUNCTION), "...", false)]
    #[case(Some(CompletionItemKind::FUNCTION), ".gitignore", false)]
    #[case(Some(CompletionItemKind::FUNCTION), ".zshrc", false)]
    #[case(Some(CompletionItemKind::FUNCTION), "::", false)]
    #[case(Some(CompletionItemKind::FUNCTION), ":wq", false)]
    #[case(Some(CompletionItemKind::FUNCTION), "git.", false)]
    #[case(Some(CompletionItemKind::FUNCTION), "test:", false)]
    #[case(Some(CompletionItemKind::FUNCTION), "_", false)]
    #[case(Some(CompletionItemKind::FUNCTION), "__", false)]
    #[case(Some(CompletionItemKind::FUNCTION), "", false)]
    #[case(Some(CompletionItemKind::FUNCTION), "   ", false)]
    #[case(Some(CompletionItemKind::FUNCTION), "foo/bar", false)]
    #[case(Some(CompletionItemKind::FUNCTION), "/usr/bin/git", false)]
    // Ineligible kinds
    #[case(Some(CompletionItemKind::FILE), "git", false)]
    #[case(Some(CompletionItemKind::FOLDER), "git", false)]
    #[case(Some(CompletionItemKind::KEYWORD), "git", false)]
    #[case(Some(CompletionItemKind::VARIABLE), "git", false)]
    fn test_is_eligible_for_external_command_cases(
        #[case] kind: Option<CompletionItemKind>,
        #[case] label: &str,
        #[case] expected: bool,
    ) {
        assert_eq!(is_eligible_for_external_command(kind, label), expected);
    }

    #[tokio::test]
    async fn test_single_flight_leader_drop_releases_in_flight_guard() {
        use std::sync::Arc;
        use tokio::sync::Notify;

        let cache = Arc::new(ManCache::new());
        let label = "leader_drop_isolated_cmd";
        let started = Arc::new(Notify::new());
        let started_clone = Arc::clone(&started);
        let cache_clone = Arc::clone(&cache);

        let leader_handle = tokio::spawn(async move {
            let item = CompletionItem {
                label: label.to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                ..Default::default()
            };
            resolve_completion_item_async_with_fetcher(
                item,
                &cache_clone,
                Duration::from_secs(5),
                move |_cmd, _dur| async move {
                    started_clone.notify_one();
                    std::future::pending::<ManPageResult>().await
                },
            )
            .await
        });

        started.notified().await;
        assert!(is_in_flight(label));

        // Abort leader task so InFlightGuard::drop runs
        leader_handle.abort();
        let _ = leader_handle.await;
        assert!(!is_in_flight(label));

        // Subsequent request can become leader and succeed
        let item = CompletionItem {
            label: label.to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        };
        let resolved = resolve_completion_item_async_with_fetcher(
            item,
            &cache,
            Duration::from_secs(1),
            |_cmd, _dur| async { ManPageResult::Found("recovered docs".to_string()) },
        )
        .await;
        assert!(resolved.documentation.is_some());
    }

    #[tokio::test]
    async fn test_single_flight_waiter_timeout_and_error_does_not_poison_cache() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::sync::Notify;

        let cache = Arc::new(ManCache::new());
        let label = "waiter_timeout_error_isolated_cmd";
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let started_clone = Arc::clone(&started);
        let release_clone = Arc::clone(&release);
        let cache_clone = Arc::clone(&cache);

        // 1. Leader blocks until released, then returns Error
        let leader = tokio::spawn(async move {
            let item = CompletionItem {
                label: label.to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                ..Default::default()
            };
            resolve_completion_item_async_with_fetcher(
                item,
                &cache_clone,
                Duration::from_secs(5),
                move |_cmd, _dur| async move {
                    started_clone.notify_one();
                    release_clone.notified().await;
                    ManPageResult::Error("mock failure".to_string())
                },
            )
            .await
        });

        started.notified().await;

        // 2. Waiter times out immediately (Duration::ZERO) and returns None
        let waiter_item = CompletionItem {
            label: label.to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        };
        let waiter_res = resolve_completion_item_async_with_fetcher(
            waiter_item,
            &cache,
            Duration::ZERO,
            |_cmd, _dur| async { unreachable!("waiter should not run fetcher") },
        )
        .await;
        assert_eq!(waiter_res.documentation, None);

        // Release leader and confirm Error did not poison cache
        release.notify_one();
        let leader_res = leader.await.unwrap();
        assert_eq!(leader_res.documentation, None);
        assert!(!cache.contains_key(label));
        assert!(!is_in_flight(label));

        // 3. Retry succeeds
        let call_count = Arc::new(AtomicUsize::new(0));
        let cnt = Arc::clone(&call_count);
        let retry_item = CompletionItem {
            label: label.to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            ..Default::default()
        };
        let retry_res = resolve_completion_item_async_with_fetcher(
            retry_item,
            &cache,
            Duration::from_secs(1),
            move |_cmd, _dur| async move {
                cnt.fetch_add(1, Ordering::SeqCst);
                ManPageResult::Found("fresh man docs".to_string())
            },
        )
        .await;
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
        assert!(retry_res.documentation.is_some());
    }

    #[tokio::test]
    async fn test_get_man_page_result_timeout() {
        let res = get_man_page_result_with_command(
            "ls",
            Duration::from_millis(20),
            "zsh",
            &["-c", "sleep 10"],
        )
        .await;
        assert_eq!(res, ManPageResult::Timeout);
    }
}

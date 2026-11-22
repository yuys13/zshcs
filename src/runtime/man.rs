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

    let mut cmd = tokio::process::Command::new("man");
    cmd.arg("-P")
        .arg("cat")
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

    #[test]
    fn test_clean_man_text_overstrikes_and_escapes() {
        let raw_bold = "N\x08NA\x08AM\x08ME\x08E";
        assert_eq!(clean_man_text(raw_bold), "NAME");

        let raw_underline = "_\x08f_\x08o_\x08o";
        assert_eq!(clean_man_text(raw_underline), "foo");

        let raw_ansi = "\x1b[1mhello\x1b[0m \x1b[4mworld\x1b[0m";
        assert_eq!(clean_man_text(raw_ansi), "hello world");

        let raw_unicode = "grep \u{2212}v pattern";
        assert_eq!(clean_man_text(raw_unicode), "grep -v pattern");
    }

    #[test]
    fn test_format_man_markdown_escaping() {
        let plain = "hello world";
        assert_eq!(format_man_markdown(plain), "```text\nhello world\n```");

        let with_backticks = "```zsh\necho hi\n```";
        assert_eq!(
            format_man_markdown(with_backticks),
            "````text\n```zsh\necho hi\n```\n````"
        );
    }
}

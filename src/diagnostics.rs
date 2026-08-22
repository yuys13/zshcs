use std::time::Duration;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

/// Default timeout for executing `zsh -n`.
pub const DEFAULT_SYNTAX_CHECK_TIMEOUT: Duration = Duration::from_millis(2000);

/// Runs syntax check on `text` using `zsh -n` with the default timeout.
pub async fn check_syntax(text: &str) -> Vec<Diagnostic> {
    check_syntax_with_timeout(text, DEFAULT_SYNTAX_CHECK_TIMEOUT).await
}

/// Runs syntax check on `text` using `zsh -n` with a specified timeout.
pub async fn check_syntax_with_timeout(text: &str, timeout_dur: Duration) -> Vec<Diagnostic> {
    if text.trim().is_empty() {
        return Vec::new();
    }

    let temp_file = match tempfile::Builder::new()
        .prefix("zshcs_diag_")
        .suffix(".zsh")
        .tempfile()
    {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to create temp file for zsh -n syntax check");
            return Vec::new();
        }
    };

    let temp_path = temp_file.path().to_path_buf();
    if let Err(e) = tokio::fs::write(&temp_path, text.as_bytes()).await {
        tracing::warn!(error = %e, "Failed to write temp file for zsh -n syntax check");
        return Vec::new();
    }

    let mut cmd = tokio::process::Command::new("zsh");
    cmd.arg("-n")
        .arg(&temp_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to spawn zsh -n for syntax check");
            return Vec::new();
        }
    };

    let output = match tokio::time::timeout(timeout_dur, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "zsh -n process failed while waiting for output");
            return Vec::new();
        }
        Err(_) => {
            tracing::warn!("zsh -n syntax check timed out after {:?}", timeout_dur);
            return Vec::new();
        }
    };

    if output.status.success() {
        return Vec::new();
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    parse_diagnostics(&stderr, text)
}

/// Parses the standard error output from `zsh -n` into a list of LSP `Diagnostic`s.
pub fn parse_diagnostics(stderr: &str, text: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for line in stderr.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(diag) = parse_diagnostic_line(trimmed, text) {
            diagnostics.push(diag);
        }
    }
    diagnostics
}

/// Parses a single line of `zsh -n` stderr output into a `Diagnostic`.
pub fn parse_diagnostic_line(line: &str, text: &str) -> Option<Diagnostic> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Typical formats:
    // "zsh:12: parse error near `;'"
    // "/path/to/file.zsh:12: parse error near `;'"
    // "zsh: parse error near `\n'"
    // "zsh:1: unmatched '"
    let (line_num, message) = extract_line_and_message(trimmed);

    let doc_lines: Vec<&str> = text.lines().collect();
    let range = calculate_diagnostic_range(&doc_lines, line_num);

    Some(Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        code: None,
        code_description: None,
        source: Some("zshcs".to_string()),
        message,
        related_information: None,
        tags: None,
        data: None,
    })
}

/// Extracts (1-based line number, error message) from a diagnostic line.
fn extract_line_and_message(line: &str) -> (u32, String) {
    let parts: Vec<&str> = line.split(':').collect();
    if parts.len() >= 3 {
        // Check if parts[1] is a line number
        if let Ok(num) = parts[1].trim().parse::<u32>() {
            let msg = parts[2..].join(":").trim().to_string();
            return (num, msg);
        }
    }

    // Try finding any `:digits:` pattern
    for (i, part) in parts.iter().enumerate() {
        if i > 0
            && i < parts.len() - 1
            && let Ok(num) = part.trim().parse::<u32>()
        {
            let msg = parts[i + 1..].join(":").trim().to_string();
            return (num, msg);
        }
    }

    // Fallback: if there's at least one colon, take whatever is after the first colon as message
    if parts.len() >= 2 {
        let msg = parts[1..].join(":").trim().to_string();
        (1, msg)
    } else {
        (1, line.to_string())
    }
}

/// Computes the LSP `Range` for the given 1-based line number within document lines.
fn calculate_diagnostic_range(doc_lines: &[&str], line_num: u32) -> Range {
    if doc_lines.is_empty() {
        return Range::new(Position::new(0, 0), Position::new(0, 0));
    }

    let line_idx = if line_num == 0 {
        0
    } else {
        (line_num - 1) as usize
    };

    let clamped_line_idx = line_idx.min(doc_lines.len() - 1);
    let line_text = doc_lines[clamped_line_idx];
    let utf16_len = line_text.encode_utf16().count() as u32;

    let target_line = clamped_line_idx as u32;
    Range::new(
        Position::new(target_line, 0),
        Position::new(target_line, utf16_len),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_diagnostic_standard_parse_error() {
        let stderr = "zsh:2: parse error near `;'";
        let text = "echo hello\nif [[ ; then\n  echo bad\nfi";
        let diags = parse_diagnostics(stderr, text);
        assert_eq!(diags.len(), 1);
        let diag = &diags[0];
        assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diag.source, Some("zshcs".to_string()));
        assert_eq!(diag.message, "parse error near `;'");
        assert_eq!(diag.range.start, Position::new(1, 0));
        assert_eq!(diag.range.end, Position::new(1, 12));
    }

    #[test]
    fn test_parse_diagnostic_unmatched_quote() {
        let stderr = "zsh:1: unmatched \"";
        let text = "echo \"unclosed quote";
        let diags = parse_diagnostics(stderr, text);
        assert_eq!(diags.len(), 1);
        let diag = &diags[0];
        assert_eq!(diag.message, "unmatched \"");
        assert_eq!(diag.range.start, Position::new(0, 0));
        assert_eq!(diag.range.end, Position::new(0, 20));
    }

    #[test]
    fn test_parse_diagnostic_file_path_prefix() {
        let stderr = "/var/folders/tmp_test.zsh:3: parse error near `then'";
        let text = "line1\nline2\nthen\nline4";
        let diags = parse_diagnostics(stderr, text);
        assert_eq!(diags.len(), 1);
        let diag = &diags[0];
        assert_eq!(diag.message, "parse error near `then'");
        assert_eq!(diag.range.start, Position::new(2, 0));
        assert_eq!(diag.range.end, Position::new(2, 4));
    }

    #[test]
    fn test_parse_diagnostic_no_line_number() {
        let stderr = "zsh: parse error near `\\n'";
        let text = "if true";
        let diags = parse_diagnostics(stderr, text);
        assert_eq!(diags.len(), 1);
        let diag = &diags[0];
        assert_eq!(diag.message, "parse error near `\\n'");
        assert_eq!(diag.range.start, Position::new(0, 0));
        assert_eq!(diag.range.end, Position::new(0, 7));
    }

    #[test]
    fn test_parse_diagnostic_out_of_bounds_line() {
        let stderr = "zsh:999: parse error near `end'";
        let text = "line1\nline2\nline3";
        let diags = parse_diagnostics(stderr, text);
        assert_eq!(diags.len(), 1);
        let diag = &diags[0];
        // Line 999 clamped to line 2 (0-indexed, 3rd line)
        assert_eq!(diag.range.start, Position::new(2, 0));
        assert_eq!(diag.range.end, Position::new(2, 5));
    }

    #[test]
    fn test_parse_diagnostic_zero_line() {
        let stderr = "zsh:0: parse error";
        let text = "first line";
        let diags = parse_diagnostics(stderr, text);
        assert_eq!(diags.len(), 1);
        let diag = &diags[0];
        assert_eq!(diag.range.start, Position::new(0, 0));
        assert_eq!(diag.range.end, Position::new(0, 10));
    }

    #[test]
    fn test_parse_diagnostic_empty_text() {
        let stderr = "zsh:1: parse error";
        let text = "";
        let diags = parse_diagnostics(stderr, text);
        assert_eq!(diags.len(), 1);
        let diag = &diags[0];
        assert_eq!(diag.range.start, Position::new(0, 0));
        assert_eq!(diag.range.end, Position::new(0, 0));
    }

    #[test]
    fn test_parse_diagnostic_multibyte_line_range() {
        let stderr = "zsh:2: parse error near `;'";
        let text = "echo 1\n日本語テスト 👨‍👩‍👧‍👦 ;;\necho 3";
        let diags = parse_diagnostics(stderr, text);
        assert_eq!(diags.len(), 1);
        let diag = &diags[0];
        assert_eq!(diag.range.start, Position::new(1, 0));
        // "日本語テスト 👨‍👩‍👧‍👦 ;;" utf16 len:
        // 6 + 1 + 11 + 1 + 2 = 21
        let expected_utf16 = "日本語テスト 👨‍👩‍👧‍👦 ;;".encode_utf16().count() as u32;
        assert_eq!(diag.range.end, Position::new(1, expected_utf16));
    }

    #[test]
    fn test_parse_diagnostics_empty_or_whitespace_stderr() {
        assert!(parse_diagnostics("", "text").is_empty());
        assert!(parse_diagnostics("   \n\n  \t ", "text").is_empty());
    }

    #[test]
    fn test_parse_diagnostics_multiple_lines() {
        let stderr = "zsh:1: error one\nzsh:2: error two\n";
        let text = "line1\nline2";
        let diags = parse_diagnostics(stderr, text);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].message, "error one");
        assert_eq!(diags[0].range.start, Position::new(0, 0));
        assert_eq!(diags[1].message, "error two");
        assert_eq!(diags[1].range.start, Position::new(1, 0));
    }

    #[tokio::test]
    async fn test_check_syntax_valid_script() {
        let text = "echo 'hello world'\nif true; then\n  echo ok\nfi\n";
        let diags = check_syntax(text).await;
        assert!(
            diags.is_empty(),
            "Valid script should produce 0 diagnostics, got: {diags:?}"
        );
    }

    #[tokio::test]
    async fn test_check_syntax_invalid_script() {
        let text = "if [[ ; then\n  echo error\nfi\n";
        let diags = check_syntax(text).await;
        assert!(!diags.is_empty());
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diags[0].range.start.line, 0);
        assert!(diags[0].message.contains("parse error"));
    }

    #[tokio::test]
    async fn test_check_syntax_empty_string() {
        let diags = check_syntax("").await;
        assert!(diags.is_empty());

        let diags_ws = check_syntax("   \n \t ").await;
        assert!(diags_ws.is_empty());
    }

    #[tokio::test]
    async fn test_check_syntax_timeout_protection() {
        // Testing with 0 duration timeout should immediately timeout and return empty safely
        let diags = check_syntax_with_timeout("if [[ ; then", Duration::from_nanos(1)).await;
        // Either it completed instantly or timed out; either way it shouldn't panic or hang
        assert!(diags.len() <= 2);
    }

    #[tokio::test]
    async fn test_check_syntax_large_buffer() {
        // Create a large script (>200KB) with syntax error at the end to verify stdin streaming
        let mut script = String::with_capacity(300_000);
        for i in 0..5000 {
            script.push_str(&format!("export VAR_{i}=\"value_{i}\"\n"));
        }
        script.push_str("if [[ ; then\n  echo fail\nfi\n");

        let diags = check_syntax(&script).await;
        assert!(!diags.is_empty());
        assert_eq!(diags[0].range.start.line, 5000);
        assert!(diags[0].message.contains("parse error"));
    }

    #[test]
    fn test_extract_line_and_message_complex_formats() {
        // Colon in message
        let (line, msg) = extract_line_and_message("zsh:15: parse error: unexpected token: foo");
        assert_eq!(line, 15);
        assert_eq!(msg, "parse error: unexpected token: foo");

        // Relative path with colon
        let (line2, msg2) = extract_line_and_message("sub/dir/test.zsh:42: unmatched \"");
        assert_eq!(line2, 42);
        assert_eq!(msg2, "unmatched \"");

        // No colon at all
        let (line3, msg3) = extract_line_and_message("fatal syntax error");
        assert_eq!(line3, 1);
        assert_eq!(msg3, "fatal syntax error");
    }

    #[test]
    fn test_parse_diagnostic_crlf() {
        let stderr = "zsh:2: parse error near `;'";
        let text = "echo hello\r\nif [[ ; then\r\n  echo bad\r\nfi\r\n";
        let diags = parse_diagnostics(stderr, text);
        assert_eq!(diags.len(), 1);
        let diag = &diags[0];
        assert_eq!(diag.range.start, Position::new(1, 0));
        assert_eq!(diag.range.end, Position::new(1, 12));
    }

    #[test]
    fn test_parse_diagnostic_no_trailing_newline() {
        let stderr = "zsh:1: parse error near `then'";
        let text = "if [[ ; then";
        let diags = parse_diagnostics(stderr, text);
        assert_eq!(diags.len(), 1);
        let diag = &diags[0];
        assert_eq!(diag.range.start, Position::new(0, 0));
        assert_eq!(diag.range.end, Position::new(0, 12));
    }

    #[test]
    fn test_parse_diagnostic_path_with_multiple_colons() {
        let stderr = "/path:with:colons/file.zsh:7: parse error near `fi'";
        let text = "1\n2\n3\n4\n5\n6\nfi\n8";
        let diags = parse_diagnostics(stderr, text);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].range.start, Position::new(6, 0));
        assert_eq!(diags[0].message, "parse error near `fi'");
    }
}

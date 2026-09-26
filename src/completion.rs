use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tower_lsp::Client;
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, Documentation, MarkupContent, MarkupKind, MessageType,
};

use crate::error::{ZshcsError, ZshcsResult};

pub const CAPTURE_ZSH: &str = include_str!("../bin/capture.zsh");
pub const ZPTYRC_ZSH: &str = include_str!("../bin/zptyrc.zsh");
pub const DAEMON_REQUEST_TIMEOUT: Duration = Duration::from_millis(5000);

pub struct CompletionRequest {
    pub prefix: String,
    pub cwd: Option<PathBuf>,
    pub responder: oneshot::Sender<ZshcsResult<Vec<CompletionItem>>>,
}

struct DaemonProcess {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout_reader: BufReader<tokio::process::ChildStdout>,
    current_cwd: Option<PathBuf>,
}

impl DaemonProcess {
    fn spawn(
        script_path: &Path,
        cache_dir: Option<&Path>,
        client: &Client,
    ) -> std::io::Result<Self> {
        tracing::info!(
            script = ?script_path,
            ?cache_dir,
            "Spawning completion daemon process"
        );

        let mut cmd = tokio::process::Command::new("zsh");
        cmd.arg(script_path);
        if let Some(dir) = cache_dir {
            std::fs::create_dir_all(dir)?;
            cmd.env("ZSHCS_CACHE_DIR", dir);
        }

        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        let stdin = child.stdin.take().ok_or_else(|| {
            tracing::error!("Failed to open stdin for completion daemon");
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Failed to open stdin")
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            tracing::error!("Failed to open stdout for completion daemon");
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Failed to open stdout")
        })?;
        let mut stderr = child.stderr.take().ok_or_else(|| {
            tracing::error!("Failed to open stderr for completion daemon");
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Failed to open stderr")
        })?;

        // Spawn stderr logger
        let client_for_stderr = client.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(&mut stderr);
            let mut line = String::new();
            while let Ok(len) = reader.read_line(&mut line).await {
                if len == 0 {
                    break;
                }
                let trimmed = line.trim_end();
                tracing::warn!(stderr = %trimmed, "capture.zsh stderr output");
                client_for_stderr
                    .log_message(
                        MessageType::WARNING,
                        format!("capture.zsh stderr: {trimmed}"),
                    )
                    .await;
                line.clear();
            }
        });

        let stdout_reader = BufReader::new(stdout);
        Ok(DaemonProcess {
            child,
            stdin,
            stdout_reader,
            current_cwd: None,
        })
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

pub async fn run_completion_daemon(
    script_path: PathBuf,
    cache_dir: Option<PathBuf>,
    mut rx: mpsc::Receiver<CompletionRequest>,
    client: Client,
) {
    tracing::info!("Starting completion daemon supervisor loop");
    let mut daemon: Option<DaemonProcess> = None;

    while let Some(req) = rx.recv().await {
        if req.responder.is_closed() {
            tracing::debug!(
                prefix = %req.prefix,
                "Completion request cancelled by client; discarding"
            );
            continue;
        }

        // Check if existing daemon process has terminated
        if let Some(proc) = daemon.as_mut()
            && !proc.is_alive()
        {
            tracing::warn!("Completion daemon process terminated, restarting...");
            client
                .log_message(
                    MessageType::WARNING,
                    "Completion daemon process terminated, restarting...",
                )
                .await;
            daemon = None;
        }

        // Spawn daemon if not currently running
        if daemon.is_none() {
            match DaemonProcess::spawn(&script_path, cache_dir.as_deref(), &client) {
                Ok(p) => {
                    tracing::info!("Completion daemon process spawned successfully");
                    daemon = Some(p);
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to spawn completion daemon");
                    client
                        .log_message(
                            MessageType::ERROR,
                            format!("Failed to spawn completion daemon: {e}"),
                        )
                        .await;
                    let _ = req.responder.send(Err(ZshcsError::Io(e)));
                    continue;
                }
            }
        }

        let Some(proc) = daemon.as_mut() else {
            continue;
        };

        // Execute request with timeout to protect supervisor against hung processes
        let exec_result = timeout(DAEMON_REQUEST_TIMEOUT, async {
            // Synchronize working directory if specified and changed
            if let Some(target_cwd) = &req.cwd {
                let need_chdir = match &proc.current_cwd {
                    Some(current) => current != target_cwd,
                    None => true,
                };
                if need_chdir {
                    tracing::debug!(?target_cwd, "Changing daemon working directory");
                    let sanitized_cwd = target_cwd.to_string_lossy().replace(['\r', '\n'], "");
                    let chdir_msg = format!("chdir:{sanitized_cwd}\n");
                    proc.stdin.write_all(chdir_msg.as_bytes()).await?;
                    proc.current_cwd = Some(target_cwd.clone());
                }
            }

            // Send input message to daemon (sanitizing newlines)
            let sanitized_prefix = req.prefix.replace(['\r', '\n'], "");
            tracing::trace!(prefix = %sanitized_prefix, "Sending input to completion daemon");
            let msg = format!("input:{sanitized_prefix}\n");
            proc.stdin.write_all(msg.as_bytes()).await?;

            // Read response until EOC
            let mut items = Vec::new();
            let mut line = String::new();

            loop {
                line.clear();
                let len = proc.stdout_reader.read_line(&mut line).await?;
                if len == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "Completion daemon stdout closed unexpectedly",
                    ));
                }

                let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
                tracing::trace!(line = %trimmed, "Received line from completion daemon stdout");
                if trimmed.ends_with("\x01EOC\x01") {
                    let content = trimmed.trim_end_matches("\x01EOC\x01");
                    if !content.is_empty() {
                        parse_candidate_line(content, &mut items);
                    }
                    break;
                }
                if !trimmed.is_empty() {
                    parse_candidate_line(trimmed, &mut items);
                }
            }

            tracing::debug!(
                item_count = items.len(),
                "Completed candidate stream parsing"
            );
            Ok(items)
        })
        .await;

        match exec_result {
            Ok(Ok(items)) => {
                let _ = req.responder.send(Ok(items));
            }
            Ok(Err(e)) => {
                tracing::error!(error = %e, "Completion daemon I/O failure; killing process");
                client
                    .log_message(
                        MessageType::ERROR,
                        format!("Completion daemon I/O failure: {e}"),
                    )
                    .await;
                if let Some(mut p) = daemon.take() {
                    let _ = p.child.start_kill();
                }
                let _ = req.responder.send(Err(ZshcsError::Io(e)));
            }
            Err(_) => {
                tracing::error!(
                    timeout_ms = DAEMON_REQUEST_TIMEOUT.as_millis(),
                    "Completion request timed out; terminating hung daemon"
                );
                client
                    .log_message(
                        MessageType::ERROR,
                        format!(
                            "Completion request timed out after {}ms, terminating hung daemon...",
                            DAEMON_REQUEST_TIMEOUT.as_millis()
                        ),
                    )
                    .await;
                if let Some(mut p) = daemon.take() {
                    let _ = p.child.start_kill();
                }
                let _ = req.responder.send(Err(ZshcsError::Daemon(
                    "Completion request timed out".to_string(),
                )));
            }
        }
    }
}

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

    if let Some(doc) = crate::hover::get_builtin_or_reserved_doc(trimmed_label) {
        item.documentation = Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: doc.to_string(),
        }));
    }

    item
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

    #[tokio::test]
    async fn test_completion_daemon_skips_cancelled_request() {
        let temp_dir = tempfile::tempdir().unwrap();
        let script_path = temp_dir.path().join("mock_daemon.zsh");
        std::fs::write(
            &script_path,
            "#!/bin/zsh\nwhile read -r line; do\n  if [[ \"$line\" == input:* ]]; then\n    echo \"success\\x01EOC\\x01\"\n  fi\ndone\n",
        )
        .unwrap();

        let (tx, rx) = mpsc::channel(16);
        let mut client_opt = None;
        let (_service, _socket) = tower_lsp::LspService::new(|client| {
            client_opt = Some(client.clone());
            crate::Backend::new(client).unwrap()
        });
        let client = client_opt.unwrap();

        let daemon_handle = tokio::spawn(run_completion_daemon(script_path, None, rx, client));

        // 1. Send cancelled request (drop receiver immediately)
        let (tx_resp1, rx_resp1) = oneshot::channel();
        drop(rx_resp1);
        tx.send(CompletionRequest {
            prefix: "cancelled".to_string(),
            cwd: None,
            responder: tx_resp1,
        })
        .await
        .unwrap();

        // 2. Send active request
        let (tx_resp2, rx_resp2) = oneshot::channel();
        tx.send(CompletionRequest {
            prefix: "active".to_string(),
            cwd: None,
            responder: tx_resp2,
        })
        .await
        .unwrap();

        let result = tokio::time::timeout(Duration::from_secs(3), rx_resp2)
            .await
            .expect("Did not timeout waiting for active request")
            .expect("Channel not closed")
            .expect("Daemon returned success");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].label, "success");

        drop(tx);
        let _ = daemon_handle.await;
    }

    #[test]
    fn test_parse_candidate_line_throughput() {
        let sample_line = "status\tshow working tree status";
        let iterations = 50_000;
        let mut items = Vec::with_capacity(1000);

        let start = std::time::Instant::now();
        for _ in 0..iterations {
            items.clear();
            parse_candidate_line(sample_line, &mut items);
        }
        let elapsed = start.elapsed();

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "status");
        assert_eq!(items[0].detail.as_deref(), Some("show working tree status"));
        assert_eq!(items[0].insert_text, None);
        assert!(elapsed.as_secs() < 5);
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
}

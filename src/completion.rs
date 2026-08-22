use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tower_lsp::Client;
use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind, MessageType};

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
        script_path: &PathBuf,
        cache_dir: Option<&PathBuf>,
        client: &Client,
    ) -> std::io::Result<Self> {
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
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Failed to open stdin")
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Failed to open stdout")
        })?;
        let mut stderr = child.stderr.take().ok_or_else(|| {
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
                client_for_stderr
                    .log_message(
                        MessageType::WARNING,
                        format!("capture.zsh stderr: {}", line.trim_end()),
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
    let mut daemon: Option<DaemonProcess> = None;

    while let Some(req) = rx.recv().await {
        if req.responder.is_closed() {
            continue;
        }

        // Check if existing daemon process has terminated
        if let Some(proc) = daemon.as_mut()
            && !proc.is_alive()
        {
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
            match DaemonProcess::spawn(&script_path, cache_dir.as_ref(), &client) {
                Ok(p) => {
                    daemon = Some(p);
                }
                Err(e) => {
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
                    let sanitized_cwd = target_cwd.to_string_lossy().replace(['\r', '\n'], "");
                    let chdir_msg = format!("chdir:{sanitized_cwd}\n");
                    proc.stdin.write_all(chdir_msg.as_bytes()).await?;
                    proc.current_cwd = Some(target_cwd.clone());
                }
            }

            // Send input message to daemon (sanitizing newlines)
            let sanitized_prefix = req.prefix.replace(['\r', '\n'], "");
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

            Ok(items)
        })
        .await;

        match exec_result {
            Ok(Ok(items)) => {
                let _ = req.responder.send(Ok(items));
            }
            Ok(Err(e)) => {
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

    items.push(CompletionItem {
        label,
        kind: Some(CompletionItemKind::TEXT),
        insert_text: None,
        detail,
        ..Default::default()
    });
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
        Some(CompletionItemKind::TEXT),
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
}

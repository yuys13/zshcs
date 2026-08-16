use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tower_lsp::Client;
use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind, MessageType};

pub const CAPTURE_ZSH: &str = include_str!("../bin/capture.zsh");
pub const ZPTYRC_ZSH: &str = include_str!("../bin/zptyrc.zsh");

pub struct CompletionRequest {
    pub prefix: String,
    pub responder: oneshot::Sender<Result<Vec<CompletionItem>, String>>,
}

pub async fn run_completion_daemon(
    script_path: PathBuf,
    mut rx: mpsc::Receiver<CompletionRequest>,
    client: Client,
) {
    let mut child = tokio::process::Command::new("zsh")
        .arg(&script_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn completion daemon");

    let mut stdin = child.stdin.take().expect("Failed to open stdin");
    let stdout = child.stdout.take().expect("Failed to open stdout");
    let mut stderr = child.stderr.take().expect("Failed to open stderr");

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

    let mut stdout_reader = BufReader::new(stdout);

    while let Some(req) = rx.recv().await {
        // Send input message to daemon
        let msg = format!("input:{}\n", req.prefix);
        if let Err(e) = stdin.write_all(msg.as_bytes()).await {
            let _ = req
                .responder
                .send(Err(format!("Failed to write to daemon: {}", e)));
            continue;
        }

        // Read response until EOC
        let mut items = Vec::new();
        let mut line = String::new();
        let mut error_msg = None;

        loop {
            line.clear();
            match stdout_reader.read_line(&mut line).await {
                Ok(0) => {
                    error_msg = Some("Daemon stdout closed unexpectedly".to_string());
                    break;
                }
                Ok(_) => {
                    let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
                    if trimmed.ends_with("\x01EOC\x01") {
                        // Trim EOC marker from the end of the line if it was appended to a candidate,
                        // otherwise it's on a line by itself.
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
                Err(e) => {
                    error_msg = Some(format!("Error reading daemon stdout: {}", e));
                    break;
                }
            }
        }

        if let Some(err) = error_msg {
            let _ = req.responder.send(Err(err));
        } else {
            let _ = req.responder.send(Ok(items));
        }
    }
}

pub fn parse_candidate_line(line: &str, items: &mut Vec<CompletionItem>) {
    // ddc-source-shell_native style outputs `candidate\tdescription`
    let parts: Vec<&str> = line.splitn(2, '\t').collect();
    let label = parts[0].to_string();
    let detail = if parts.len() > 1 && !parts[1].trim().is_empty() {
        Some(parts[1].to_string())
    } else {
        None
    };

    items.push(CompletionItem {
        label: label.clone(),
        kind: Some(CompletionItemKind::TEXT),
        insert_text: Some(label),
        detail,
        ..Default::default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_candidate_line() {
        let mut items = Vec::new();

        // 1. Only candidate without tab
        parse_candidate_line("git status", &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "git status");
        assert_eq!(items[0].detail, None);

        items.clear();

        // 2. Candidate with tab and description
        parse_candidate_line("status\tshow working tree status", &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "status");
        assert_eq!(items[0].detail.as_deref(), Some("show working tree status"));

        items.clear();

        // 3. Candidate with tab but empty or whitespace description
        parse_candidate_line("status\t   ", &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "status");
        assert_eq!(items[0].detail, None);

        items.clear();

        // 4. Multiple tabs in description
        parse_candidate_line("foo\tbar\tbaz", &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "foo");
        assert_eq!(items[0].detail.as_deref(), Some("bar\tbaz"));
    }

    #[test]
    fn test_parse_candidate_line_empty_and_whitespace() {
        let mut items = Vec::new();

        // 1. Empty string
        parse_candidate_line("", &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "");
        assert_eq!(items[0].detail, None);
        assert_eq!(items[0].insert_text.as_deref(), Some(""));
        assert_eq!(items[0].kind, Some(CompletionItemKind::TEXT));

        items.clear();

        // 2. Whitespace only
        parse_candidate_line("   ", &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "   ");
        assert_eq!(items[0].detail, None);

        items.clear();

        // 3. Single tab only
        parse_candidate_line("\t", &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "");
        assert_eq!(items[0].detail, None);

        items.clear();

        // 4. Tab with whitespace
        parse_candidate_line("\t   ", &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "");
        assert_eq!(items[0].detail, None);

        items.clear();

        parse_candidate_line("   \t   ", &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "   ");
        assert_eq!(items[0].detail, None);

        items.clear();

        // 5. Consecutive tabs only
        parse_candidate_line("\t\t", &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "");
        assert_eq!(items[0].detail, None);

        items.clear();

        parse_candidate_line("\t\t\t", &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "");
        assert_eq!(items[0].detail, None);

        items.clear();

        // 6. Empty label with description
        parse_candidate_line("\tdescription only", &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "");
        assert_eq!(items[0].detail.as_deref(), Some("description only"));
    }

    #[test]
    fn test_parse_candidate_line_leading_trailing_spaces() {
        let mut items = Vec::new();

        // Leading whitespace in label
        parse_candidate_line("  status\tdesc", &mut items);
        assert_eq!(items[0].label, "  status");
        assert_eq!(items[0].detail.as_deref(), Some("desc"));

        items.clear();

        // Trailing whitespace in label
        parse_candidate_line("status  \tdesc", &mut items);
        assert_eq!(items[0].label, "status  ");
        assert_eq!(items[0].detail.as_deref(), Some("desc"));

        items.clear();

        // Both leading and trailing in label
        parse_candidate_line("  cmd  \tdesc", &mut items);
        assert_eq!(items[0].label, "  cmd  ");
        assert_eq!(items[0].detail.as_deref(), Some("desc"));

        items.clear();

        // Preserving leading and trailing spaces in detail
        parse_candidate_line("status\t  detailed description  ", &mut items);
        assert_eq!(items[0].label, "status");
        assert_eq!(items[0].detail.as_deref(), Some("  detailed description  "));

        items.clear();

        // Trailing tab only
        parse_candidate_line("status\t", &mut items);
        assert_eq!(items[0].label, "status");
        assert_eq!(items[0].detail, None);

        items.clear();

        // Without tab, spaces preserved
        parse_candidate_line("  leading and trailing  ", &mut items);
        assert_eq!(items[0].label, "  leading and trailing  ");
        assert_eq!(items[0].detail, None);
    }

    #[test]
    fn test_parse_candidate_line_tab_delimiters() {
        let mut items = Vec::new();

        // Consecutive tabs with description
        parse_candidate_line("status\t\tdesc", &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "status");
        assert_eq!(items[0].detail.as_deref(), Some("\tdesc"));

        items.clear();

        // Multiple tabs in description
        parse_candidate_line("part1\tpart2\tpart3", &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "part1");
        assert_eq!(items[0].detail.as_deref(), Some("part2\tpart3"));

        items.clear();

        // Many tabs in description
        parse_candidate_line("cmd\topt1\topt2\topt3\topt4", &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "cmd");
        assert_eq!(items[0].detail.as_deref(), Some("opt1\topt2\topt3\topt4"));
    }

    #[test]
    fn test_parse_candidate_line_quotes_and_escapes() {
        let mut items = Vec::new();

        // Double and single quotes
        parse_candidate_line("\"double quoted\"\t'single quoted' desc", &mut items);
        assert_eq!(items[0].label, "\"double quoted\"");
        assert_eq!(items[0].detail.as_deref(), Some("'single quoted' desc"));

        items.clear();

        // Backslashes
        parse_candidate_line("path\\to\\file\tdesc\\path", &mut items);
        assert_eq!(items[0].label, "path\\to\\file");
        assert_eq!(items[0].detail.as_deref(), Some("desc\\path"));

        items.clear();

        // Escaped quotes
        parse_candidate_line("escaped\\\"quote\\\"\tdesc", &mut items);
        assert_eq!(items[0].label, "escaped\\\"quote\\\"");
        assert_eq!(items[0].detail.as_deref(), Some("desc"));

        items.clear();

        // Single quote label, double quote desc
        parse_candidate_line("'single'\t\"double\"", &mut items);
        assert_eq!(items[0].label, "'single'");
        assert_eq!(items[0].detail.as_deref(), Some("\"double\""));
    }

    #[test]
    fn test_parse_candidate_line_shell_metacharacters() {
        let mut items = Vec::new();

        // Environment variables and parameter expansions
        parse_candidate_line("$VAR\tenvironment variable", &mut items);
        assert_eq!(items[0].label, "$VAR");
        assert_eq!(items[0].detail.as_deref(), Some("environment variable"));

        items.clear();

        parse_candidate_line("${VAR:-default}\tparameter expansion", &mut items);
        assert_eq!(items[0].label, "${VAR:-default}");
        assert_eq!(items[0].detail.as_deref(), Some("parameter expansion"));

        items.clear();

        // Globs and braces
        parse_candidate_line("*.tar.gz\tglob pattern", &mut items);
        assert_eq!(items[0].label, "*.tar.gz");
        assert_eq!(items[0].detail.as_deref(), Some("glob pattern"));

        items.clear();

        parse_candidate_line("{a,b,c}\tbrace expansion", &mut items);
        assert_eq!(items[0].label, "{a,b,c}");
        assert_eq!(items[0].detail.as_deref(), Some("brace expansion"));

        items.clear();

        parse_candidate_line("[0-9]*.txt\trange glob", &mut items);
        assert_eq!(items[0].label, "[0-9]*.txt");
        assert_eq!(items[0].detail.as_deref(), Some("range glob"));

        items.clear();

        // Pipes, redirects, and background
        parse_candidate_line("cmd1 | cmd2\tpipeline", &mut items);
        assert_eq!(items[0].label, "cmd1 | cmd2");
        assert_eq!(items[0].detail.as_deref(), Some("pipeline"));

        items.clear();

        parse_candidate_line("cmd &\tbackground", &mut items);
        assert_eq!(items[0].label, "cmd &");
        assert_eq!(items[0].detail.as_deref(), Some("background"));

        items.clear();

        parse_candidate_line("cmd1; cmd2\tseparator", &mut items);
        assert_eq!(items[0].label, "cmd1; cmd2");
        assert_eq!(items[0].detail.as_deref(), Some("separator"));

        items.clear();

        parse_candidate_line("cmd1 && cmd2\tand operator", &mut items);
        assert_eq!(items[0].label, "cmd1 && cmd2");
        assert_eq!(items[0].detail.as_deref(), Some("and operator"));

        items.clear();

        parse_candidate_line(">out.log\tredirect stdout", &mut items);
        assert_eq!(items[0].label, ">out.log");
        assert_eq!(items[0].detail.as_deref(), Some("redirect stdout"));

        items.clear();

        parse_candidate_line("2>&1\tredirect stderr", &mut items);
        assert_eq!(items[0].label, "2>&1");
        assert_eq!(items[0].detail.as_deref(), Some("redirect stderr"));

        items.clear();

        // Subshells and process substitution
        parse_candidate_line("<(cmd)\tprocess substitution", &mut items);
        assert_eq!(items[0].label, "<(cmd)");
        assert_eq!(items[0].detail.as_deref(), Some("process substitution"));

        items.clear();

        parse_candidate_line("$(whoami)\tsubshell", &mut items);
        assert_eq!(items[0].label, "$(whoami)");
        assert_eq!(items[0].detail.as_deref(), Some("subshell"));

        items.clear();

        parse_candidate_line("`pwd`\tbacktick", &mut items);
        assert_eq!(items[0].label, "`pwd`");
        assert_eq!(items[0].detail.as_deref(), Some("backtick"));

        items.clear();

        // Flags and options
        parse_candidate_line("-o:fmt\tcolon flag", &mut items);
        assert_eq!(items[0].label, "-o:fmt");
        assert_eq!(items[0].detail.as_deref(), Some("colon flag"));

        items.clear();

        parse_candidate_line("--opt=val\tequals flag", &mut items);
        assert_eq!(items[0].label, "--opt=val");
        assert_eq!(items[0].detail.as_deref(), Some("equals flag"));

        items.clear();

        parse_candidate_line("~/.zshrc\ttilde path", &mut items);
        assert_eq!(items[0].label, "~/.zshrc");
        assert_eq!(items[0].detail.as_deref(), Some("tilde path"));
    }

    #[test]
    fn test_parse_candidate_line_ansi_escapes() {
        let mut items = Vec::new();

        // ANSI color in label
        parse_candidate_line("\x1b[32m--verbose\x1b[0m\tenable verbose", &mut items);
        assert_eq!(items[0].label, "\x1b[32m--verbose\x1b[0m");
        assert_eq!(items[0].detail.as_deref(), Some("enable verbose"));

        items.clear();

        // ANSI color in detail
        parse_candidate_line("--color\t\x1b[1mcolored description\x1b[0m", &mut items);
        assert_eq!(items[0].label, "--color");
        assert_eq!(
            items[0].detail.as_deref(),
            Some("\x1b[1mcolored description\x1b[0m")
        );

        items.clear();

        // Protocol control character literal
        parse_candidate_line("\x01EOC\x01\tmarker in label", &mut items);
        assert_eq!(items[0].label, "\x01EOC\x01");
        assert_eq!(items[0].detail.as_deref(), Some("marker in label"));
    }

    #[test]
    fn test_parse_candidate_line_multibyte_and_unicode() {
        let mut items = Vec::new();

        // CJK Japanese
        parse_candidate_line("コミット\t変更内容を記録する", &mut items);
        assert_eq!(items[0].label, "コミット");
        assert_eq!(items[0].detail.as_deref(), Some("変更内容を記録する"));

        items.clear();

        parse_candidate_line("設定ファイル.zsh\t設定の概要", &mut items);
        assert_eq!(items[0].label, "設定ファイル.zsh");
        assert_eq!(items[0].detail.as_deref(), Some("設定の概要"));

        items.clear();

        // CJK Chinese
        parse_candidate_line("分支\t显示分支列表", &mut items);
        assert_eq!(items[0].label, "分支");
        assert_eq!(items[0].detail.as_deref(), Some("显示分支列表"));

        items.clear();

        // CJK Korean
        parse_candidate_line("커밋\t커밋 생성", &mut items);
        assert_eq!(items[0].label, "커밋");
        assert_eq!(items[0].detail.as_deref(), Some("커밋 생성"));

        items.clear();

        // Emojis with ZWJ and skin tones
        parse_candidate_line("✨ feat\t新機能の追加", &mut items);
        assert_eq!(items[0].label, "✨ feat");
        assert_eq!(items[0].detail.as_deref(), Some("新機能の追加"));

        items.clear();

        parse_candidate_line("🚀 deploy\tデプロイ実行", &mut items);
        assert_eq!(items[0].label, "🚀 deploy");
        assert_eq!(items[0].detail.as_deref(), Some("デプロイ実行"));

        items.clear();

        parse_candidate_line("👨‍👩‍👧‍👦 family\t家族絵文字", &mut items);
        assert_eq!(items[0].label, "👨‍👩‍👧‍👦 family");
        assert_eq!(items[0].detail.as_deref(), Some("家族絵文字"));

        items.clear();

        parse_candidate_line("👍🏽 thumbs_up\tskin tone emoji", &mut items);
        assert_eq!(items[0].label, "👍🏽 thumbs_up");
        assert_eq!(items[0].detail.as_deref(), Some("skin tone emoji"));

        items.clear();

        // Accents
        parse_candidate_line("café\tFrench cafe", &mut items);
        assert_eq!(items[0].label, "café");
        assert_eq!(items[0].detail.as_deref(), Some("French cafe"));

        items.clear();

        parse_candidate_line("üñîçødé\tcombining and accents", &mut items);
        assert_eq!(items[0].label, "üñîçødé");
        assert_eq!(items[0].detail.as_deref(), Some("combining and accents"));

        items.clear();

        // RTL
        parse_candidate_line("مرحبا\tArabic greeting", &mut items);
        assert_eq!(items[0].label, "مرحبا");
        assert_eq!(items[0].detail.as_deref(), Some("Arabic greeting"));

        items.clear();

        parse_candidate_line("שלום\tHebrew greeting", &mut items);
        assert_eq!(items[0].label, "שלום");
        assert_eq!(items[0].detail.as_deref(), Some("Hebrew greeting"));
    }

    #[test]
    fn test_parse_candidate_line_extremely_long() {
        let mut items = Vec::new();
        let long_label = "a".repeat(10_000);
        let long_detail = "b".repeat(10_000);
        let line = format!("{}\t{}", long_label, long_detail);

        parse_candidate_line(&line, &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label.len(), 10_000);
        assert_eq!(items[0].label, long_label);
        assert_eq!(items[0].detail.as_deref(), Some(long_detail.as_str()));
    }

    #[test]
    fn test_parse_candidate_line_item_properties() {
        let mut items = Vec::new();
        parse_candidate_line("checkout\tswitch branch", &mut items);
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.label, "checkout");
        assert_eq!(item.kind, Some(CompletionItemKind::TEXT));
        assert_eq!(item.insert_text.as_deref(), Some("checkout"));
        assert_eq!(item.detail.as_deref(), Some("switch branch"));
    }

    #[test]
    fn test_parse_candidate_line_accumulation() {
        let mut items = Vec::new();
        parse_candidate_line("first\tdesc1", &mut items);
        parse_candidate_line("second\tdesc2", &mut items);
        parse_candidate_line("third", &mut items);

        assert_eq!(items.len(), 3);
        assert_eq!(items[0].label, "first");
        assert_eq!(items[0].detail.as_deref(), Some("desc1"));
        assert_eq!(items[1].label, "second");
        assert_eq!(items[1].detail.as_deref(), Some("desc2"));
        assert_eq!(items[2].label, "third");
        assert_eq!(items[2].detail, None);
    }
}

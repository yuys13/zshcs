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
    #[case("", "", None, Some(""), Some(CompletionItemKind::TEXT))]
    // 2. Whitespace only
    #[case("   ", "   ", None, Some("   "), Some(CompletionItemKind::TEXT))]
    // 3. Single tab only
    #[case("\t", "", None, Some(""), Some(CompletionItemKind::TEXT))]
    // 4. Tab with whitespace
    #[case("\t   ", "", None, Some(""), Some(CompletionItemKind::TEXT))]
    #[case("   \t   ", "   ", None, Some("   "), Some(CompletionItemKind::TEXT))]
    // 5. Consecutive tabs only
    #[case("\t\t", "", None, Some(""), Some(CompletionItemKind::TEXT))]
    #[case("\t\t\t", "", None, Some(""), Some(CompletionItemKind::TEXT))]
    // 6. Empty label with description
    #[case(
        "\tdescription only",
        "",
        Some("description only"),
        Some(""),
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
        Some("checkout"),
        Some("switch branch")
    )]
    #[case(
        "commit",
        "commit",
        Some(CompletionItemKind::TEXT),
        Some("commit"),
        None
    )]
    #[case(
        "--help\tshow help",
        "--help",
        Some(CompletionItemKind::TEXT),
        Some("--help"),
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
}

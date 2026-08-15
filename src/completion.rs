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
}

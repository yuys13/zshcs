use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tower_lsp::Client;
use tower_lsp::lsp_types::{CompletionItem, MessageType};

use crate::error::{ZshcsError, ZshcsResult};
use crate::lsp::completion::{deduplicate_completion_items, parse_candidate_line};

pub const CAPTURE_ZSH: &str = include_str!("../../bin/capture.zsh");
pub const ZPTYRC_ZSH: &str = include_str!("../../bin/zptyrc.zsh");
pub const DAEMON_REQUEST_TIMEOUT: Duration = Duration::from_millis(5000);

pub enum CompletionRequest {
    Complete {
        prefix: String,
        cwd: Option<PathBuf>,
        responder: oneshot::Sender<ZshcsResult<Vec<CompletionItem>>>,
    },
    Shutdown {
        responder: oneshot::Sender<()>,
    },
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

/// Sanitizes an input string (completion prefix or working directory path) before sending it
/// to the completion daemon over its line-based stdin protocol, stripping ASCII control characters
/// (`0x00..=0x1F`, `0x7F`) such as `\0`, `\r`, `\n`, `\t`, and ZLE/PTY control sequences.
pub fn sanitize_daemon_input(input: &str) -> String {
    input.chars().filter(|c| !c.is_ascii_control()).collect()
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
        let (prefix, cwd, responder) = match req {
            CompletionRequest::Shutdown { responder } => {
                tracing::info!("Shutting down completion daemon process");
                if let Some(mut p) = daemon.take() {
                    let _ = p.child.start_kill();
                    let _ = timeout(Duration::from_millis(500), p.child.wait()).await;
                }
                let _ = responder.send(());
                break;
            }
            CompletionRequest::Complete {
                prefix,
                cwd,
                responder,
            } => (prefix, cwd, responder),
        };

        if responder.is_closed() {
            tracing::debug!(
                prefix = %prefix,
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
                    let _ = responder.send(Err(ZshcsError::Io(e)));
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
            if let Some(target_cwd) = &cwd {
                let need_chdir = match &proc.current_cwd {
                    Some(current) => current != target_cwd,
                    None => true,
                };
                if need_chdir {
                    tracing::debug!(?target_cwd, "Changing daemon working directory");
                    let sanitized_cwd = sanitize_daemon_input(&target_cwd.to_string_lossy());
                    let chdir_msg = format!("chdir:{sanitized_cwd}\n");
                    proc.stdin.write_all(chdir_msg.as_bytes()).await?;
                    proc.current_cwd = Some(target_cwd.clone());
                }
            }

            // Send input message to daemon (sanitizing control characters)
            let sanitized_prefix = sanitize_daemon_input(&prefix);
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

            deduplicate_completion_items(&mut items);

            tracing::debug!(
                item_count = items.len(),
                "Completed candidate stream parsing"
            );
            Ok(items)
        })
        .await;

        match exec_result {
            Ok(Ok(items)) => {
                let _ = responder.send(Ok(items));
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
                let _ = responder.send(Err(ZshcsError::Io(e)));
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
                let _ = responder.send(Err(ZshcsError::Daemon(
                    "Completion request timed out".to_string(),
                )));
            }
        }
    }

    if let Some(mut p) = daemon.take() {
        let _ = p.child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_daemon_input_strips_ascii_control_chars_and_preserves_multibyte() {
        // Control characters interfering with capture.zsh / zptyrc.zsh / PTY:
        // \0 (null delimiter), \r, \n, \t (^I complete-word), \x15 (^U kill-buffer),
        // \x02 (^B backward-char), \x01 (EOC marker), \x1b (ESC), \x7f (DEL)
        let input = "git\0\r\n\t\x15\x02\x01\x1b\x7f status --short";
        assert_eq!(sanitize_daemon_input(input), "git status --short");

        // All ASCII control characters (0x00..=0x1F and 0x7F) must be stripped
        let all_controls: String = (0x00u8..=0x1Fu8)
            .chain(std::iter::once(0x7Fu8))
            .map(char::from)
            .collect();
        assert_eq!(sanitize_daemon_input(&format!("a{all_controls}b")), "ab");

        // Spaces, shell punctuation, Japanese, and emoji (including ZWJ) must be preserved
        let normal = "echo $HOME/${VAR} '~/path/file name.zsh' \"こんにちは\" 🎉🚀👨‍👩‍👧‍👦";
        assert_eq!(sanitize_daemon_input(normal), normal);
    }

    #[tokio::test]
    async fn test_run_completion_daemon_sanitizes_control_chars_in_prefix_and_cwd() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log_path = temp_dir.path().join("received.log");
        let script_path = temp_dir.path().join("mock_capture.zsh");
        let log_path_str = log_path.to_string_lossy().replace('\'', "'\\''");
        let script = format!(
            r#"#!/usr/bin/env zsh
log_file='{log_path_str}'
while IFS= read -r line; do
    print -r -- "$line" >> "$log_file"
    if [[ "$line" == input:* ]]; then
        printf "ok\titem\n\x01EOC\x01\n"
    fi
done
"#
        );
        std::fs::write(&script_path, script).unwrap();

        let (tx, rx) = mpsc::channel(8);
        let mut client_opt = None;
        let (_service, _socket) = tower_lsp::LspService::new(|client| {
            client_opt = Some(client.clone());
            crate::Backend::new_with_scripts(client, "#!/bin/zsh\nexit 0\n", "").unwrap()
        });
        let client = client_opt.unwrap();

        let daemon_handle = tokio::spawn(run_completion_daemon(script_path, None, rx, client));

        let dirty_cwd = PathBuf::from("/tmp/dir\twith\x15ctrl\x02/こんにちは");
        let dirty_prefix = "git\t\x15\x02\x01\r\n status '日本語' 🎉".to_string();

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send(CompletionRequest::Complete {
            prefix: dirty_prefix,
            cwd: Some(dirty_cwd),
            responder: resp_tx,
        })
        .await
        .unwrap();

        let items = tokio::time::timeout(Duration::from_secs(3), resp_rx)
            .await
            .expect("daemon response timeout")
            .expect("responder channel")
            .expect("daemon result");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "ok");

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        tx.send(CompletionRequest::Shutdown {
            responder: shutdown_tx,
        })
        .await
        .unwrap();
        let _ = shutdown_rx.await;
        let _ = daemon_handle.await;

        let logged = std::fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = logged.lines().collect();
        assert_eq!(
            lines,
            vec![
                "chdir:/tmp/dirwithctrl/こんにちは",
                "input:git status '日本語' 🎉",
            ]
        );
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
        tx.send(CompletionRequest::Complete {
            prefix: "cancelled".to_string(),
            cwd: None,
            responder: tx_resp1,
        })
        .await
        .unwrap();

        // 2. Send active request
        let (tx_resp2, rx_resp2) = oneshot::channel();
        tx.send(CompletionRequest::Complete {
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
}

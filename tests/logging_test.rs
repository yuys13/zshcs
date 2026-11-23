use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

struct StdioLspProcess {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
    stderr_handle: Option<std::thread::JoinHandle<String>>,
}

impl StdioLspProcess {
    fn spawn(zshcs_log: Option<&str>, rust_log: Option<&str>) -> Self {
        let bin_path = env!("CARGO_BIN_EXE_zshcs");
        let mut cmd = Command::new(bin_path);
        cmd.arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        match zshcs_log {
            Some(val) => {
                cmd.env("ZSHCS_LOG", val);
            }
            None => {
                cmd.env_remove("ZSHCS_LOG");
            }
        }
        match rust_log {
            Some(val) => {
                cmd.env("RUST_LOG", val);
            }
            None => {
                cmd.env_remove("RUST_LOG");
            }
        }

        let mut child = cmd.spawn().expect("Failed to spawn zshcs process");
        let stdin = child.stdin.take().expect("Failed to open stdin");
        let stdout = child.stdout.take().expect("Failed to open stdout");
        let stderr = child.stderr.take().expect("Failed to open stderr");

        let stderr_handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut output = String::new();
            let mut line = String::new();
            while let Ok(len) = reader.read_line(&mut line) {
                if len == 0 {
                    break;
                }
                output.push_str(&line);
                line.clear();
            }
            output
        });

        Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            stderr_handle: Some(stderr_handle),
        }
    }

    fn send_message(&mut self, val: serde_json::Value) {
        let stdin = self.stdin.as_mut().expect("stdin already closed");
        let text = val.to_string();
        let msg = format!("Content-Length: {}\r\n\r\n{}", text.len(), text);
        stdin
            .write_all(msg.as_bytes())
            .expect("Failed to write message to stdin");
        stdin.flush().expect("Failed to flush stdin");
    }

    fn read_message(&mut self) -> serde_json::Value {
        let mut content_len: Option<usize> = None;
        loop {
            let mut header_line = String::new();
            let bytes = self
                .stdout
                .read_line(&mut header_line)
                .expect("Failed to read header from stdout");
            assert!(bytes > 0, "Unexpected EOF while reading LSP headers");
            let trimmed = header_line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break;
            }
            if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
                content_len = Some(
                    len_str
                        .trim()
                        .parse()
                        .expect("Failed to parse Content-Length"),
                );
            }
        }

        let content_len = content_len.expect("stdout must contain Content-Length header");
        let mut body_buf = vec![0u8; content_len];
        std::io::Read::read_exact(&mut self.stdout, &mut body_buf)
            .expect("Failed to read response body from stdout");
        serde_json::from_slice(&body_buf).expect("stdout body must be valid JSON")
    }

    fn finish(mut self) -> (std::process::ExitStatus, String) {
        drop(self.stdin.take());
        let status = self.child.wait().expect("Failed to wait on child");
        let stderr_output = self
            .stderr_handle
            .take()
            .expect("stderr handle missing")
            .join()
            .expect("stderr reader thread panicked");
        (status, stderr_output)
    }
}

#[test]
fn test_binary_logging_routes_to_stderr_and_stdout_is_clean_jsonrpc() {
    let mut proc = StdioLspProcess::spawn(Some("debug"), None);

    proc.send_message(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "capabilities": {}
        }
    }));

    let resp_json = proc.read_message();
    assert_eq!(resp_json["id"], 1);
    assert_eq!(
        resp_json["result"]["serverInfo"]["name"],
        "zshcs-language-server"
    );

    let (status, stderr_output) = proc.finish();
    assert!(status.success());

    // Verify stderr contains tracing logs
    assert!(
        stderr_output.contains("INFO") || stderr_output.contains("DEBUG"),
        "stderr should contain structured tracing logs, got: {stderr_output}"
    );
    assert!(
        stderr_output.contains("initialize request received")
            || stderr_output.contains("Client initialization parameters"),
        "stderr should log initialize request lifecycle, got: {stderr_output}"
    );
}

#[test]
fn test_binary_logging_with_rust_log_env() {
    let mut proc = StdioLspProcess::spawn(None, Some("info"));

    proc.send_message(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "capabilities": {}
        }
    }));

    let resp_json = proc.read_message();
    assert_eq!(resp_json["id"], 1);

    let (status, stderr_output) = proc.finish();
    assert!(status.success());

    assert!(
        stderr_output.contains("INFO"),
        "stderr should contain INFO level logs when RUST_LOG=info, got: {stderr_output}"
    );
    assert!(
        !stderr_output.contains("DEBUG"),
        "stderr should NOT contain DEBUG level logs when RUST_LOG=info, got: {stderr_output}"
    );
}

#[test]
fn test_binary_logging_error_level_suppresses_info_logs() {
    let mut proc = StdioLspProcess::spawn(Some("error"), None);

    proc.send_message(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "capabilities": {}
        }
    }));

    let resp_json = proc.read_message();
    assert_eq!(resp_json["id"], 1);

    let (status, stderr_output) = proc.finish();
    assert!(status.success());

    assert!(
        !stderr_output.contains("INFO") && !stderr_output.contains("DEBUG"),
        "stderr should NOT contain INFO or DEBUG logs when ZSHCS_LOG=error, got: {stderr_output}"
    );
}

#[test]
fn test_binary_logging_did_open_and_did_change() {
    let mut proc = StdioLspProcess::spawn(Some("debug"), None);

    // 1. Initialize
    proc.send_message(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "capabilities": {}
        }
    }));

    let init_resp = proc.read_message();
    assert_eq!(init_resp["id"], 1);

    // 2. Initialized notification
    proc.send_message(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {}
    }));

    // 3. didOpen notification
    proc.send_message(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": "file:///tmp/test_log.zsh",
                "languageId": "zsh",
                "version": 1,
                "text": "echo initial\n"
            }
        }
    }));

    // 4. didChange notification
    proc.send_message(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": {
                "uri": "file:///tmp/test_log.zsh",
                "version": 2
            },
            "contentChanges": [
                {
                    "text": "echo changed\n"
                }
            ]
        }
    }));

    // 5. executeCommand request to confirm didChange was applied
    proc.send_message(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "workspace/executeCommand",
        "params": {
            "command": "zshcs/getDocumentContent",
            "arguments": ["file:///tmp/test_log.zsh"]
        }
    }));

    // Read until response id 2
    loop {
        let msg = proc.read_message();
        if msg.get("id") == Some(&serde_json::json!(2)) {
            assert_eq!(msg["result"], "echo changed\n");
            break;
        }
    }

    // 6. Shutdown request
    proc.send_message(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "shutdown",
        "params": null
    }));

    loop {
        let msg = proc.read_message();
        if msg.get("id") == Some(&serde_json::json!(3)) {
            break;
        }
    }

    // 7. Exit notification
    proc.send_message(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "exit",
        "params": null
    }));

    let (status, stderr_output) = proc.finish();
    assert!(status.success());

    assert!(
        stderr_output.contains("textDocument/didOpen"),
        "didOpen must be logged: {stderr_output}"
    );
    assert!(
        stderr_output.contains("textDocument/didChange"),
        "didChange must be logged: {stderr_output}"
    );
    assert!(
        stderr_output.contains("execute_command invoked"),
        "execute_command must be logged: {stderr_output}"
    );
    assert!(
        stderr_output.contains("shutting down"),
        "shutdown must be logged: {stderr_output}"
    );
}

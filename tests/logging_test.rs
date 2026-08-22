use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;
use zshcs::{create_env_filter, init_logging, try_init_logging};

#[derive(Clone)]
struct InMemoryBuffer(Arc<Mutex<Vec<u8>>>);

impl<'a> MakeWriter<'a> for InMemoryBuffer {
    type Writer = InMemoryWriter;

    fn make_writer(&'a self) -> Self::Writer {
        InMemoryWriter(self.0.clone())
    }
}

struct InMemoryWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for InMemoryWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

static ENV_MUTEX: Mutex<()> = Mutex::new(());

#[test]
fn test_init_logging_idempotency() {
    init_logging();
    init_logging();
    let _ = try_init_logging();
}

#[test]
fn test_create_env_filter_logic() {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("ZSHCS_LOG", "debug");
        std::env::set_var("RUST_LOG", "error");
    }
    let filter = create_env_filter();
    assert_eq!(filter.to_string(), "debug");

    unsafe {
        std::env::remove_var("ZSHCS_LOG");
        std::env::set_var("RUST_LOG", "warn");
    }
    let filter2 = create_env_filter();
    assert_eq!(filter2.to_string(), "warn");

    unsafe {
        std::env::remove_var("RUST_LOG");
    }
    let filter3 = create_env_filter();
    assert_eq!(filter3.to_string(), "info");
}

#[test]
fn test_create_env_filter_invalid_and_empty_edge_cases() {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    // Invalid ZSHCS_LOG falls back to RUST_LOG
    unsafe {
        std::env::set_var("ZSHCS_LOG", "invalid[filter");
        std::env::set_var("RUST_LOG", "warn");
    }
    let filter = create_env_filter();
    assert_eq!(filter.to_string(), "warn");

    // Both invalid falls back to default info
    unsafe {
        std::env::set_var("ZSHCS_LOG", "invalid[1");
        std::env::set_var("RUST_LOG", "invalid[2");
    }
    let filter2 = create_env_filter();
    assert_eq!(filter2.to_string(), "info");

    // Empty string falls back to default info
    unsafe {
        std::env::set_var("ZSHCS_LOG", "");
        std::env::remove_var("RUST_LOG");
    }
    let filter3 = create_env_filter();
    assert_eq!(filter3.to_string(), "info");

    unsafe {
        std::env::remove_var("ZSHCS_LOG");
    }
}

#[test]
fn test_structured_log_levels_and_fields() {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let in_mem = InMemoryBuffer(buffer.clone());

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("debug"))
        .with_writer(in_mem)
        .with_ansi(false)
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(server_id = "test_1", version = "0.1.0", "Server started");
        tracing::debug!(line = 42, "Processing completion");
        tracing::trace!("This trace should be filtered out");
    });

    let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert!(output.contains("INFO"));
    assert!(output.contains("Server started"));
    assert!(output.contains("server_id"));
    assert!(output.contains("DEBUG"));
    assert!(output.contains("Processing completion"));
    assert!(!output.contains("This trace should be filtered out"));
}

#[test]
fn test_binary_logging_routes_to_stderr_and_stdout_is_clean_jsonrpc() {
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let mut child = Command::new(bin_path)
        .arg("--stdio")
        .env("ZSHCS_LOG", "debug")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn zshcs process");

    let mut stdin = child.stdin.take().expect("Failed to open stdin");
    let stdout = child.stdout.take().expect("Failed to open stdout");
    let stderr = child.stderr.take().expect("Failed to open stderr");

    // Send LSP initialize request
    let init_json = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "capabilities": {}
        }
    })
    .to_string();

    let message = format!("Content-Length: {}\r\n\r\n{}", init_json.len(), init_json);
    stdin
        .write_all(message.as_bytes())
        .expect("Failed to write initialize to stdin");
    stdin.flush().expect("Failed to flush stdin");

    // Read response from stdout
    let mut stdout_reader = BufReader::new(stdout);
    let mut header_line = String::new();
    stdout_reader
        .read_line(&mut header_line)
        .expect("Failed to read header from stdout");

    assert!(
        header_line.starts_with("Content-Length: "),
        "stdout must contain pure LSP protocol headers, got: {header_line}"
    );

    // Read remaining headers until blank line
    let mut empty_line = String::new();
    stdout_reader
        .read_line(&mut empty_line)
        .expect("Failed to read empty line");

    let content_len: usize = header_line
        .trim_start_matches("Content-Length: ")
        .trim()
        .parse()
        .expect("Failed to parse Content-Length");

    let mut body_buf = vec![0u8; content_len];
    std::io::Read::read_exact(&mut stdout_reader, &mut body_buf)
        .expect("Failed to read response body from stdout");

    let resp_json: serde_json::Value =
        serde_json::from_slice(&body_buf).expect("stdout body must be valid JSON");
    assert_eq!(resp_json["id"], 1);
    assert_eq!(
        resp_json["result"]["serverInfo"]["name"],
        "zshcs-language-server"
    );

    // Close stdin to let server shutdown
    drop(stdin);

    let mut stderr_reader = BufReader::new(stderr);
    let mut stderr_output = String::new();
    let mut line = String::new();
    while let Ok(len) = stderr_reader.read_line(&mut line) {
        if len == 0 {
            break;
        }
        stderr_output.push_str(&line);
        line.clear();
    }

    let status = child.wait().expect("Failed to wait on child");
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
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let mut child = Command::new(bin_path)
        .arg("--stdio")
        .env_remove("ZSHCS_LOG")
        .env("RUST_LOG", "info")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn zshcs process");

    let mut stdin = child.stdin.take().expect("Failed to open stdin");
    let stdout = child.stdout.take().expect("Failed to open stdout");
    let stderr = child.stderr.take().expect("Failed to open stderr");

    let init_json = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "capabilities": {}
        }
    })
    .to_string();

    let message = format!("Content-Length: {}\r\n\r\n{}", init_json.len(), init_json);
    stdin
        .write_all(message.as_bytes())
        .expect("Failed to write initialize to stdin");
    stdin.flush().expect("Failed to flush stdin");

    let mut stdout_reader = BufReader::new(stdout);
    let mut header_line = String::new();
    stdout_reader
        .read_line(&mut header_line)
        .expect("Failed to read header from stdout");

    assert!(
        header_line.starts_with("Content-Length: "),
        "stdout must contain pure LSP protocol headers, got: {header_line}"
    );

    let mut empty_line = String::new();
    stdout_reader
        .read_line(&mut empty_line)
        .expect("Failed to read empty line");

    let content_len: usize = header_line
        .trim_start_matches("Content-Length: ")
        .trim()
        .parse()
        .expect("Failed to parse Content-Length");

    let mut body_buf = vec![0u8; content_len];
    std::io::Read::read_exact(&mut stdout_reader, &mut body_buf)
        .expect("Failed to read response body from stdout");

    let resp_json: serde_json::Value =
        serde_json::from_slice(&body_buf).expect("stdout body must be valid JSON");
    assert_eq!(resp_json["id"], 1);

    drop(stdin);

    let mut stderr_reader = BufReader::new(stderr);
    let mut stderr_output = String::new();
    let mut line = String::new();
    while let Ok(len) = stderr_reader.read_line(&mut line) {
        if len == 0 {
            break;
        }
        stderr_output.push_str(&line);
        line.clear();
    }

    let status = child.wait().expect("Failed to wait on child");
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
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let mut child = Command::new(bin_path)
        .arg("--stdio")
        .env("ZSHCS_LOG", "error")
        .env_remove("RUST_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn zshcs process");

    let mut stdin = child.stdin.take().expect("Failed to open stdin");
    let stdout = child.stdout.take().expect("Failed to open stdout");
    let stderr = child.stderr.take().expect("Failed to open stderr");

    let init_json = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "capabilities": {}
        }
    })
    .to_string();

    let message = format!("Content-Length: {}\r\n\r\n{}", init_json.len(), init_json);
    stdin
        .write_all(message.as_bytes())
        .expect("Failed to write initialize to stdin");
    stdin.flush().expect("Failed to flush stdin");

    let mut stdout_reader = BufReader::new(stdout);
    let mut header_line = String::new();
    stdout_reader
        .read_line(&mut header_line)
        .expect("Failed to read header from stdout");

    assert!(
        header_line.starts_with("Content-Length: "),
        "stdout must contain pure LSP protocol headers, got: {header_line}"
    );

    let mut empty_line = String::new();
    stdout_reader
        .read_line(&mut empty_line)
        .expect("Failed to read empty line");

    let content_len: usize = header_line
        .trim_start_matches("Content-Length: ")
        .trim()
        .parse()
        .expect("Failed to parse Content-Length");

    let mut body_buf = vec![0u8; content_len];
    std::io::Read::read_exact(&mut stdout_reader, &mut body_buf)
        .expect("Failed to read response body from stdout");

    let resp_json: serde_json::Value =
        serde_json::from_slice(&body_buf).expect("stdout body must be valid JSON");
    assert_eq!(resp_json["id"], 1);

    drop(stdin);

    let mut stderr_reader = BufReader::new(stderr);
    let mut stderr_output = String::new();
    let mut line = String::new();
    while let Ok(len) = stderr_reader.read_line(&mut line) {
        if len == 0 {
            break;
        }
        stderr_output.push_str(&line);
        line.clear();
    }

    let status = child.wait().expect("Failed to wait on child");
    assert!(status.success());

    assert!(
        !stderr_output.contains("INFO") && !stderr_output.contains("DEBUG"),
        "stderr should NOT contain INFO or DEBUG logs when ZSHCS_LOG=error, got: {stderr_output}"
    );
}

#[test]
fn test_binary_logging_did_open_and_did_change() {
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let mut child = Command::new(bin_path)
        .arg("--stdio")
        .env("ZSHCS_LOG", "debug")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn zshcs process");

    let mut stdin = child.stdin.take().expect("Failed to open stdin");
    let stdout = child.stdout.take().expect("Failed to open stdout");
    let stderr = child.stderr.take().expect("Failed to open stderr");
    let mut stdout_reader = BufReader::new(stdout);

    let read_message = |reader: &mut BufReader<std::process::ChildStdout>| -> serde_json::Value {
        let mut header_line = String::new();
        reader.read_line(&mut header_line).expect("read header");
        assert!(
            header_line.starts_with("Content-Length: "),
            "expected Content-Length header, got: {header_line}"
        );
        let mut empty_line = String::new();
        reader.read_line(&mut empty_line).expect("read empty line");
        let content_len: usize = header_line
            .trim_start_matches("Content-Length: ")
            .trim()
            .parse()
            .expect("parse content len");
        let mut body_buf = vec![0u8; content_len];
        std::io::Read::read_exact(reader, &mut body_buf).expect("read body");
        serde_json::from_slice(&body_buf).expect("valid json")
    };

    let send_message = |sin: &mut std::process::ChildStdin, val: serde_json::Value| {
        let text = val.to_string();
        let msg = format!("Content-Length: {}\r\n\r\n{}", text.len(), text);
        sin.write_all(msg.as_bytes()).expect("write message");
        sin.flush().expect("flush stdin");
    };

    // 1. Initialize
    send_message(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "capabilities": {}
            }
        }),
    );

    let init_resp = read_message(&mut stdout_reader);
    assert_eq!(init_resp["id"], 1);

    // 2. Initialized notification
    send_message(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }),
    );

    // 3. didOpen notification
    send_message(
        &mut stdin,
        serde_json::json!({
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
        }),
    );

    // 4. didChange notification
    send_message(
        &mut stdin,
        serde_json::json!({
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
        }),
    );

    // 5. executeCommand request to confirm didChange was applied
    send_message(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "workspace/executeCommand",
            "params": {
                "command": "zshcs/getDocumentContent",
                "arguments": ["file:///tmp/test_log.zsh"]
            }
        }),
    );

    // Read until response id 2
    loop {
        let msg = read_message(&mut stdout_reader);
        if msg.get("id") == Some(&serde_json::json!(2)) {
            assert_eq!(msg["result"], "echo changed\n");
            break;
        }
    }

    // 6. Shutdown request
    send_message(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "shutdown",
            "params": null
        }),
    );

    loop {
        let msg = read_message(&mut stdout_reader);
        if msg.get("id") == Some(&serde_json::json!(3)) {
            break;
        }
    }

    // 7. Exit notification
    send_message(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }),
    );

    drop(stdin);

    let mut stderr_reader = BufReader::new(stderr);
    let mut stderr_output = String::new();
    let mut line = String::new();
    while let Ok(len) = stderr_reader.read_line(&mut line) {
        if len == 0 {
            break;
        }
        stderr_output.push_str(&line);
        line.clear();
    }

    let status = child.wait().expect("wait child");
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

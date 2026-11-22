use std::time::Duration;

/// Default timeout for executing `zsh -n`.
pub const DEFAULT_SYNTAX_CHECK_TIMEOUT: Duration = Duration::from_millis(2000);

/// Runs `zsh -n` on `text` with `timeout_dur` and returns `Some(stderr)` if syntax errors are reported.
pub async fn run_zsh_syntax_check(text: &str, timeout_dur: Duration) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }

    let temp_file = match tempfile::Builder::new()
        .prefix("zshcs_diag_")
        .suffix(".zsh")
        .tempfile()
    {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to create temp file for zsh -n syntax check");
            return None;
        }
    };

    let temp_path = temp_file.path().to_path_buf();
    if let Err(e) = tokio::fs::write(&temp_path, text.as_bytes()).await {
        tracing::warn!(error = %e, "Failed to write temp file for zsh -n syntax check");
        return None;
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
            return None;
        }
    };

    let output = match tokio::time::timeout(timeout_dur, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "zsh -n process failed while waiting for output");
            return None;
        }
        Err(_) => {
            tracing::warn!("zsh -n syntax check timed out after {:?}", timeout_dur);
            return None;
        }
    };

    if output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stderr).into_owned())
}

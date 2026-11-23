use std::time::Duration;

/// Default timeout for executing `zsh -n`.
pub const DEFAULT_SYNTAX_CHECK_TIMEOUT: Duration = Duration::from_millis(2000);

/// Runs `zsh -n` on `text` with `timeout_dur` and returns `Some(stderr)` if syntax errors are reported.
pub async fn run_zsh_syntax_check(text: &str, timeout_dur: Duration) -> Option<String> {
    run_zsh_syntax_check_with_command(text, timeout_dur, "zsh", &["-n"]).await
}

pub(crate) async fn run_zsh_syntax_check_with_command(
    text: &str,
    timeout_dur: Duration,
    program: &str,
    args: &[&str],
) -> Option<String> {
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

    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_run_zsh_syntax_check_empty_and_whitespace() {
        assert_eq!(
            run_zsh_syntax_check("", DEFAULT_SYNTAX_CHECK_TIMEOUT).await,
            None
        );
        assert_eq!(
            run_zsh_syntax_check("   \n\t ", DEFAULT_SYNTAX_CHECK_TIMEOUT).await,
            None
        );
    }

    #[tokio::test]
    async fn test_run_zsh_syntax_check_valid_script() {
        let res = run_zsh_syntax_check(
            "echo ok\nif true; then :; fi\n",
            DEFAULT_SYNTAX_CHECK_TIMEOUT,
        )
        .await;
        assert_eq!(res, None);
    }

    #[tokio::test]
    async fn test_run_zsh_syntax_check_invalid_script() {
        let res = run_zsh_syntax_check("if [[ ; then\n", DEFAULT_SYNTAX_CHECK_TIMEOUT).await;
        let stderr = res.expect("Expected syntax error stderr");
        assert!(stderr.contains("parse error"), "Got stderr: {stderr}");
    }

    #[tokio::test]
    async fn test_run_zsh_syntax_check_timeout() {
        let res = run_zsh_syntax_check_with_command(
            "if [[ ; then\n",
            Duration::from_millis(20),
            "zsh",
            &["-c", "sleep 10"],
        )
        .await;
        assert_eq!(res, None);
    }
}

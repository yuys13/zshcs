//! Environment health check and diagnostic subsystem for `zshcs`.
//!
//! Provides automated diagnosis of runtime dependencies, shell modules,
//! cache directory permissions, and completion capture dry-runs.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::completion::{CAPTURE_ZSH, ZPTYRC_ZSH};

/// Diagnostic status for an individual check item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// The diagnostic check passed successfully.
    Pass,
    /// The diagnostic check encountered a non-fatal warning.
    Warn,
    /// The diagnostic check failed.
    Fail,
}

impl CheckStatus {
    /// Returns the symbol representation for formatting.
    #[must_use]
    pub fn symbol(&self) -> &'static str {
        match self {
            CheckStatus::Pass => "[✓]",
            CheckStatus::Warn => "[!]",
            CheckStatus::Fail => "[✗]",
        }
    }

    /// Returns the textual label representation for formatting.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            CheckStatus::Pass => "PASS",
            CheckStatus::Warn => "WARN",
            CheckStatus::Fail => "FAIL",
        }
    }
}

/// The result of an individual diagnostic check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    /// Human-readable name of the diagnostic check.
    pub name: String,
    /// Diagnostic outcome status.
    pub status: CheckStatus,
    /// Detailed diagnostic message or explanation.
    pub message: String,
}

impl CheckResult {
    /// Creates a new `CheckResult`.
    pub fn new(name: impl Into<String>, status: CheckStatus, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status,
            message: message.into(),
        }
    }

    /// Returns `true` if the check passed.
    #[must_use]
    pub fn is_pass(&self) -> bool {
        self.status == CheckStatus::Pass
    }

    /// Returns `true` if the check failed.
    #[must_use]
    pub fn is_fail(&self) -> bool {
        self.status == CheckStatus::Fail
    }

    /// Returns `true` if the check issued a warning.
    #[must_use]
    pub fn is_warn(&self) -> bool {
        self.status == CheckStatus::Warn
    }
}

/// Comprehensive report containing outcomes of all executed diagnostic checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    /// Ordered list of diagnostic check results.
    pub checks: Vec<CheckResult>,
}

impl DoctorReport {
    /// Creates a new `DoctorReport` from a list of `CheckResult`s.
    #[must_use]
    pub fn new(checks: Vec<CheckResult>) -> Self {
        Self { checks }
    }

    /// Returns `true` if all diagnostic checks passed without failure.
    #[must_use]
    pub fn is_all_passed(&self) -> bool {
        !self.checks.iter().any(|c| c.is_fail())
    }

    /// Returns the total count of passed checks.
    #[must_use]
    pub fn pass_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.status == CheckStatus::Pass)
            .count()
    }

    /// Returns the total count of failed checks.
    #[must_use]
    pub fn fail_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.status == CheckStatus::Fail)
            .count()
    }

    /// Returns the total count of warning checks.
    #[must_use]
    pub fn warn_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.status == CheckStatus::Warn)
            .count()
    }

    /// Formats and renders the diagnostic report to the specified writer.
    pub fn render<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
        writeln!(writer, "zshcs doctor - Environment Health Check")?;
        writeln!(writer)?;

        for check in &self.checks {
            writeln!(
                writer,
                "{} {}: {}",
                check.status.symbol(),
                check.name,
                check.message
            )?;
        }

        writeln!(writer)?;
        if self.is_all_passed() {
            let warn_suffix = if self.warn_count() > 0 {
                format!(", {} warning(s)", self.warn_count())
            } else {
                String::new()
            };
            writeln!(
                writer,
                "Result: {}/{} checks passed{}. Your environment is ready for zshcs!",
                self.pass_count(),
                self.checks.len(),
                warn_suffix
            )?;
        } else {
            let warn_suffix = if self.warn_count() > 0 {
                format!(", {} warning(s)", self.warn_count())
            } else {
                String::new()
            };
            writeln!(
                writer,
                "Result: {}/{} checks passed, {} failed{}. Please address the failed items above.",
                self.pass_count(),
                self.checks.len(),
                self.fail_count(),
                warn_suffix
            )?;
        }

        Ok(())
    }
}

const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// Executes a command with a timeout guard, returning an error if execution exceeds the timeout.
fn run_command_with_timeout(
    mut cmd: Command,
    timeout: Duration,
) -> std::io::Result<std::process::Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let start = std::time::Instant::now();

    loop {
        match child.try_wait()? {
            Some(status) => {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = std::io::Read::read_to_end(&mut out, &mut stdout);
                }
                if let Some(mut err) = child.stderr.take() {
                    let _ = std::io::Read::read_to_end(&mut err, &mut stderr);
                }
                return Ok(std::process::Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            None => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("Command timed out after {} seconds", timeout.as_secs()),
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// Diagnoses the existence, executability, and version of the `zsh` binary.
#[must_use]
pub fn check_zsh_executable(zsh_binary: Option<&str>) -> CheckResult {
    let bin = zsh_binary.unwrap_or("zsh");
    let mut cmd = Command::new(bin);
    cmd.arg("--version");
    match run_command_with_timeout(cmd, DEFAULT_COMMAND_TIMEOUT) {
        Ok(output) if output.status.success() => {
            let stdout_str = String::from_utf8_lossy(&output.stdout);
            let version_str = stdout_str
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_string();
            let version = if version_str.is_empty() {
                format!("'{}' executable found (empty version output)", bin)
            } else {
                version_str
            };
            CheckResult::new("Zsh Executable", CheckStatus::Pass, version)
        }
        Ok(output) => {
            let stderr_str = String::from_utf8_lossy(&output.stderr);
            let stderr = stderr_str
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_string();
            let msg = if stderr.is_empty() {
                format!("'{} --version' exited with status {}", bin, output.status)
            } else {
                format!("'{} --version' failed ({}): {}", bin, output.status, stderr)
            };
            CheckResult::new("Zsh Executable", CheckStatus::Fail, msg)
        }
        Err(e) => CheckResult::new(
            "Zsh Executable",
            CheckStatus::Fail,
            format!("'{}' not found in PATH or failed to execute: {}", bin, e),
        ),
    }
}

/// Diagnoses availability of the `zsh/zpty` module.
#[must_use]
pub fn check_zpty_module(zsh_binary: Option<&str>) -> CheckResult {
    let bin = zsh_binary.unwrap_or("zsh");
    let mut cmd = Command::new(bin);
    cmd.args(["-c", "zmodload zsh/zpty"]);
    match run_command_with_timeout(cmd, DEFAULT_COMMAND_TIMEOUT) {
        Ok(output) if output.status.success() => CheckResult::new(
            "Zpty Module",
            CheckStatus::Pass,
            "Module 'zsh/zpty' loaded successfully",
        ),
        Ok(output) => {
            let stderr_str = String::from_utf8_lossy(&output.stderr);
            let stderr = stderr_str
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_string();
            let msg = if stderr.is_empty() {
                format!(
                    "Failed to load 'zsh/zpty' module (exit code {})",
                    output.status
                )
            } else {
                format!("Failed to load 'zsh/zpty' module: {}", stderr)
            };
            CheckResult::new("Zpty Module", CheckStatus::Fail, msg)
        }
        Err(e) => CheckResult::new(
            "Zpty Module",
            CheckStatus::Fail,
            format!("Failed to execute '{}' to check 'zsh/zpty': {}", bin, e),
        ),
    }
}

/// Diagnoses availability of the `zsh/zutil` module.
#[must_use]
pub fn check_zutil_module(zsh_binary: Option<&str>) -> CheckResult {
    let bin = zsh_binary.unwrap_or("zsh");
    let mut cmd = Command::new(bin);
    cmd.args(["-c", "zmodload zsh/zutil"]);
    match run_command_with_timeout(cmd, DEFAULT_COMMAND_TIMEOUT) {
        Ok(output) if output.status.success() => CheckResult::new(
            "Zutil Module",
            CheckStatus::Pass,
            "Module 'zsh/zutil' loaded successfully",
        ),
        Ok(output) => {
            let stderr_str = String::from_utf8_lossy(&output.stderr);
            let stderr = stderr_str
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_string();
            let msg = if stderr.is_empty() {
                format!(
                    "Failed to load 'zsh/zutil' module (exit code {})",
                    output.status
                )
            } else {
                format!("Failed to load 'zsh/zutil' module: {}", stderr)
            };
            CheckResult::new("Zutil Module", CheckStatus::Fail, msg)
        }
        Err(e) => CheckResult::new(
            "Zutil Module",
            CheckStatus::Fail,
            format!("Failed to execute '{}' to check 'zsh/zutil': {}", bin, e),
        ),
    }
}

/// Diagnoses the cache directory creation and write permissions.
#[must_use]
pub fn check_cache_directory(custom_cache_dir: Option<&Path>) -> CheckResult {
    let cache_dir = if let Some(dir) = custom_cache_dir {
        dir.to_path_buf()
    } else if let Some(dir) = std::env::var_os("ZSHCS_CACHE_DIR").filter(|s| !s.is_empty()) {
        PathBuf::from(dir)
    } else if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME").filter(|s| !s.is_empty()) {
        PathBuf::from(xdg).join("zshcs/zsh")
    } else if let Some(home) = std::env::var_os("HOME").filter(|s| !s.is_empty()) {
        PathBuf::from(home).join(".cache/zshcs/zsh")
    } else {
        std::env::temp_dir().join("zshcs/zsh")
    };

    if let Err(e) = std::fs::create_dir_all(&cache_dir) {
        return CheckResult::new(
            "Cache Directory",
            CheckStatus::Fail,
            format!(
                "Failed to create cache directory '{}': {}",
                cache_dir.display(),
                e
            ),
        );
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let test_file = cache_dir.join(format!(
        ".zshcs_doctor_write_test_{}_{}",
        std::process::id(),
        timestamp
    ));

    if let Err(e) = std::fs::write(&test_file, b"zshcs doctor test") {
        return CheckResult::new(
            "Cache Directory",
            CheckStatus::Fail,
            format!(
                "Cache directory '{}' is not writable: {}",
                cache_dir.display(),
                e
            ),
        );
    }

    let _ = std::fs::remove_file(&test_file);

    CheckResult::new(
        "Cache Directory",
        CheckStatus::Pass,
        format!("'{}' exists and is writable", cache_dir.display()),
    )
}

/// Performs a dry-run test of the embedded capture script and interactive completion engine.
#[must_use]
pub fn check_capture_dry_run(
    zsh_binary: Option<&str>,
    custom_cache_dir: Option<&Path>,
) -> CheckResult {
    let bin = zsh_binary.unwrap_or("zsh");

    let temp_dir = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => {
            return CheckResult::new(
                "Capture Script Dry-Run",
                CheckStatus::Fail,
                format!("Failed to create temporary directory for dry-run: {e}"),
            );
        }
    };

    let capture_path = temp_dir.path().join("capture.zsh");
    let zptyrc_path = temp_dir.path().join("zptyrc.zsh");

    if let Err(e) = std::fs::write(&capture_path, CAPTURE_ZSH) {
        return CheckResult::new(
            "Capture Script Dry-Run",
            CheckStatus::Fail,
            format!("Failed to write capture.zsh: {e}"),
        );
    }

    if let Err(e) = std::fs::write(&zptyrc_path, ZPTYRC_ZSH) {
        return CheckResult::new(
            "Capture Script Dry-Run",
            CheckStatus::Fail,
            format!("Failed to write zptyrc.zsh: {e}"),
        );
    }

    let bin_str = bin.to_string();
    let capture_path_buf = capture_path.clone();
    let cache_dir_buf = custom_cache_dir.map(PathBuf::from);
    let (tx, rx) = std::sync::mpsc::channel();
    let child_holder = std::sync::Arc::new(std::sync::Mutex::new(None::<std::process::Child>));
    let child_holder_worker = std::sync::Arc::clone(&child_holder);

    let reap_worker = {
        let child_holder_worker = std::sync::Arc::clone(&child_holder_worker);
        move || {
            if let Ok(mut lock) = child_holder_worker.lock()
                && let Some(mut child) = lock.take()
            {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    };

    let worker = std::thread::spawn(move || {
        let mut cmd = Command::new(&bin_str);
        cmd.arg(&capture_path_buf);
        if let Some(ref dir) = cache_dir_buf {
            cmd.env("ZSHCS_CACHE_DIR", dir);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Err(format!("Failed to spawn {bin_str}: {e}")));
                return;
            }
        };

        let mut stdin = match child.stdin.take() {
            Some(s) => s,
            None => {
                let _ = tx.send(Err("Failed to open stdin to capture script".to_string()));
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        };

        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                let _ = tx.send(Err("Failed to open stdout from capture script".to_string()));
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        };

        let stderr = child.stderr.take();
        let stderr_handle = std::thread::spawn(move || {
            if let Some(mut err) = stderr {
                let mut buf = String::new();
                let _ = std::io::Read::read_to_string(&mut err, &mut buf);
                buf
            } else {
                String::new()
            }
        });

        if let Ok(mut lock) = child_holder_worker.lock() {
            *lock = Some(child);
        }

        if let Err(e) = stdin.write_all(b"input:echo \n") {
            let _ = tx.send(Err(format!("Failed to write input to capture script: {e}")));
            reap_worker();
            let _ = stderr_handle.join();
            return;
        }
        let _ = stdin.flush();

        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let mut saw_eoc = false;
        let mut candidate_count = 0;

        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim_end_matches(['\r', '\n']);
                    if trimmed.contains("\x01EOC\x01") {
                        saw_eoc = true;
                        break;
                    }
                    if !trimmed.is_empty() {
                        candidate_count += 1;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(format!("Error reading from capture script: {e}")));
                    reap_worker();
                    let _ = stderr_handle.join();
                    return;
                }
            }
        }

        reap_worker();
        let stderr_output = stderr_handle.join().unwrap_or_default();
        let stderr_trimmed = stderr_output.trim();

        if saw_eoc {
            let _ = tx.send(Ok(candidate_count));
        } else if !stderr_trimmed.is_empty() {
            let first_err_line = stderr_trimmed
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or(stderr_trimmed);
            let _ = tx.send(Err(format!("Capture script failed: {first_err_line}")));
        } else {
            let _ = tx.send(Err(
                "Capture script closed stream without EOC token".to_string()
            ));
        }
    });

    match rx.recv_timeout(DEFAULT_COMMAND_TIMEOUT) {
        Ok(Ok(count)) => {
            let _ = worker.join();
            CheckResult::new(
                "Capture Script Dry-Run",
                CheckStatus::Pass,
                format!(
                    "Interactive completion engine responded successfully (received {count} candidate(s))"
                ),
            )
        }
        Ok(Err(err_msg)) => {
            let _ = worker.join();
            CheckResult::new("Capture Script Dry-Run", CheckStatus::Fail, err_msg)
        }
        Err(_) => {
            if let Ok(mut lock) = child_holder.lock()
                && let Some(mut child) = lock.take()
            {
                let _ = child.kill();
                let _ = child.wait();
            }
            let _ = worker.join();
            CheckResult::new(
                "Capture Script Dry-Run",
                CheckStatus::Fail,
                format!(
                    "Interactive completion dry-run timed out after {} seconds",
                    DEFAULT_COMMAND_TIMEOUT.as_secs()
                ),
            )
        }
    }
}

/// Executes all standard diagnostic health checks and returns a `DoctorReport`.
#[must_use]
pub fn run_doctor_checks() -> DoctorReport {
    let check1 = check_zsh_executable(None);
    let check2 = check_zpty_module(None);
    let check3 = check_zutil_module(None);
    let check4 = check_cache_directory(None);
    let check5 = check_capture_dry_run(None, None);

    DoctorReport::new(vec![check1, check2, check3, check4, check5])
}

/// Executes all health checks, renders the report to the writer, and returns `true` if all passed.
pub fn run_doctor_with_writer<W: Write>(writer: &mut W) -> bool {
    let report = run_doctor_checks();
    let _ = report.render(writer);
    report.is_all_passed()
}

/// Entry point for `zshcs doctor` subcommand.
///
/// Executes diagnostics, writes results to stdout, and returns exit code (`0` for success, `1` on failure).
#[must_use]
pub fn run_doctor() -> i32 {
    let mut stdout = std::io::stdout().lock();
    if run_doctor_with_writer(&mut stdout) {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_result_helpers() {
        let pass = CheckResult::new("Test", CheckStatus::Pass, "ok");
        assert!(pass.is_pass());
        assert!(!pass.is_fail());
        assert!(!pass.is_warn());

        let fail = CheckResult::new("Test", CheckStatus::Fail, "bad");
        assert!(!fail.is_pass());
        assert!(fail.is_fail());
        assert!(!fail.is_warn());

        let warn = CheckResult::new("Test", CheckStatus::Warn, "warn");
        assert!(!warn.is_pass());
        assert!(!warn.is_fail());
        assert!(warn.is_warn());
    }

    #[test]
    fn test_report_all_pass_calculation() {
        let r = DoctorReport::new(vec![
            CheckResult::new("C1", CheckStatus::Pass, "ok"),
            CheckResult::new("C2", CheckStatus::Pass, "ok"),
            CheckResult::new("C3", CheckStatus::Warn, "warning"),
        ]);
        assert!(r.is_all_passed());
        assert_eq!(r.pass_count(), 2);
        assert_eq!(r.fail_count(), 0);
        assert_eq!(r.warn_count(), 1);
    }

    #[test]
    fn test_run_command_with_timeout_success() {
        let mut cmd = Command::new("echo");
        cmd.arg("hello");
        let res = run_command_with_timeout(cmd, Duration::from_secs(2));
        assert!(res.is_ok());
        let output = res.unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
    }

    #[test]
    fn test_run_command_with_timeout_expired() {
        let mut cmd = Command::new("sleep");
        cmd.arg("2");
        let res = run_command_with_timeout(cmd, Duration::from_millis(50));
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    }
}

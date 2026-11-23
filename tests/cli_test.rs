use rstest::rstest;
use std::process::{Command, Stdio};

#[rstest]
#[case(&["--help"], "zshcs", Some("--stdio"))]
#[case(&["-h"], "zshcs", Some("--stdio"))]
#[case(&["--version"], "zshcs", Some(env!("CARGO_PKG_VERSION")))]
#[case(&["-V"], "zshcs", Some(env!("CARGO_PKG_VERSION")))]
#[case(&["--stdio", "--help"], "zshcs", None)]
#[case(&["--stdio", "--version"], env!("CARGO_PKG_VERSION"), None)]
fn test_binary_success_flags(
    #[case] args: &[&str],
    #[case] expected_1: &str,
    #[case] expected_2: Option<&str>,
) {
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let output = Command::new(bin_path)
        .args(args)
        .output()
        .expect("Failed to execute zshcs binary");

    assert!(output.status.success(), "args={args:?} should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(expected_1));
    if let Some(exp2) = expected_2 {
        assert!(stdout.contains(exp2));
    }
}

#[rstest]
#[case(&["--unknown-flag"], "error: unexpected argument '--unknown-flag'")]
#[case(&["--stdio", "--unknown-flag"], "unexpected argument")]
fn test_binary_invalid_flags(#[case] args: &[&str], #[case] expected_stderr: &str) {
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let output = Command::new(bin_path)
        .args(args)
        .output()
        .expect("Failed to execute zshcs binary");

    assert!(!output.status.success(), "args={args:?} should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(expected_stderr));
}

#[rstest]
#[case(&["--stdio"])]
#[case(&[])]
fn test_binary_starts_and_handles_eof(#[case] args: &[&str]) {
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let mut child = Command::new(bin_path)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("Failed to spawn zshcs");

    // Drop stdin to send EOF
    drop(child.stdin.take());

    let status = child.wait().expect("Failed to wait on child");
    assert!(status.success());
}

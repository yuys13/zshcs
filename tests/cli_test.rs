use clap::Parser;
use clap::error::ErrorKind;
use std::process::{Command, Stdio};
use zshcs::Cli;

#[test]
fn test_cli_parse_default() {
    let cli = Cli::try_parse_from(["zshcs"]).expect("Failed to parse default CLI args");
    assert!(!cli.stdio);
    assert!(cli.command.is_none());
}

#[test]
fn test_cli_parse_stdio_flag() {
    let cli = Cli::try_parse_from(["zshcs", "--stdio"]).expect("Failed to parse --stdio");
    assert!(cli.stdio);
    assert!(cli.command.is_none());
}

#[test]
fn test_cli_parse_help_flag() {
    let res = Cli::try_parse_from(["zshcs", "--help"]);
    assert!(res.is_err());
    let err = res.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::DisplayHelp);

    let res_short = Cli::try_parse_from(["zshcs", "-h"]);
    assert!(res_short.is_err());
    let err_short = res_short.unwrap_err();
    assert_eq!(err_short.kind(), ErrorKind::DisplayHelp);
}

#[test]
fn test_cli_parse_version_flag() {
    let res = Cli::try_parse_from(["zshcs", "--version"]);
    assert!(res.is_err());
    let err = res.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::DisplayVersion);

    let res_short = Cli::try_parse_from(["zshcs", "-V"]);
    assert!(res_short.is_err());
    let err_short = res_short.unwrap_err();
    assert_eq!(err_short.kind(), ErrorKind::DisplayVersion);
}

#[test]
fn test_cli_parse_invalid_flag() {
    let res = Cli::try_parse_from(["zshcs", "--unknown-flag"]);
    assert!(res.is_err());
    let err = res.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UnknownArgument);
}

#[test]
fn test_cli_parse_invalid_subcommand() {
    let res = Cli::try_parse_from(["zshcs", "nonexistent-subcommand"]);
    assert!(res.is_err());
}

#[test]
fn test_binary_help_output() {
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let output = Command::new(bin_path)
        .arg("--help")
        .output()
        .expect("Failed to execute zshcs --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("zshcs"));
    assert!(stdout.contains("--stdio"));
    assert!(stdout.contains("--help"));
    assert!(stdout.contains("--version"));
}

#[test]
fn test_binary_short_help_output() {
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let output = Command::new(bin_path)
        .arg("-h")
        .output()
        .expect("Failed to execute zshcs -h");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("zshcs"));
    assert!(stdout.contains("--stdio"));
}

#[test]
fn test_binary_version_output() {
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let output = Command::new(bin_path)
        .arg("--version")
        .output()
        .expect("Failed to execute zshcs --version");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("zshcs"));
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn test_binary_short_version_output() {
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let output = Command::new(bin_path)
        .arg("-V")
        .output()
        .expect("Failed to execute zshcs -V");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("zshcs"));
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn test_binary_invalid_argument() {
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let output = Command::new(bin_path)
        .arg("--unknown-flag")
        .output()
        .expect("Failed to execute zshcs --unknown-flag");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error: unexpected argument '--unknown-flag'"));
}

#[test]
fn test_binary_starts_and_handles_eof_stdio() {
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let mut child = Command::new(bin_path)
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("Failed to spawn zshcs --stdio");

    // Drop stdin to send EOF
    drop(child.stdin.take());

    let status = child.wait().expect("Failed to wait on child");
    assert!(status.success());
}

#[test]
fn test_binary_starts_and_handles_eof_default() {
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let mut child = Command::new(bin_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("Failed to spawn zshcs (default)");

    // Drop stdin to send EOF
    drop(child.stdin.take());

    let status = child.wait().expect("Failed to wait on child");
    assert!(status.success());
}

#[test]
fn test_cli_struct_traits() {
    let cli1 = Cli {
        stdio: true,
        command: None,
    };
    let cli2 = cli1.clone();
    assert_eq!(cli1, cli2);
    let debug_str = format!("{cli1:?}");
    assert!(debug_str.contains("Cli"));
    assert!(debug_str.contains("stdio: true"));
}

#[test]
fn test_cli_parse_duplicate_stdio_flag() {
    let res = Cli::try_parse_from(["zshcs", "--stdio", "--stdio"]);
    assert!(res.is_err());
    assert_eq!(res.unwrap_err().kind(), ErrorKind::ArgumentConflict);
}

#[test]
fn test_cli_parse_case_sensitivity() {
    let res = Cli::try_parse_from(["zshcs", "--STDIO"]);
    assert!(res.is_err());
    assert_eq!(res.unwrap_err().kind(), ErrorKind::UnknownArgument);
}

#[test]
fn test_cli_parse_empty_positional_arg() {
    let res = Cli::try_parse_from(["zshcs", ""]);
    assert!(res.is_err());
    assert_eq!(res.unwrap_err().kind(), ErrorKind::UnknownArgument);
}

#[test]
fn test_binary_help_with_stdio_flag() {
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let output = Command::new(bin_path)
        .args(["--stdio", "--help"])
        .output()
        .expect("Failed to execute zshcs --stdio --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("zshcs"));
}

#[test]
fn test_binary_version_with_stdio_flag() {
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let output = Command::new(bin_path)
        .args(["--stdio", "--version"])
        .output()
        .expect("Failed to execute zshcs --stdio --version");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn test_binary_invalid_flag_with_stdio() {
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let output = Command::new(bin_path)
        .args(["--stdio", "--unknown-flag"])
        .output()
        .expect("Failed to execute zshcs --stdio --unknown-flag");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected argument"));
}

#[cfg(unix)]
#[test]
fn test_cli_parse_non_utf8_os_argument() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    // Construct an invalid UTF-8 OsString
    let invalid_utf8_arg = OsString::from_vec(vec![0x66, 0x6f, 0x80, 0x6f]); // "fo\x80o"
    let args = [OsString::from("zshcs"), invalid_utf8_arg];
    let res = Cli::try_parse_from(args);
    assert!(
        res.is_err(),
        "Invalid UTF-8 arguments must result in an error, not panic"
    );
}

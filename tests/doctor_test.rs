use clap::Parser;
use std::process::Command;
use tempfile::tempdir;
use zshcs::cli::{Cli, Commands};
use zshcs::doctor::{
    CheckResult, CheckStatus, DoctorReport, check_cache_directory, check_capture_dry_run,
    check_zpty_module, check_zsh_executable, check_zutil_module, run_doctor, run_doctor_checks,
    run_doctor_with_writer,
};

#[test]
fn test_cli_parse_doctor_subcommand() {
    let args = ["zshcs", "doctor"];
    let cli = Cli::try_parse_from(args).expect("Should parse 'doctor' subcommand");
    assert_eq!(cli.command, Some(Commands::Doctor));
    assert!(!cli.stdio);
}

#[test]
fn test_binary_help_includes_doctor_subcommand() {
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let output = Command::new(bin_path)
        .arg("--help")
        .output()
        .expect("Failed to execute zshcs --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("doctor"),
        "Help output should include 'doctor' subcommand"
    );
}

#[test]
fn test_binary_doctor_subcommand_execution() {
    let bin_path = env!("CARGO_BIN_EXE_zshcs");
    let output = Command::new(bin_path)
        .arg("doctor")
        .output()
        .expect("Failed to execute zshcs doctor");

    assert!(
        output.status.success(),
        "zshcs doctor should exit with code 0 on healthy environment"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("zshcs doctor - Environment Health Check"));
    assert!(stdout.contains("Zsh Executable"));
    assert!(stdout.contains("Zpty Module"));
    assert!(stdout.contains("Zutil Module"));
    assert!(stdout.contains("Cache Directory"));
    assert!(stdout.contains("Capture Script Dry-Run"));
    assert!(stdout.contains("5/5 checks passed"));
}

#[test]
fn test_check_zsh_executable_success() {
    let res = check_zsh_executable(None);
    assert_eq!(res.status, CheckStatus::Pass);
    assert_eq!(res.name, "Zsh Executable");
    assert!(res.message.contains("zsh"));
    assert!(res.is_pass());
    assert!(!res.is_fail());
    assert!(!res.is_warn());
}

#[test]
fn test_check_zsh_executable_nonexistent() {
    let res = check_zsh_executable(Some("nonexistent_zsh_binary_12345"));
    assert_eq!(res.status, CheckStatus::Fail);
    assert_eq!(res.name, "Zsh Executable");
    assert!(res.is_fail());
    assert!(!res.is_pass());
}

#[test]
fn test_check_zpty_module_success() {
    let res = check_zpty_module(None);
    assert_eq!(res.status, CheckStatus::Pass);
    assert_eq!(res.name, "Zpty Module");
    assert!(res.message.contains("zsh/zpty"));
}

#[test]
fn test_check_zpty_module_nonexistent_binary() {
    let res = check_zpty_module(Some("nonexistent_zsh_binary_12345"));
    assert_eq!(res.status, CheckStatus::Fail);
    assert_eq!(res.name, "Zpty Module");
}

#[test]
fn test_check_zutil_module_success() {
    let res = check_zutil_module(None);
    assert_eq!(res.status, CheckStatus::Pass);
    assert_eq!(res.name, "Zutil Module");
    assert!(res.message.contains("zsh/zutil"));
}

#[test]
fn test_check_zutil_module_nonexistent_binary() {
    let res = check_zutil_module(Some("nonexistent_zsh_binary_12345"));
    assert_eq!(res.status, CheckStatus::Fail);
    assert_eq!(res.name, "Zutil Module");
}

#[test]
fn test_check_cache_directory_custom_tempdir() {
    let temp_dir = tempdir().unwrap();
    let cache_path = temp_dir.path().join("sub/dir/cache");

    let res = check_cache_directory(Some(&cache_path));
    assert_eq!(res.status, CheckStatus::Pass);
    assert_eq!(res.name, "Cache Directory");
    assert!(cache_path.exists());
}

#[test]
fn test_check_cache_directory_default_env() {
    let temp_dir = tempdir().unwrap();
    let cache_path = temp_dir.path().join("env_cache");
    // SAFETY: Single-threaded test execution scope for env var test
    unsafe {
        std::env::set_var("ZSHCS_CACHE_DIR", &cache_path);
    }

    let res = check_cache_directory(None);
    assert_eq!(res.status, CheckStatus::Pass);
    assert_eq!(res.name, "Cache Directory");
    assert!(cache_path.exists());

    unsafe {
        std::env::remove_var("ZSHCS_CACHE_DIR");
    }
}

#[test]
fn test_check_cache_directory_permission_failure() {
    let temp_dir = tempdir().unwrap();
    // Create a regular file where directory creation should fail
    let file_path = temp_dir.path().join("blocking_file");
    std::fs::write(&file_path, b"dummy").unwrap();

    let invalid_cache_path = file_path.join("uncreatable_subfolder");
    let res = check_cache_directory(Some(&invalid_cache_path));
    assert_eq!(res.status, CheckStatus::Fail);
    assert_eq!(res.name, "Cache Directory");
}

#[test]
fn test_check_capture_dry_run_success() {
    let temp_dir = tempdir().unwrap();
    let res = check_capture_dry_run(None, Some(temp_dir.path()));
    assert_eq!(res.status, CheckStatus::Pass);
    assert_eq!(res.name, "Capture Script Dry-Run");
    assert!(
        res.message
            .contains("Interactive completion engine responded")
    );
}

#[test]
fn test_check_capture_dry_run_nonexistent_binary() {
    let temp_dir = tempdir().unwrap();
    let res = check_capture_dry_run(Some("nonexistent_zsh_binary_12345"), Some(temp_dir.path()));
    assert_eq!(res.status, CheckStatus::Fail);
    assert_eq!(res.name, "Capture Script Dry-Run");
}

#[test]
fn test_doctor_report_rendering_all_pass() {
    let checks = vec![
        CheckResult::new("Zsh Executable", CheckStatus::Pass, "zsh 5.9"),
        CheckResult::new("Zpty Module", CheckStatus::Pass, "module loaded"),
    ];
    let report = DoctorReport::new(checks);
    assert!(report.is_all_passed());
    assert_eq!(report.pass_count(), 2);
    assert_eq!(report.fail_count(), 0);
    assert_eq!(report.warn_count(), 0);

    let mut output = Vec::new();
    report.render(&mut output).unwrap();
    let rendered = String::from_utf8(output).unwrap();

    assert!(rendered.contains("[✓] Zsh Executable: zsh 5.9"));
    assert!(rendered.contains("[✓] Zpty Module: module loaded"));
    assert!(rendered.contains("Result: 2/2 checks passed"));
}

#[test]
fn test_doctor_report_rendering_with_failures_and_warnings() {
    let checks = vec![
        CheckResult::new("Zsh Executable", CheckStatus::Pass, "zsh 5.9"),
        CheckResult::new("Zpty Module", CheckStatus::Fail, "module not found"),
        CheckResult::new("Custom Check", CheckStatus::Warn, "deprecated config"),
    ];
    let report = DoctorReport::new(checks);
    assert!(!report.is_all_passed());
    assert_eq!(report.pass_count(), 1);
    assert_eq!(report.fail_count(), 1);
    assert_eq!(report.warn_count(), 1);

    let mut output = Vec::new();
    report.render(&mut output).unwrap();
    let rendered = String::from_utf8(output).unwrap();

    assert!(rendered.contains("[✓] Zsh Executable: zsh 5.9"));
    assert!(rendered.contains("[✗] Zpty Module: module not found"));
    assert!(rendered.contains("[!] Custom Check: deprecated config"));
    assert!(rendered.contains("Result: 1/3 checks passed, 1 failed, 1 warning(s)"));
}

#[test]
fn test_doctor_report_rendering_failure_without_warnings() {
    let checks = vec![
        CheckResult::new("Zsh Executable", CheckStatus::Pass, "zsh 5.9"),
        CheckResult::new("Zpty Module", CheckStatus::Fail, "module not found"),
    ];
    let report = DoctorReport::new(checks);
    assert!(!report.is_all_passed());
    assert_eq!(report.pass_count(), 1);
    assert_eq!(report.fail_count(), 1);
    assert_eq!(report.warn_count(), 0);

    let mut output = Vec::new();
    report.render(&mut output).unwrap();
    let rendered = String::from_utf8(output).unwrap();

    assert!(rendered.contains("[✓] Zsh Executable: zsh 5.9"));
    assert!(rendered.contains("[✗] Zpty Module: module not found"));
    assert!(
        rendered.contains(
            "Result: 1/2 checks passed, 1 failed. Please address the failed items above."
        )
    );
}

#[test]
fn test_check_cache_directory_empty_env_vars() {
    let temp_dir = tempdir().unwrap();
    let home_path = temp_dir.path().join("home");
    std::fs::create_dir_all(&home_path).unwrap();

    // Set empty strings for ZSHCS_CACHE_DIR and XDG_CACHE_HOME, valid HOME
    unsafe {
        std::env::set_var("ZSHCS_CACHE_DIR", "");
        std::env::set_var("XDG_CACHE_HOME", "");
        std::env::set_var("HOME", &home_path);
    }

    let res = check_cache_directory(None);
    assert_eq!(res.status, CheckStatus::Pass);
    assert_eq!(res.name, "Cache Directory");
    assert!(home_path.join(".cache/zshcs/zsh").exists());

    unsafe {
        std::env::remove_var("ZSHCS_CACHE_DIR");
        std::env::remove_var("XDG_CACHE_HOME");
    }
}

#[test]
fn test_check_status_symbols_and_labels() {
    assert_eq!(CheckStatus::Pass.symbol(), "[✓]");
    assert_eq!(CheckStatus::Pass.label(), "PASS");

    assert_eq!(CheckStatus::Warn.symbol(), "[!]");
    assert_eq!(CheckStatus::Warn.label(), "WARN");

    assert_eq!(CheckStatus::Fail.symbol(), "[✗]");
    assert_eq!(CheckStatus::Fail.label(), "FAIL");
}

#[test]
fn test_run_doctor_checks_returns_five_checks() {
    let report = run_doctor_checks();
    assert_eq!(report.checks.len(), 5);
    assert_eq!(report.checks[0].name, "Zsh Executable");
    assert_eq!(report.checks[1].name, "Zpty Module");
    assert_eq!(report.checks[2].name, "Zutil Module");
    assert_eq!(report.checks[3].name, "Cache Directory");
    assert_eq!(report.checks[4].name, "Capture Script Dry-Run");
    assert!(report.is_all_passed());
}

#[test]
fn test_run_doctor_with_writer_writes_and_returns_true() {
    let mut buffer = Vec::new();
    let success = run_doctor_with_writer(&mut buffer);
    assert!(success);

    let output = String::from_utf8(buffer).unwrap();
    assert!(output.contains("zshcs doctor - Environment Health Check"));
    assert!(output.contains("5/5 checks passed"));
}

#[test]
fn test_run_doctor_function_returns_zero_exit_code() {
    let code = run_doctor();
    assert_eq!(code, 0);
}

#[test]
fn test_doctor_report_warning_only_rendering() {
    let checks = vec![
        CheckResult::new("Zsh Executable", CheckStatus::Pass, "zsh 5.9"),
        CheckResult::new("Zpty Module", CheckStatus::Pass, "module loaded"),
        CheckResult::new(
            "Cache Directory",
            CheckStatus::Warn,
            "sub-optimal directory",
        ),
    ];
    let report = DoctorReport::new(checks);
    assert!(report.is_all_passed());
    assert_eq!(report.pass_count(), 2);
    assert_eq!(report.fail_count(), 0);
    assert_eq!(report.warn_count(), 1);

    let mut output = Vec::new();
    report.render(&mut output).unwrap();
    let rendered = String::from_utf8(output).unwrap();

    assert!(rendered.contains("[!] Cache Directory: sub-optimal directory"));
    assert!(
        rendered.contains(
            "Result: 2/3 checks passed, 1 warning(s). Your environment is ready for zshcs!"
        )
    );
}

#[test]
fn test_doctor_report_single_failure_zero_passes() {
    let checks = vec![CheckResult::new(
        "Zsh Executable",
        CheckStatus::Fail,
        "missing zsh",
    )];
    let report = DoctorReport::new(checks);
    assert!(!report.is_all_passed());
    assert_eq!(report.pass_count(), 0);
    assert_eq!(report.fail_count(), 1);
    assert_eq!(report.warn_count(), 0);

    let mut output = Vec::new();
    report.render(&mut output).unwrap();
    let rendered = String::from_utf8(output).unwrap();

    assert!(rendered.contains("[✗] Zsh Executable: missing zsh"));
    assert!(
        rendered.contains(
            "Result: 0/1 checks passed, 1 failed. Please address the failed items above."
        )
    );
}

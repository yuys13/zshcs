use std::process::Command;
use std::sync::Mutex;
use tempfile::tempdir;
use zshcs::doctor::{
    CheckStatus, check_cache_directory, check_capture_dry_run, check_zpty_module,
    check_zsh_executable, check_zutil_module, run_doctor_with_writer,
};

static ENV_MUTEX: Mutex<()> = Mutex::new(());

struct EnvVarGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set<V: AsRef<std::ffi::OsStr>>(key: &'static str, value: V) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: Tests calling this hold ENV_MUTEX.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: Tests calling this hold ENV_MUTEX.
        unsafe {
            match &self.original {
                Some(val) => std::env::set_var(self.key, val),
                None => std::env::remove_var(self.key),
            }
        }
    }
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
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
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
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let temp_dir = tempdir().unwrap();
    let cache_path = temp_dir.path().join("env_cache");
    let _env_guard = EnvVarGuard::set("ZSHCS_CACHE_DIR", &cache_path);

    let res = check_cache_directory(None);
    assert_eq!(res.status, CheckStatus::Pass);
    assert_eq!(res.name, "Cache Directory");
    assert!(cache_path.exists());
}

#[test]
fn test_check_cache_directory_permission_failure() {
    let temp_dir = tempdir().unwrap();
    let file_path = temp_dir.path().join("blocking_file");
    std::fs::write(&file_path, b"dummy").unwrap();

    let invalid_cache_path = file_path.join("uncreatable_subfolder");
    let res = check_cache_directory(Some(&invalid_cache_path));
    assert_eq!(res.status, CheckStatus::Fail);
    assert_eq!(res.name, "Cache Directory");
}

#[test]
fn test_check_capture_dry_run_nonexistent_binary() {
    let temp_dir = tempdir().unwrap();
    let res = check_capture_dry_run(Some("nonexistent_zsh_binary_12345"), Some(temp_dir.path()));
    assert_eq!(res.status, CheckStatus::Fail);
    assert_eq!(res.name, "Capture Script Dry-Run");
}

#[test]
fn test_check_cache_directory_empty_env_vars() {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let temp_dir = tempdir().unwrap();
    let home_path = temp_dir.path().join("home");
    std::fs::create_dir_all(&home_path).unwrap();

    let _zshcs_cache_guard = EnvVarGuard::set("ZSHCS_CACHE_DIR", "");
    let _xdg_cache_guard = EnvVarGuard::set("XDG_CACHE_HOME", "");
    let _home_guard = EnvVarGuard::set("HOME", &home_path);

    let res = check_cache_directory(None);
    assert_eq!(res.status, CheckStatus::Pass);
    assert_eq!(res.name, "Cache Directory");
    assert!(home_path.join(".cache/zshcs/zsh").exists());
}

#[test]
fn test_run_doctor_with_writer_writes_and_returns_true() {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let mut buffer = Vec::new();
    let success = run_doctor_with_writer(&mut buffer);
    assert!(success);

    let output = String::from_utf8(buffer).unwrap();
    assert!(output.contains("zshcs doctor - Environment Health Check"));
    assert!(output.contains("5/5 checks passed"));
}

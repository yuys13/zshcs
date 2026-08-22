pub mod cli;
pub mod completion;
pub mod config;
pub mod diagnostics;
pub mod doctor;
pub mod document;
pub mod error;
pub mod logging;
pub mod server;

pub use cli::{Cli, Commands};
pub use completion::{CAPTURE_ZSH, ZPTYRC_ZSH, infer_completion_kind};
pub use config::{Config, ExperimentalConfig, extract_experimental_diagnostics};
pub use diagnostics::{
    DEFAULT_SYNTAX_CHECK_TIMEOUT, check_syntax, check_syntax_with_timeout, parse_diagnostic_line,
    parse_diagnostics,
};
pub use doctor::{
    CheckResult, CheckStatus, DoctorReport, check_cache_directory, check_capture_dry_run,
    check_zpty_module, check_zsh_executable, check_zutil_module, run_doctor, run_doctor_checks,
    run_doctor_with_writer,
};
pub use document::{DocumentError, DocumentManager, DocumentState};
pub use error::{ZshcsError, ZshcsResult};
pub use logging::{create_env_filter, init_logging, try_init_logging};
pub use server::Backend;

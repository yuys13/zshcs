pub mod cli;
pub mod completion;
pub mod config;
pub mod definition;
pub mod diagnostics;
pub mod doctor;
pub mod document;
pub mod error;
pub mod hover;
pub mod logging;
pub mod server;

pub use cli::{Cli, Commands};
pub use completion::{CAPTURE_ZSH, ZPTYRC_ZSH, infer_completion_kind};
pub use config::{
    Config, ExperimentalConfig, extract_experimental_definition, extract_experimental_diagnostics,
    extract_experimental_hover,
};
pub use definition::{
    DeclarationToken, DefinitionTarget, StatementSpan, byte_to_utf16_col,
    extract_source_path_at_position, extract_word_and_target_at_position, find_definition,
    is_func_ident_char, is_ident_char, is_var_ident_char, resolve_source_path,
    scan_function_definitions, scan_variable_definitions, split_declaration_tokens,
    split_line_statements, utf16_col_to_byte,
};
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
pub use hover::{
    DEFAULT_HOVER_MAN_TIMEOUT, clean_man_text, extract_word_at_position,
    get_builtin_or_reserved_doc, get_hover_info, get_hover_info_with_timeout, get_man_page,
    is_word_char,
};
pub use logging::{create_env_filter, init_logging, try_init_logging};
pub use server::Backend;

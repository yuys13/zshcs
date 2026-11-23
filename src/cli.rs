//! Command line interface definition and argument parsing for `zshcs`.

use clap::{Parser, Subcommand};

/// Command line arguments for `zshcs`.
///
/// `zshcs` is a Language Server Protocol (LSP) implementation providing
/// intelligent completion features for the Zsh shell.
#[derive(Parser, Debug, Clone, PartialEq, Eq)]
#[command(
    name = "zshcs",
    author,
    version,
    about = "Zsh Completion Server - Language Server Protocol implementation for Zsh",
    long_about = "zshcs is a Language Server Protocol (LSP) implementation providing intelligent autocompletion for the Zsh shell."
)]
pub struct Cli {
    /// Use stdio for Language Server Protocol communication (default)
    #[arg(
        long,
        help = "Use stdio for Language Server Protocol communication (default)"
    )]
    pub stdio: bool,

    /// Optional subcommand for future extensions
    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// Supported subcommands for `zshcs`.
#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum Commands {
    /// Check health of the environment, zsh installation, modules, and cache
    Doctor,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use rstest::rstest;

    #[rstest]
    #[case(&["zshcs"], false, None)]
    #[case(&["zshcs", "--stdio"], true, None)]
    #[case(&["zshcs", "doctor"], false, Some(Commands::Doctor))]
    fn test_cli_valid_arguments(
        #[case] args: &[&str],
        #[case] expected_stdio: bool,
        #[case] expected_command: Option<Commands>,
    ) {
        let cli = Cli::try_parse_from(args).expect("Should parse valid CLI arguments");
        assert_eq!(cli.stdio, expected_stdio);
        assert_eq!(cli.command, expected_command);
    }

    #[rstest]
    #[case(&["zshcs", "--help"], ErrorKind::DisplayHelp)]
    #[case(&["zshcs", "-h"], ErrorKind::DisplayHelp)]
    #[case(&["zshcs", "--version"], ErrorKind::DisplayVersion)]
    #[case(&["zshcs", "-V"], ErrorKind::DisplayVersion)]
    #[case(&["zshcs", "--invalid-option"], ErrorKind::UnknownArgument)]
    #[case(&["zshcs", "invalid-subcommand"], ErrorKind::InvalidSubcommand)]
    #[case(&["zshcs", "--stdio", "--stdio"], ErrorKind::ArgumentConflict)]
    #[case(&["zshcs", "--STDIO"], ErrorKind::UnknownArgument)]
    #[case(&["zshcs", ""], ErrorKind::InvalidSubcommand)]
    fn test_cli_error_arguments(#[case] args: &[&str], #[case] expected_kind: ErrorKind) {
        let err = Cli::try_parse_from(args).unwrap_err();
        assert_eq!(err.kind(), expected_kind);
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

        let cli_doc = Cli {
            stdio: false,
            command: Some(Commands::Doctor),
        };
        let cli_doc_clone = cli_doc.clone();
        assert_eq!(cli_doc, cli_doc_clone);
        assert!(format!("{cli_doc:?}").contains("Doctor"));
    }

    #[cfg(unix)]
    #[test]
    fn test_cli_parse_non_utf8_os_argument() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let invalid_utf8_arg = OsString::from_vec(vec![0x66, 0x6f, 0x80, 0x6f]); // "fo\x80o"
        let args = [OsString::from("zshcs"), invalid_utf8_arg];
        let res = Cli::try_parse_from(args);
        assert!(
            res.is_err(),
            "Invalid UTF-8 arguments must result in an error, not panic"
        );
    }
}

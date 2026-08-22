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

    #[test]
    fn test_cli_default_arguments() {
        let args = ["zshcs"];
        let cli = Cli::try_parse_from(args).expect("Should parse with no arguments");
        assert!(!cli.stdio);
        assert!(cli.command.is_none());
    }

    #[test]
    fn test_cli_stdio_flag() {
        let args = ["zshcs", "--stdio"];
        let cli = Cli::try_parse_from(args).expect("Should parse with --stdio");
        assert!(cli.stdio);
        assert!(cli.command.is_none());
    }

    #[test]
    fn test_cli_doctor_subcommand() {
        let args = ["zshcs", "doctor"];
        let cli = Cli::try_parse_from(args).expect("Should parse doctor subcommand");
        assert!(!cli.stdio);
        assert_eq!(cli.command, Some(Commands::Doctor));
    }

    #[test]
    fn test_cli_help_flag() {
        let err = Cli::try_parse_from(["zshcs", "--help"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);

        let err_short = Cli::try_parse_from(["zshcs", "-h"]).unwrap_err();
        assert_eq!(err_short.kind(), ErrorKind::DisplayHelp);
    }

    #[test]
    fn test_cli_version_flag() {
        let err = Cli::try_parse_from(["zshcs", "--version"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::DisplayVersion);

        let err_short = Cli::try_parse_from(["zshcs", "-V"]).unwrap_err();
        assert_eq!(err_short.kind(), ErrorKind::DisplayVersion);
    }

    #[test]
    fn test_cli_invalid_argument() {
        let err = Cli::try_parse_from(["zshcs", "--invalid-option"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UnknownArgument);
    }

    #[test]
    fn test_cli_unexpected_positional_argument() {
        let err = Cli::try_parse_from(["zshcs", "invalid-subcommand"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidSubcommand);
    }
}

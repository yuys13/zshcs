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
    fn test_cli_error_arguments(#[case] args: &[&str], #[case] expected_kind: ErrorKind) {
        let err = Cli::try_parse_from(args).unwrap_err();
        assert_eq!(err.kind(), expected_kind);
    }
}

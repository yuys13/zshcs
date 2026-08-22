use clap::Parser;
use tower_lsp::{LspService, Server};
use zshcs::{Backend, Cli, Commands, init_logging, run_doctor};

#[tokio::main]
async fn main() {
    init_logging();
    let cli = Cli::parse();

    if let Some(Commands::Doctor) = cli.command {
        let exit_code = run_doctor();
        std::process::exit(exit_code);
    }

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(|client| {
        Backend::new(client).unwrap_or_else(|err| {
            tracing::error!("Failed to initialize zshcs backend: {err}");
            eprintln!("Failed to initialize zshcs backend: {err}");
            std::process::exit(1);
        })
    });
    Server::new(stdin, stdout, socket).serve(service).await;
}

use clap::Parser;
use tower_lsp::{LspService, Server};
use zshcs::{Backend, Cli, init_logging};

#[tokio::main]
async fn main() {
    init_logging();
    let _cli = Cli::parse();

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

use anyhow::Result;
use clap::Parser;
use tower_lsp::{LspService, Server};
use tracing_subscriber::EnvFilter;

mod backend;
mod schema;
mod tls;
mod validate;

#[derive(Parser, Debug)]
#[command(name = "jylsp", version, about = "JSON/YAML $schema validator LSP")]
struct Args {
    /// Serve LSP over stdin/stdout (for Vim/YouCompleteMe, etc.)
    #[arg(long)]
    stdio: bool,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tower_lsp=info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    // Reqwest is compiled with rustls "no-provider" to avoid C/cc + OpenSSL.
    // Install a pure-Rust crypto provider (RustCrypto) early.
    tls::ensure_rustls_rustcrypto_provider();

    let args = Args::parse();
    if !args.stdio {
        eprintln!("error: only --stdio is supported");
        std::process::exit(2);
    }

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(|client| backend::Backend::new(client));
    Server::new(stdin, stdout, socket).serve(service).await;

    Ok(())
}

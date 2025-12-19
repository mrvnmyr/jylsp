use clap::Parser;
use tokio::io::{stdin, stdout};
use tower_lsp::{LspService, Server};
use tracing_subscriber::EnvFilter;

mod backend;
mod schema;
mod text_index;
mod validate;
mod yaml_json;

use backend::Backend;
use schema::ServerConfig;

/// JSON/YAML `$schema` validator LSP server.
#[derive(Debug, Parser)]
#[command(name = "jylsp", version, about)]
struct Args {
    /// Run the language server over stdio (required by most editors).
    #[arg(long)]
    stdio: bool,

    /// Enable `format` checks in JSON Schema validation (off by default).
    #[arg(long, default_value_t = false)]
    validate_formats: bool,

    /// Maximum number of diagnostics per document.
    #[arg(long, default_value_t = 100)]
    max_errors: usize,

    /// Maximum number of compiled schemas kept in memory.
    #[arg(long, default_value_t = 64)]
    schema_cache_size: usize,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    // Only log to stderr; stdout is reserved for the LSP wire protocol.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    if !args.stdio {
        anyhow::bail!("This server is intended to be run with --stdio");
    }

    let cfg = ServerConfig {
        validate_formats: args.validate_formats,
        max_errors: args.max_errors,
        schema_cache_size: args.schema_cache_size,
    };

    let handle = tokio::runtime::Handle::current();
    let (service, socket) = LspService::new(move |client| Backend::new(client, cfg, handle.clone()));
    Server::new(stdin(), stdout(), socket).serve(service).await;

    Ok(())
}

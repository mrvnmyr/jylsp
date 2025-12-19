use anyhow::Result;
use clap::Parser;
use tokio::runtime::Handle;
use tower_lsp::{LspService, Server};
use tracing_subscriber::EnvFilter;

mod backend;
mod schema;
mod text_index;
mod tls;
mod validate;
mod yaml_json;

fn debug_enabled() -> bool {
    std::env::var_os("DEBUG").is_some()
}

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

    // Reqwest is compiled with rustls "no-provider" so we must install a provider
    // before any TLS client config is constructed.
    tls::ensure_rustls_rustcrypto_provider();

    let args = Args::parse();
    if !args.stdio {
        eprintln!("error: only --stdio is supported");
        std::process::exit(2);
    }

    let cfg = schema::ServerConfig {
        validate_formats: true,
        max_errors: 64,
        schema_cache_size: 128,
    };
    let handle = Handle::current();

    if debug_enabled() {
        eprintln!("[DEBUG] starting jylsp --stdio");
        eprintln!(
            "[DEBUG] cfg: validate_formats={} max_errors={} schema_cache_size={}",
            cfg.validate_formats, cfg.max_errors, cfg.schema_cache_size
        );
    }

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(move |client| {
        if debug_enabled() {
            eprintln!("[DEBUG] LSP service created");
        }
        backend::Backend::new(client, cfg, handle.clone())
    });
    Server::new(stdin, stdout, socket).serve(service).await;

    Ok(())
}

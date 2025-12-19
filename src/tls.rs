use std::sync::Once;

static INIT: Once = Once::new();

fn dprint(msg: &str) {
    if std::env::var_os("DEBUG").is_some() {
        eprintln!("[DEBUG] {msg}");
    }
}

/// Install Rustls' process-wide CryptoProvider (pure Rust, via RustCrypto).
///
/// Reqwest is configured with a rustls "*-no-provider" feature, so the application
/// must install a provider before the first TLS connector/config is built.
pub fn ensure_rustls_rustcrypto_provider() {
    INIT.call_once(|| {
        dprint("tls: installing rustls-rustcrypto CryptoProvider");
        match rustls_rustcrypto::provider().install_default() {
            Ok(()) => dprint("tls: provider installed"),
            Err(_already_installed) => dprint("tls: provider already installed"),
        }
    });
}

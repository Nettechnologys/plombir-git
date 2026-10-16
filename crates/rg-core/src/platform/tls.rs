//! The process-wide rustls crypto provider (card_64adc63cdd02).
//!
//! Two providers are compiled in: `aws-lc-rs` arrives through `tokio-rustls`,
//! `reqwest` and `axum-server`, `ring` through `ldap3`, `lettre` and `reqwest`.
//! With both present rustls refuses to guess, and every `ServerConfig::builder()`
//! / `ClientConfig::builder()` that relies on the process default panics —
//! the built-in HTTPS listener at startup, and `ldap3` on the first `ldaps://`
//! login. Choosing one here, before any TLS is set up, is what those builders
//! need; a component that picks its provider explicitly is unaffected.

use std::sync::Once;

/// Make `aws-lc-rs` — rustls' own default — the process provider, once.
///
/// Idempotent and cheap after the first call, so every entry point that may
/// be the first to need TLS calls it: `plombir-git` at startup, the HTTP server
/// for an embedder without that `main`, the LDAP client before it connects.
pub fn install_crypto_provider() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        if tokio_rustls::rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .is_err()
        {
            // Something chose a provider before us; that choice stands, and
            // the builders have the default they need either way.
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_rustls::rustls::crypto::CryptoProvider;
    use tokio_rustls::rustls::ServerConfig;

    /// The precondition is part of the test: with both providers in the tree
    /// and none chosen, the builder panics. Only checked while nothing in this
    /// process has chosen one yet — nextest runs each test alone, `cargo test`
    /// may not.
    #[test]
    fn a_tls_builder_works_once_the_provider_is_chosen() {
        if CryptoProvider::get_default().is_none() {
            let ambiguous = std::panic::catch_unwind(|| drop(ServerConfig::builder()));
            assert!(
                ambiguous.is_err(),
                "a single provider in the tree: this guard is no longer needed"
            );
        }
        install_crypto_provider();
        install_crypto_provider();
        drop(ServerConfig::builder());
        assert!(CryptoProvider::get_default().is_some());
    }
}

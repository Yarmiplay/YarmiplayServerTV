pub mod acme;
pub mod ip;
pub mod upnp;

#[cfg(test)]
pub mod testutil;

/// reqwest is built with rustls-*-no-provider, so a process-wide CryptoProvider
/// must exist before any client is built (even for plain-HTTP router calls).
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Every reqwest client starts here so the crypto provider is always present.
pub fn client_builder() -> reqwest::ClientBuilder {
    install_crypto_provider();
    reqwest::Client::builder()
}

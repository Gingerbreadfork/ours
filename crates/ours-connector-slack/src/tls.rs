//! TLS for both the Web API and Socket Mode: rustls on aws-lc-rs, the one
//! crypto provider in the workspace, with the bundled Mozilla roots.

use std::sync::Arc;

pub fn client_config() -> Result<rustls::ClientConfig, rustls::Error> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_config_builds_without_alpn_so_hyper_can_set_it() {
        let config = super::client_config().unwrap();
        assert!(config.alpn_protocols.is_empty());
    }
}

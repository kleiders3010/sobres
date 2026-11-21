//! The one way this crate makes HTTPS clients: rustls with the bundled WebPKI roots
//! (webpki-roots), the same on every platform. reqwest's default verifier
//! (rustls-platform-verifier) needs JNI setup on Android and crashed there (2026-09-25).

use std::sync::Arc;
use std::time::Duration;

pub fn tls_config() -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("default TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// An HTTPS client with this timeout.
pub fn client(timeout: Duration) -> Option<reqwest::Client> {
    reqwest::Client::builder().tls_backend_preconfigured(tls_config()).timeout(timeout).build().ok()
}

/// A response body of at most `max` bytes, read as it arrives: a broken or taken-over server can't make the app
/// hold an endless answer (safety audit round 4, T15b). None if it is longer or fails.
pub async fn bytes_capped(mut resp: reqwest::Response, max: usize) -> Option<Vec<u8>> {
    if resp.content_length().is_some_and(|l| l as usize > max) {
        return None;
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.ok()? {
        if out.len() + chunk.len() > max {
            return None;
        }
        out.extend_from_slice(&chunk);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn builds_without_platform_setup() {
        assert!(super::client(std::time::Duration::from_secs(5)).is_some());
    }
}

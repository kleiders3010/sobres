//! TLS for helper relays without certificate authorities (D51, decided 2026-09-25).
//!
//! A desktop relaying for others has no domain name, and home connections rarely get the
//! fixed ports (80/443) that ACME needs. Instead, a helper relay presents a **self-signed
//! certificate made from its own iroh endpoint key**, and clients accept it only when the
//! certificate's public key is exactly the one in that helper's announcement (which is
//! signed by the same key). No CA, any port, nothing extra to publish.
//!
//! One verifier serves every relay connection of an endpoint: relays by IP address with a
//! pinned key are checked against the pin; everything else (n0's relays, hosted relays
//! with domain names) goes through the standard WebPKI roots.
//!
//! The key is read from the certificate's real SubjectPublicKeyInfo (parsed, not searched
//! for), and rustls then checks the handshake signature against that same key, so only
//! the holder of the pinned private key can pass.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, RwLock};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error, SignatureScheme};

/// DER prefix of an Ed25519 SubjectPublicKeyInfo (RFC 8410); the 32 key bytes follow.
const ED25519_SPKI_PREFIX: [u8; 12] = [0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];

/// Relay IP address -> the helper's endpoint key (from its signed announcement).
pub type Pins = Arc<RwLock<HashMap<IpAddr, [u8; 32]>>>;

#[derive(Debug)]
struct PinnedVerifier {
    standard: Arc<WebPkiServerVerifier>,
    pins: Pins,
    provider: Arc<CryptoProvider>,
}

/// The Ed25519 key in a certificate's SubjectPublicKeyInfo, if that's what it holds.
pub fn cert_ed25519_key(cert: &CertificateDer<'_>) -> Option<[u8; 32]> {
    let ee = webpki::EndEntityCert::try_from(cert).ok()?;
    let spki = ee.subject_public_key_info();
    let b = spki.as_ref();
    (b.len() == 44 && b[..12] == ED25519_SPKI_PREFIX).then(|| b[12..].try_into().expect("32"))
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        if let ServerName::IpAddress(ip) = server_name {
            let ip: IpAddr = (*ip).into();
            let pin = self.pins.read().expect("pins lock").get(&ip).copied();
            if let Some(pin) = pin {
                return match cert_ed25519_key(end_entity) {
                    Some(k) if k == pin => Ok(ServerCertVerified::assertion()),
                    _ => Err(Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure)),
                };
            }
        }
        self.standard.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// The relay TLS configuration for an endpoint: pinned helper relays, standard roots
/// for everything else.
pub fn ca_tls_config(pins: Pins) -> iroh::tls::CaTlsConfig {
    iroh::tls::CaTlsConfig::custom_server_cert_verifier(Arc::new(move |provider: Arc<CryptoProvider>| {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let standard = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone()).build().map_err(std::io::Error::other)?;
        Ok(Arc::new(PinnedVerifier { standard, pins: pins.clone(), provider }) as Arc<dyn ServerCertVerifier>)
    }))
}

/// A rustls client configuration with the same verifier (for probing helper relays).
pub fn client_config(pins: Pins) -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let standard = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone()).build().expect("webpki roots");
    let verifier = Arc::new(PinnedVerifier { standard, pins, provider: provider.clone() });
    rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth()
}

/// A helper relay's certificate and key: self-signed with its endpoint key (Ed25519).
#[cfg(feature = "relay-server")]
pub fn self_signed_cert(secret: &[u8; 32]) -> Result<(CertificateDer<'static>, rustls::pki_types::PrivateKeyDer<'static>), String> {
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    // PKCS#8 v1 for an Ed25519 seed (RFC 8410).
    let mut pkcs8 = vec![0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20];
    pkcs8.extend_from_slice(secret);
    let kp = rcgen::KeyPair::try_from(pkcs8.as_slice()).map_err(|e| e.to_string())?;
    let cert = rcgen::CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?.self_signed(&kp).map_err(|e| e.to_string())?;
    Ok((cert.der().clone(), PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8))))
}

/// The helper relay's TLS server configuration.
#[cfg(feature = "relay-server")]
pub fn server_config(secret: &[u8; 32]) -> Result<rustls::ServerConfig, String> {
    let (cert, key) = self_signed_cert(secret)?;
    rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .map_err(|e| e.to_string())
}

#[cfg(all(test, feature = "relay-server"))]
mod tests {
    use super::*;

    #[test]
    fn the_certificate_carries_the_endpoint_key() {
        let secret = [7u8; 32];
        let public = *iroh::SecretKey::from_bytes(&secret).public().as_bytes();
        let (cert, _) = self_signed_cert(&secret).unwrap();
        assert_eq!(cert_ed25519_key(&cert), Some(public));
        assert!(server_config(&secret).is_ok());
    }
}

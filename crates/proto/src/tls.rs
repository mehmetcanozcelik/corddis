//! Sertifika parmak izi sabitlemeli TLS (MVP.md §5.2).
//!
//! Sunucu kendi ürettiği (self-signed) sertifikayı kullanır. İstemci CA zincirine değil,
//! davet bağlantısındaki SHA-256 parmak izine bakar: eşleşmezse bağlantıyı reddeder.
//! İmza doğrulaması (sunucunun gerçekten o sertifikanın özel anahtarına sahip olduğu)
//! normal şekilde rustls tarafından yapılır.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

pub fn fingerprint(cert_der: &[u8]) -> [u8; 32] {
    Sha256::digest(cert_der).into()
}

pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

#[derive(Debug)]
struct PinnedCert {
    expected: [u8; 32],
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if fingerprint(end_entity.as_ref()) == self.expected {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "sunucu sertifikasının parmak izi davet bağlantısındakiyle eşleşmiyor".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// Yalnızca parmak izi `expected` olan sunucuya bağlanan istemci ayarı.
pub fn pinned_client_config(expected: [u8; 32]) -> ClientConfig {
    let provider = provider();
    ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("ring sağlayıcısı varsayılan TLS sürümlerini destekler")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedCert { expected, provider }))
        .with_no_client_auth()
}

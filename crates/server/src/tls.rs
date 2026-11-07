//! Sunucu sertifikası: ilk çalıştırmada üretilir, veri klasöründe saklanır (MVP.md §10.6).

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::TlsAcceptor;

const CERT_FILE: &str = "cert.der";
const KEY_FILE: &str = "key.der";

pub struct Identity {
    pub cert_der: Vec<u8>,
    key_der: Vec<u8>,
}

impl Identity {
    pub fn fingerprint(&self) -> [u8; 32] {
        corddis_proto::tls::fingerprint(&self.cert_der)
    }

    pub fn acceptor(&self) -> Result<TlsAcceptor> {
        let cfg = rustls::ServerConfig::builder_with_provider(corddis_proto::tls::provider())
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(self.cert_der.clone())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_der.clone())),
            )
            .context("sertifika yüklenemedi")?;
        Ok(TlsAcceptor::from(Arc::new(cfg)))
    }
}

/// Varsa yükler; yoksa yeni bir self-signed sertifika üretip kaydeder.
/// Sertifika değişirse tüm davet bağlantıları geçersiz olur, bu yüzden dosyaları yedekleyin.
pub fn load_or_create(dir: &Path) -> Result<(Identity, bool)> {
    let (cp, kp) = (dir.join(CERT_FILE), dir.join(KEY_FILE));
    if cp.exists() && kp.exists() {
        return Ok((Identity { cert_der: std::fs::read(&cp)?, key_der: std::fs::read(&kp)? }, false));
    }
    let ck = rcgen::generate_simple_self_signed(vec!["corddis".to_string()])?;
    let id = Identity { cert_der: ck.cert.der().to_vec(), key_der: ck.signing_key.serialize_der() };
    write_private(&kp, &id.key_der)?;
    std::fs::write(&cp, &id.cert_der)?;
    Ok((id, true))
}

#[cfg(unix)]
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    f.write_all(data)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    std::fs::write(path, data)?;
    Ok(())
}

//! TLS for ferro (B8): rustls via axum-server, optional rcgen self-signed cert.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rcgen::{CertificateParams, DnType, KeyPair};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub enum TlsConfig {
    None,
    Rustls(Arc<rustls::ServerConfig>),
}

impl TlsConfig {
    pub fn is_https(&self) -> bool {
        !matches!(self, TlsConfig::None)
    }
}

pub struct LoadedTls {
    pub config: TlsConfig,
    /// SHA-256 fingerprint of the leaf certificate (hex pairs), when TLS is on.
    pub fingerprint: Option<String>,
}

pub fn load_pem_files(cert: &Path, key: &Path) -> std::io::Result<LoadedTls> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert_pem = std::fs::read(cert)?;
    let key_pem = std::fs::read(key)?;
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&cert_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let key = PrivateKeyDer::from_pem_slice(&key_pem)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let leaf = certs
        .first()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "no certificate"))?;
    let fp = cert_fingerprint(leaf.as_ref());
    let cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(LoadedTls {
        config: TlsConfig::Rustls(Arc::new(cfg)),
        fingerprint: Some(fp),
    })
}

/// Generate a localhost self-signed cert; optionally write PEM files under `state_dir/tls/`.
pub fn self_signed(state_dir: &Path) -> std::io::Result<(LoadedTls, PathBuf, PathBuf)> {
    let mut params = CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()])
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    params.distinguished_name.push(DnType::CommonName, "ferro");
    let key_pair = KeyPair::generate().map_err(|e| std::io::Error::other(e.to_string()))?;
    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();
    let dir = state_dir.join("tls");
    std::fs::create_dir_all(&dir)?;
    let cert_path = dir.join("self-signed.crt");
    let key_path = dir.join("self-signed.key");
    std::fs::write(&cert_path, &cert_pem)?;
    std::fs::write(&key_path, &key_pem)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
    }
    let loaded = load_pem_files(&cert_path, &key_path)?;
    Ok((loaded, cert_path, key_path))
}

pub fn cert_fingerprint(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    digest
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Line printed at startup. The same string the HTTPS tests compare to the leaf.
pub fn fingerprint_announcement(fingerprint: &str) -> String {
    format!("ferro tls fingerprint SHA-256: {fingerprint}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_signed_key_is_owner_only_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let (loaded, cert, key) = self_signed(dir.path()).unwrap();
        let fp = loaded.fingerprint.expect("fingerprint");
        assert_eq!(
            fingerprint_announcement(&fp),
            format!("ferro tls fingerprint SHA-256: {fp}")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "private key must not be group/world readable");
        }
        let again = load_pem_files(&cert, &key).unwrap();
        assert_eq!(again.fingerprint.as_deref(), Some(fp.as_str()));
    }
}

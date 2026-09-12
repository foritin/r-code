//! Self-signed identity and fingerprint pinning (R03, F3): one persistent
//! ECDSA P-256 certificate per profile under `certs/identity.pem` (0600);
//! clients trust *exactly* the fingerprint paired over the QR/manual code —
//! no CA chain, no hostname, no "accept the risk" path anywhere.

use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::WebPkiClientVerifier;
use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_rustls::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use tokio_rustls::rustls::DigitallySignedStruct;
use tokio_rustls::rustls::SignatureScheme;
use tokio_rustls::rustls::{ClientConfig, ServerConfig};

/// The daemon's persistent TLS identity.
#[derive(Debug)]
pub struct Identity {
    pub certificate_der: CertificateDer<'static>,
    pub private_key_der: PrivateKeyDer<'static>,
    /// SHA-256 of the DER certificate, lowercase hex (QR-friendly).
    pub fingerprint: String,
}

impl Identity {
    /// Manual clone (PrivateKeyDer implements clone_key, not Clone).
    pub fn duplicate(&self) -> Identity {
        Identity {
            certificate_der: self.certificate_der.clone(),
            private_key_der: self.private_key_der.clone_key(),
            fingerprint: self.fingerprint.clone(),
        }
    }
}

/// SHA-256 fingerprint of a DER certificate (lowercase hex, no separators).
pub fn fingerprint_of(cert_der: &[u8]) -> String {
    let digest = Sha256::digest(cert_der);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn write_private(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn read_identity(path: &Path) -> Option<Identity> {
    let pem = std::fs::read_to_string(path).ok()?;
    let mut certificate: Option<CertificateDer<'static>> = None;
    let mut key: Option<PrivateKeyDer<'static>> = None;
    for (label, contents) in pem_parser(&pem) {
        match label.as_str() {
            "CERTIFICATE" => certificate = Some(CertificateDer::from(contents)),
            "PRIVATE KEY" => {
                key = Some(PrivateKeyDer::Pkcs8(contents.into()));
            }
            _ => {}
        }
    }
    let certificate = certificate?;
    let private_key_der = key?;
    let fingerprint = fingerprint_of(certificate.as_ref());
    Some(Identity {
        certificate_der: certificate,
        private_key_der,
        fingerprint,
    })
}

/// Minimal PEM block splitter (identity files only; no external pem dep).
fn pem_parser(text: &str) -> Vec<(String, Vec<u8>)> {
    use base64::Engine as _;
    let mut blocks = Vec::new();
    let mut label = String::new();
    let mut body = String::new();
    let mut in_block = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("-----BEGIN ") {
            if let Some(label_text) = rest.strip_suffix("-----") {
                label = label_text.to_string();
                body.clear();
                in_block = true;
            }
        } else if let Some(rest) = line.strip_prefix("-----END ") {
            if rest.ends_with("-----") && in_block {
                if let Ok(contents) = base64::engine::general_purpose::STANDARD.decode(&body) {
                    blocks.push((label.clone(), contents));
                }
                in_block = false;
            }
        } else if in_block {
            body.push_str(line);
        }
    }
    blocks
}

/// Load the profile's identity, generating and persisting a fresh
/// self-signed ECDSA P-256 certificate when none exists. The certificate
/// survives restarts — regenerating would invalidate every paired device.
pub fn ensure_identity(harness_v2_root: &Path) -> Result<Identity, String> {
    let path: PathBuf = harness_v2_root.join("certs").join("identity.pem");
    if let Some(existing) = read_identity(&path) {
        return Ok(existing);
    }
    // Generate: ECDSA P-256, self-signed, 10-year validity, CN fixed (the
    // client verifies only the fingerprint — names never matter).
    // Default key kind is ECDSA P-256 with SHA-256 (rcgen 0.13).
    let key_pair = rcgen::KeyPair::generate().map_err(|e| format!("key generation: {e}"))?;
    let mut params = rcgen::CertificateParams::new(vec!["r-code-daemon".to_string()])
        .map_err(|e| format!("certificate params: {e}"))?;
    params.distinguished_name = {
        let mut name = rcgen::DistinguishedName::new();
        name.push(rcgen::DnType::CommonName, "r-code-daemon");
        name
    };
    let certificate = params
        .self_signed(&key_pair)
        .map_err(|e| format!("self-signed certificate: {e}"))?;
    let certificate_der = certificate.der().clone();
    let pem = format!("{}{}", certificate.pem(), key_pair.serialize_pem());
    write_private(&path, &pem).map_err(|e| format!("identity write: {e}"))?;
    let fingerprint = fingerprint_of(certificate_der.as_ref());
    Ok(Identity {
        certificate_der,
        private_key_der: PrivateKeyDer::Pkcs8(key_pair.serialize_der().into()),
        fingerprint,
    })
}

/// Server-side TLS config: the pinned identity, no client auth (devices
/// authenticate with their token inside the encrypted channel).
pub fn server_config(identity: &Identity) -> Result<ServerConfig, String> {
    ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_client_cert_verifier(WebPkiClientVerifier::no_client_auth())
        .with_single_cert(
            vec![identity.certificate_der.clone()],
            identity.private_key_der.clone_key(),
        )
        .map_err(|e| format!("server certificate: {e}"))
}

/// A verifier that accepts exactly one certificate fingerprint. Not the
/// system store, not any CA, not the hostname — only the pin (TOFU, F3);
/// a mismatch fails the TLS handshake itself.
#[derive(Debug)]
pub struct PinnedVerifier {
    pub fingerprint: String,
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, tokio_rustls::rustls::Error> {
        let fingerprint = fingerprint_of(end_entity.as_ref());
        if fingerprint == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(tokio_rustls::rustls::Error::General(format!(
                "certificate fingerprint mismatch (pinned {})",
                self.fingerprint
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Client-side TLS config pinned to one fingerprint: the only way a remote
/// client will talk to this daemon (F3 — no bypass exists).
pub fn pinned_client_config(fingerprint: &str) -> Result<ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Ok(ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedVerifier {
            fingerprint: fingerprint.to_string(),
        }))
        .with_no_client_auth())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r03_a1_identity_is_stable_across_restarts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = ensure_identity(dir.path()).expect("identity");
        let second = ensure_identity(dir.path()).expect("reload");
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(first.certificate_der, second.certificate_der);
        assert_eq!(first.fingerprint.len(), 64, "sha256 hex");
        // The private key never lands in registry.json-adjacent stores: it
        // lives only in certs/identity.pem with 0600 semantics.
        let identity_text = std::fs::read_to_string(dir.path().join("certs/identity.pem")).unwrap();
        assert!(identity_text.contains("PRIVATE KEY"));
    }
}

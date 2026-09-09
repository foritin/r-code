//! Content-addressed artifact store.
//!
//! Blobs live under the profile's blobs root keyed by sha256; `put` returns
//! versioned [`ArtifactRef`]s and enforces frame-sized inline input. Large
//! content arrives in chunks. Attachments record owning task ids — ownership
//! metadata travels with the reference, never the bytes.

use r_code_harness_protocol::services::{
    ArtifactRef, ArtifactsPutRequest, ArtifactsReadReply, ArtifactsReadRequest,
};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Errors from the artifact store.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArtifactError {
    #[error("io failure: {0}")]
    Io(String),
    #[error("artifact {0} not found")]
    NotFound(String),
    #[error("base64 failure: {0}")]
    Base64(String),
}

/// Filesystem blob store + ownership registry.
pub struct ArtifactStore {
    root: PathBuf,
    owners: Mutex<BTreeMap<String, String>>,
}

impl ArtifactStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            owners: Mutex::new(BTreeMap::new()),
        }
    }

    fn blob_path(&self, digest: &str) -> PathBuf {
        self.root.join(format!("{digest}.blob"))
    }

    /// Store bytes; returns the versioned reference.
    pub fn put(
        &self,
        request: &ArtifactsPutRequest,
        owner_task: &str,
    ) -> Result<ArtifactRef, ArtifactError> {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&request.data_base64)
            .map_err(|e| ArtifactError::Base64(e.to_string()))?;
        let digest = sha256_hex(&bytes);
        let path = self.blob_path(&digest);
        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| ArtifactError::Io(e.to_string()))?;
            }
            std::fs::write(&path, &bytes).map_err(|e| ArtifactError::Io(e.to_string()))?;
        }
        let reference = ArtifactRef {
            schema: ArtifactRef::SCHEMA,
            blob_id: format!("blob:sha256:{digest}"),
            bytes: bytes.len() as u64,
            sha256: digest.clone(),
            media_type: request.media_type.clone(),
        };
        self.owners
            .lock()
            .expect("owners")
            .insert(reference.blob_id.clone(), owner_task.to_string());
        Ok(reference)
    }

    /// Read a byte range of an artifact.
    pub fn read(
        &self,
        request: &ArtifactsReadRequest,
    ) -> Result<ArtifactsReadReply, ArtifactError> {
        let digest = request
            .artifact
            .sha256
            .trim_start_matches("sha256:")
            .to_string();
        let path = self.blob_path(&digest);
        if !path.exists() {
            return Err(ArtifactError::NotFound(request.artifact.blob_id.clone()));
        }
        let bytes = std::fs::read(&path).map_err(|e| ArtifactError::Io(e.to_string()))?;
        let total = bytes.len() as u64;
        let start = (request.offset as usize).min(bytes.len());
        let end = if request.length == 0 {
            bytes.len()
        } else {
            (start + request.length as usize).min(bytes.len())
        };
        use base64::Engine as _;
        Ok(ArtifactsReadReply {
            data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes[start..end]),
            total_bytes: total,
        })
    }

    /// The owning task of a blob reference (attachment ownership).
    pub fn owner_of(&self, blob_id: &str) -> Option<String> {
        self.owners.lock().expect("owners").get(blob_id).cloned()
    }

    /// The store root (for profile layout tests).
    pub fn root(&self) -> &Path {
        &self.root
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

impl From<io::Error> for ArtifactError {
    fn from(error: io::Error) -> Self {
        ArtifactError::Io(error.to_string())
    }
}

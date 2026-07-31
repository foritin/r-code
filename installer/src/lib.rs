//! Overlay container used by the branded installer.
//!
//! The outer executable stays a valid PE file. The regular Tauri NSIS package is
//! appended after it, followed by a small footer containing its length and SHA-256.

use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const MAGIC: &[u8; 16] = b"RCODESETUPv1\0\0\0\0";
pub const FOOTER_LEN: u64 = 8 + 32 + MAGIC.len() as u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadMetadata {
    pub offset: u64,
    pub length: u64,
    pub sha256: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractControl {
    Continue,
    Cancel,
}

#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error("安装包不完整：未找到 R-Code 安装载荷")]
    MissingPayload,
    #[error("安装包结构无效")]
    InvalidLayout,
    #[error("安装包校验失败，请重新下载安装包")]
    HashMismatch,
    #[error("安装已取消")]
    Cancelled,
    #[error("无法读写安装包：{0}")]
    Io(#[from] io::Error),
}

pub fn inspect_payload(path: &Path) -> Result<PayloadMetadata, OverlayError> {
    let mut file = File::open(path)?;
    inspect_reader(&mut file)
}

fn inspect_reader(file: &mut File) -> Result<PayloadMetadata, OverlayError> {
    let total = file.seek(SeekFrom::End(0))?;
    if total < FOOTER_LEN {
        return Err(OverlayError::MissingPayload);
    }

    file.seek(SeekFrom::End(-(FOOTER_LEN as i64)))?;
    let mut length_bytes = [0_u8; 8];
    let mut sha256 = [0_u8; 32];
    let mut magic = [0_u8; 16];
    file.read_exact(&mut length_bytes)?;
    file.read_exact(&mut sha256)?;
    file.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(OverlayError::MissingPayload);
    }

    let length = u64::from_le_bytes(length_bytes);
    let Some(offset) = total
        .checked_sub(FOOTER_LEN)
        .and_then(|v| v.checked_sub(length))
    else {
        return Err(OverlayError::InvalidLayout);
    };
    if length == 0 {
        return Err(OverlayError::InvalidLayout);
    }

    Ok(PayloadMetadata {
        offset,
        length,
        sha256,
    })
}

pub fn extract_payload<F>(
    source: &Path,
    destination: &Path,
    mut on_progress: F,
) -> Result<PayloadMetadata, OverlayError>
where
    F: FnMut(u64, u64) -> ExtractControl,
{
    let mut input = File::open(source)?;
    let metadata = inspect_reader(&mut input)?;
    input.seek(SeekFrom::Start(metadata.offset))?;

    let mut output = File::create(destination)?;
    let mut hasher = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = vec![0_u8; 1024 * 1024];

    while copied < metadata.length {
        let remaining = metadata.length - copied;
        let read_len = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| OverlayError::InvalidLayout)?;
        input.read_exact(&mut buffer[..read_len])?;
        output.write_all(&buffer[..read_len])?;
        hasher.update(&buffer[..read_len]);
        copied += read_len as u64;

        if on_progress(copied, metadata.length) == ExtractControl::Cancel {
            drop(output);
            let _ = fs::remove_file(destination);
            return Err(OverlayError::Cancelled);
        }
    }
    output.flush()?;
    drop(output);

    let actual: [u8; 32] = hasher.finalize().into();
    if actual != metadata.sha256 {
        let _ = fs::remove_file(destination);
        return Err(OverlayError::HashMismatch);
    }
    Ok(metadata)
}

pub fn append_payload(
    outer_executable: &Path,
    payload: &Path,
    output: &Path,
) -> Result<PayloadMetadata, OverlayError> {
    let outer_canonical = outer_executable.canonicalize()?;
    let payload_canonical = payload.canonicalize()?;
    if outer_canonical == payload_canonical {
        return Err(OverlayError::InvalidLayout);
    }
    if output.exists() {
        let output_canonical = output.canonicalize()?;
        if output_canonical == outer_canonical || output_canonical == payload_canonical {
            return Err(OverlayError::InvalidLayout);
        }
    }

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut outer = File::open(outer_executable)?;
    let mut payload_file = File::open(payload)?;
    let payload_length = payload_file.metadata()?.len();
    if payload_length == 0 {
        return Err(OverlayError::InvalidLayout);
    }

    let temporary = temporary_output_path(output);
    let mut combined = File::create(&temporary)?;
    let offset = io::copy(&mut outer, &mut combined)?;

    let mut hasher = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = vec![0_u8; 1024 * 1024];
    while copied < payload_length {
        let read = payload_file.read(&mut buffer)?;
        if read == 0 {
            let _ = fs::remove_file(&temporary);
            return Err(OverlayError::InvalidLayout);
        }
        combined.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        copied += read as u64;
    }

    let sha256: [u8; 32] = hasher.finalize().into();
    combined.write_all(&payload_length.to_le_bytes())?;
    combined.write_all(&sha256)?;
    combined.write_all(MAGIC)?;
    combined.flush()?;
    drop(combined);

    if output.exists() {
        fs::remove_file(output)?;
    }
    fs::rename(&temporary, output)?;

    Ok(PayloadMetadata {
        offset,
        length: payload_length,
        sha256,
    })
}

fn temporary_output_path(output: &Path) -> PathBuf {
    let mut file_name = output
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "r-code-installer".into());
    file_name.push(format!(".{}.tmp", std::process::id()));
    output.with_file_name(file_name)
}

pub fn hex_sha256(value: &[u8; 32]) -> String {
    value.iter().map(|byte| format!("{byte:02X}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom};

    #[test]
    fn round_trip_overlay_and_verify_hash() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("outer.exe");
        let payload = dir.path().join("payload.exe");
        let combined = dir.path().join("combined.exe");
        let extracted = dir.path().join("extracted.exe");
        fs::write(&outer, b"MZ fake outer").unwrap();
        fs::write(&payload, b"MZ real payload bytes").unwrap();

        let packed = append_payload(&outer, &payload, &combined).unwrap();
        let inspected = inspect_payload(&combined).unwrap();
        assert_eq!(packed, inspected);
        extract_payload(&combined, &extracted, |_done, _total| {
            ExtractControl::Continue
        })
        .unwrap();
        assert_eq!(fs::read(extracted).unwrap(), fs::read(payload).unwrap());
    }

    #[test]
    fn extraction_rejects_corrupted_payload() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("outer.exe");
        let payload = dir.path().join("payload.exe");
        let combined = dir.path().join("combined.exe");
        let extracted = dir.path().join("extracted.exe");
        fs::write(&outer, b"MZ outer").unwrap();
        fs::write(&payload, b"payload").unwrap();
        let metadata = append_payload(&outer, &payload, &combined).unwrap();

        let mut file = File::options().write(true).open(&combined).unwrap();
        file.seek(SeekFrom::Start(metadata.offset)).unwrap();
        file.write_all(b"X").unwrap();
        drop(file);

        assert!(matches!(
            extract_payload(&combined, &extracted, |_done, _total| {
                ExtractControl::Continue
            }),
            Err(OverlayError::HashMismatch)
        ));
        assert!(!extracted.exists());
    }

    #[test]
    fn extraction_can_be_cancelled_without_leaving_a_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("outer.exe");
        let payload = dir.path().join("payload.exe");
        let combined = dir.path().join("combined.exe");
        let extracted = dir.path().join("extracted.exe");
        fs::write(&outer, b"MZ outer").unwrap();
        fs::write(&payload, vec![7_u8; 2 * 1024 * 1024]).unwrap();
        append_payload(&outer, &payload, &combined).unwrap();

        assert!(matches!(
            extract_payload(&combined, &extracted, |_done, _total| {
                ExtractControl::Cancel
            }),
            Err(OverlayError::Cancelled)
        ));
        assert!(!extracted.exists());
    }
}

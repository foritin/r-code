//! M7-01：平台资产 versioned manifest（Node/Playwright/Chromium 安装面）。
//!
//! 合同：
//! - 每个支持 platform/arch 唯一解析到完整 manifest，未知组合明确 unsupported；
//! - size/sha256/license/source 任一缺失或 mismatch → 拒绝安装/执行；
//! - 同 manifest 重复验证 digest 稳定（幂等），SBOM/来源字段机器可读。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const ASSET_MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformAssetKind {
    Node,
    Playwright,
    Chromium,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformAsset {
    pub kind: PlatformAssetKind,
    pub platform: String,
    pub arch: String,
    pub version: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub license: String,
    pub source_url_class: String,
}

impl PlatformAsset {
    /// 结构完整性：size/license/source/sha 任一缺失即拒绝安装/执行。
    pub fn validate(&self) -> Result<(), String> {
        if self.size_bytes == 0 {
            return Err("size_bytes is zero".into());
        }
        if self.sha256.len() != 64 || !self.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("sha256 is not a valid hex digest".into());
        }
        if self.license.trim().is_empty() {
            return Err("license is missing".into());
        }
        if self.source_url_class.trim().is_empty() {
            return Err("source_url_class is missing".into());
        }
        if self.version.trim().is_empty() {
            return Err("version is missing".into());
        }
        Ok(())
    }

    /// SBOM 摘要行（机器可读，供 CI verifier 比对）。
    pub fn sbom_line(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}",
            format!("{:?}", self.kind).to_lowercase(),
            self.platform,
            self.arch,
            self.version,
            self.license,
            self.sha256
        )
    }
}

/// digest 稳定性：对同一 manifest 重复计算结果一致。
pub fn manifest_digest(assets: &[PlatformAsset]) -> String {
    let mut hasher = Sha256::new();
    let mut lines: Vec<String> = assets.iter().map(|a| a.sbom_line()).collect();
    lines.sort();
    for line in lines {
        hasher.update(line.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

/// 平台/架构解析：唯一命中，未知组合明确 unsupported。
pub fn resolve_asset<'a>(
    assets: &'a [PlatformAsset],
    kind: PlatformAssetKind,
    platform: &str,
    arch: &str,
) -> Result<&'a PlatformAsset, String> {
    let matches: Vec<&PlatformAsset> = assets
        .iter()
        .filter(|a| a.kind == kind && a.platform == platform && a.arch == arch)
        .collect();
    match matches.len() {
        1 => Ok(matches[0]),
        0 => Err(format!(
            "unsupported platform asset: {kind:?}/{platform}/{arch}"
        )),
        _ => Err(format!(
            "ambiguous platform asset: {kind:?}/{platform}/{arch}"
        )),
    }
}

/// 安装前完整性：实际字节数与 sha256 都必须与 manifest 一致，否则拒绝执行。
pub fn verify_downloaded_bytes(asset: &PlatformAsset, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() as u64 != asset.size_bytes {
        return Err(format!(
            "size mismatch: expected {} bytes, got {}",
            asset.size_bytes,
            bytes.len()
        ));
    }
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != asset.sha256 {
        return Err(format!("sha256 mismatch: expected {}", asset.sha256));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(kind: PlatformAssetKind, platform: &str, arch: &str) -> PlatformAsset {
        PlatformAsset {
            kind,
            platform: platform.into(),
            arch: arch.into(),
            version: "1.54.2".into(),
            size_bytes: 1024,
            sha256: "a".repeat(64),
            license: "Apache-2.0 OR MIT".into(),
            source_url_class: "vendor_cdn".into(),
        }
    }

    #[test]
    fn a1_unique_resolution_and_unknown_unsupported() {
        let assets = vec![
            asset(PlatformAssetKind::Node, "linux", "x64"),
            asset(PlatformAssetKind::Chromium, "linux", "x64"),
        ];
        assert!(resolve_asset(&assets, PlatformAssetKind::Node, "linux", "x64").is_ok());
        let err = resolve_asset(&assets, PlatformAssetKind::Playwright, "linux", "x64")
            .expect_err("unknown kind must be unsupported");
        assert!(err.contains("unsupported"));
    }

    #[test]
    fn a2_mismatch_rejects_install_and_execution() {
        let mut asset = asset(PlatformAssetKind::Chromium, "linux", "x64");
        let bytes = vec![0u8; 1024];
        asset.sha256 = format!("{:x}", Sha256::digest(&bytes));
        assert!(
            verify_downloaded_bytes(&asset, &bytes).is_ok(),
            "一致字节应通过"
        );
        // 篡改一个字节 → 拒绝
        let tampered = vec![1u8; 1024];
        assert!(
            verify_downloaded_bytes(&asset, &tampered).is_err(),
            "篡改必须拒绝"
        );
        // size 缺失（0）→ validate 拒绝
        let mut broken = asset.clone();
        broken.size_bytes = 0;
        assert!(broken.validate().is_err());
        let mut no_license = asset.clone();
        no_license.license = "  ".into();
        assert!(no_license.validate().is_err());
    }

    #[test]
    fn a3_manifest_digest_is_stable_and_sbom_machine_readable() {
        let assets = vec![
            asset(PlatformAssetKind::Node, "linux", "x64"),
            asset(PlatformAssetKind::Chromium, "darwin", "arm64"),
        ];
        let d1 = manifest_digest(&assets);
        let d2 = manifest_digest(&assets);
        assert_eq!(d1, d2, "digest 必须稳定");
        assert_eq!(d1.len(), 64);
        let line = assets[0].sbom_line();
        assert!(line.contains("node|"), "SBOM 行应含 kind");
        assert!(line.contains("Apache-2.0"), "SBOM 行应含 license");
    }
}

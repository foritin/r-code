//! Packaging configuration -- 打包、签名、自动更新、SBOM [doc-14 阶段8] [doc-18 M12]。
//!
//! 定义生产 / Beta 两套打包配置：产品名、版本、identifier、bundle targets、
//! 平台签名配置。提供自动更新通道配置与 SBOM（软件物料清单）生成器。
//!
//! [doc-14 阶段8] [doc-18 M12-01, M12-03, M12-04]

use std::path::PathBuf;

use r_code_core::error::ProductError;
use r_code_core::process::hide_background_console;

/// Packaging configuration for R-Code.
/// [doc-14 阶段8] [doc-18 M12-01]
#[derive(Debug, Clone, serde::Serialize)]
pub struct PackagingConfig {
    /// 产品名
    pub product_name: String,
    /// 版本号
    pub version: String,
    /// 应用 identifier（反向域名）
    pub identifier: String,
    /// 打包目标列表
    pub targets: Vec<BundleTarget>,
    /// macOS 配置
    pub macos: Option<MacOSConfig>,
    /// Windows 配置
    pub windows: Option<WindowsConfig>,
    /// Linux 配置
    pub linux: Option<LinuxConfig>,
}

/// Bundle target type.
#[derive(Debug, Clone, serde::Serialize)]
pub enum BundleTarget {
    /// Windows MSI
    #[serde(rename = "msi")]
    Msi,
    /// Windows NSIS
    #[serde(rename = "nsis")]
    Nsis,
    /// macOS DMG
    #[serde(rename = "dmg")]
    Dmg,
    /// Linux AppImage
    #[serde(rename = "appimage")]
    AppImage,
    /// Linux deb
    #[serde(rename = "deb")]
    Deb,
}

/// macOS packaging configuration.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MacOSConfig {
    /// 签名身份（通过 env: APPLE_SIGNING_IDENTITY 设置）
    pub signing_identity: Option<String>,
    /// 是否做公证
    pub notarization: bool,
    /// Entitlements 文件路径
    pub entitlements: Option<String>,
}

/// Windows packaging configuration.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WindowsConfig {
    /// WiX 安装器语言
    pub wix_language: Vec<String>,
    /// NSIS 安装器语言
    pub nsis_language: Vec<String>,
    /// 证书指纹（通过 env: WINDOWS_CERTIFICATE_THUMBPRINT 设置）
    pub certificate_thumbprint: Option<String>,
}

/// Linux packaging configuration.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LinuxConfig {
    /// 是否生成 deb
    pub deb: bool,
    /// 是否生成 AppImage
    pub appimage: bool,
}

impl PackagingConfig {
    /// Production packaging configuration.
    pub fn production() -> Self {
        Self {
            product_name: "R-Code".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            identifier: crate::app_paths::production_bundle_identifier().to_string(),
            targets: vec![
                BundleTarget::Msi,
                BundleTarget::Nsis,
                BundleTarget::Dmg,
                BundleTarget::AppImage,
            ],
            macos: Some(MacOSConfig {
                signing_identity: None, // Set via env: APPLE_SIGNING_IDENTITY
                notarization: true,
                entitlements: None,
            }),
            windows: Some(WindowsConfig {
                wix_language: vec!["en-US".into(), "zh-CN".into()],
                nsis_language: vec!["en-US".into(), "zh-CN".into()],
                certificate_thumbprint: None, // Set via env: WINDOWS_CERTIFICATE_THUMBPRINT
            }),
            linux: Some(LinuxConfig {
                deb: true,
                appimage: true,
            }),
        }
    }

    /// Beta packaging configuration (no signing, prerelease).
    pub fn beta() -> Self {
        let mut config = Self::production();
        if let Some(ref mut macos) = config.macos {
            macos.signing_identity = None;
            macos.notarization = false;
        }
        if let Some(ref mut windows) = config.windows {
            windows.certificate_thumbprint = None;
        }
        config
    }

    /// Check if signing is configured.
    pub fn is_signing_configured(&self) -> bool {
        if let Some(ref macos) = self.macos {
            if macos.signing_identity.is_some() {
                return true;
            }
        }
        if let Some(ref windows) = self.windows {
            if windows.certificate_thumbprint.is_some() {
                return true;
            }
        }
        false
    }
}

/// Auto-update configuration.
/// [doc-14 阶段8] [doc-18 M12-03]
#[derive(Debug, Clone, serde::Serialize)]
pub struct UpdateConfig {
    /// 更新通道
    pub channel: UpdateChannel,
    /// 更新端点
    pub endpoint: String,
    /// 签名公钥（通过 env: UPDATE_PUBKEY 设置）
    pub pubkey: Option<String>,
}

/// Update channel.
#[derive(Debug, Clone, serde::Serialize)]
pub enum UpdateChannel {
    /// 稳定通道
    #[serde(rename = "stable")]
    Stable,
    /// Beta 通道
    #[serde(rename = "beta")]
    Beta,
}

impl UpdateConfig {
    /// Stable channel configuration.
    pub fn stable() -> Self {
        Self {
            channel: UpdateChannel::Stable,
            endpoint: "https://updates.r-code.app/stable".to_string(),
            pubkey: None, // Set via env: UPDATE_PUBKEY
        }
    }

    /// Beta channel configuration.
    pub fn beta() -> Self {
        Self {
            channel: UpdateChannel::Beta,
            endpoint: "https://updates.r-code.app/beta".to_string(),
            pubkey: None,
        }
    }
}

/// SBOM (Software Bill of Materials) generator.
/// [doc-18 M12-04]
///
/// Runs `cargo metadata` to build a dependency inventory, then formats it
/// as SPDX 2.3 JSON or a flat license list.
pub struct SbomGenerator {
    workspace_path: PathBuf,
}

impl SbomGenerator {
    pub fn new(workspace_path: PathBuf) -> Self {
        Self { workspace_path }
    }

    /// Run `cargo metadata --format-version 1` and return parsed JSON.
    fn cargo_metadata(&self) -> Result<serde_json::Value, ProductError> {
        let mut command = std::process::Command::new("cargo");
        command
            .arg("metadata")
            .arg("--format-version")
            .arg("1")
            .current_dir(&self.workspace_path);
        hide_background_console(&mut command);
        let output = command
            .output()
            .map_err(|e| ProductError::Other(format!("failed to run cargo metadata: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProductError::Other(format!(
                "cargo metadata failed: {stderr}"
            )));
        }

        let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        Ok(json)
    }

    /// Extract the `packages` array from cargo metadata output.
    fn extract_packages(metadata: &serde_json::Value) -> Vec<&serde_json::Value> {
        metadata
            .get("packages")
            .and_then(|p| p.as_array())
            .map(|arr| arr.iter().collect())
            .unwrap_or_default()
    }

    /// Generate SBOM in SPDX format.
    /// Runs `cargo metadata` to get dependency tree, then formats as SPDX JSON.
    pub async fn generate_spdx(&self) -> Result<String, ProductError> {
        let metadata = self.cargo_metadata()?;
        let packages = Self::extract_packages(&metadata);

        let created = chrono::Utc::now().to_rfc3339();
        let namespace = format!("https://r-code.app/spdx/{}", chrono::Utc::now().timestamp());

        let spdx_packages: Vec<serde_json::Value> = packages
            .iter()
            .map(|p| {
                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let version = p.get("version").and_then(|v| v.as_str()).unwrap_or("");
                let license = p
                    .get("license")
                    .and_then(|v| v.as_str())
                    .unwrap_or("NOASSERTION");
                let source = p
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("NOASSERTION");

                // SPDXID: only alphanumeric + hyphen
                let sanitized_name: String = name
                    .chars()
                    .map(|c| {
                        if c.is_alphanumeric() || c == '-' {
                            c
                        } else {
                            '-'
                        }
                    })
                    .collect();
                let sanitized_version = version.replace('.', "-");
                let spdx_id = format!("SPDXRef-Package-{sanitized_name}-{sanitized_version}");

                serde_json::json!({
                    "name": name,
                    "SPDXID": spdx_id,
                    "versionInfo": version,
                    "licenseConcluded": license,
                    "licenseDeclared": license,
                    "downloadLocation": source,
                })
            })
            .collect();

        let spdx = serde_json::json!({
            "spdxVersion": "SPDX-2.3",
            "dataLicense": "CC0-1.0",
            "SPDXID": "SPDXRef-DOCUMENT",
            "name": "R-Code",
            "documentNamespace": namespace,
            "creationInfo": {
                "created": created,
                "creators": ["Tool: r-code-sbom-generator"],
            },
            "packages": spdx_packages,
        });

        Ok(serde_json::to_string_pretty(&spdx)?)
    }

    /// Generate license inventory.
    pub async fn license_inventory(&self) -> Result<Vec<LicenseEntry>, ProductError> {
        let metadata = self.cargo_metadata()?;
        let packages = Self::extract_packages(&metadata);

        let entries = packages
            .iter()
            .map(|p| LicenseEntry {
                name: p
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                version: p
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                license: p
                    .get("license")
                    .and_then(|v| v.as_str())
                    .unwrap_or("NOASSERTION")
                    .to_string(),
                source: p
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
            .collect();

        Ok(entries)
    }
}

/// A single license entry in the SBOM.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LicenseEntry {
    /// 包名
    pub name: String,
    /// 版本
    pub version: String,
    /// 许可证
    pub license: String,
    /// 来源
    pub source: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── PackagingConfig ───────────────────────────────────────────

    #[test]
    fn production_config_has_all_targets() {
        let cfg = PackagingConfig::production();
        assert_eq!(cfg.product_name, "R-Code");
        assert_eq!(
            cfg.identifier,
            crate::app_paths::production_bundle_identifier()
        );
        assert!(!cfg.version.is_empty());
        assert_eq!(cfg.targets.len(), 4);
        assert!(cfg.macos.is_some());
        assert!(cfg.windows.is_some());
        assert!(cfg.linux.is_some());
    }

    #[test]
    fn production_macos_has_notarization() {
        let cfg = PackagingConfig::production();
        let macos = cfg.macos.as_ref().unwrap();
        assert!(macos.notarization);
        assert!(macos.signing_identity.is_none()); // set via env
    }

    #[test]
    fn production_windows_has_languages() {
        let cfg = PackagingConfig::production();
        let windows = cfg.windows.as_ref().unwrap();
        assert!(windows.wix_language.contains(&"en-US".to_string()));
        assert!(windows.wix_language.contains(&"zh-CN".to_string()));
    }

    #[test]
    fn production_linux_has_both_formats() {
        let cfg = PackagingConfig::production();
        let linux = cfg.linux.as_ref().unwrap();
        assert!(linux.deb);
        assert!(linux.appimage);
    }

    #[test]
    fn beta_config_disables_signing() {
        let cfg = PackagingConfig::beta();
        let macos = cfg.macos.as_ref().unwrap();
        assert!(!macos.notarization);
        assert!(macos.signing_identity.is_none());

        let windows = cfg.windows.as_ref().unwrap();
        assert!(windows.certificate_thumbprint.is_none());
    }

    #[test]
    fn is_signing_configured_false_by_default() {
        let cfg = PackagingConfig::production();
        assert!(!cfg.is_signing_configured());
    }

    #[test]
    fn is_signing_configured_true_with_macos_identity() {
        let mut cfg = PackagingConfig::production();
        cfg.macos.as_mut().unwrap().signing_identity = Some("Developer ID".into());
        assert!(cfg.is_signing_configured());
    }

    #[test]
    fn is_signing_configured_true_with_windows_thumbprint() {
        let mut cfg = PackagingConfig::production();
        cfg.windows.as_mut().unwrap().certificate_thumbprint = Some("ABC123".into());
        assert!(cfg.is_signing_configured());
    }

    // ── UpdateConfig ──────────────────────────────────────────────

    #[test]
    fn stable_update_config() {
        let cfg = UpdateConfig::stable();
        assert!(matches!(cfg.channel, UpdateChannel::Stable));
        assert!(cfg.endpoint.contains("/stable"));
        assert!(cfg.pubkey.is_none());
    }

    #[test]
    fn beta_update_config() {
        let cfg = UpdateConfig::beta();
        assert!(matches!(cfg.channel, UpdateChannel::Beta));
        assert!(cfg.endpoint.contains("/beta"));
    }

    // ── SbomGenerator ─────────────────────────────────────────────

    #[tokio::test]
    async fn sbom_generates_spdx_json() {
        let gen = SbomGenerator::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
        let spdx = match gen.generate_spdx().await {
            Ok(s) => s,
            Err(_) => return, // skip if cargo not available in test environment
        };
        assert!(spdx.contains("SPDX-2.3"));
        assert!(spdx.contains("r-code-core"));
    }

    #[tokio::test]
    async fn sbom_license_inventory_nonempty() {
        let gen = SbomGenerator::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
        let entries = match gen.license_inventory().await {
            Ok(e) => e,
            Err(_) => return, // skip if cargo not available
        };
        assert!(!entries.is_empty());
        assert!(entries.iter().any(|e| e.name == "r-code-core"));
    }
}

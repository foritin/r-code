//! 与 Tauri bundle identifier 对齐的应用数据路径。
//!
//! stdio MCP、启动期日志和 Codex 子进程在 Tauri `AppHandle` 创建前就可能需要
//! 数据目录，因此不能调用 `app.path().app_data_dir()`。这里集中维护平台 identifier，
//! 并用配置契约测试防止它与 Tauri 的平台覆盖配置再次漂移。

use std::path::PathBuf;

pub const DEFAULT_BUNDLE_IDENTIFIER: &str = "com.r-code.app";
pub const MACOS_BUNDLE_IDENTIFIER: &str = "com.rcode.desktop";

pub const fn bundle_identifier() -> &'static str {
    if cfg!(target_os = "macos") {
        MACOS_BUNDLE_IDENTIFIER
    } else {
        DEFAULT_BUNDLE_IDENTIFIER
    }
}

/// 与 Tauri `app_data_dir()?.join("r-code")` 相同的默认产品数据根目录。
pub fn default_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|root| root.join(bundle_identifier()).join("r-code"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_match_tauri_configs() {
        let common: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let macos: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.macos.conf.json")).unwrap();

        assert_eq!(
            common["identifier"].as_str(),
            Some(DEFAULT_BUNDLE_IDENTIFIER)
        );
        assert_eq!(macos["identifier"].as_str(), Some(MACOS_BUNDLE_IDENTIFIER));
    }

    #[test]
    fn default_data_dir_uses_the_current_platform_identifier() {
        let path = default_data_dir().expect("the test platform should expose a data directory");
        let rendered = path.to_string_lossy().replace('\\', "/");
        assert!(rendered.ends_with(&format!("{}/r-code", bundle_identifier())));
    }
}

//! 安全配置 -- 渲染器隔离、CSP、导航限制 [doc-07 §2, §8]。
//!
//! 定义生产 / 开发两套安全配置：CSP、devtools、远程调试、沙箱、
//! 阻断 URL scheme、允许的 IPC origin。提供 URL 安全性检查与导航阻断。
//!
//! [doc-07 §2] [doc-07 §8]

/// 安全配置。
///
/// [doc-07 §2] Renderer isolation, CSP, navigation restrictions.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SecurityConfig {
    /// Content Security Policy
    pub csp: String,
    /// Whether devtools is enabled (false in production)
    pub devtools_enabled: bool,
    /// Whether remote debugging port is disabled
    pub remote_debugging_disabled: bool,
    /// Sandbox mode
    pub sandbox_enabled: bool,
    /// Blocked URL schemes
    pub blocked_schemes: Vec<String>,
    /// Allowed origins for IPC
    pub allowed_origins: Vec<String>,
}

impl SecurityConfig {
    /// 生产环境安全配置（最严格）。
    ///
    /// Production security configuration.
    pub fn production() -> Self {
        Self {
            csp: "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ipc: http://ipc.localhost".to_string(),
            devtools_enabled: false,
            remote_debugging_disabled: true,
            sandbox_enabled: true,
            blocked_schemes: vec!["javascript:".into(), "file:".into(), "vbscript:".into()],
            allowed_origins: vec!["tauri://localhost".into(), "http://tauri.localhost".into()],
        }
    }

    /// 开发环境安全配置（更宽松）。
    ///
    /// Development security configuration (more permissive).
    pub fn development() -> Self {
        Self {
            csp: "default-src 'self'; script-src 'self' 'unsafe-eval' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ipc: http://ipc.localhost ws://localhost:*".to_string(),
            devtools_enabled: true,
            remote_debugging_disabled: false,
            sandbox_enabled: false,
            blocked_schemes: vec!["javascript:".into(), "vbscript:".into()],
            allowed_origins: vec!["http://localhost:*".into(), "tauri://localhost".into()],
        }
    }

    /// 检查 URL 是否安全（非阻断 scheme）。
    ///
    /// Check if a URL is safe (not blocked scheme).
    pub fn is_url_safe(&self, url: &str) -> bool {
        !self
            .blocked_schemes
            .iter()
            .any(|s| url.starts_with(s.as_str()))
    }

    /// 检查 origin 是否在允许列表中（精确匹配）。
    ///
    /// Check if an origin is allowed.
    pub fn is_origin_allowed(&self, origin: &str) -> bool {
        self.allowed_origins.iter().any(|o| origin == o.as_str())
    }
}

/// 检查 URL 是否应被阻断（外部导航）[doc-07 §8]。
///
/// Check if a URL should be blocked (external navigation).
pub fn should_block_navigation(url: &str) -> bool {
    let blocked = ["javascript:", "file:", "vbscript:", "data:"];
    blocked.iter().any(|s| url.starts_with(s))
}

/// 检查 `window.open` 调用是否应被阻断。
///
/// Check if a window.open call should be blocked.
/// Always block in R-Code.
pub fn should_block_window_open() -> bool {
    true // Always block in R-Code
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── production / development ──────────────────────────────────

    #[test]
    fn production_disables_devtools() {
        let cfg = SecurityConfig::production();
        assert!(!cfg.devtools_enabled);
        assert!(cfg.remote_debugging_disabled);
        assert!(cfg.sandbox_enabled);
    }

    #[test]
    fn development_enables_devtools() {
        let cfg = SecurityConfig::development();
        assert!(cfg.devtools_enabled);
        assert!(!cfg.remote_debugging_disabled);
        assert!(!cfg.sandbox_enabled);
    }

    #[test]
    fn production_csp_has_no_unsafe_eval() {
        let cfg = SecurityConfig::production();
        assert!(!cfg.csp.contains("unsafe-eval"));
        // 生产环境不应允许 ws://
        assert!(!cfg.csp.contains("ws://"));
    }

    #[test]
    fn development_csp_allows_unsafe_eval() {
        let cfg = SecurityConfig::development();
        assert!(cfg.csp.contains("unsafe-eval"));
        assert!(cfg.csp.contains("ws://localhost:*"));
    }

    #[test]
    fn production_blocks_file_scheme() {
        let cfg = SecurityConfig::production();
        assert!(cfg.blocked_schemes.contains(&"file:".to_string()));
    }

    #[test]
    fn development_does_not_block_file_scheme() {
        let cfg = SecurityConfig::development();
        assert!(!cfg.blocked_schemes.contains(&"file:".to_string()));
    }

    // ── is_url_safe ───────────────────────────────────────────────

    #[test]
    fn is_url_safe_blocks_javascript_scheme() {
        let cfg = SecurityConfig::production();
        assert!(!cfg.is_url_safe("javascript:alert(1)"));
        assert!(!cfg.is_url_safe("file:///etc/passwd"));
        assert!(!cfg.is_url_safe("vbscript:msgbox"));
    }

    #[test]
    fn is_url_safe_allows_https() {
        let cfg = SecurityConfig::production();
        assert!(cfg.is_url_safe("https://example.com"));
        assert!(cfg.is_url_safe("http://localhost:3000"));
        assert!(cfg.is_url_safe("tauri://localhost"));
    }

    #[test]
    fn development_allows_file_scheme() {
        let cfg = SecurityConfig::development();
        // 开发环境不阻断 file:
        assert!(cfg.is_url_safe("file:///tmp/x"));
        // 但仍阻断 javascript:
        assert!(!cfg.is_url_safe("javascript:alert(1)"));
    }

    // ── is_origin_allowed ─────────────────────────────────────────

    #[test]
    fn is_origin_allowed_production() {
        let cfg = SecurityConfig::production();
        assert!(cfg.is_origin_allowed("tauri://localhost"));
        assert!(cfg.is_origin_allowed("http://tauri.localhost"));
        assert!(!cfg.is_origin_allowed("https://evil.com"));
    }

    #[test]
    fn is_origin_allowed_development_exact_match() {
        let cfg = SecurityConfig::development();
        // 精确匹配允许列表中的条目
        assert!(cfg.is_origin_allowed("http://localhost:*"));
        assert!(cfg.is_origin_allowed("tauri://localhost"));
        assert!(!cfg.is_origin_allowed("https://evil.com"));
    }

    // ── should_block_navigation ──────────────────────────────────

    #[test]
    fn should_block_navigation_blocks_dangerous_schemes() {
        assert!(should_block_navigation("javascript:alert(1)"));
        assert!(should_block_navigation("file:///etc/passwd"));
        assert!(should_block_navigation("vbscript:foo"));
        assert!(should_block_navigation("data:text/html,<script>"));
    }

    #[test]
    fn should_block_navigation_allows_normal_urls() {
        assert!(!should_block_navigation("https://example.com"));
        assert!(!should_block_navigation("http://localhost:3000/path"));
    }

    // ── should_block_window_open ─────────────────────────────────

    #[test]
    fn should_block_window_open_always_true() {
        assert!(should_block_window_open());
    }

    // ── 序列化 ───────────────────────────────────────────────────

    #[test]
    fn security_config_is_serializable() {
        let cfg = SecurityConfig::production();
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("csp"));
        assert!(json.contains("blocked_schemes"));
        assert!(json.contains("allowed_origins"));
        assert!(json.contains("devtools_enabled"));
    }
}

//! 密钥存储与日志脱敏 [doc-07 §5, §6]。
//!
//! ## §5 SecretStore - OS Keychain 集成
//! API key 不以明文落盘；仅短暂驻留内存。持久化凭据交由 OS keychain
//! （macOS Keychain / Windows Credential Manager / Linux Secret Service）保管。
//!
//! ## §6 日志脱敏
//! 所有进入日志 / telemetry 的文本必须先经 [`redact_text`] 处理，抹除
//! API key、Bearer token、Authorization / Cookie 头、`token=` 参数等敏感片段。
//! 设计原则：**过脱敏优于欠脱敏**（over-redaction is safe; under-redaction
//! is a leak）。

use std::sync::LazyLock;

use keyring::Entry;
use regex::Regex;

use crate::error::ProductError;

// ===========================================================================
// §5 SecretStore - OS Keychain 集成
// ===========================================================================

/// `SecretStore` - 通过 OS keychain 管理 API key 与 token [doc-07 §5]。
///
/// API key 不以明文落盘，仅在内存中短暂存在；所有持久化凭据由 OS keychain
/// （macOS Keychain / Windows Credential Manager / Linux Secret Service）保管。
///
/// [`SecretStore::new`] 仅记录 service 名，**不触碰 keychain**；真正的 keychain
/// 访问发生在 `store` / `get` / `delete` 调用时。在无可用 keychain 后端的环境
/// （如无 D-Bus Secret Service 的 Linux CI）中，这些调用会返回
/// [`ProductError::SecretError`]。
pub struct SecretStore {
    service_name: String,
}

impl SecretStore {
    /// 创建以 `service_name`（例如 `"r-code"`）为服务名的 `SecretStore`。
    ///
    /// 此调用不访问 keychain，始终成功。
    pub fn new(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
        }
    }

    fn entry(&self, key: &str) -> Result<Entry, ProductError> {
        Entry::new(&self.service_name, key)
            .map_err(|e| ProductError::SecretError(format!("keychain entry creation failed: {e}")))
    }

    fn store_entry(entry: &Entry, value: &str) -> Result<(), ProductError> {
        entry
            .set_password(value)
            .map_err(|e| ProductError::SecretError(format!("keychain store failed: {e}")))
    }

    fn get_entry(entry: &Entry) -> Result<Option<String>, ProductError> {
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(ProductError::SecretError(format!(
                "keychain get failed: {e}"
            ))),
        }
    }

    fn delete_entry(entry: &Entry) -> Result<(), ProductError> {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(ProductError::SecretError(format!(
                "keychain delete failed: {e}"
            ))),
        }
    }

    /// 将 `value` 以 `key` 存入 OS keychain。
    pub fn store(&self, key: &str, value: &str) -> Result<(), ProductError> {
        Self::store_entry(&self.entry(key)?, value)
    }

    /// 从 OS keychain 读取 `key`。
    ///
    /// 若 key 不存在（`NoEntry`），返回 `Ok(None)` 而非错误。
    pub fn get(&self, key: &str) -> Result<Option<String>, ProductError> {
        Self::get_entry(&self.entry(key)?)
    }

    /// 从 OS keychain 删除 `key`。
    ///
    /// 若 key 不存在（`NoEntry`），视为已删除成功，返回 `Ok(())`。
    pub fn delete(&self, key: &str) -> Result<(), ProductError> {
        Self::delete_entry(&self.entry(key)?)
    }
}

// ===========================================================================
// §6 日志脱敏
// ===========================================================================

/// 一条脱敏规则：编译好的正则 + 替换模板。
struct RedactionPattern {
    re: Regex,
    replacement: &'static str,
}

/// 编译一次、复用的脱敏规则集合。**顺序敏感**：更具体的模式先执行，避免
/// 敏感片段残留。
///
/// 1. `Bearer <token>`（先于 Authorization，整体吞掉 token）
/// 2. `Authorization:` 头（吞到行尾，覆盖 `Basic <b64>` 等多 token 值）
/// 3. `Cookie:` 头（吞到行尾）
/// 4. `token=` 参数
/// 5. API key（`sk-...` / `sk-ant-...`，`\b` 防止 `risk-area` 误伤）
static REDACTION_PATTERNS: LazyLock<Vec<RedactionPattern>> = LazyLock::new(|| {
    vec![
        RedactionPattern {
            re: Regex::new(r"Bearer\s+[a-zA-Z0-9_.-]+").unwrap(),
            replacement: "Bearer ***",
        },
        RedactionPattern {
            re: Regex::new(r"[Aa]uthorization:\s*[^\r\n]*").unwrap(),
            replacement: "Authorization: ***",
        },
        RedactionPattern {
            re: Regex::new(r"[Cc]ookie:\s*[^\r\n]*").unwrap(),
            replacement: "Cookie: ***",
        },
        RedactionPattern {
            re: Regex::new(r"token=[a-zA-Z0-9_-]+").unwrap(),
            replacement: "token=***",
        },
        RedactionPattern {
            re: Regex::new(r"\bsk-[a-zA-Z0-9_-]+").unwrap(),
            replacement: "sk-***",
        },
    ]
});

/// 在文本进入日志 / telemetry 前脱敏 [doc-07 §6]。
///
/// 覆盖：API key（`sk-...` / `sk-ant-...`）、Bearer token、Authorization 头、
/// `token=` 参数、Cookie 头。设计原则：**过脱敏优于欠脱敏**。
///
/// # 示例
/// ```
/// # use r_code_core::secret::redact_text;
/// assert_eq!(redact_text("key=sk-abc123"), "key=sk-***");
/// assert_eq!(redact_text("Authorization: Bearer xyz"), "Authorization: ***");
/// ```
pub fn redact_text(text: &str) -> String {
    let mut current = text.to_string();
    for pattern in REDACTION_PATTERNS.iter() {
        current = pattern
            .re
            .replace_all(&current, pattern.replacement)
            .into_owned();
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- redact_text：行为测试（纯函数，无 OS 依赖）----

    #[test]
    fn redact_api_key() {
        assert_eq!(redact_text("key=sk-abc123"), "key=sk-***");
        assert_eq!(redact_text("sk-ant-xyz456"), "sk-***");
        assert_eq!(
            redact_text("using sk-AbC_1-2 to call"),
            "using sk-*** to call"
        );
    }

    #[test]
    fn redact_bearer_token() {
        // Bearer + Authorization 同时出现：Bearer 先吞 token，Authorization 再吞整行
        assert_eq!(
            redact_text("Authorization: Bearer abc.def.ghi"),
            "Authorization: ***"
        );
        assert_eq!(redact_text("Bearer token123"), "Bearer ***");
    }

    #[test]
    fn redact_authorization_header() {
        assert_eq!(
            redact_text("Authorization: secret123"),
            "Authorization: ***"
        );
        // Basic 认证：整行必须被吞掉，不能泄露 base64 凭据
        assert_eq!(
            redact_text("Authorization: Basic dXNlcjpwYXNz"),
            "Authorization: ***"
        );
        // 小写
        assert_eq!(redact_text("authorization: xyz"), "Authorization: ***");
    }

    #[test]
    fn redact_token_param() {
        assert_eq!(redact_text("token=abc123"), "token=***");
        assert_eq!(
            redact_text("url?token=secret_value&foo=1"),
            "url?token=***&foo=1"
        );
    }

    #[test]
    fn redact_cookie_header() {
        assert_eq!(redact_text("Cookie: session=abc"), "Cookie: ***");
        // 多值 cookie 整行吞掉，避免泄露后续值
        assert_eq!(redact_text("cookie: a=1; b=2"), "Cookie: ***");
    }

    #[test]
    fn redact_preserves_non_sensitive_text() {
        assert_eq!(redact_text("hello world"), "hello world");
        assert_eq!(redact_text("the task is running"), "the task is running");
        // 不应误伤普通含 "sk-" 子串的词（\b 边界保护）
        assert_eq!(redact_text("risk-area is high"), "risk-area is high");
        assert_eq!(redact_text("desk-lamp"), "desk-lamp");
    }

    #[test]
    fn redact_multiple_sensitive_items() {
        let input = "Authorization: Bearer sk-abc123\ntoken=xyz\nCookie: session=abc";
        let output = redact_text(input);
        // 任何原始敏感片段都不应残留
        assert!(!output.contains("abc123"));
        assert!(!output.contains("xyz"));
        assert!(!output.contains("session=abc"));
        // 各脱敏标记应存在
        assert_eq!(output, "Authorization: ***\ntoken=***\nCookie: ***");
    }

    #[test]
    fn redact_api_key_in_various_contexts() {
        assert_eq!(redact_text("(sk-abc123)"), "(sk-***)");
        assert_eq!(redact_text("'sk-abc123'"), "'sk-***'");
        assert_eq!(redact_text("key:sk-abc123"), "key:sk-***");
    }

    #[test]
    fn redact_empty_and_no_match() {
        assert_eq!(redact_text(""), "");
        assert_eq!(
            redact_text("nothing sensitive here"),
            "nothing sensitive here"
        );
    }

    // ---- SecretStore ----

    #[test]
    fn secret_store_new_stores_service_name() {
        let store = SecretStore::new("r-code");
        assert_eq!(store.service_name, "r-code");
    }

    #[test]
    fn secret_store_new_accepts_string() {
        let name = String::from("r-code-prod");
        let store = SecretStore::new(name);
        assert_eq!(store.service_name, "r-code-prod");
    }

    #[test]
    fn secret_entry_round_trip_uses_a_deterministic_backend() {
        // A headless runner is not a valid integration environment for macOS
        // Keychain, Windows Credential Manager, or Linux Secret Service. Use
        // keyring's entry-scoped in-memory credential to verify our mapping
        // and idempotent-delete semantics without reading/writing real secrets.
        let entry = Entry::new_with_credential(Box::new(keyring::mock::MockCredential::default()));

        SecretStore::store_entry(&entry, "super-secret-value").expect("store should succeed");
        let got = SecretStore::get_entry(&entry).expect("get should succeed after store");
        assert_eq!(got.as_deref(), Some("super-secret-value"));

        SecretStore::delete_entry(&entry).expect("delete should succeed");
        let after = SecretStore::get_entry(&entry).expect("get after delete should not error");
        assert_eq!(after, None);

        // 删除不存在的 key 应返回 Ok(()) 而非错误（幂等删除）。
        SecretStore::delete_entry(&entry).expect("delete of missing key should be Ok");
    }
}

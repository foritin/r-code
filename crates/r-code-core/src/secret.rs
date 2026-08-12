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
        Self::store_entry(&self.entry(key)?, value)?;

        // 必须用全新的 Entry 回读。keyring 在未启用平台原生后端时会回落到
        // entry-scoped mock：set_password 返回成功，但下一次业务调用创建新 Entry 后
        // 立即得到 NoEntry。保存阶段 fail closed，确保设置页不会虚报成功，也确保
        // 旧明文迁移不会在凭据实际不可读时清空 TOML。
        match Self::get_entry(&self.entry(key)?)? {
            Some(stored) if stored == value => Ok(()),
            Some(_) => Err(ProductError::SecretError(
                "keychain verification failed: stored credential does not match".to_string(),
            )),
            None => Err(ProductError::SecretError(
                "keychain verification failed: stored credential is not readable".to_string(),
            )),
        }
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
/// 1. PEM 私钥与 URL userinfo
/// 2. Authorization / Cookie / Bearer 头
/// 3. 常见敏感字段赋值（API key、密码、client secret、各种 token、AWS key）
/// 4. 常见提供商 token 格式
/// 5. OpenAI / Anthropic 风格 API key（`\b` 防止 `risk-area` 误伤）
static REDACTION_PATTERNS: LazyLock<Vec<RedactionPattern>> = LazyLock::new(|| {
    vec![
        RedactionPattern {
            re: Regex::new(
                r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
            )
            .unwrap(),
            replacement: "[PRIVATE KEY REDACTED]",
        },
        RedactionPattern {
            re: Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://[^/\s:@]+:)[^@\s/]+@").unwrap(),
            replacement: "$1***@",
        },
        RedactionPattern {
            re: Regex::new(r"(?i)Bearer\s+[a-zA-Z0-9_.-]+").unwrap(),
            replacement: "Bearer ***",
        },
        RedactionPattern {
            re: Regex::new(r"(?i)\b(?:proxy-)?authorization\s*:\s*[^\r\n]*").unwrap(),
            replacement: "Authorization: ***",
        },
        RedactionPattern {
            re: Regex::new(r"(?i)\b(?:set-)?cookie\s*:\s*[^\r\n]*").unwrap(),
            replacement: "Cookie: ***",
        },
        RedactionPattern {
            re: Regex::new(
                r#"(?i)\b(api[_-]?key|x[_-]?api[_-]?key|password|passwd|pwd|client[_-]?secret|access[_-]?token|refresh[_-]?token|id[_-]?token|session[_-]?token|token|aws[_-]?access[_-]?key[_-]?id|aws[_-]?secret[_-]?access[_-]?key|aws[_-]?session[_-]?token|private[_-]?key|credential(?:s)?)\b(\s*["']?\s*[:=]\s*)(?:"[^"]*"|'[^']*'|[^\s,}\]&]+)"#,
            )
            .unwrap(),
            replacement: "$1$2***",
        },
        RedactionPattern {
            re: Regex::new(r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})\b")
                .unwrap(),
            replacement: "github_***",
        },
        RedactionPattern {
            re: Regex::new(r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b").unwrap(),
            replacement: "AWS_ACCESS_KEY_***",
        },
        RedactionPattern {
            re: Regex::new(r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b").unwrap(),
            replacement: "slack_***",
        },
        RedactionPattern {
            re: Regex::new(r"\bAIza[A-Za-z0-9_-]{20,}\b").unwrap(),
            replacement: "google_***",
        },
        RedactionPattern {
            re: Regex::new(r"\bsk-[a-zA-Z0-9_-]+").unwrap(),
            replacement: "sk-***",
        },
    ]
});

/// 在文本进入日志 / telemetry 前脱敏 [doc-07 §6]。
///
/// 覆盖：API key、密码与 client secret、常见 provider token、Bearer token、
/// Authorization/Cookie 头、URL userinfo 和 PEM 私钥。设计原则：**过脱敏优于欠脱敏**。
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
    fn redact_common_credential_assignments_and_provider_tokens() {
        let input = concat!(
            "api_key=plain-secret password: hunter2 client_secret='client-value' ",
            "x-api-key=header-value access_token=access-value ",
            "ghp_abcdefghijklmnopqrstuvwxyz123456 ",
            "AKIAABCDEFGHIJKLMNOP ",
            "xoxb-1234567890-secret"
        );
        let output = redact_text(input);

        for secret in [
            "plain-secret",
            "hunter2",
            "client-value",
            "header-value",
            "access-value",
            "ghp_abcdefghijklmnopqrstuvwxyz123456",
            "AKIAABCDEFGHIJKLMNOP",
            "xoxb-1234567890-secret",
        ] {
            assert!(!output.contains(secret), "credential leaked: {secret}");
        }
        assert!(output.contains("api_key=***"));
        assert!(output.contains("password: ***"));
        assert!(output.contains("client_secret=***"));
    }

    #[test]
    fn redact_json_credentials_url_userinfo_and_private_keys() {
        let input = "{\"api_key\":\"json-secret\"} postgres://user:db-password@example.test/db\n-----BEGIN PRIVATE KEY-----\nsecret-body\n-----END PRIVATE KEY-----";
        let output = redact_text(input);

        assert!(!output.contains("json-secret"));
        assert!(!output.contains("db-password"));
        assert!(!output.contains("secret-body"));
        assert!(output.contains("postgres://user:***@example.test/db"));
        assert!(output.contains("[PRIVATE KEY REDACTED]"));
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

    /// macOS CI 必须覆盖真实 Keychain 的跨 Entry 往返；普通单元测试使用的
    /// entry-scoped mock 无法证明应用下一次调用或重启后仍能读取凭据。
    #[cfg(target_os = "macos")]
    #[test]
    fn secret_store_round_trip_across_entries_on_macos() {
        let nonce = uuid::Uuid::new_v4();
        let service = format!("r-code-keychain-test-{nonce}");
        let key = "round-trip";
        let value = format!("temporary-test-secret-{nonce}");

        let store_result = SecretStore::new(&service).store(key, &value);
        let read_result = SecretStore::new(&service).get(key);
        let delete_result = SecretStore::new(&service).delete(key);

        store_result.expect("macOS Keychain should persist across Entry instances");
        let stored = read_result.expect("macOS Keychain read should succeed");
        assert_eq!(stored.as_deref(), Some(value.as_str()));
        delete_result.expect("temporary macOS Keychain credential should be removed");
        assert_eq!(SecretStore::new(&service).get(key).unwrap(), None);
    }
}

//! 密钥存储与日志脱敏 [doc-07 §5, §6]。
//!
//! ## §5 SecretStore - 平台凭据存储
//! API key 不以明文写入普通配置。Windows/Linux 继续使用平台凭据库；macOS 为避免
//! Keychain 授权弹窗，使用应用数据目录中权限收紧的 AEAD 加密文件。
//!
//! ## §6 日志脱敏
//! 所有进入日志 / telemetry 的文本必须先经 [`redact_text`] 处理，抹除
//! API key、Bearer token、Authorization / Cookie 头、`token=` 参数等敏感片段。
//! 设计原则：**过脱敏优于欠脱敏**（over-redaction is safe; under-redaction
//! is a leak）。

use std::sync::LazyLock;

#[cfg(not(target_os = "macos"))]
use keyring::Entry;
use regex::Regex;

#[cfg(target_os = "macos")]
use {
    base64::{engine::general_purpose::STANDARD as BASE64, Engine as _},
    ring::{
        aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305},
        rand::{SecureRandom, SystemRandom},
    },
    serde::{Deserialize, Serialize},
    std::{
        collections::BTreeMap,
        fs::{self, File, OpenOptions},
        io::{Read, Write},
        os::{
            fd::AsRawFd,
            unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        },
        path::{Path, PathBuf},
        sync::Mutex,
    },
    zeroize::Zeroizing,
};

use crate::error::ProductError;

// ===========================================================================
// §5 SecretStore - 平台凭据存储
// ===========================================================================

#[cfg(not(target_os = "macos"))]
/// `SecretStore` - 通过 Windows/Linux 系统凭据库管理 API key 与 token [doc-07 §5]。
///
/// API key 不以明文落盘，仅在内存中短暂存在；所有持久化凭据由 OS keychain
/// （Windows Credential Manager / Linux Secret Service）保管。
///
/// [`SecretStore::new`] 仅记录 service 名，**不触碰 keychain**；真正的 keychain
/// 访问发生在 `store` / `get` / `delete` 调用时。在无可用 keychain 后端的环境
/// （如无 D-Bus Secret Service 的 Linux CI）中，这些调用会返回
/// [`ProductError::SecretError`]。
pub struct SecretStore {
    service_name: String,
}

#[cfg(not(target_os = "macos"))]
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

#[cfg(target_os = "macos")]
const MAC_CREDENTIAL_DIRECTORY: &str = "credentials";
#[cfg(target_os = "macos")]
const MAC_MASTER_KEY_FILE: &str = "master.key";
#[cfg(target_os = "macos")]
const MAC_ENCRYPTED_STORE_FILE: &str = "store.v1.enc";
#[cfg(target_os = "macos")]
const MAC_CREDENTIAL_LOCK_FILE: &str = "store.lock";
#[cfg(target_os = "macos")]
const MAC_CREDENTIAL_FORMAT_VERSION: u8 = 1;
#[cfg(target_os = "macos")]
const MAC_CREDENTIAL_AAD: &[u8] = b"r-code/mac-credential-store/v1";
#[cfg(target_os = "macos")]
const MAC_MASTER_KEY_LEN: usize = 32;
#[cfg(target_os = "macos")]
const MAC_NONCE_LEN: usize = 12;
#[cfg(target_os = "macos")]
const MAC_MAX_ACCOUNTS: usize = 512;
#[cfg(target_os = "macos")]
const MAC_MAX_ACCOUNT_BYTES: usize = 512;
#[cfg(target_os = "macos")]
const MAC_MAX_SECRET_BYTES: usize = 64 * 1024;
#[cfg(target_os = "macos")]
const MAC_MAX_ENCRYPTED_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Provider 与 MCP 共用同一个进程锁，避免各自执行 read-modify-write 时丢失另一个入口的更新。
#[cfg(target_os = "macos")]
static MAC_CREDENTIAL_STORE_LOCK: Mutex<()> = Mutex::new(());

#[cfg(target_os = "macos")]
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MacCredentialEnvelope {
    version: u8,
    nonce: String,
    ciphertext: String,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MacCredentialPayload {
    version: u8,
    entries: BTreeMap<String, String>,
}

/// macOS 专用凭据存储。
///
/// 主密钥与 ChaCha20-Poly1305 密文分别保存，文件权限固定为 `0600`，父目录固定为
/// `0700`。这避免凭据出现在普通配置、日志与支持包中，并彻底绕开 Keychain API；它不
/// 试图防御已经能读取当前 macOS 用户应用数据目录的恶意进程。
#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
pub struct EncryptedFileSecretStore {
    root: PathBuf,
}

#[cfg(target_os = "macos")]
impl EncryptedFileSecretStore {
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: config_dir.into().join(MAC_CREDENTIAL_DIRECTORY),
        }
    }

    pub fn store(&self, account: &str, value: &str) -> Result<(), ProductError> {
        validate_mac_credential(account, value)?;
        let _guard = MAC_CREDENTIAL_STORE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _file_guard = MacCredentialFileLock::acquire(&self.root)?;
        let mut payload = self.read_payload_locked()?;
        payload
            .entries
            .insert(account.to_string(), value.to_string());
        self.write_payload_locked(&payload)
    }

    pub fn get(&self, account: &str) -> Result<Option<String>, ProductError> {
        validate_mac_account(account)?;
        let _guard = MAC_CREDENTIAL_STORE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _file_guard = MacCredentialFileLock::acquire(&self.root)?;
        Ok(self.read_payload_locked()?.entries.get(account).cloned())
    }

    pub fn delete(&self, account: &str) -> Result<(), ProductError> {
        validate_mac_account(account)?;
        let _guard = MAC_CREDENTIAL_STORE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _file_guard = MacCredentialFileLock::acquire(&self.root)?;
        let mut payload = self.read_payload_locked()?;
        if payload.entries.remove(account).is_some() {
            self.write_payload_locked(&payload)?;
        }
        Ok(())
    }

    fn key_path(&self) -> PathBuf {
        self.root.join(MAC_MASTER_KEY_FILE)
    }

    fn store_path(&self) -> PathBuf {
        self.root.join(MAC_ENCRYPTED_STORE_FILE)
    }

    fn read_payload_locked(&self) -> Result<MacCredentialPayload, ProductError> {
        let store_path = self.store_path();
        if !path_exists_without_following_links(&store_path)? {
            return Ok(MacCredentialPayload {
                version: MAC_CREDENTIAL_FORMAT_VERSION,
                entries: BTreeMap::new(),
            });
        }

        let encoded = read_private_file(&store_path, MAC_MAX_ENCRYPTED_FILE_BYTES)?;
        let envelope: MacCredentialEnvelope =
            serde_json::from_slice(&encoded).map_err(|error| {
                ProductError::SecretError(format!(
                    "encrypted credential envelope is invalid: {error}"
                ))
            })?;
        if envelope.version != MAC_CREDENTIAL_FORMAT_VERSION {
            return Err(ProductError::SecretError(
                "encrypted credential envelope has an unsupported version".to_string(),
            ));
        }

        let nonce_bytes = BASE64.decode(envelope.nonce).map_err(|_| {
            ProductError::SecretError("encrypted credential nonce is invalid".to_string())
        })?;
        let nonce_bytes: [u8; MAC_NONCE_LEN] = nonce_bytes.try_into().map_err(|_| {
            ProductError::SecretError(
                "encrypted credential nonce has an invalid length".to_string(),
            )
        })?;
        let ciphertext = BASE64.decode(envelope.ciphertext).map_err(|_| {
            ProductError::SecretError("encrypted credential ciphertext is invalid".to_string())
        })?;
        if ciphertext.len() < CHACHA20_POLY1305.tag_len()
            || ciphertext.len() as u64 > MAC_MAX_ENCRYPTED_FILE_BYTES
        {
            return Err(ProductError::SecretError(
                "encrypted credential ciphertext has an invalid length".to_string(),
            ));
        }

        let master_key = self.read_master_key_locked()?;
        let key = LessSafeKey::new(
            UnboundKey::new(&CHACHA20_POLY1305, &master_key[..]).map_err(|_| {
                ProductError::SecretError("encrypted credential master key is invalid".to_string())
            })?,
        );
        let mut plaintext = Zeroizing::new(ciphertext);
        let opened = key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce_bytes),
                Aad::from(MAC_CREDENTIAL_AAD),
                &mut plaintext,
            )
            .map_err(|_| {
                ProductError::SecretError(
                    "encrypted credential authentication failed; refusing corrupted data"
                        .to_string(),
                )
            })?;
        let payload: MacCredentialPayload = serde_json::from_slice(opened).map_err(|error| {
            ProductError::SecretError(format!("encrypted credential payload is invalid: {error}"))
        })?;
        validate_mac_payload(&payload)?;
        Ok(payload)
    }

    fn write_payload_locked(&self, payload: &MacCredentialPayload) -> Result<(), ProductError> {
        validate_mac_payload(payload)?;
        ensure_private_directory(&self.root)?;
        let master_key = self.load_or_create_master_key_locked()?;
        let key = LessSafeKey::new(
            UnboundKey::new(&CHACHA20_POLY1305, &master_key[..]).map_err(|_| {
                ProductError::SecretError("encrypted credential master key is invalid".to_string())
            })?,
        );
        let random = SystemRandom::new();
        let mut nonce_bytes = [0_u8; MAC_NONCE_LEN];
        random.fill(&mut nonce_bytes).map_err(|_| {
            ProductError::SecretError("secure random nonce generation failed".to_string())
        })?;
        let mut ciphertext = serde_json::to_vec(payload).map_err(|error| {
            ProductError::SecretError(format!("serialize encrypted credentials: {error}"))
        })?;
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce_bytes),
            Aad::from(MAC_CREDENTIAL_AAD),
            &mut ciphertext,
        )
        .map_err(|_| ProductError::SecretError("credential encryption failed".to_string()))?;
        let envelope = MacCredentialEnvelope {
            version: MAC_CREDENTIAL_FORMAT_VERSION,
            nonce: BASE64.encode(nonce_bytes),
            ciphertext: BASE64.encode(ciphertext),
        };
        let mut encoded = serde_json::to_vec(&envelope).map_err(|error| {
            ProductError::SecretError(format!("serialize credential envelope: {error}"))
        })?;
        encoded.push(b'\n');
        if encoded.len() as u64 > MAC_MAX_ENCRYPTED_FILE_BYTES {
            return Err(ProductError::SecretError(
                "encrypted credential store exceeds its size limit".to_string(),
            ));
        }
        atomic_write_private(&self.store_path(), &encoded)
    }

    fn read_master_key_locked(&self) -> Result<Zeroizing<[u8; MAC_MASTER_KEY_LEN]>, ProductError> {
        let key_path = self.key_path();
        if !path_exists_without_following_links(&key_path)? {
            return Err(ProductError::SecretError(
                "encrypted credential master key is missing".to_string(),
            ));
        }
        let bytes = Zeroizing::new(read_private_file(&key_path, MAC_MASTER_KEY_LEN as u64)?);
        let key: [u8; MAC_MASTER_KEY_LEN] = bytes.as_slice().try_into().map_err(|_| {
            ProductError::SecretError(
                "encrypted credential master key has an invalid length".to_string(),
            )
        })?;
        Ok(Zeroizing::new(key))
    }

    fn load_or_create_master_key_locked(
        &self,
    ) -> Result<Zeroizing<[u8; MAC_MASTER_KEY_LEN]>, ProductError> {
        let key_path = self.key_path();
        if path_exists_without_following_links(&key_path)? {
            return self.read_master_key_locked();
        }

        ensure_private_directory(&self.root)?;
        let mut key = Zeroizing::new([0_u8; MAC_MASTER_KEY_LEN]);
        SystemRandom::new().fill(&mut key[..]).map_err(|_| {
            ProductError::SecretError("secure master key generation failed".to_string())
        })?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.root)?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        temporary.write_all(&key[..])?;
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(&key_path) {
            Ok(_) => {
                sync_directory(&self.root)?;
                Ok(key)
            }
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.read_master_key_locked()
            }
            Err(error) => Err(ProductError::from(error.error)),
        }
    }
}

/// Cross-process advisory lock for desktop, MCP sibling processes, and accidental second app
/// instances. Atomic rename protects readers from partial files; this lock additionally protects
/// the read-modify-write transaction from lost updates.
#[cfg(target_os = "macos")]
struct MacCredentialFileLock {
    file: File,
}

#[cfg(target_os = "macos")]
impl MacCredentialFileLock {
    fn acquire(root: &Path) -> Result<Self, ProductError> {
        ensure_private_directory(root)?;
        let path = root.join(MAC_CREDENTIAL_LOCK_FILE);
        if path_exists_without_following_links(&path)? {
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.is_file() || metadata.nlink() != 1 {
                return Err(ProductError::SecretError(
                    "credential lock must be a private regular file".to_string(),
                ));
            }
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(ProductError::SecretError(
                "credential lock must be a private regular file".to_string(),
            ));
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        loop {
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if result == 0 {
                return Ok(Self { file });
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(ProductError::from(error));
            }
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for MacCredentialFileLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(target_os = "macos")]
fn validate_mac_account(account: &str) -> Result<(), ProductError> {
    if account.is_empty()
        || account.len() > MAC_MAX_ACCOUNT_BYTES
        || account.chars().any(char::is_control)
    {
        return Err(ProductError::SecretError(
            "credential account is empty, too long, or contains control characters".to_string(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn validate_mac_credential(account: &str, value: &str) -> Result<(), ProductError> {
    validate_mac_account(account)?;
    if value.len() > MAC_MAX_SECRET_BYTES {
        return Err(ProductError::SecretError(
            "credential value exceeds its size limit".to_string(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn validate_mac_payload(payload: &MacCredentialPayload) -> Result<(), ProductError> {
    if payload.version != MAC_CREDENTIAL_FORMAT_VERSION {
        return Err(ProductError::SecretError(
            "encrypted credential payload has an unsupported version".to_string(),
        ));
    }
    if payload.entries.len() > MAC_MAX_ACCOUNTS {
        return Err(ProductError::SecretError(
            "encrypted credential store has too many entries".to_string(),
        ));
    }
    for (account, value) in &payload.entries {
        validate_mac_credential(account, value)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn ensure_private_directory(path: &Path) -> Result<(), ProductError> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ProductError::SecretError(
            "credential directory must be a real directory".to_string(),
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn path_exists_without_following_links(path: &Path) -> Result<bool, ProductError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(ProductError::SecretError(
                    "credential path must not be a symbolic link".to_string(),
                ));
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(ProductError::from(error)),
    }
}

#[cfg(target_os = "macos")]
fn read_private_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, ProductError> {
    // Open the path without following symlinks, then validate and read from that same handle. A
    // path-level metadata check followed by `fs::read` would leave a check/use race where another
    // process could swap in a link between the two operations.
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(ProductError::SecretError(
            "credential path must be a private regular file".to_string(),
        ));
    }
    if metadata.len() > max_bytes {
        return Err(ProductError::SecretError(
            "credential file exceeds its size limit".to_string(),
        ));
    }
    if metadata.permissions().mode() & 0o777 != 0o600 {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    let capacity = usize::try_from(metadata.len().min(max_bytes)).unwrap_or(0);
    let mut content = Vec::with_capacity(capacity);
    Read::by_ref(&mut file)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut content)?;
    if content.len() as u64 > max_bytes {
        return Err(ProductError::SecretError(
            "credential file exceeds its size limit".to_string(),
        ));
    }
    Ok(content)
}

#[cfg(target_os = "macos")]
fn atomic_write_private(path: &Path, content: &[u8]) -> Result<(), ProductError> {
    let parent = path.parent().ok_or_else(|| {
        ProductError::SecretError("credential file has no parent directory".to_string())
    })?;
    ensure_private_directory(parent)?;
    if path_exists_without_following_links(path)? {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(ProductError::SecretError(
                "credential path must be a private regular file".to_string(),
            ));
        }
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temporary.write_all(content)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| ProductError::from(error.error))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    sync_directory(parent)
}

#[cfg(target_os = "macos")]
fn sync_directory(path: &Path) -> Result<(), ProductError> {
    File::open(path)?.sync_all()?;
    Ok(())
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

    // ---- SecretStore / macOS encrypted credential file ----

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn secret_store_new_stores_service_name() {
        let store = SecretStore::new("r-code");
        assert_eq!(store.service_name, "r-code");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn secret_store_new_accepts_string() {
        let name = String::from("r-code-prod");
        let store = SecretStore::new(name);
        assert_eq!(store.service_name, "r-code-prod");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn secret_entry_round_trip_uses_a_deterministic_backend() {
        // A headless runner is not a valid integration environment for Windows Credential
        // Manager or Linux Secret Service. Use
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

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_encrypted_store_round_trips_provider_and_mcp_across_instances() {
        let temp = tempfile::tempdir().unwrap();
        let provider = "provider:deepseek";
        let mcp = "mcp:github:header:Authorization";

        EncryptedFileSecretStore::new(temp.path())
            .store(provider, "provider-secret")
            .unwrap();
        EncryptedFileSecretStore::new(temp.path())
            .store(mcp, "mcp-secret")
            .unwrap();

        let reopened = EncryptedFileSecretStore::new(temp.path());
        assert_eq!(
            reopened.get(provider).unwrap().as_deref(),
            Some("provider-secret")
        );
        assert_eq!(reopened.get(mcp).unwrap().as_deref(), Some("mcp-secret"));
        reopened.delete(provider).unwrap();
        reopened.delete(provider).unwrap();
        assert_eq!(
            EncryptedFileSecretStore::new(temp.path())
                .get(provider)
                .unwrap(),
            None
        );
        assert_eq!(
            EncryptedFileSecretStore::new(temp.path())
                .get(mcp)
                .unwrap()
                .as_deref(),
            Some("mcp-secret")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_encrypted_store_never_writes_plaintext_and_uses_private_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let store = EncryptedFileSecretStore::new(temp.path());
        let secret = "sentinel-secret-that-must-not-appear-on-disk";
        store.store("provider:openai", secret).unwrap();

        let key_path = temp
            .path()
            .join(MAC_CREDENTIAL_DIRECTORY)
            .join(MAC_MASTER_KEY_FILE);
        let store_path = temp
            .path()
            .join(MAC_CREDENTIAL_DIRECTORY)
            .join(MAC_ENCRYPTED_STORE_FILE);
        let lock_path = temp
            .path()
            .join(MAC_CREDENTIAL_DIRECTORY)
            .join(MAC_CREDENTIAL_LOCK_FILE);
        let ciphertext = fs::read(&store_path).unwrap();
        assert!(!ciphertext
            .windows(secret.len())
            .any(|window| window == secret.as_bytes()));
        assert!(!ciphertext
            .windows("provider:openai".len())
            .any(|window| { window == b"provider:openai" }));
        assert_eq!(
            fs::metadata(&key_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&store_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&lock_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(temp.path().join(MAC_CREDENTIAL_DIRECTORY))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_encrypted_store_fails_closed_when_ciphertext_is_tampered() {
        let temp = tempfile::tempdir().unwrap();
        let store = EncryptedFileSecretStore::new(temp.path());
        store
            .store("provider:anthropic", "tamper-test-secret")
            .unwrap();
        let path = store.store_path();
        let mut envelope: MacCredentialEnvelope =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let mut ciphertext = BASE64.decode(&envelope.ciphertext).unwrap();
        ciphertext[0] ^= 0x80;
        envelope.ciphertext = BASE64.encode(ciphertext);
        fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();

        let error = EncryptedFileSecretStore::new(temp.path())
            .get("provider:anthropic")
            .unwrap_err();
        assert!(error.to_string().contains("authentication failed"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_encrypted_store_isolates_different_config_directories() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let account = "provider:openai";
        EncryptedFileSecretStore::new(first.path())
            .store(account, "first-secret")
            .unwrap();
        EncryptedFileSecretStore::new(second.path())
            .store(account, "second-secret")
            .unwrap();

        assert_eq!(
            EncryptedFileSecretStore::new(first.path())
                .get(account)
                .unwrap()
                .as_deref(),
            Some("first-secret")
        );
        assert_eq!(
            EncryptedFileSecretStore::new(second.path())
                .get(account)
                .unwrap()
                .as_deref(),
            Some("second-secret")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_encrypted_store_rejects_symlinks_and_hardlinks() {
        use std::os::unix::fs::symlink;

        let symlink_case = tempfile::tempdir().unwrap();
        let symlink_store = EncryptedFileSecretStore::new(symlink_case.path());
        ensure_private_directory(&symlink_store.root).unwrap();
        let outside = symlink_case.path().join("outside");
        fs::write(&outside, b"not-a-credential-store").unwrap();
        symlink(&outside, symlink_store.store_path()).unwrap();
        assert!(symlink_store.get("provider:openai").is_err());

        let hardlink_case = tempfile::tempdir().unwrap();
        let hardlink_store = EncryptedFileSecretStore::new(hardlink_case.path());
        hardlink_store
            .store("provider:openai", "hardlink-test-secret")
            .unwrap();
        fs::hard_link(
            hardlink_store.store_path(),
            hardlink_case.path().join("credential-hardlink"),
        )
        .unwrap();
        assert!(hardlink_store.get("provider:openai").is_err());
    }
}

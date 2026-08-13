//! 设置服务 -- 全局配置 + 工作区覆盖。
//!
//! 优先级链：默认值 < 全局配置文件 < 工作区配置 < 环境变量 < 显式参数。
//! 全局配置位于 `config_dir/config.toml`（TOML）。
//! 工作区覆盖位于 `<workspace>/.r-code/config.toml`，与全局配置递归合并
//! （工作区字段覆盖全局同名标量；嵌套表深度合并）。
//!
//! 环境变量覆盖（`ANTHROPIC_API_KEY` / `OPENAI_API_KEY` / `DEEPSEEK_API_KEY`
//! 或 `R_CODE_PROVIDER_<配置名>_API_KEY`）在合并后应用，
//! 优先级最高（仅次于显式参数）。校验在最后执行。
//!
//! [doc-14 阶段1] [agent-core/08]

#[cfg(all(not(test), not(target_os = "macos")))]
use std::sync::LazyLock;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use hermes_config::Config;
use r_code_agent_worker::AgentPromptPolicy;
use r_code_core::error::ProductError;
#[cfg(all(not(test), target_os = "macos"))]
use r_code_core::secret::EncryptedFileSecretStore;
#[cfg(all(not(test), not(target_os = "macos")))]
use r_code_core::secret::SecretStore;

#[cfg(all(not(test), not(target_os = "macos")))]
const SECRET_SERVICE: &str = "r-code";
const AGENT_PROMPTS_FILE: &str = "agent-prompts.toml";
const MAX_AGENT_PROMPT_CHARS: usize = 20_000;

trait ProviderCredentialBackend: Send + Sync {
    fn set(&self, provider: &str, value: &str) -> Result<(), ProductError>;
    fn get(&self, provider: &str) -> Result<Option<String>, ProductError>;
    fn delete(&self, provider: &str) -> Result<(), ProductError>;
}

/// Keep credentials that were already resolved in process memory.
///
/// Runtime configuration is intentionally reconstructed at several product boundaries (settings,
/// task launch, model discovery, MCP host startup). Going back to the platform credential backend at
/// every boundary is unnecessary. The cache also coalesces concurrent first reads by holding its
/// lock across the blocking platform call. Values are never logged or written to the config file.
struct CachedProviderCredentialBackend {
    inner: Arc<dyn ProviderCredentialBackend>,
    values: Mutex<HashMap<String, Option<String>>>,
}

impl CachedProviderCredentialBackend {
    fn new(inner: Arc<dyn ProviderCredentialBackend>) -> Self {
        Self {
            inner,
            values: Mutex::new(HashMap::new()),
        }
    }

    fn values(&self) -> std::sync::MutexGuard<'_, HashMap<String, Option<String>>> {
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl ProviderCredentialBackend for CachedProviderCredentialBackend {
    fn set(&self, provider: &str, value: &str) -> Result<(), ProductError> {
        let mut values = self.values();
        if values
            .get(provider)
            .and_then(Option::as_deref)
            .is_some_and(|stored| stored == value)
        {
            return Ok(());
        }
        self.inner.set(provider, value)?;
        values.insert(provider.to_string(), Some(value.to_string()));
        Ok(())
    }

    fn get(&self, provider: &str) -> Result<Option<String>, ProductError> {
        let mut values = self.values();
        if let Some(value) = values.get(provider) {
            return Ok(value.clone());
        }
        let value = self.inner.get(provider)?;
        values.insert(provider.to_string(), value.clone());
        Ok(value)
    }

    fn delete(&self, provider: &str) -> Result<(), ProductError> {
        let mut values = self.values();
        if matches!(values.get(provider), Some(None)) {
            return Ok(());
        }
        self.inner.delete(provider)?;
        values.insert(provider.to_string(), None);
        Ok(())
    }
}

#[cfg(all(not(test), not(target_os = "macos")))]
#[derive(Default)]
struct OsProviderCredentialBackend;

#[cfg(all(not(test), not(target_os = "macos")))]
impl ProviderCredentialBackend for OsProviderCredentialBackend {
    fn set(&self, provider: &str, value: &str) -> Result<(), ProductError> {
        SecretStore::new(SECRET_SERVICE).store(&SettingsService::secret_key(provider), value)
    }

    fn get(&self, provider: &str) -> Result<Option<String>, ProductError> {
        SecretStore::new(SECRET_SERVICE).get(&SettingsService::secret_key(provider))
    }

    fn delete(&self, provider: &str) -> Result<(), ProductError> {
        SecretStore::new(SECRET_SERVICE).delete(&SettingsService::secret_key(provider))
    }
}

#[cfg(all(not(test), not(target_os = "macos")))]
static OS_PROVIDER_CREDENTIALS: LazyLock<Arc<dyn ProviderCredentialBackend>> =
    LazyLock::new(|| {
        Arc::new(CachedProviderCredentialBackend::new(Arc::new(
            OsProviderCredentialBackend,
        )))
    });

#[cfg(all(not(test), target_os = "macos"))]
struct MacFileProviderCredentialBackend {
    store: EncryptedFileSecretStore,
}

#[cfg(all(not(test), target_os = "macos"))]
impl ProviderCredentialBackend for MacFileProviderCredentialBackend {
    fn set(&self, provider: &str, value: &str) -> Result<(), ProductError> {
        self.store
            .store(&SettingsService::secret_key(provider), value)
    }

    fn get(&self, provider: &str) -> Result<Option<String>, ProductError> {
        self.store.get(&SettingsService::secret_key(provider))
    }

    fn delete(&self, provider: &str) -> Result<(), ProductError> {
        self.store.delete(&SettingsService::secret_key(provider))
    }
}

// Settings unit tests must not mutate a developer or runner's real credential store.
// Platform credential backends own their integration tests; settings tests use a process-local
// backend namespaced by config directory so parallel states do not see each other's credentials.
#[cfg(test)]
static TEST_PROVIDER_CREDENTIALS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<(PathBuf, String), String>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[cfg(test)]
struct TestProviderCredentialBackend {
    namespace: PathBuf,
}

#[cfg(test)]
impl TestProviderCredentialBackend {
    fn values(
    ) -> std::sync::MutexGuard<'static, std::collections::HashMap<(PathBuf, String), String>> {
        TEST_PROVIDER_CREDENTIALS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn key(&self, provider: &str) -> (PathBuf, String) {
        (self.namespace.clone(), provider.to_string())
    }
}

#[cfg(test)]
impl ProviderCredentialBackend for TestProviderCredentialBackend {
    fn set(&self, provider: &str, value: &str) -> Result<(), ProductError> {
        Self::values().insert(self.key(provider), value.to_string());
        Ok(())
    }

    fn get(&self, provider: &str) -> Result<Option<String>, ProductError> {
        Ok(Self::values().get(&self.key(provider)).cloned())
    }

    fn delete(&self, provider: &str) -> Result<(), ProductError> {
        Self::values().remove(&self.key(provider));
        Ok(())
    }
}

/// 设置服务 -- 管理全局配置 + 工作区覆盖。
///
/// 优先级：默认值 < 配置文件 < 环境变量 < 显式参数。
pub struct SettingsService {
    config_dir: PathBuf,
    credentials: Arc<dyn ProviderCredentialBackend>,
}

impl SettingsService {
    /// 创建设置服务，`config_dir` 为全局配置目录。
    pub fn new(config_dir: PathBuf) -> Self {
        #[cfg(all(not(test), not(target_os = "macos")))]
        let credentials = OS_PROVIDER_CREDENTIALS.clone();
        #[cfg(all(not(test), target_os = "macos"))]
        let credentials: Arc<dyn ProviderCredentialBackend> = Arc::new(
            CachedProviderCredentialBackend::new(Arc::new(MacFileProviderCredentialBackend {
                store: EncryptedFileSecretStore::new(config_dir.clone()),
            })),
        );
        #[cfg(test)]
        let credentials: Arc<dyn ProviderCredentialBackend> =
            Arc::new(TestProviderCredentialBackend {
                namespace: config_dir.clone(),
            });
        Self {
            config_dir,
            credentials,
        }
    }

    /// Product-owned MCP metadata is stored beside the other user-level settings.
    pub fn mcp_settings(&self) -> crate::mcp_settings::McpSettingsService {
        crate::mcp_settings::McpSettingsService::new(self.config_dir.clone())
    }

    /// 全局配置文件路径：`config_dir/config.toml`。
    pub fn config_path(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// 用户级 Agent 协作 Prompt 独立保存在 AppData，不参与工作区配置合并，也不会
    /// 写进项目目录或 Git 工作树。
    pub fn agent_prompts_path(&self) -> PathBuf {
        self.config_dir.join(AGENT_PROMPTS_FILE)
    }

    pub fn load_agent_prompts(&self) -> Result<AgentPromptPolicy, ProductError> {
        let path = self.agent_prompts_path();
        if !path.exists() {
            return Ok(AgentPromptPolicy::default());
        }
        let content = std::fs::read_to_string(&path).map_err(|error| {
            ProductError::ConfigError(format!("read {}: {error}", path.display()))
        })?;
        let prompts: AgentPromptPolicy = toml::from_str(&content).map_err(|error| {
            ProductError::ConfigError(format!("parse {}: {error}", path.display()))
        })?;
        Self::validate_agent_prompts(&prompts)?;
        Ok(prompts)
    }

    pub fn save_agent_prompts(&self, prompts: &AgentPromptPolicy) -> Result<(), ProductError> {
        Self::validate_agent_prompts(prompts)?;
        std::fs::create_dir_all(&self.config_dir)?;
        let content = toml::to_string_pretty(prompts)
            .map_err(|error| ProductError::ConfigError(format!("serialize prompts: {error}")))?;
        std::fs::write(self.agent_prompts_path(), content)?;
        Ok(())
    }

    pub fn reset_agent_prompts(&self) -> Result<AgentPromptPolicy, ProductError> {
        let prompts = AgentPromptPolicy::default();
        self.save_agent_prompts(&prompts)?;
        Ok(prompts)
    }

    fn validate_agent_prompts(prompts: &AgentPromptPolicy) -> Result<(), ProductError> {
        for (label, value) in [
            ("main_agent", prompts.main_agent.as_str()),
            ("subagent", prompts.subagent.as_str()),
        ] {
            if value.contains('\0') {
                return Err(ProductError::ConfigError(format!(
                    "agent prompt '{label}' contains a null character"
                )));
            }
            if value.chars().count() > MAX_AGENT_PROMPT_CHARS {
                return Err(ProductError::ConfigError(format!(
                    "agent prompt '{label}' exceeds {MAX_AGENT_PROMPT_CHARS} characters"
                )));
            }
        }
        Ok(())
    }

    /// 加载全局配置。
    ///
    /// 若配置文件存在则解析（默认值 < 文件），否则使用默认值；
    /// 随后应用环境变量覆盖，最后校验。
    pub fn load_global(&self) -> Result<Config, ProductError> {
        let config = self.load_global_unvalidated()?;
        Self::validate(&config)?;
        Ok(config)
    }

    /// 加载运行时配置但不校验。设置页可借此展示未完成的 Provider 草稿。
    pub fn load_global_unvalidated(&self) -> Result<Config, ProductError> {
        let path = self.config_path();
        let mut config = if path.exists() {
            Self::parse_config_file(&path)?
        } else {
            Config::default()
        };
        // Environment credentials have the highest runtime priority. Apply them before the
        // persisted-credential fallback so an explicitly supplied key never causes an unnecessary
        // platform store read.
        apply_env(&mut config);
        self.hydrate_secrets(&mut config)?;
        Ok(config)
    }

    /// 加载配置并应用工作区覆盖。
    ///
    /// 优先级：默认值 < 全局配置 < 工作区 `.r-code/config.toml` < 环境变量。
    pub fn load_with_workspace(&self, workspace_path: &str) -> Result<Config, ProductError> {
        // 1. 全局配置（文件或默认）作为 base，以 toml::Value 表示便于递归合并
        let global_path = self.config_path();
        let global_str = if global_path.exists() {
            std::fs::read_to_string(&global_path).map_err(|e| {
                ProductError::ConfigError(format!("read {}: {e}", global_path.display()))
            })?
        } else {
            toml::to_string(&Config::default())
                .map_err(|e| ProductError::ConfigError(format!("serialize defaults: {e}")))?
        };
        let mut base: toml::Value = toml::from_str(&global_str)
            .map_err(|e| ProductError::ConfigError(format!("parse global config: {e}")))?;

        // 2. 合并工作区覆盖（若存在）
        let ws_path = PathBuf::from(workspace_path)
            .join(".r-code")
            .join("config.toml");
        if ws_path.exists() {
            let content = std::fs::read_to_string(&ws_path).map_err(|e| {
                ProductError::ConfigError(format!("read {}: {e}", ws_path.display()))
            })?;
            let over: toml::Value = toml::from_str(&content).map_err(|e| {
                ProductError::ConfigError(format!("parse {}: {e}", ws_path.display()))
            })?;
            merge_toml(&mut base, &over);
        }

        // 3. 反序列化合并结果 -> Config
        let merged_str = toml::to_string(&base)
            .map_err(|e| ProductError::ConfigError(format!("serialize merged: {e}")))?;
        let mut config: Config = toml::from_str(&merged_str)
            .map_err(|e| ProductError::ConfigError(format!("deserialize merged: {e}")))?;

        // 4. 环境变量覆盖（最高优先级，仅次于显式参数）。先应用环境凭据可避免
        // 已明确提供 key 时仍访问持久化凭据。
        apply_env(&mut config);

        // 5. 平台凭据后端只填充仍为空的 api_key。
        self.hydrate_secrets(&mut config)?;

        // 6. 校验
        Self::validate(&config)?;
        Ok(config)
    }

    /// 保存全局配置到 `config_dir/config.toml`（TOML）。
    ///
    /// API key 始终剥离；macOS 写入本地加密文件，其他平台写入系统凭据库。
    pub fn save_global(&self, config: &Config) -> Result<(), ProductError> {
        // 先写入平台凭据后端，再落无密钥的 TOML。顺序不能反过来，否则持久化
        // 失败时会把用户唯一的凭据静默抹掉。环境变量只影响本次进程，不应被复制落盘。
        for (name, provider) in &config.providers {
            if !provider.api_key.trim().is_empty() {
                if provider_env_value(name, provider.provider_kind.as_deref()).as_deref()
                    == Some(provider.api_key.as_str())
                {
                    continue;
                }
                self.set_provider_secret(name, &provider.api_key)?;
            }
        }
        let mut sanitized = config.clone();
        for provider in sanitized.providers.values_mut() {
            provider.api_key.clear();
        }
        self.write_global(&sanitized)
    }

    /// 持久化 Provider API key。macOS 使用本地 AEAD 文件；空值代表删除已有凭据。
    pub fn set_provider_secret(&self, provider: &str, value: &str) -> Result<(), ProductError> {
        if provider.trim().is_empty() {
            return Err(ProductError::ConfigError(
                "provider name cannot be empty".to_string(),
            ));
        }
        if value.trim().is_empty() {
            self.credentials.delete(provider)
        } else {
            self.credentials.set(provider, value)
        }
    }

    /// 读取 Provider 已保存的密钥；不会将值记录到日志。
    pub fn provider_secret(&self, provider: &str) -> Result<Option<String>, ProductError> {
        self.credentials.get(provider)
    }

    /// 将旧版 TOML 中的明文 api_key 迁移至当前平台凭据后端。
    ///
    /// 返回迁移条数。调用方可选择把凭据后端不可用作为软错误处理，避免阻塞应用
    /// 启动；迁移成功后 TOML 会立即改写为无密钥版本。
    pub fn migrate_legacy_provider_secrets(&self) -> Result<usize, ProductError> {
        let path = self.config_path();
        if !path.exists() {
            return Ok(0);
        }
        let mut config = Self::parse_config_file(&path)?;
        let mut migrated = 0usize;
        for (name, provider) in &mut config.providers {
            if provider.api_key.trim().is_empty() {
                continue;
            }
            self.set_provider_secret(name, &provider.api_key)?;
            provider.api_key.clear();
            migrated += 1;
        }
        if migrated > 0 {
            self.write_global(&config)?;
        }
        Ok(migrated)
    }

    /// Persist stable provider identities for configurations written before the field existed.
    /// Inference is deliberately one-shot: later profile renames or gateway edits cannot change it.
    pub fn migrate_legacy_provider_kinds(&self) -> Result<usize, ProductError> {
        let path = self.config_path();
        if !path.exists() {
            return Ok(0);
        }
        let mut config = Self::parse_config_file(&path)?;
        let mut migrated = 0usize;
        for (name, provider) in &mut config.providers {
            if provider.provider_kind.is_some() {
                continue;
            }
            let Some(kind) =
                crate::provider_catalog::infer_legacy_provider_kind(name, &provider.base_url)
            else {
                continue;
            };
            provider.provider_kind = Some(kind.to_string());
            migrated += 1;
        }
        if migrated > 0 {
            self.write_global(&config)?;
        }
        Ok(migrated)
    }

    fn write_global(&self, config: &Config) -> Result<(), ProductError> {
        let path = self.config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let toml_str = toml::to_string(config)
            .map_err(|e| ProductError::ConfigError(format!("serialize config: {e}")))?;
        std::fs::write(&path, toml_str)?;
        Ok(())
    }

    #[cfg(not(test))]
    fn secret_key(provider: &str) -> String {
        format!("provider:{provider}")
    }

    fn hydrate_secrets(&self, config: &mut Config) -> Result<(), ProductError> {
        for (name, provider) in &mut config.providers {
            if provider.api_key.trim().is_empty() {
                if let Some(secret) = self.provider_secret(name)? {
                    provider.api_key = secret;
                }
            }
        }
        Ok(())
    }

    /// 解析单个配置文件为 `Config`（不含 env / 校验）。
    fn parse_config_file(path: &PathBuf) -> Result<Config, ProductError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| ProductError::ConfigError(format!("read {}: {e}", path.display())))?;
        toml::from_str(&content)
            .map_err(|e| ProductError::ConfigError(format!("parse {}: {e}", path.display())))
    }

    /// 校验配置，将 `hermes_error::Error` 映射为 `ProductError::ConfigError`。
    fn validate(config: &Config) -> Result<(), ProductError> {
        config
            .validate()
            .map_err(|e| ProductError::ConfigError(e.to_string()))
    }
}

fn provider_env_name(name: &str) -> Option<String> {
    let mut normalized = String::with_capacity(name.len());
    let mut previous_underscore = false;
    for ch in name.chars() {
        let next = if ch.is_ascii_alphanumeric() {
            previous_underscore = false;
            ch.to_ascii_uppercase()
        } else if previous_underscore {
            continue;
        } else {
            previous_underscore = true;
            '_'
        };
        normalized.push(next);
    }
    let normalized = normalized.trim_matches('_');
    (!normalized.is_empty()).then(|| format!("R_CODE_PROVIDER_{normalized}_API_KEY"))
}

/// Resolve an explicit environment credential for one saved provider profile.
///
/// The profile-scoped variable has highest priority and works for every custom profile. Common
/// vendor variables remain convenient aliases and follow the stable `provider_kind`, so renaming a
/// DeepSeek profile does not unexpectedly make `DEEPSEEK_API_KEY` stop working.
pub(crate) fn provider_env_value(name: &str, provider_kind: Option<&str>) -> Option<String> {
    if let Some(variable) = provider_env_name(name) {
        if let Ok(value) = std::env::var(variable) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }

    let kind = provider_kind.unwrap_or(name).trim().to_ascii_lowercase();
    let variable = match kind.as_str() {
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "deepseek" | "deepseek_anthropic" => "DEEPSEEK_API_KEY",
        _ => return None,
    };
    std::env::var(variable)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// Environment credentials override persisted credentials without touching the platform store.
/// Only existing provider entries are modified.
fn apply_env(config: &mut Config) {
    for (name, provider) in &mut config.providers {
        if let Some(key) = provider_env_value(name, provider.provider_kind.as_deref()) {
            provider.api_key = key;
        }
    }
}

/// 递归合并两个 TOML 值：`over` 覆盖 `base`。
///
/// - 两边均为 table：深度合并（`over` 的叶子覆盖 `base` 同名键）。
/// - 否则：`over` 整体覆盖 `base`。
fn merge_toml(base: &mut toml::Value, over: &toml::Value) {
    use toml::Value;
    match (base, over) {
        (Value::Table(base_tbl), Value::Table(over_tbl)) => {
            for (k, v) in over_tbl {
                if let Some(existing) = base_tbl.get_mut(k) {
                    if matches!(existing, Value::Table(_)) && matches!(v, Value::Table(_)) {
                        merge_toml(existing, v);
                        continue;
                    }
                }
                base_tbl.insert(k.clone(), v.clone());
            }
        }
        (base, over) => {
            *base = over.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::ProviderConfig;
    use std::path::Path;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    };

    // 环境变量是进程级全局状态，多个测试并行操作会竞态；用锁串行化所有
    // 设置测试（它们都调用 apply_env，会读取 ANTHROPIC_API_KEY）。
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[derive(Default)]
    struct CountingCredentials {
        values: Mutex<HashMap<String, String>>,
        reads: AtomicUsize,
        writes: AtomicUsize,
        deletes: AtomicUsize,
    }

    impl ProviderCredentialBackend for CountingCredentials {
        fn set(&self, provider: &str, value: &str) -> Result<(), ProductError> {
            self.writes.fetch_add(1, Ordering::Relaxed);
            self.values
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(provider.to_string(), value.to_string());
            Ok(())
        }

        fn get(&self, provider: &str) -> Result<Option<String>, ProductError> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            Ok(self
                .values
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(provider)
                .cloned())
        }

        fn delete(&self, provider: &str) -> Result<(), ProductError> {
            self.deletes.fetch_add(1, Ordering::Relaxed);
            self.values
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(provider);
            Ok(())
        }
    }

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn valid_provider(api_key: &str) -> ProviderConfig {
        ProviderConfig {
            base_url: "https://api.anthropic.com".into(),
            api_key: api_key.into(),
            model: "claude-sonnet-4".into(),
            max_tokens: None,
            temperature: None,
            protocol: None,
            provider_kind: None,
            show_reasoning: false,
        }
    }

    #[test]
    fn process_credential_cache_reads_and_writes_each_value_once() {
        let inner = Arc::new(CountingCredentials::default());
        let cached = CachedProviderCredentialBackend::new(inner.clone());

        assert_eq!(cached.get("deepseek").unwrap(), None);
        assert_eq!(cached.get("deepseek").unwrap(), None);
        assert_eq!(inner.reads.load(Ordering::Relaxed), 1);

        cached.set("deepseek", "sentinel-secret").unwrap();
        cached.set("deepseek", "sentinel-secret").unwrap();
        assert_eq!(inner.writes.load(Ordering::Relaxed), 1);
        assert_eq!(
            cached.get("deepseek").unwrap().as_deref(),
            Some("sentinel-secret")
        );
        assert_eq!(inner.reads.load(Ordering::Relaxed), 1);

        cached.delete("deepseek").unwrap();
        cached.delete("deepseek").unwrap();
        assert_eq!(inner.deletes.load(Ordering::Relaxed), 1);
        assert_eq!(cached.get("deepseek").unwrap(), None);
        assert_eq!(inner.reads.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn environment_credentials_skip_platform_credential_lookup() {
        let _guard = lock_env();
        std::env::set_var("ANTHROPIC_API_KEY", "sentinel-env-secret");

        let result = {
            let tmp = tempfile::tempdir().unwrap();
            let inner = Arc::new(CountingCredentials::default());
            let service = SettingsService {
                config_dir: tmp.path().to_path_buf(),
                credentials: Arc::new(CachedProviderCredentialBackend::new(inner.clone())),
            };
            let mut config = Config::default();
            config
                .providers
                .insert("anthropic".to_string(), valid_provider(""));
            service.write_global(&config).unwrap();
            let loaded = service.load_global_unvalidated();
            (loaded, inner.reads.load(Ordering::Relaxed))
        };

        std::env::remove_var("ANTHROPIC_API_KEY");
        let (loaded, reads) = result;
        assert_eq!(
            loaded.unwrap().providers["anthropic"].api_key,
            "sentinel-env-secret"
        );
        assert_eq!(reads, 0);
    }

    #[test]
    fn environment_credentials_are_not_copied_into_persistent_store_on_save() {
        let _guard = lock_env();
        std::env::set_var("ANTHROPIC_API_KEY", "sentinel-env-only-secret");

        let result = {
            let tmp = tempfile::tempdir().unwrap();
            let inner = Arc::new(CountingCredentials::default());
            let service = SettingsService {
                config_dir: tmp.path().to_path_buf(),
                credentials: Arc::new(CachedProviderCredentialBackend::new(inner.clone())),
            };
            let mut config = Config::default();
            let mut provider = valid_provider("sentinel-env-only-secret");
            provider.provider_kind = Some("anthropic".to_string());
            config.providers.insert("anthropic".to_string(), provider);
            let saved = service.save_global(&config);
            (saved, inner.writes.load(Ordering::Relaxed))
        };

        std::env::remove_var("ANTHROPIC_API_KEY");
        assert!(result.0.is_ok());
        assert_eq!(result.1, 0);
    }

    #[test]
    fn deepseek_and_profile_scoped_environment_credentials_skip_platform_lookup() {
        let _guard = lock_env();
        std::env::set_var("DEEPSEEK_API_KEY", "sentinel-deepseek-secret");
        std::env::set_var(
            "R_CODE_PROVIDER_DEEPSEEK_WORK_API_KEY",
            "sentinel-profile-secret",
        );

        let result = {
            let tmp = tempfile::tempdir().unwrap();
            let inner = Arc::new(CountingCredentials::default());
            let service = SettingsService {
                config_dir: tmp.path().to_path_buf(),
                credentials: Arc::new(CachedProviderCredentialBackend::new(inner.clone())),
            };
            let mut config = Config::default();
            let mut vendor = valid_provider("");
            vendor.provider_kind = Some("deepseek".to_string());
            let mut profile = valid_provider("");
            profile.provider_kind = Some("deepseek".to_string());
            config.providers.insert("deepseek".to_string(), vendor);
            config
                .providers
                .insert("DeepSeek Work".to_string(), profile);
            service.write_global(&config).unwrap();
            let loaded = service.load_global_unvalidated();
            (loaded, inner.reads.load(Ordering::Relaxed))
        };

        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("R_CODE_PROVIDER_DEEPSEEK_WORK_API_KEY");
        let (loaded, reads) = result;
        let loaded = loaded.unwrap();
        assert_eq!(
            loaded.providers["deepseek"].api_key,
            "sentinel-deepseek-secret"
        );
        assert_eq!(
            loaded.providers["DeepSeek Work"].api_key,
            "sentinel-profile-secret"
        );
        assert_eq!(reads, 0);
    }

    /// 写入一份合法的全局配置到 `dir/config.toml`，返回其路径。
    fn write_valid_config(dir: &Path, log_level: &str, api_key: &str) -> PathBuf {
        let base_dir = dir.join("data");
        // Windows 路径含反斜杠，写入 TOML basic string 会触发 \U 转义错误 → 统一为正斜杠
        let slash = |p: &Path| p.display().to_string().replace('\\', "/");
        let toml_str = format!(
            r#"
default_provider = "anthropic"
log_level = "{log_level}"

[providers.anthropic]
base_url = "https://api.anthropic.com"
api_key = "{api_key}"
model = "claude-sonnet-4"

[storage]
base_dir = "{base}"
sessions_dir = "{s}"
skills_dir = "{k}"
memories_dir = "{m}"
"#,
            base = slash(&base_dir),
            s = slash(&base_dir.join("s")),
            k = slash(&base_dir.join("k")),
            m = slash(&base_dir.join("m")),
        );
        let path = dir.join("config.toml");
        std::fs::write(&path, toml_str).unwrap();
        path
    }

    #[test]
    fn config_path_joins_config_toml() {
        let _guard = lock_env();
        let svc = SettingsService::new(PathBuf::from("/tmp/r-code-cfg"));
        assert_eq!(
            svc.config_path(),
            PathBuf::from("/tmp/r-code-cfg/config.toml")
        );
    }

    #[test]
    fn agent_prompts_roundtrip_in_user_config_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = SettingsService::new(tmp.path().to_path_buf());
        let custom = AgentPromptPolicy {
            main_agent: "main custom".to_string(),
            subagent: "child custom".to_string(),
        };

        svc.save_agent_prompts(&custom).unwrap();
        assert_eq!(svc.load_agent_prompts().unwrap(), custom);
        assert_eq!(
            svc.agent_prompts_path(),
            tmp.path().join(AGENT_PROMPTS_FILE)
        );

        let reset = svc.reset_agent_prompts().unwrap();
        assert_eq!(reset, AgentPromptPolicy::default());
        assert_eq!(svc.load_agent_prompts().unwrap(), reset);
    }

    #[test]
    fn load_global_reads_config_file() {
        let _guard = lock_env();
        let tmp = tempfile::tempdir().unwrap();
        write_valid_config(tmp.path(), "debug", "sk-test");

        let svc = SettingsService::new(tmp.path().to_path_buf());
        let cfg = svc.load_global().unwrap();
        assert_eq!(cfg.log_level, "debug");
        assert_eq!(cfg.default_provider, "anthropic");
        assert_eq!(cfg.providers.get("anthropic").unwrap().api_key, "sk-test");
    }

    #[test]
    fn load_global_without_file_uses_defaults() {
        let _guard = lock_env();
        // 确保没有遗留的 ANTHROPIC_API_KEY 导致默认配置意外「合法」
        std::env::remove_var("ANTHROPIC_API_KEY");

        let tmp = tempfile::tempdir().unwrap();
        let svc = SettingsService::new(tmp.path().to_path_buf());
        // 默认配置无 provider，校验应失败
        let result = svc.load_global();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ProductError::ConfigError(_)));
        // 默认值仍然体现在错误前的配置上（语义上 default_provider == anthropic）
        assert_eq!(Config::default().default_provider, "anthropic");
    }

    #[test]
    fn save_and_load_roundtrip() {
        let _guard = lock_env();
        std::env::remove_var("ANTHROPIC_API_KEY");

        let tmp = tempfile::tempdir().unwrap();
        let svc = SettingsService::new(tmp.path().to_path_buf());

        let mut cfg = Config {
            log_level: "warn".into(),
            storage: hermes_config::StorageConfig {
                base_dir: tmp.path().join("data"),
                sessions_dir: tmp.path().join("s"),
                skills_dir: tmp.path().join("k"),
                memories_dir: tmp.path().join("m"),
            },
            ..Config::default()
        };
        cfg.providers
            .insert("anthropic".into(), valid_provider("sk-roundtrip"));

        svc.save_global(&cfg).unwrap();
        assert!(svc.config_path().exists());

        let loaded = svc.load_global().unwrap();
        assert_eq!(loaded.log_level, "warn");
        assert_eq!(loaded.default_provider, "anthropic");
        assert_eq!(
            loaded.providers.get("anthropic").unwrap().api_key,
            "sk-roundtrip"
        );
    }

    #[test]
    fn legacy_provider_kind_migration_is_one_shot_and_preserves_explicit_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = SettingsService::new(tmp.path().to_path_buf());
        let mut config = Config {
            default_provider: "deepseek".into(),
            ..Config::default()
        };
        config.providers.clear();

        let mut by_name = valid_provider("");
        by_name.base_url = "https://legacy-gateway.example/v1".into();
        by_name.model = "deepseek-v4-pro".into();
        config.providers.insert("deepseek".into(), by_name);

        let mut by_official_host = valid_provider("");
        by_official_host.base_url = "https://api.deepseek.com/v1".into();
        by_official_host.model = "deepseek-v4-flash".into();
        config
            .providers
            .insert("renamed-legacy-profile".into(), by_official_host);

        let mut explicit_other = valid_provider("");
        explicit_other.base_url = "https://api.deepseek.com".into();
        explicit_other.model = "deepseek-v4-pro".into();
        explicit_other.provider_kind = Some("openai".into());
        config
            .providers
            .insert("deepseek_team".into(), explicit_other);

        let mut unrelated = valid_provider("");
        unrelated.base_url = "https://gateway.example/v1".into();
        config.providers.insert("custom".into(), unrelated);
        svc.write_global(&config).unwrap();

        assert_eq!(svc.migrate_legacy_provider_kinds().unwrap(), 2);
        let mut migrated = SettingsService::parse_config_file(&svc.config_path()).unwrap();
        assert_eq!(
            migrated.providers["deepseek"].provider_kind.as_deref(),
            Some("deepseek")
        );
        assert_eq!(
            migrated.providers["renamed-legacy-profile"]
                .provider_kind
                .as_deref(),
            Some("deepseek")
        );
        assert_eq!(
            migrated.providers["deepseek_team"].provider_kind.as_deref(),
            Some("openai"),
            "an explicit identity must not be replaced by legacy name or host inference"
        );
        assert_eq!(migrated.providers["custom"].provider_kind, None);
        assert!(
            std::fs::read_to_string(svc.config_path())
                .unwrap()
                .contains("provider_kind = \"deepseek\""),
            "the inferred identity must be persisted, not only returned in memory"
        );

        let mut renamed = migrated.providers.remove("deepseek").unwrap();
        renamed.base_url = "https://second-gateway.example/v1".into();
        migrated
            .providers
            .insert("renamed-after-migration".into(), renamed);
        svc.write_global(&migrated).unwrap();

        assert_eq!(svc.migrate_legacy_provider_kinds().unwrap(), 0);
        let after_second_run = SettingsService::parse_config_file(&svc.config_path()).unwrap();
        assert_eq!(
            after_second_run.providers["renamed-after-migration"]
                .provider_kind
                .as_deref(),
            Some("deepseek"),
            "later profile renames and gateway edits must not change persisted identity"
        );
        assert_eq!(
            after_second_run.providers["deepseek_team"]
                .provider_kind
                .as_deref(),
            Some("openai")
        );
    }

    #[test]
    fn workspace_override_overrides_scalar() {
        let _guard = lock_env();
        std::env::remove_var("ANTHROPIC_API_KEY");

        let tmp = tempfile::tempdir().unwrap();
        // 全局：log_level = info, api_key = sk-global
        write_valid_config(tmp.path(), "info", "sk-global");

        // 工作区：仅覆盖 log_level
        let ws_dir = tmp.path().join("ws");
        let ws_r_code = ws_dir.join(".r-code");
        std::fs::create_dir_all(&ws_r_code).unwrap();
        std::fs::write(ws_r_code.join("config.toml"), r#"log_level = "debug""#).unwrap();

        let svc = SettingsService::new(tmp.path().to_path_buf());
        let cfg = svc.load_with_workspace(ws_dir.to_str().unwrap()).unwrap();
        assert_eq!(cfg.log_level, "debug"); // 被工作区覆盖
        assert_eq!(cfg.default_provider, "anthropic"); // 继承自全局
        assert_eq!(
            cfg.providers.get("anthropic").unwrap().api_key,
            "sk-global" // 继承自全局
        );
    }

    #[test]
    fn workspace_override_without_file_falls_back_to_global() {
        let _guard = lock_env();
        std::env::remove_var("ANTHROPIC_API_KEY");

        let tmp = tempfile::tempdir().unwrap();
        write_valid_config(tmp.path(), "info", "sk-global");

        let ws_dir = tmp.path().join("ws");
        std::fs::create_dir_all(&ws_dir).unwrap();
        // 无 .r-code/config.toml

        let svc = SettingsService::new(tmp.path().to_path_buf());
        let cfg = svc.load_with_workspace(ws_dir.to_str().unwrap()).unwrap();
        assert_eq!(cfg.log_level, "info");
    }

    #[test]
    fn env_var_overrides_config_file() {
        let _guard = lock_env();
        std::env::set_var("ANTHROPIC_API_KEY", "sk-from-env");

        let cfg = {
            let tmp = tempfile::tempdir().unwrap();
            write_valid_config(tmp.path(), "info", "sk-config");
            let svc = SettingsService::new(tmp.path().to_path_buf());
            svc.load_global()
        };
        // 无论结果如何，先清理环境变量
        std::env::remove_var("ANTHROPIC_API_KEY");

        let cfg = cfg.expect("load_global should succeed");
        assert_eq!(
            cfg.providers.get("anthropic").unwrap().api_key,
            "sk-from-env"
        );
    }

    #[test]
    fn env_var_overrides_workspace_merge() {
        let _guard = lock_env();
        std::env::set_var("ANTHROPIC_API_KEY", "sk-from-env");

        let cfg = {
            let tmp = tempfile::tempdir().unwrap();
            write_valid_config(tmp.path(), "info", "sk-global");
            let ws_dir = tmp.path().join("ws");
            let ws_r_code = ws_dir.join(".r-code");
            std::fs::create_dir_all(&ws_r_code).unwrap();
            // 工作区也设置了 api_key，但 env 应胜出
            std::fs::write(
                ws_r_code.join("config.toml"),
                r#"
log_level = "debug"

[providers.anthropic]
base_url = "https://api.anthropic.com"
api_key = "sk-workspace"
model = "claude-sonnet-4"
"#,
            )
            .unwrap();
            let svc = SettingsService::new(tmp.path().to_path_buf());
            svc.load_with_workspace(ws_dir.to_str().unwrap())
        };
        std::env::remove_var("ANTHROPIC_API_KEY");

        let cfg = cfg.expect("load_with_workspace should succeed");
        assert_eq!(cfg.log_level, "debug"); // 工作区覆盖
        assert_eq!(
            cfg.providers.get("anthropic").unwrap().api_key,
            "sk-from-env" // env 胜出
        );
    }
}

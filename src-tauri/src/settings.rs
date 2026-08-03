//! 设置服务 -- 全局配置 + 工作区覆盖。
//!
//! 优先级链：默认值 < 全局配置文件 < 工作区配置 < 环境变量 < 显式参数。
//! 全局配置位于 `config_dir/config.toml`（TOML）。
//! 工作区覆盖位于 `<workspace>/.r-code/config.toml`，与全局配置递归合并
//! （工作区字段覆盖全局同名标量；嵌套表深度合并）。
//!
//! 环境变量覆盖（`ANTHROPIC_API_KEY` / `OPENAI_API_KEY`）在合并后应用，
//! 优先级最高（仅次于显式参数）。校验在最后执行。
//!
//! [doc-14 阶段1] [agent-core/08]

use std::path::PathBuf;

use hermes_config::Config;
use r_code_agent_worker::AgentPromptPolicy;
use r_code_core::error::ProductError;
use r_code_core::secret::SecretStore;

const SECRET_SERVICE: &str = "r-code";
const AGENT_PROMPTS_FILE: &str = "agent-prompts.toml";
const MAX_AGENT_PROMPT_CHARS: usize = 20_000;

/// 设置服务 -- 管理全局配置 + 工作区覆盖。
///
/// 优先级：默认值 < 配置文件 < 环境变量 < 显式参数。
pub struct SettingsService {
    config_dir: PathBuf,
}

impl SettingsService {
    /// 创建设置服务，`config_dir` 为全局配置目录。
    pub fn new(config_dir: PathBuf) -> Self {
        Self { config_dir }
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
        self.hydrate_secrets(&mut config)?;
        apply_env(&mut config);
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

        // 4. OS 密钥链填充文件中被刻意留空的 api_key。
        self.hydrate_secrets(&mut config)?;

        // 5. 环境变量覆盖（最高优先级，仅次于显式参数）
        apply_env(&mut config);

        // 6. 校验
        Self::validate(&config)?;
        Ok(config)
    }

    /// 保存全局配置到 `config_dir/config.toml`（TOML）。
    ///
    /// API key 始终剥离，凭据只由 Windows Credential Manager / OS keychain 保存。
    pub fn save_global(&self, config: &Config) -> Result<(), ProductError> {
        // 先写入系统凭据库，再落无密钥的 TOML。顺序不能反过来，否则 keychain
        // 失败时会把用户唯一的凭据静默抹掉。
        for (name, provider) in &config.providers {
            if !provider.api_key.trim().is_empty() {
                self.set_provider_secret(name, &provider.api_key)?;
            }
        }
        let mut sanitized = config.clone();
        for provider in sanitized.providers.values_mut() {
            provider.api_key.clear();
        }
        self.write_global(&sanitized)
    }

    /// 将 Provider API key 写入 OS keychain。空值代表删除已有凭据。
    pub fn set_provider_secret(&self, provider: &str, value: &str) -> Result<(), ProductError> {
        if provider.trim().is_empty() {
            return Err(ProductError::ConfigError(
                "provider name cannot be empty".to_string(),
            ));
        }
        let store = SecretStore::new(SECRET_SERVICE);
        if value.trim().is_empty() {
            store.delete(&Self::secret_key(provider))
        } else {
            store.store(&Self::secret_key(provider), value)
        }
    }

    /// 读取 Provider 已保存的密钥；不会将值记录到日志。
    pub fn provider_secret(&self, provider: &str) -> Result<Option<String>, ProductError> {
        SecretStore::new(SECRET_SERVICE).get(&Self::secret_key(provider))
    }

    /// 将旧版 TOML 中的明文 api_key 迁移至 OS keychain。
    ///
    /// 返回迁移条数。调用方可选择把 keychain 不可用作为软错误处理，避免阻塞应用
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

/// 环境变量覆盖（`ANTHROPIC_API_KEY` / `OPENAI_API_KEY`）。
///
/// 仅修改已存在的 provider 条目（与 `hermes_config::Config::apply_env` 语义一致）。
fn apply_env(config: &mut Config) {
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        config
            .providers
            .entry("anthropic".into())
            .and_modify(|p| p.api_key = key.clone());
    }
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        config
            .providers
            .entry("openai".into())
            .and_modify(|p| p.api_key = key.clone());
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
    use std::sync::Mutex;

    // 环境变量是进程级全局状态，多个测试并行操作会竞态；用锁串行化所有
    // 设置测试（它们都调用 apply_env，会读取 ANTHROPIC_API_KEY）。
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn valid_provider(api_key: &str) -> ProviderConfig {
        ProviderConfig {
            base_url: "https://api.anthropic.com".into(),
            api_key: api_key.into(),
            model: "claude-sonnet-4".into(),
            max_tokens: None,
            temperature: None,
            protocol: None,
        }
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
        let _guard = ENV_LOCK.lock().unwrap();
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
        let _guard = ENV_LOCK.lock().unwrap();
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
        let _guard = ENV_LOCK.lock().unwrap();
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
        let _guard = ENV_LOCK.lock().unwrap();
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
    fn workspace_override_overrides_scalar() {
        let _guard = ENV_LOCK.lock().unwrap();
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
        let _guard = ENV_LOCK.lock().unwrap();
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
        let _guard = ENV_LOCK.lock().unwrap();
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
        let _guard = ENV_LOCK.lock().unwrap();
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

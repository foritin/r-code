//! 声明式 Provider 端点接入（docs/pi-alignment PRD §4.1 R-PRV-02 / M1-02）。
//!
//! 任意 OpenAI 兼容端点用一份最小声明接入：`base_url + api + api_key 引用 +
//! models`。声明存于 R-Code 自有的 sidecar 文件 `config_dir/provider-decls.toml`
//! ——vendor `Config` 的 TOML round-trip 会丢弃未知表（`save_global` 每次
//! 全量序列化），扩展数据因此必须独立成文件，不能塞进 `config.toml`。
//!
//! 值解析规则（不落明文优先，宁可报错）：
//! - `$ENV:NAME` —— 进程环境变量；
//! - `credential:<account>` —— 平台凭据后端按 account 名取（同设置页保存的
//!   `provider:<name>` 命名空间）；
//! - 字面量 —— `base_url` 允许；`api_key` **拒绝**（明文密钥不得落盘，
//!   见 PRD 固定约束与失败处理）。
//!
//! `provider_kind` 稳定身份：显式声明后改名（TOML 表键）/改 URL 都不影响；
//! 缺省取首次接入时的 decl 名并随声明显式保持。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use agent_config::Config;
use r_code_core::error::ProductError;
use serde::{Deserialize, Serialize};

use crate::model_pricing::DeclCost;

/// sidecar 文件名（与 config.toml 同目录）。
pub const DECL_FILE_NAME: &str = "provider-decls.toml";

/// 声明文件里一个值的三种形态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueRef {
    /// `$ENV:NAME`：进程环境变量。
    Env(String),
    /// `credential:<account>`：平台凭据后端的 account 名。
    Credential(String),
    /// 字面量。
    Literal(String),
}

/// 解析值引用。空串按字面量处理（调用方对必填字段自行判空）。
pub fn parse_value_ref(raw: &str) -> ValueRef {
    if let Some(name) = raw.strip_prefix("$ENV:") {
        return ValueRef::Env(name.trim().to_string());
    }
    if let Some(account) = raw.strip_prefix("credential:") {
        return ValueRef::Credential(account.trim().to_string());
    }
    ValueRef::Literal(raw.to_string())
}

/// 单个声明式端点（TOML 原始形态：字段值可能是引用字符串）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderDecl {
    /// 端点地址：字面量或 `$ENV:VAR`。
    pub base_url: String,
    /// 线路协议 slug：`anthropic_messages` / `openai_chat` / `openai_responses`
    /// （与 `provider_catalog::Protocol` 同一套字面量）。
    pub api: String,
    /// 密钥引用：`$ENV:VAR` 或 `credential:<account>`。字面量明文拒绝加载。
    pub api_key: String,
    /// 声明的模型清单：进入模型列表的离线真值（不做网络发现）。
    #[serde(default)]
    pub models: Vec<String>,
    /// 稳定厂商身份；缺省 = 首次接入时的 decl 名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_kind: Option<String>,
    /// M1-04 分层定价与思考等级映射（`[decls.<name>.cost]`）；缺省为空
    /// （无定价 → usage_json 不归因；无映射 → 档位全走省略态）。
    #[serde(default, skip_serializing_if = "DeclCost::is_empty")]
    pub cost: DeclCost,
}

/// sidecar 文件的顶层结构。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeclFile {
    #[serde(default)]
    pub decls: BTreeMap<String, ProviderDecl>,
}

pub fn decls_path(config_dir: &Path) -> PathBuf {
    config_dir.join(DECL_FILE_NAME)
}

/// 读取声明文件；文件不存在视为空声明集（首次使用前）。
pub fn load_decls(config_dir: &Path) -> Result<BTreeMap<String, ProviderDecl>, ProductError> {
    let path = decls_path(config_dir);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|e| ProductError::ConfigError(format!("read {}: {e}", path.display())))?;
    let file: DeclFile = toml::from_str(&content)
        .map_err(|e| ProductError::ConfigError(format!("parse {}: {e}", path.display())))?;
    Ok(file.decls)
}

/// 凭据查询抽象：解耦 settings.rs 的私有 backend trait，测试可注入闭包。
pub type CredentialLookup<'a> = &'a dyn Fn(&str) -> Result<Option<String>, ProductError>;

/// 解析声明的密钥引用。字面量明文一律拒绝（不落盘优先，宁可报错）。
pub fn resolve_decl_key(
    name: &str,
    decl: &ProviderDecl,
    credentials: CredentialLookup<'_>,
) -> Result<String, ProductError> {
    match parse_value_ref(&decl.api_key) {
        ValueRef::Env(var) => {
            if var.is_empty() {
                return Err(ProductError::ConfigError(format!(
                    "provider decl '{name}': $ENV: 引用缺少变量名"
                )));
            }
            std::env::var(&var).map_err(|_| {
                ProductError::ConfigError(format!("provider decl '{name}': 环境变量 {var} 未设置"))
            })
        }
        ValueRef::Credential(account) => {
            if account.is_empty() {
                return Err(ProductError::ConfigError(format!(
                    "provider decl '{name}': credential: 引用缺少 account 名"
                )));
            }
            credentials(&account)?.ok_or_else(|| {
                ProductError::ConfigError(format!(
                    "provider decl '{name}': 凭据后端没有 account '{account}'"
                ))
            })
        }
        ValueRef::Literal(_) => Err(ProductError::ConfigError(format!(
            "provider decl '{name}': api_key 只接受 $ENV: / credential: 引用，\
             明文密钥不落盘（请改用引用或经设置页保存）"
        ))),
    }
}

/// 解析声明的 base_url（字面量或 $ENV；凭据引用对地址无意义）。
pub fn resolve_decl_base_url(name: &str, decl: &ProviderDecl) -> Result<String, ProductError> {
    match parse_value_ref(&decl.base_url) {
        ValueRef::Literal(url) => Ok(url),
        ValueRef::Env(var) => std::env::var(&var).map_err(|_| {
            ProductError::ConfigError(format!(
                "provider decl '{name}': 环境变量 {var} 未设置（base_url 引用）"
            ))
        }),
        ValueRef::Credential(_) => Err(ProductError::ConfigError(format!(
            "provider decl '{name}': base_url 不支持 credential: 引用"
        ))),
    }
}

/// 声明的稳定身份：显式 provider_kind 优先，缺省回落 decl 名。
pub fn decl_provider_kind(name: &str, decl: &ProviderDecl) -> String {
    decl.provider_kind
        .clone()
        .unwrap_or_else(|| name.to_string())
}

/// 声明合成的非致命诊断（M1-03）：缺鉴权/缺地址的声明降级为三态快照的
/// all-not-available / composition_error，不再让整个设置加载失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclIssue {
    pub provider: String,
    pub reason: String,
}

/// 把声明集合成到运行时 Config：decl 对已声明字段（base_url/api_key/
/// protocol/provider_kind/models 首项为默认模型）是权威。
///
/// 降级语义（M1-03）：
/// - base_url 的 $ENV 未设置 → 无法组装网络身份，跳过该 provider 并记 issue；
/// - api_key 的 $ENV 未设置 / credential account 缺失 → provider 以空 key
///   落进 config（"配置解析但缺鉴权"，在 all 不在 available）并记 issue；
/// - api_key 字面量明文 → 仍然硬错误（不落明文优先，宁可报错）。
///
/// 调用次序约定（settings 加载链路）：**在 apply_env 之前**调用本函数，
/// 让 `R_CODE_PROVIDER_<NAME>_API_KEY` 等环境覆盖仍拥有最高优先级；
/// hydrate_secrets 之后只填充仍空的 key。
pub fn apply_decls(
    config: &mut Config,
    decls: &BTreeMap<String, ProviderDecl>,
    credentials: CredentialLookup<'_>,
) -> Result<Vec<DeclIssue>, ProductError> {
    let mut issues = Vec::new();
    for (name, decl) in decls {
        let base_url = match resolve_decl_base_url(name, decl) {
            Ok(base_url) => base_url,
            Err(error) => {
                issues.push(DeclIssue {
                    provider: name.clone(),
                    reason: format!("base_url 未解析，provider 未组装：{error}"),
                });
                continue;
            }
        };
        let (api_key, key_issue) = match parse_value_ref(&decl.api_key) {
            ValueRef::Literal(_) => {
                // 明文密钥：硬错误（不落盘优先），不降级。
                return Err(resolve_decl_key(name, decl, credentials).unwrap_err());
            }
            _ => match resolve_decl_key(name, decl, credentials) {
                Ok(key) => (key, None),
                Err(error) => (String::new(), Some(error.to_string())),
            },
        };
        if let Some(reason) = key_issue {
            issues.push(DeclIssue {
                provider: name.clone(),
                reason: format!("缺鉴权：{reason}"),
            });
        }
        let kind = decl_provider_kind(name, decl);
        let provider =
            config
                .providers
                .entry(name.clone())
                .or_insert_with(|| agent_config::ProviderConfig {
                    base_url: String::new(),
                    api_key: String::new(),
                    model: String::new(),
                    provider_kind: None,
                    max_tokens: None,
                    temperature: None,
                    protocol: None,
                    show_reasoning: true,
                });
        provider.base_url = base_url;
        provider.api_key = api_key;
        provider.provider_kind = Some(kind);
        provider.protocol = Some(decl.api.clone());
        if let Some(default_model) = decl.models.first() {
            provider.model = default_model.clone();
        }
    }
    Ok(issues)
}

/// `save_global` 防重复落凭据：decl 引用解析出的值与当前内存 key 一致时，
/// 声明文件仍是真值来源，不再复制进 `provider:<name>` 凭据位。
pub fn decl_covers_secret(
    decls: &BTreeMap<String, ProviderDecl>,
    name: &str,
    key: &str,
    credentials: CredentialLookup<'_>,
) -> bool {
    let Some(decl) = decls.get(name) else {
        return false;
    };
    // 引用解析失败（如环境变量本次未设置）时不得跳过——保守落凭据，
    // 避免静默抹掉用户唯一密钥（与 save_global 的顺序约束同理）。
    resolve_decl_key(name, decl, credentials).is_ok_and(|resolved| resolved == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "r-code-provider-decl-test-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn mock_credentials(
        pairs: &[(&str, &str)],
    ) -> impl Fn(&str) -> Result<Option<String>, ProductError> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(account, value)| (account.to_string(), value.to_string()))
            .collect();
        move |account: &str| Ok(map.get(account).cloned())
    }

    fn minimal_decl() -> ProviderDecl {
        ProviderDecl {
            base_url: "https://relay.example.com/v1".to_string(),
            api: "openai_chat".to_string(),
            api_key: "credential:my-relay".to_string(),
            models: vec!["model-a".to_string(), "model-b".to_string()],
            provider_kind: None,
            cost: DeclCost::default(),
        }
    }

    /// M1-02.A1：最小声明接入后进入模型列表（三态快照口径，含纯声明）。
    #[test]
    fn minimal_decl_enters_model_listing() {
        let dir = fixture_dir("a1-listing");
        let decl = minimal_decl();
        let mut file = DeclFile::default();
        file.decls.insert("my-relay".to_string(), decl.clone());
        std::fs::write(dir.join(DECL_FILE_NAME), toml::to_string(&file).unwrap()).unwrap();

        let decls = load_decls(&dir).unwrap();
        assert_eq!(decls.len(), 1);

        // 纯声明（config.toml 不存在）也进入快照并列出声明模型。
        let snapshot = crate::model_availability::build_snapshot(
            &Config::default(),
            &decls,
            None,
            &|_, _, _| true,
        );
        assert_eq!(snapshot.all.len(), 2);
        assert!(snapshot
            .all
            .iter()
            .all(|entry| entry.provider == "my-relay" && entry.source == "decl"));
        let models: Vec<&str> = snapshot
            .all
            .iter()
            .map(|entry| entry.model.as_str())
            .collect();
        assert_eq!(models, vec!["model-a", "model-b"]);

        // 合成进 config 后字段正确。
        let mut config = Config::default();
        let creds = mock_credentials(&[("my-relay", "sk-live")]);
        let issues = apply_decls(&mut config, &decls, &creds).unwrap();
        assert!(issues.is_empty());
        let applied = &config.providers["my-relay"];
        assert_eq!(applied.base_url, "https://relay.example.com/v1");
        assert_eq!(applied.protocol.as_deref(), Some("openai_chat"));
        assert_eq!(applied.model, "model-a");
        assert_eq!(applied.api_key, "sk-live");
    }

    /// M1-02.A2：$ENV / credential 引用解析；字面量明文拒绝；sidecar 往返无明文。
    #[test]
    fn value_resolution_refs_only_no_plaintext() {
        // $ENV 引用。
        std::env::set_var("R_CODE_TEST_DECL_KEY", "sk-from-env");
        let mut decl = minimal_decl();
        decl.api_key = "$ENV:R_CODE_TEST_DECL_KEY".to_string();
        let creds = mock_credentials(&[]);
        assert_eq!(
            resolve_decl_key("my-relay", &decl, &creds).unwrap(),
            "sk-from-env"
        );
        std::env::remove_var("R_CODE_TEST_DECL_KEY");
        assert!(
            resolve_decl_key("my-relay", &decl, &creds).is_err(),
            "未设置的 $ENV 必须报错"
        );

        // credential 引用。
        let decl = minimal_decl();
        let creds = mock_credentials(&[("my-relay", "sk-from-store")]);
        assert_eq!(
            resolve_decl_key("my-relay", &decl, &creds).unwrap(),
            "sk-from-store"
        );
        let creds = mock_credentials(&[]);
        assert!(
            resolve_decl_key("my-relay", &decl, &creds).is_err(),
            "缺失 account 必须报错"
        );

        // 字面量明文拒绝（不落盘优先，宁可报错）。
        let mut plain = minimal_decl();
        plain.api_key = "sk-plaintext".to_string();
        let error = resolve_decl_key("my-relay", &plain, &creds).unwrap_err();
        assert!(
            error.to_string().contains("明文"),
            "错误需点名明文拒绝: {error}"
        );

        // sidecar 序列化只含引用字符串，绝无密钥真值。
        let mut file = DeclFile::default();
        file.decls.insert("my-relay".to_string(), minimal_decl());
        let text = toml::to_string(&file).unwrap();
        assert!(text.contains("credential:my-relay"));
        assert!(!text.contains("sk-"), "sidecar 不得出现密钥真值: {text}");

        // decl_covers_secret：引用值一致才跳过凭据复制。
        let decls = file.decls.clone();
        let creds = mock_credentials(&[("my-relay", "sk-live")]);
        assert!(decl_covers_secret(&decls, "my-relay", "sk-live", &creds));
        assert!(!decl_covers_secret(&decls, "my-relay", "sk-other", &creds));
        assert!(!decl_covers_secret(&decls, "unknown", "sk-live", &creds));
    }

    /// M1-02.A3：provider_kind 改名/改 URL 不变。
    #[test]
    fn provider_kind_stable_across_rename_and_url_change() {
        // 显式声明的 kind 与表键名、URL 解耦。
        let mut decl = minimal_decl();
        decl.provider_kind = Some("my-relay-stable".to_string());
        assert_eq!(decl_provider_kind("my-relay", &decl), "my-relay-stable");
        assert_eq!(
            decl_provider_kind("renamed-relay", &decl),
            "my-relay-stable"
        );
        decl.base_url = "https://other-gateway.example.com/v1".to_string();
        assert_eq!(
            decl_provider_kind("renamed-relay", &decl),
            "my-relay-stable"
        );

        // 缺省 kind = decl 名（首次接入身份），此后应显式保持。
        let decl = minimal_decl();
        assert_eq!(decl_provider_kind("my-relay", &decl), "my-relay");

        // 合成进 config 后 kind 落进 ProviderConfig.provider_kind（稳定身份字段）。
        let mut decls = BTreeMap::new();
        let mut explicit = minimal_decl();
        explicit.provider_kind = Some("my-relay-stable".to_string());
        decls.insert("display-name-a".to_string(), explicit);
        let mut config = Config::default();
        let creds = mock_credentials(&[("my-relay", "sk-live")]);
        apply_decls(&mut config, &decls, &creds).unwrap();
        assert_eq!(
            config.providers["display-name-a"].provider_kind.as_deref(),
            Some("my-relay-stable")
        );
    }

    /// 合成次序：decl 在 apply_env 之前应用时，环境覆盖仍最高优先。
    #[test]
    fn env_override_still_wins_over_decl() {
        let mut decls = BTreeMap::new();
        decls.insert("my-relay".to_string(), minimal_decl());
        let mut config = Config::default();
        let creds = mock_credentials(&[("my-relay", "sk-from-store")]);
        apply_decls(&mut config, &decls, &creds).unwrap();
        assert_eq!(config.providers["my-relay"].api_key, "sk-from-store");
        // settings 链路随后跑 apply_env：R_CODE_PROVIDER_MY_RELAY_API_KEY 覆盖。
        std::env::set_var("R_CODE_PROVIDER_MY_RELAY_API_KEY", "sk-env-override");
        crate::settings::apply_env(&mut config);
        std::env::remove_var("R_CODE_PROVIDER_MY_RELAY_API_KEY");
        assert_eq!(config.providers["my-relay"].api_key, "sk-env-override");
    }

    /// M1-04：`[decls.<name>.cost]` 解析——tier 表键成层名、无 threshold =
    /// 基础档、thinking_level_map/hidden_thinking_levels 就位；费率平铺四桶。
    #[test]
    fn cost_table_parses_tiers_and_level_map() {
        let dir = fixture_dir("m104-cost");
        std::fs::write(
            dir.join(DECL_FILE_NAME),
            r#"
[decls.priced-relay]
base_url = "https://relay.example.com/v1"
api = "openai_chat"
api_key = "credential:priced"
models = ["model-a"]

[decls.priced-relay.cost.tiers.base]
input_per_mtok = 0.5
cache_read_per_mtok = 0.1
cache_write_per_mtok = 0.6
output_per_mtok = 2.0

[decls.priced-relay.cost.tiers.long]
threshold_tokens = 200000
input_per_mtok = 1.0
cache_read_per_mtok = 0.2
cache_write_per_mtok = 1.2
output_per_mtok = 4.0

[decls.priced-relay.cost.thinking_level_map]
low = "low"
medium = "high"

[decls.priced-relay.cost]
hidden_thinking_levels = ["high"]
"#,
        )
        .unwrap();
        let decls = load_decls(&dir).unwrap();
        let cost = &decls["priced-relay"].cost;
        assert_eq!(cost.tiers.len(), 2);
        let base = cost.tiers.iter().find(|t| t.name == "base").unwrap();
        assert_eq!(base.threshold_tokens, None);
        assert_eq!(base.rates.input_per_mtok, 0.5);
        let long = cost.tiers.iter().find(|t| t.name == "long").unwrap();
        assert_eq!(long.threshold_tokens, Some(200_000));
        assert_eq!(long.rates.cache_write_per_mtok, 1.2);
        assert_eq!(
            cost.thinking_level_map.get("medium").map(String::as_str),
            Some("high")
        );
        assert_eq!(cost.hidden_thinking_levels, vec!["high".to_string()]);
        // 无 cost 表的声明：默认空（usage_json 不归因）。
        let dir2 = fixture_dir("m104-nocost");
        std::fs::write(
            dir2.join(DECL_FILE_NAME),
            r#"
[decls.free-relay]
base_url = "https://relay.example.com/v1"
api = "openai_chat"
api_key = "credential:free"
models = ["model-a"]
"#,
        )
        .unwrap();
        assert!(load_decls(&dir2).unwrap()["free-relay"].cost.is_empty());
    }

    /// M1-04（负向）：负费率是声明错误——load 时报错，不静默装载。
    #[test]
    fn negative_rates_rejected_at_load() {
        let dir = fixture_dir("m104-negative");
        std::fs::write(
            dir.join(DECL_FILE_NAME),
            r#"
[decls.bad-relay]
base_url = "https://relay.example.com/v1"
api = "openai_chat"
api_key = "credential:bad"
models = ["model-a"]

[decls.bad-relay.cost.tiers.base]
input_per_mtok = -0.5
cache_read_per_mtok = 0.1
cache_write_per_mtok = 0.6
output_per_mtok = 2.0
"#,
        )
        .unwrap();
        let error = load_decls(&dir).unwrap_err();
        assert!(error.to_string().contains("费率"));
    }
}

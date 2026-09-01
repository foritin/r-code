//! ModelAvailability 三态快照（docs/pi-alignment PRD §4.1 R-PRV-03 / M1-03）。
//!
//! 三态语义：
//! - `all`：加载成功的 (provider, model) 条目全集；
//! - `available`：其中持有可用鉴权（合成后 api_key 非空 / 声明引用可解析）的
//!   子集——模型选择面（/model、`--list-models`、设置页模型菜单）只渲染它；
//! - `composition_errors`：无法组装成可用 provider 配置的声明/文件级错误，
//!   附人可读诊断（不含密钥材料），设置页可展开。
//!
//! "配置解析但缺鉴权"（如 `$ENV` 未设置的声明）在 all 不在 available。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use agent_config::{Config, ProviderConfig};
use r_code_core::dto::{ModelAvailabilityEntry, ModelAvailabilitySnapshot, ModelCompositionError};

use crate::provider_catalog;
use crate::provider_decl::{
    self, decl_provider_kind, resolve_decl_base_url, resolve_decl_key, CredentialLookup,
    ProviderDecl,
};

/// 鉴权判定回调：app 内（hydrate 后）看 api_key 非空；CLI 离线场景看
/// env/声明引用/凭据存在性。provider 为 None 表示纯声明（尚未进 config）。
pub type HasAuth<'a> = &'a dyn Fn(&str, Option<&ProviderConfig>, Option<&ProviderDecl>) -> bool;

/// hydrate 后的运行时判定：合成进 config 的 api_key 非空即有鉴权。
/// （apply_decls 保证声明 provider 一定进入 config，故无需处理纯声明。）
pub fn runtime_has_auth(
    _name: &str,
    provider: Option<&ProviderConfig>,
    _decl: Option<&ProviderDecl>,
) -> bool {
    provider.is_some_and(|entry| !entry.api_key.trim().is_empty())
}

/// 构建三态快照（纯函数；decls_load_error 为声明文件级解析失败）。
pub fn build_snapshot(
    config: &Config,
    decls: &BTreeMap<String, ProviderDecl>,
    decls_load_error: Option<String>,
    has_auth: HasAuth<'_>,
) -> ModelAvailabilitySnapshot {
    let mut snapshot = ModelAvailabilitySnapshot::default();
    if let Some(error) = decls_load_error {
        snapshot.composition_errors.push(ModelCompositionError {
            provider: "<decls-file>".to_string(),
            model: None,
            reason: error,
        });
    }
    let names: BTreeSet<String> = config
        .providers
        .keys()
        .cloned()
        .chain(decls.keys().cloned())
        .collect();
    for name in names {
        let provider = config.providers.get(&name);
        let decl = decls.get(&name);
        // 声明级组装失败：协议 slug 非法 / base_url 引用未解析。
        if let Some(decl) = decl {
            if provider_catalog::Protocol::parse(&decl.api).is_none() {
                snapshot.composition_errors.push(ModelCompositionError {
                    provider: name.clone(),
                    model: None,
                    reason: format!(
                        "api '{}' 不是受支持的协议 slug（anthropic_messages / openai_chat / openai_responses）",
                        decl.api
                    ),
                });
                continue;
            }
            if let Err(error) = resolve_decl_base_url(&name, decl) {
                snapshot.composition_errors.push(ModelCompositionError {
                    provider: name.clone(),
                    model: None,
                    reason: format!("base_url 未解析，provider 未组装：{error}"),
                });
                continue;
            }
        }
        // 模型清单来源：decl > catalog（地址未改写的预设）> config 单 model。
        let (models, source) = if let Some(decl) = decl {
            if !decl.models.is_empty() {
                (decl.models.clone(), "decl")
            } else {
                catalog_or_config_models(&name, provider, Some(decl))
            }
        } else {
            catalog_or_config_models(&name, provider, None)
        };
        if models.is_empty() {
            if decl.is_some() {
                snapshot.composition_errors.push(ModelCompositionError {
                    provider: name.clone(),
                    model: None,
                    reason: "声明未提供 models，且目录预设与 config 均无可用模型".to_string(),
                });
            }
            continue;
        }
        let auth = has_auth(&name, provider, decl);
        for model in models {
            let entry = ModelAvailabilityEntry {
                provider: name.clone(),
                model,
                source: source.to_string(),
                has_auth: auth,
            };
            if auth {
                snapshot.available.push(entry.clone());
            }
            snapshot.all.push(entry);
        }
    }
    snapshot
}

fn catalog_or_config_models(
    name: &str,
    provider: Option<&ProviderConfig>,
    decl: Option<&ProviderDecl>,
) -> (Vec<String>, &'static str) {
    let (identity, base_url) = match (provider, decl) {
        (Some(entry), _) => (
            entry
                .provider_kind
                .clone()
                .unwrap_or_else(|| name.to_string()),
            entry.base_url.clone(),
        ),
        (None, Some(decl)) => (
            decl_provider_kind(name, decl),
            resolve_decl_base_url(name, decl).unwrap_or_default(),
        ),
        (None, None) => return (Vec::new(), "config"),
    };
    if let Some(preset) = provider_catalog::preset_for(&identity, &base_url) {
        return (
            preset
                .models
                .iter()
                .map(|entry| entry.id.to_string())
                .collect(),
            "catalog",
        );
    }
    let model = provider
        .map(|entry| entry.model.trim().to_string())
        .unwrap_or_default();
    if model.is_empty() {
        (Vec::new(), "config")
    } else {
        (vec![model], "config")
    }
}

/// `r-code-host list-models`：只列 available（有鉴权）条目；composition
/// errors 走 stderr。离线：不 hydrate、不触网；鉴权判定 = env 覆盖已设 /
/// 声明引用本次可解析（$ENV 已设或凭据 account 存在）/ 平台凭据已有该 key。
pub fn list_models_cli(config_dir: &Path, provider_filter: Option<&str>) -> i32 {
    let config_path = config_dir.join("config.toml");
    let config: Config = if config_path.exists() {
        let content = match std::fs::read_to_string(&config_path) {
            Ok(content) => content,
            Err(error) => {
                eprintln!("list-models: read {}: {error}", config_path.display());
                return 2;
            }
        };
        match toml::from_str(&content) {
            Ok(config) => config,
            Err(error) => {
                eprintln!("list-models: parse {}: {error}", config_path.display());
                return 2;
            }
        }
    } else {
        Config::default()
    };
    let (decls, decls_error) = match provider_decl::load_decls(config_dir) {
        Ok(decls) => (decls, None),
        Err(error) => (BTreeMap::new(), Some(error.to_string())),
    };
    let settings = crate::settings::SettingsService::new(config_dir.to_path_buf());
    let has_auth =
        |name: &str, provider: Option<&ProviderConfig>, decl: Option<&ProviderDecl>| -> bool {
            let kind = provider
                .and_then(|entry| entry.provider_kind.clone())
                .or_else(|| decl.map(|decl| decl_provider_kind(name, decl)));
            if crate::settings::provider_env_value(name, kind.as_deref()).is_some() {
                return true;
            }
            let credentials: CredentialLookup<'_> = &|account| settings.provider_secret(account);
            if let Some(decl) = decl {
                if resolve_decl_key(name, decl, &credentials).is_ok() {
                    return true;
                }
            }
            settings.provider_secret(name).ok().flatten().is_some()
        };
    let snapshot = build_snapshot(&config, &decls, decls_error, &has_auth);
    for entry in &snapshot.available {
        if let Some(filter) = provider_filter {
            if entry.provider != filter {
                continue;
            }
        }
        println!("{}\t{}", entry.provider, entry.model);
    }
    for error in &snapshot.composition_errors {
        eprintln!("composition-error: {}: {}", error.provider, error.reason);
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(api_key: &str) -> ProviderDecl {
        ProviderDecl {
            base_url: "https://relay.example.com/v1".to_string(),
            api: "openai_chat".to_string(),
            api_key: api_key.to_string(),
            models: vec!["model-a".to_string(), "model-b".to_string()],
            provider_kind: None,
            cost: crate::model_pricing::DeclCost::default(),
        }
    }

    fn config_with(name: &str, api_key: &str, model: &str) -> Config {
        let mut config = Config::default();
        config.providers.insert(
            name.to_string(),
            ProviderConfig {
                base_url: "https://selfhost.example.com/v1".to_string(),
                api_key: api_key.to_string(),
                model: model.to_string(),
                provider_kind: None,
                max_tokens: None,
                temperature: None,
                protocol: None,
                show_reasoning: true,
            },
        );
        config
    }

    /// M1-03.A1：三态快照结构完整——all ⊇ available，composition_errors 独立。
    #[test]
    fn snapshot_three_state_structure() {
        let mut decls = BTreeMap::new();
        decls.insert("good".to_string(), decl("credential:good"));
        let mut bad_slug = decl("credential:bad");
        bad_slug.api = "not_a_protocol".to_string();
        decls.insert("bad-slug".to_string(), bad_slug);
        let config = config_with("selfhost", "sk-live", "selfhost-model");
        let has_auth = |_: &str,
                        provider: Option<&ProviderConfig>,
                        _: Option<&ProviderDecl>|
         -> bool { provider.is_some_and(|entry| !entry.api_key.is_empty()) };
        let snapshot = build_snapshot(&config, &decls, None, &has_auth);
        // all 覆盖：good(2) + bad-slug(0，进 errors) + selfhost(1)。
        assert_eq!(snapshot.all.len(), 3);
        assert!(snapshot
            .available
            .iter()
            .all(|entry| snapshot.all.contains(entry)));
        assert_eq!(snapshot.composition_errors.len(), 1);
        assert_eq!(snapshot.composition_errors[0].provider, "bad-slug");
        assert!(snapshot.composition_errors[0]
            .reason
            .contains("not_a_protocol"));
        // 序列化字段齐全（IPC 合同）。
        let json = serde_json::to_string(&snapshot).unwrap();
        for key in [
            "all",
            "available",
            "composition_errors",
            "has_auth",
            "source",
        ] {
            assert!(json.contains(key), "missing {key} in snapshot json");
        }
    }

    /// M1-03.A2：配置解析但缺鉴权 → 在 all 不在 available。
    #[test]
    fn missing_auth_lands_in_all_not_available() {
        let config = config_with("no-key", "", "some-model");
        let has_auth = |_: &str,
                        provider: Option<&ProviderConfig>,
                        _: Option<&ProviderDecl>|
         -> bool { provider.is_some_and(|entry| !entry.api_key.is_empty()) };
        let snapshot = build_snapshot(&config, &BTreeMap::new(), None, &has_auth);
        assert_eq!(snapshot.all.len(), 1);
        assert!(!snapshot.all[0].has_auth);
        assert!(snapshot.available.is_empty(), "缺鉴权不得进 available");
        assert!(snapshot.composition_errors.is_empty(), "缺鉴权不是组装失败");

        // 声明 $ENV 未设置（hydrate 后仍空 key）同语义。
        let mut decls = BTreeMap::new();
        decls.insert(
            "env-missing".to_string(),
            decl("$ENV:R_CODE_TEST_NEVER_SET"),
        );
        let snapshot = build_snapshot(&Config::default(), &decls, None, &|_, _, _| false);
        assert_eq!(snapshot.all.len(), 2, "env-missing 的两个声明模型进 all");
        assert!(snapshot.available.is_empty());
    }

    /// 组装失败（base_url $ENV 未解析 / 声明无模型）进 composition_errors。
    #[test]
    fn composition_failures_are_diagnosed() {
        let mut decls = BTreeMap::new();
        let mut bad_url = decl("credential:x");
        bad_url.base_url = "$ENV:R_CODE_TEST_URL_NEVER_SET".to_string();
        decls.insert("bad-url".to_string(), bad_url);
        let mut no_models = decl("credential:x");
        no_models.models = vec![];
        decls.insert("no-models".to_string(), no_models);
        let snapshot = build_snapshot(&Config::default(), &decls, None, &|_, _, _| true);
        let providers: Vec<&str> = snapshot
            .composition_errors
            .iter()
            .map(|error| error.provider.as_str())
            .collect();
        assert!(
            providers.contains(&"bad-url"),
            "base_url 未解析: {providers:?}"
        );
        assert!(
            providers.contains(&"no-models"),
            "声明无模型: {providers:?}"
        );
        assert!(snapshot.all.is_empty());
    }

    /// 声明文件级错误进入 composition_errors（provider 占位 <decls-file>）。
    #[test]
    fn decls_file_level_error_is_surfaced() {
        let snapshot = build_snapshot(
            &Config::default(),
            &BTreeMap::new(),
            Some("parse provider-decls.toml: expected value".to_string()),
            &|_, _, _| false,
        );
        assert_eq!(snapshot.composition_errors.len(), 1);
        assert_eq!(snapshot.composition_errors[0].provider, "<decls-file>");
    }
}

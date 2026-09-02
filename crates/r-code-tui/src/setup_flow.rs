//! `/setup` 引导式模型服务配置（症状3：无配置时 `/model` 是死端，没有
//! 可操作流程）。
//!
//! 复用桌面设置页同一套基建：`provider_catalog::PRESETS` 预设目录 +
//! `SettingsService`（api_key 经 `save_global` 迁入平台凭据后端，配置文件
//! 不落明文）。两步流：选预设（输入过滤 + ↑↓）→ 输 API key（掩码）→
//! 保存即 `default_provider` + 立即可用（`ensure_real_runtime` 每次 send
//! 重读配置，无需重建 runtime）。
//!
//! 2026-09-02 G11（pi `envApiKeyAuth()` 对齐）：key 步可 **Tab 切换环境变量
//! 鉴权模式**——不保存任何密钥，落盘的 provider `api_key` 留空，由宿主
//! `settings::apply_env` 在每次加载时从环境变量回填（`R_CODE_PROVIDER_<ID>
//! _API_KEY` 或厂商别名）。零凭据落盘 + SSH/CI 场景天然契合。

use r_code_host::provider_catalog::{Preset, PRESETS};

/// 可配置预设（与设置页同口径：排除 deepseek_anthropic 聚合口）。
pub fn setup_presets() -> Vec<&'static Preset> {
    PRESETS
        .iter()
        .filter(|preset| preset.id != "deepseek_anthropic")
        .collect()
}

/// 子串模糊（同 model_selector 口径：查询字符按序出现即命中，大小写不敏感）。
fn fuzzy_matches(preset: &Preset, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let haystack = format!("{}/{}", preset.id, preset.label).to_lowercase();
    let mut cursor = 0;
    for ch in query.chars() {
        match haystack[cursor..].find(ch) {
            Some(offset) => cursor += offset + ch.len_utf8(),
            None => return false,
        }
    }
    true
}

/// 引导流状态机（渲染与键位由 app.rs 驱动；apply 是独立函数便于单测）。
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// 选预设：输入即过滤；selection 是过滤结果内的下标。
    PickProvider { query: String, selection: usize },
    /// 输入 API key：掩码显示，不回显原文；`env_mode` = 环境变量鉴权
    /// （G11：key 不落盘不进凭据后端，加载时由 apply_env 回填）。
    EnterKey {
        preset_id: String,
        key: String,
        env_mode: bool,
    },
}

#[derive(Debug)]
pub struct SetupFlow {
    step: Step,
}

/// `submit()` 的结果，由调用方决定后续（开选择器/提示）。
#[derive(Debug, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// 已保存：预设 id + 默认模型。
    Applied { provider: String, model: String },
    /// G11 环境变量模式已保存：预设 id + 默认模型 + 需要的环境变量清单。
    AppliedEnv {
        provider: String,
        model: String,
        env_vars: Vec<String>,
    },
    /// key 为空，未保存。
    EmptyKey,
}

impl SetupFlow {
    pub fn new() -> Self {
        Self {
            step: Step::PickProvider {
                query: String::new(),
                selection: 0,
            },
        }
    }

    pub fn step(&self) -> &Step {
        &self.step
    }

    fn filtered(&self) -> Vec<&'static Preset> {
        let Step::PickProvider { query, .. } = &self.step else {
            return Vec::new();
        };
        setup_presets()
            .into_iter()
            .filter(|preset| fuzzy_matches(preset, query))
            .collect()
    }

    /// 当前选中的预设（PickProvider 态）。
    pub fn selected_preset(&self) -> Option<&'static Preset> {
        let Step::PickProvider { selection, .. } = &self.step else {
            return None;
        };
        self.filtered().get(*selection).copied()
    }

    /// EnterKey 态的预设。
    pub fn key_step_preset(&self) -> Option<&'static Preset> {
        let Step::EnterKey { preset_id, .. } = &self.step else {
            return None;
        };
        setup_presets()
            .into_iter()
            .find(|preset| preset.id == *preset_id)
    }

    pub fn input_char(&mut self, ch: char) {
        match &mut self.step {
            Step::PickProvider { query, selection } => {
                query.push(ch);
                *selection = 0;
            }
            Step::EnterKey { key, .. } => key.push(ch),
        }
    }

    pub fn backspace(&mut self) {
        match &mut self.step {
            Step::PickProvider { query, selection } => {
                query.pop();
                *selection = 0;
            }
            Step::EnterKey { key, .. } => {
                key.pop();
            }
        }
    }

    /// G11：key 步 Tab 切换环境变量鉴权模式（切换即清空已输 key——两种
    /// 鉴权来源互斥，避免半截明文 key 残留在状态里）。
    pub fn toggle_env_mode(&mut self) {
        if let Step::EnterKey { key, env_mode, .. } = &mut self.step {
            *env_mode = !*env_mode;
            if *env_mode {
                key.clear();
            }
        }
    }

    pub fn move_up(&mut self) {
        if let Step::PickProvider { selection, .. } = &mut self.step {
            *selection = selection.saturating_sub(1);
        }
    }

    pub fn move_down(&mut self) {
        let len = self.filtered().len();
        if let Step::PickProvider { selection, .. } = &mut self.step {
            if len > 0 {
                *selection = (*selection + 1).min(len - 1);
            }
        }
    }

    /// Enter：PickProvider → EnterKey；EnterKey → submit（调用方执行 apply）。
    pub fn advance(&mut self) -> Option<SubmitOutcome> {
        match &self.step {
            Step::PickProvider { .. } => {
                let preset = self.selected_preset()?;
                self.step = Step::EnterKey {
                    preset_id: preset.id.to_string(),
                    key: String::new(),
                    env_mode: false,
                };
                None
            }
            Step::EnterKey { key, env_mode, .. } => {
                if *env_mode {
                    let preset = self.key_step_preset()?;
                    return Some(SubmitOutcome::AppliedEnv {
                        provider: preset.id.to_string(),
                        model: preset.model.to_string(),
                        env_vars: env_var_names(preset.id),
                    });
                }
                if key.trim().is_empty() {
                    return Some(SubmitOutcome::EmptyKey);
                }
                let preset = self.key_step_preset()?;
                Some(SubmitOutcome::Applied {
                    provider: preset.id.to_string(),
                    model: preset.model.to_string(),
                })
            }
        }
    }

    /// Esc：EnterKey → PickProvider；PickProvider → true（调用方关闭浮层）。
    pub fn back(&mut self) -> bool {
        match &self.step {
            Step::EnterKey { preset_id, .. } => {
                self.step = Step::PickProvider {
                    query: preset_id.clone(),
                    selection: 0,
                };
                false
            }
            Step::PickProvider { .. } => true,
        }
    }

    /// 掩码后的 key 显示（EnterKey 态）。
    pub fn masked_key(&self) -> String {
        match &self.step {
            Step::EnterKey { key, .. } => "*".repeat(key.chars().count()),
            Step::PickProvider { .. } => String::new(),
        }
    }

    /// 渲染行（dim 风格由 display.rs 上色；这里只出纯文本）。
    pub fn render_rows(&self) -> Vec<String> {
        match &self.step {
            Step::PickProvider { query, selection } => {
                let mut rows = vec![
                    "配置模型服务 — 选择预设（输入过滤 · ↑↓ 选择 · Enter 下一步 · Esc 取消）"
                        .to_string(),
                ];
                let filtered = self.filtered();
                if filtered.is_empty() {
                    rows.push("  没有匹配的预设".to_string());
                }
                const MAX_ROWS: usize = 8;
                for (index, preset) in filtered.iter().take(MAX_ROWS).enumerate() {
                    let cursor = if index == *selection { "›" } else { " " };
                    rows.push(format!(
                        "  {cursor} {:<18} {}",
                        preset.label, preset.base_url
                    ));
                }
                if filtered.len() > MAX_ROWS {
                    rows.push(format!("  … 共 {} 个预设（继续输入过滤）", filtered.len()));
                }
                if !query.is_empty() {
                    rows.push(format!("  过滤：{query}"));
                }
                rows
            }
            Step::EnterKey { env_mode: true, .. } => {
                let Some(preset) = self.key_step_preset() else {
                    return vec!["配置流程异常：预设缺失".to_string()];
                };
                let mut rows = vec![
                    format!(
                        "配置 {} — 环境变量鉴权（Enter 保存 · Tab 切回输 key · Esc 返回）",
                        preset.label
                    ),
                    format!("  端点  {}", preset.base_url),
                    "  加载时按顺序读取（任一非空即生效）：".to_string(),
                ];
                for (index, var) in env_var_names(preset.id).iter().enumerate() {
                    let mark = if std::env::var(var)
                        .map(|value| !value.trim().is_empty())
                        .unwrap_or(false)
                    {
                        "✓ 已设置"
                    } else {
                        "· 未设置"
                    };
                    rows.push(format!("    {}. {:<36} {mark}", index + 1, var));
                }
                rows.push(
                    "  不落任何密钥；变量未设置时首次发送会报缺鉴权，设置后重试即可".to_string(),
                );
                rows
            }
            Step::EnterKey { .. } => {
                let Some(preset) = self.key_step_preset() else {
                    return vec!["配置流程异常：预设缺失".to_string()];
                };
                vec![
                    format!(
                        "配置 {} — 输入 API Key（Enter 保存 · Tab 改用环境变量 · Esc 返回）",
                        preset.label
                    ),
                    format!("  端点  {}", preset.base_url),
                    match preset.api_key_url {
                        Some(url) => format!("  取key {url}"),
                        None => "  取key 见厂商控制台".to_string(),
                    },
                    format!("  key: {}", self.masked_key()),
                ]
            }
        }
    }
}

impl Default for SetupFlow {
    fn default() -> Self {
        Self::new()
    }
}

/// 保存配置：插入/覆盖该预设的 ProviderConfig 并设为默认。
/// api_key 明文只在这条路径上出现一次——`save_global` 会把它迁入平台
/// 凭据后端并在落盘的 TOML 里清空。
pub fn apply(config_dir: &std::path::Path, preset: &Preset, api_key: &str) -> Result<(), String> {
    let key = api_key.trim();
    if key.is_empty() {
        return Err("API key 为空".to_string());
    }
    let settings = r_code_host::settings::SettingsService::new(config_dir.to_path_buf());
    let mut config = settings
        .load_global_unvalidated()
        .map_err(|error| format!("读取配置失败：{error}"))?;
    config.providers.insert(
        preset.id.to_string(),
        agent_config::ProviderConfig {
            base_url: preset.base_url.to_string(),
            api_key: key.to_string(),
            model: preset.model.to_string(),
            provider_kind: Some(preset.id.to_string()),
            max_tokens: preset.max_output_tokens,
            temperature: None,
            protocol: Some(preset.protocol.as_str().to_string()),
            show_reasoning: true,
        },
    );
    config.default_provider = preset.id.to_string();
    settings
        .save_global(&config)
        .map_err(|error| format!("保存配置失败：{error}"))?;
    Ok(())
}

/// G11：该预设环境变量鉴权会读取的变量名（与宿主
/// `settings::provider_env_value` 同一顺序：厂商别名在前、profile 作用域
/// 变量在后——别名是用户 shell 里最常见的既有形态）。
pub fn env_var_names(preset_id: &str) -> Vec<String> {
    let mut names = Vec::new();
    match preset_id {
        "anthropic" => names.push("ANTHROPIC_API_KEY".to_string()),
        "openai" => names.push("OPENAI_API_KEY".to_string()),
        "deepseek" => names.push("DEEPSEEK_API_KEY".to_string()),
        _ => {}
    }
    names.push(format!(
        "R_CODE_PROVIDER_{}_API_KEY",
        preset_id.to_ascii_uppercase()
    ));
    names
}

/// G11 环境变量鉴权落盘：provider 配置 `api_key` 留空 + 设为默认。
///
/// 空密钥不触碰平台凭据后端（`save_global` 只迁移非空 key）——密钥始终
/// 只存在于用户 shell 环境里，加载时由 `apply_env` 回填（pi envApiKeyAuth
/// 同款语义：auth.json 缺失回落环境变量）。
pub fn apply_env_mode(config_dir: &std::path::Path, preset: &Preset) -> Result<(), String> {
    let settings = r_code_host::settings::SettingsService::new(config_dir.to_path_buf());
    let mut config = settings
        .load_global_unvalidated()
        .map_err(|error| format!("读取配置失败：{error}"))?;
    config.providers.insert(
        preset.id.to_string(),
        agent_config::ProviderConfig {
            base_url: preset.base_url.to_string(),
            // 环境变量模式：密钥永不落盘；加载链 apply_env 回填。
            api_key: String::new(),
            model: preset.model.to_string(),
            provider_kind: Some(preset.id.to_string()),
            max_tokens: preset.max_output_tokens,
            temperature: None,
            protocol: Some(preset.protocol.as_str().to_string()),
            show_reasoning: true,
        },
    );
    config.default_provider = preset.id.to_string();
    settings
        .save_global(&config)
        .map_err(|error| format!("保存配置失败：{error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(id: &str) -> &'static Preset {
        setup_presets()
            .iter()
            .find(|p| p.id == id)
            .copied()
            .unwrap_or_else(|| {
                panic!(
                    "预设 {id} 必须存在；现有：{:?}",
                    setup_presets().iter().map(|p| p.id).collect::<Vec<_>>()
                )
            })
    }

    /// 两步流：过滤 → 选中 → Enter 进 key 步 → 输入 → submit 出 Applied。
    #[test]
    fn two_step_flow_with_filter() {
        let mut flow = SetupFlow::new();
        for ch in "open".chars() {
            flow.input_char(ch);
        }
        let selected = flow.selected_preset().expect("过滤后有选中");
        assert_eq!(selected.id, "openai", "open 应首选命中 openai");
        assert!(
            flow.advance().is_none(),
            "PickProvider 的 Enter 不产出 submit"
        );
        match flow.step() {
            Step::EnterKey {
                preset_id,
                key,
                env_mode,
            } => {
                assert_eq!(preset_id, "openai");
                assert!(key.is_empty());
                assert!(!*env_mode, "默认走输 key 模式");
            }
            other => panic!("应进入 EnterKey：{other:?}"),
        }
        for ch in "sk-test-123".chars() {
            flow.input_char(ch);
        }
        assert_eq!(flow.masked_key(), "*".repeat(11), "key 不回显原文");
        assert_eq!(
            flow.advance(),
            Some(SubmitOutcome::Applied {
                provider: "openai".to_string(),
                model: preset("openai").model.to_string(),
            })
        );
        // Esc 回到选预设步，query 预填当前 id。
        let mut flow2 = SetupFlow::new();
        flow2.advance();
        assert!(!flow2.back());
        match flow2.step() {
            Step::PickProvider { query, .. } => assert_eq!(query, "anthropic"),
            other => panic!("{other:?}"),
        }
    }

    /// 空过滤命中全部预设；无匹配显示空态行。
    #[test]
    fn filter_empty_and_no_match() {
        let mut flow = SetupFlow::new();
        assert_eq!(
            flow.selected_preset().unwrap().id,
            setup_presets()[0].id,
            "默认选中第一个预设"
        );
        for ch in "zzz-nope".chars() {
            flow.input_char(ch);
        }
        assert!(flow.selected_preset().is_none());
        assert!(flow
            .render_rows()
            .iter()
            .any(|l| l.contains("没有匹配的预设")));
    }

    /// 空 key submit 拒绝。
    #[test]
    fn empty_key_rejected() {
        let mut flow = SetupFlow::new();
        flow.advance();
        assert_eq!(flow.advance(), Some(SubmitOutcome::EmptyKey));
    }

    /// G11.A1：Tab 切环境变量模式——切换清空残留 key；submit 出 AppliedEnv
    /// （携带变量清单）；再 Tab 切回输 key 模式。
    #[test]
    fn env_mode_toggle_and_submit() {
        let mut flow = SetupFlow::new();
        flow.advance(); // 进 anthropic 的 key 步
        for ch in "sk-half".chars() {
            flow.input_char(ch);
        }
        flow.toggle_env_mode();
        match flow.step() {
            Step::EnterKey { key, env_mode, .. } => {
                assert!(env_mode, "Tab 后进 env 模式");
                assert!(key.is_empty(), "切换即清空半截明文 key");
            }
            other => panic!("{other:?}"),
        }
        let rows = flow.render_rows();
        assert!(
            rows.iter().any(|line| line.contains("环境变量鉴权")),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|line| line.contains("ANTHROPIC_API_KEY")),
            "厂商别名必须展示：{rows:?}"
        );
        assert!(
            rows.iter()
                .any(|line| line.contains("R_CODE_PROVIDER_ANTHROPIC_API_KEY")),
            "profile 作用域变量必须展示：{rows:?}"
        );
        assert_eq!(
            flow.advance(),
            Some(SubmitOutcome::AppliedEnv {
                provider: "anthropic".to_string(),
                model: preset("anthropic").model.to_string(),
                env_vars: vec![
                    "ANTHROPIC_API_KEY".to_string(),
                    "R_CODE_PROVIDER_ANTHROPIC_API_KEY".to_string(),
                ],
            })
        );
        // 再 Tab 切回输 key 模式：Enter 仍走明文 key 路径。
        flow.toggle_env_mode();
        for ch in "sk-ant-x".chars() {
            flow.input_char(ch);
        }
        assert!(matches!(
            flow.advance(),
            Some(SubmitOutcome::Applied { .. })
        ));
    }

    /// G11.A2：变量名清单——厂商别名在前；非厂商预设只有 profile 变量。
    #[test]
    fn env_var_names_cover_vendor_alias_and_profile_scope() {
        assert_eq!(
            env_var_names("anthropic"),
            vec!["ANTHROPIC_API_KEY", "R_CODE_PROVIDER_ANTHROPIC_API_KEY"]
        );
        assert_eq!(
            env_var_names("openai"),
            vec!["OPENAI_API_KEY", "R_CODE_PROVIDER_OPENAI_API_KEY"]
        );
        assert_eq!(
            env_var_names("deepseek"),
            vec!["DEEPSEEK_API_KEY", "R_CODE_PROVIDER_DEEPSEEK_API_KEY"]
        );
        assert_eq!(
            env_var_names("azure_openai"),
            vec!["R_CODE_PROVIDER_AZURE_OPENAI_API_KEY"]
        );
    }

    /// G11.A3：apply_env_mode 落盘——api_key 空、不触碰凭据后端、默认生效。
    #[test]
    fn apply_env_mode_writes_empty_key_without_credentials() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path().join("config");
        std::fs::create_dir_all(&config_dir).expect("mkdir");
        apply_env_mode(&config_dir, preset("openai")).expect("apply env mode");

        let toml = std::fs::read_to_string(config_dir.join("config.toml")).expect("TOML");
        assert!(toml.contains("default_provider = \"openai\""), "{toml}");
        // 落盘 api_key 为空（无密钥可泄；加载期由 apply_env 回填）。
        assert!(!toml.contains("sk-"), "不得出现任何密钥形态：{toml}");
        let settings = r_code_host::settings::SettingsService::new(config_dir.clone());
        let config = settings.load_global_unvalidated().expect("reload");
        let provider = config.providers.get("openai").expect("provider 已写入");
        assert!(provider.api_key.is_empty(), "空密钥落盘");
        // 凭据后端无该 provider 条目（没写过任何东西）。
        assert!(
            settings.provider_secret("openai").expect("read").is_none(),
            "环境变量模式不得写入平台凭据后端"
        );
    }

    /// apply：写盘 + 平台凭据 + 默认 provider + 协议 slug；文件无明文 key。
    #[test]
    fn apply_writes_config_without_plaintext_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path().join("config");
        std::fs::create_dir_all(&config_dir).expect("mkdir");
        apply(&config_dir, preset("anthropic"), "  sk-ant-secret-1  ").expect("apply");

        let settings = r_code_host::settings::SettingsService::new(config_dir.clone());
        let config = settings.load_global_unvalidated().expect("reload");
        assert_eq!(config.default_provider, "anthropic");
        let provider = config.providers.get("anthropic").expect("provider 已写入");
        assert_eq!(provider.base_url, "https://api.anthropic.com");
        assert_eq!(provider.protocol.as_deref(), Some("anthropic_messages"));
        assert_eq!(provider.model, preset("anthropic").model);
        // 落盘 TOML 不含明文 key（已被迁入平台凭据后端）。
        let toml = std::fs::read_to_string(config_dir.join("config.toml")).expect("读 TOML");
        assert!(
            !toml.contains("sk-ant-secret-1"),
            "明文 key 不得落盘：{toml}"
        );
        // 重新加载能从凭据后端取回 key（trim 已生效）。
        let resolved = settings.load_global().expect("validated load");
        let secret = resolved
            .providers
            .get("anthropic")
            .expect("provider")
            .api_key
            .clone();
        assert_eq!(secret, "sk-ant-secret-1", "key 从平台凭据后端回填");
    }
}

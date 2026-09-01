//! Deferred Tools 分流（docs/pi-alignment PRD §4.1 R-CCH-01 / M3-01）。
//!
//! 目标：会话中途新增的工具不再击穿 tools 定义前缀的 provider 缓存——
//! 新增工具**不进**本轮请求的 `tools` 数组，改以一条 user 消息序列化到
//! transcript 尾（模型按需经由文字描述感知它；真正可调用要等下一个
//! 前缀本来就要变的时机——新 run / 压缩点——再进 tools）。
//!
//! 判定规则（PRD 固定约束）：
//! - 只搬**新增**工具（上一 run 未见过的名字）；已消失的工具不补；
//! - **已被实际调用过的工具不搬移**（调用历史里的 tool_use 引用必须在
//!   tools 里可解析，否则请求非法）；
//! - 所有工具都被判 deferred 时**无条件回退 immediate**（tools 数组不能为
//!   空——空数组等于让模型裸跑，破坏正确性优先于缓存）。
//!
//! 能力探测与白名单开关在 M3-02（`supports_tool_reference`）；本模块只提供
//! 纯分流函数，默认（未启用）路径 = 全部 immediate、零行为变化。

use agent_contract::ToolSpec;

/// 支持 tool reference（deferred tools 序列化进 transcript 即可被引用）的
/// provider/模型白名单（R-CCH-02：默认关闭、白名单开启）。
///
/// 当前为空：没有任何线路经实机核实支持该形态（真实 Provider 复测属
/// §11.3 外部放行）。接线点唯一：[`split_deferred_tools`] 的 `enable` 由
/// 调用方用本函数解析——非白名单一律不启用，不影响请求正确性。
const TOOL_REFERENCE_WHITELIST: &[(&str, &str)] = &[];

/// 能力探测：该 provider/model 是否启用 deferred tools。
/// 精确匹配白名单（provider_kind + model）；其余全部 false。
pub fn tool_reference_enabled(provider_kind: &str, model: &str) -> bool {
    TOOL_REFERENCE_WHITELIST
        .iter()
        .any(|(kind, allowed_model)| *kind == provider_kind && *allowed_model == model)
}

/// 一次分流决策。
#[derive(Debug, Clone, Default)]
pub struct DeferredToolsSplit {
    /// 本轮请求 tools 数组继续携带的工具（原顺序）。
    pub immediate: Vec<ToolSpec>,
    /// 序列化到 transcript 尾的新增工具（名字集合，供注入层取规格）。
    pub deferred: Vec<String>,
    /// 全部工具被判 deferred 后的回退标记（immediate = 全量，deferred 空）。
    pub fell_back_to_immediate: bool,
}

/// 中途新增工具判定：`current` 相对 `previous_run_tools` 的增量名字集。
/// 已调用过的工具（`called_tools`）即使新增也不得 deferred（请求合法性优先）。
pub fn newly_added_tools(
    previous_run_tools: &[ToolSpec],
    current_tools: &[ToolSpec],
    called: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let previous_names: std::collections::BTreeSet<&str> = previous_run_tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    current_tools
        .iter()
        .filter(|tool| !previous_names.contains(tool.name.as_str()))
        .filter(|tool| !called(tool.name.as_str()))
        .map(|tool| tool.name.clone())
        .collect()
}

/// 分流：`enable` 关闭（默认）时零行为变化（全部 immediate）。
pub fn split_deferred_tools(
    previous_run_tools: &[ToolSpec],
    current_tools: &[ToolSpec],
    called_tools: &dyn Fn(&str) -> bool,
    enable: bool,
) -> DeferredToolsSplit {
    if !enable {
        return DeferredToolsSplit {
            immediate: current_tools.to_vec(),
            deferred: Vec::new(),
            fell_back_to_immediate: false,
        };
    }
    let deferred_names: std::collections::BTreeSet<String> =
        newly_added_tools(previous_run_tools, current_tools, called_tools)
            .into_iter()
            .collect();
    let immediate: Vec<ToolSpec> = current_tools
        .iter()
        .filter(|tool| !deferred_names.contains(&tool.name))
        .cloned()
        .collect();
    // 空回退：所有工具都被判 deferred 时无条件回退 immediate（正确性优先）。
    if immediate.is_empty() && !current_tools.is_empty() {
        return DeferredToolsSplit {
            immediate: current_tools.to_vec(),
            deferred: Vec::new(),
            fell_back_to_immediate: true,
        };
    }
    DeferredToolsSplit {
        immediate,
        deferred: deferred_names.into_iter().collect(),
        fell_back_to_immediate: false,
    }
}

/// deferred 工具的 transcript 尾注入文案（模型可读的一行描述；不含 schema
/// 全文——渐进披露，schema 在真正进 tools 时模型才需要）。
pub fn deferred_tools_note(deferred: &[ToolSpec]) -> String {
    if deferred.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = deferred
        .iter()
        .map(|tool| format!("- {}: {}", tool.name, tool.description))
        .collect();
    format!(
        "以下工具在本轮暂不可调用，仅作预告（后续轮次可用）：\n{}",
        lines.join("\n")
    )
}

/// 从完整工具集中取出 deferred 名单对应的规格（注入层用）。
pub fn specs_by_names<'a>(tools: &'a [ToolSpec], names: &[String]) -> Vec<&'a ToolSpec> {
    let wanted: std::collections::BTreeSet<&str> = names.iter().map(String::as_str).collect();
    tools
        .iter()
        .filter(|tool| wanted.contains(tool.name.as_str()))
        .collect()
}

/// 诊断信息（cache_guard 断言用：分流后老工具是否保序保留——前缀字节稳定）。
pub fn tools_prefix_stable(before: &[ToolSpec], after: &[ToolSpec]) -> bool {
    let before_names: Vec<String> = before.iter().map(|tool| tool.name.clone()).collect();
    let after_names: Vec<String> = after.iter().map(|tool| tool.name.clone()).collect();
    subsequence_preserved(&before_names, &after_names)
}

fn subsequence_preserved(before: &[String], after: &[String]) -> bool {
    let mut cursor = 0usize;
    for name in before {
        // 老工具必须在 immediate 中按原相对顺序出现。
        match after[cursor..].iter().position(|item| item == name) {
            Some(offset) => cursor += offset + 1,
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_contract::ToolSource;

    fn spec(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.to_string(),
            description: format!("{name} tool"),
            input_schema: serde_json::json!({"type": "object"}),
            source: ToolSource::Builtin,
            requires_confirmation: false,
        }
    }

    fn never_called(_: &str) -> bool {
        false
    }

    /// M3-01.A1：分流逻辑正确——新增工具 deferred，老工具保留。
    /// 输入按 gateway.tool_specs 的真实语义（名字排序，跨 run 稳定）。
    #[test]
    fn split_defers_newly_added_tools_only() {
        let previous = vec![spec("bash"), spec("read")];
        let current = vec![spec("bash"), spec("mcp__new__tool"), spec("read")];
        let split = split_deferred_tools(&previous, &current, &never_called, true);
        // 老工具保留（顺序随 current，但集合一致），新增 deferred。
        assert_eq!(split.deferred, vec!["mcp__new__tool".to_string()]);
        let names: Vec<&str> = split.immediate.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["bash", "read"]);
        assert!(!split.fell_back_to_immediate);
        // 分流后 immediate 保留 previous 的全部老工具（保序）——前缀字节稳定。
        assert!(tools_prefix_stable(&previous, &split.immediate));
    }

    /// M3-01.A2：已被实际调用过的工具不搬移（即使它是新增的）。
    #[test]
    fn called_tools_are_never_deferred() {
        let previous = vec![spec("read")];
        let current = vec![spec("read"), spec("new_called"), spec("new_uncalled")];
        let called = |name: &str| name == "new_called";
        let split = split_deferred_tools(&previous, &current, &called, true);
        assert_eq!(split.deferred, vec!["new_uncalled".to_string()]);
        assert!(
            split.immediate.iter().any(|tool| tool.name == "new_called"),
            "调用过的工具必须留在 tools"
        );
    }

    /// M3-01.A3：空 immediate 无条件回退（全部 deferred 时全量 immediate）。
    #[test]
    fn all_deferred_falls_back_to_immediate() {
        // 上一 run 无工具（首 run 场景的特殊化）——全部工具都是"新增"。
        let current = vec![spec("a"), spec("b")];
        let split = split_deferred_tools(&[], &current, &never_called, true);
        assert!(split.fell_back_to_immediate);
        assert!(split.deferred.is_empty());
        assert_eq!(split.immediate.len(), 2);
    }

    /// 默认关闭：零行为变化（全部 immediate、无 deferred、无回退标记）。
    #[test]
    fn disabled_by_default_is_noop() {
        let previous = vec![spec("read")];
        let current = vec![spec("read"), spec("new")];
        let split = split_deferred_tools(&previous, &current, &never_called, false);
        assert_eq!(split.immediate.len(), 2);
        assert!(split.deferred.is_empty());
        assert!(!split.fell_back_to_immediate);
    }

    /// transcript 尾注入：名称 + 一行描述；空名单为空串。
    #[test]
    fn deferred_note_renders_names_and_descriptions() {
        let tools = vec![spec("web_fetch"), spec("browser")];
        let note = deferred_tools_note(&tools);
        assert!(note.contains("web_fetch: web_fetch tool"));
        assert!(note.contains("browser: browser tool"));
        assert!(note.starts_with("以下工具在本轮暂不可调用"));
        assert_eq!(deferred_tools_note(&[]), "");
    }

    /// specs_by_names 取规格。
    #[test]
    fn specs_lookup_by_names() {
        let tools = vec![spec("a"), spec("b"), spec("c")];
        let found = specs_by_names(&tools, &["c".to_string(), "a".to_string()]);
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|tool| tool.name != "b"));
    }
}

#[cfg(test)]
mod whitelist_tests {
    use super::tool_reference_enabled;

    /// M3-02.A1：白名单能力探测正确——默认（空白名单）全部不启用。
    #[test]
    fn non_whitelisted_providers_stay_disabled() {
        assert!(!tool_reference_enabled("deepseek", "deepseek-v4"));
        assert!(!tool_reference_enabled("kimi_coding", "kimi-k3"));
        assert!(!tool_reference_enabled("custom_relay", "relay-model"));
        assert!(!tool_reference_enabled("", ""));
    }

    /// 精确匹配语义锚（白名单非空时的行为合同；当前表为空，用函数性质断言）。
    #[test]
    fn whitelist_matching_is_exact_provider_and_model() {
        // 空表恒 false；未来加入条目后此测试锁定"精确二元组匹配"语义。
        for (kind, model) in [
            ("deepseek", "deepseek-v4"),
            ("deepseek", "other-model"),
            ("other", "deepseek-v4"),
        ] {
            assert!(!tool_reference_enabled(kind, model));
        }
    }
}

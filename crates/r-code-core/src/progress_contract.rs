//! 低噪声公开进度合同（PRD §5.3 / R-COM-02，M1-02 冻结）。
//!
//! 原生 R-Code workspace 主代理与托管的 Codex 主代理共用同一份进度播报
//! 规则：单一事实源，两个注入点只拼接文本，禁止复制成会漂移的副本。
//!
//! 四类 fixture 语义（任务卡 M1-02 步骤 3）都由本文本承载：
//! - 多阶段：首个实质工具批次前/方案实质变化时一句简短播报；
//! - 新证据：证据改变诊断或完成阶段时说明发现与下一步；
//! - 简单问答：不得为简单任务制造播报；
//! - 重复工具：不复述工具名/参数、不播报例行继续（“继续读取…”）。

/// 注入到两条主代理路径 system prompt 的进度合同正文（逐字冻结）。
pub const PUBLIC_PROGRESS_CONTRACT: &str = "Keep the user oriented during multi-stage work:\n\
- Before the first tool batch, or when the approach materially changes, give one brief public progress update describing the current action.\n\
- When tool evidence changes the diagnosis or completes a meaningful stage, briefly state the finding and next step before continuing.\n\
- Keep updates factual and useful. Do not narrate every tool call, repeat visible tool names or arguments, manufacture updates for a simple task, or expose private chain-of-thought.\n\
- Never announce a routine continuation such as \"继续读取…\" or \"Let me continue reading…\". A progress update must carry a new finding, decision, or material change; if the only content is restating the next tool call, stay silent.\n\
- Preserve chronological order: progress update, related tools, next update, then the final answer.";

/// 子代理交付报告合同（单一事实源，注入原生 R-Code 子代理与 Codex CLI
/// 委派两条路径）：三档输出 + 证据强制 + 禁止占位摘要。占位/降级必须
/// 自报，宿主据此把 unresolved 透传给父代理（转派或显式丢弃）。
pub const SUBAGENT_REPORTING_CONTRACT: &str = "Final report contract (host-enforced):\n\
- Structure the final report in exactly three tiers: **Verified** (each finding cites `file:line` or a minimal reproduction step), **Inferred** (state the evidence chain), **Unverified** (start that section with the exact heading `### 无法验证`; list what evidence is missing — never guess and never present an unverified item as a conclusion).\n\
- Every claim in the Verified tier MUST carry a file path with line number or a reproducible step. A finding without evidence belongs in Inferred at best.\n\
- If the task is unfinished (budget, timeout, or blocked), say so explicitly in the first line (\"未完成：…\") and delimit what was and was not covered. Never emit a generic summary in place of substance — a placeholder summary is a contract violation.\n\
- Keep the final report in the user's language.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m1_02_a3_shared_progress_contract_fixture() {
        // 多阶段：首次实质批次 + 阶段/方案变化。
        assert!(PUBLIC_PROGRESS_CONTRACT.contains("Before the first tool batch"));
        assert!(PUBLIC_PROGRESS_CONTRACT.contains("when the approach materially changes"));
        // 新证据：改变诊断或完成阶段。
        assert!(PUBLIC_PROGRESS_CONTRACT
            .contains("tool evidence changes the diagnosis or completes a meaningful stage"));
        // 简单问答：不制造播报。
        assert!(PUBLIC_PROGRESS_CONTRACT.contains("manufacture updates for a simple task"));
        // 重复工具：不复述工具调用；禁止例行继续播报（中英两种形态）。
        assert!(PUBLIC_PROGRESS_CONTRACT.contains("Do not narrate every tool call"));
        assert!(PUBLIC_PROGRESS_CONTRACT.contains("repeat visible tool names or arguments"));
        assert!(PUBLIC_PROGRESS_CONTRACT.contains("继续读取"));
        assert!(PUBLIC_PROGRESS_CONTRACT.contains("Let me continue reading"));
        // 私有推理禁令。
        assert!(PUBLIC_PROGRESS_CONTRACT.contains("private chain-of-thought"));
        // 顺序合同：播报 → 工具 → 下一次播报 → 最终回答。
        assert!(PUBLIC_PROGRESS_CONTRACT.contains("then the final answer"));
    }
}

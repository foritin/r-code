//! 长任务循环护栏（宿主侧硬预算 + 可观察停止信号）。
//!
//! 这里只承载与 Provider 请求形状无关的纯判定逻辑：
//! - 硬预算：工具轮数、墙钟、思考字符量；
//! - 停止信号：同一错误连败、持续调用零进展 / replay、diff 发散、测试连败。
//!
//! 所有状态都由宿主在 loop 外侧维护并按真实工具结果喂入，模型自己不能
//! 触碰或声明“已失控”。触发结果由 `llm_runtime` 映射为
//! `AgentEvent::GuardTrip` 并进入 `ReviewReady` 收尾。

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

const fn default_max_tool_rounds() -> u32 {
    60
}
const fn default_max_run_seconds() -> u64 {
    14_400
}
const fn default_reasoning_budget_chars() -> u64 {
    120_000
}
const fn default_same_error_limit() -> u8 {
    3
}
const fn default_no_progress_rounds() -> u32 {
    24
}
const fn default_replay_detection() -> bool {
    true
}
const fn default_diff_file_limit() -> u32 {
    60
}
const fn default_diff_byte_limit() -> u64 {
    262_144
}
const fn default_test_fail_limit() -> u8 {
    3
}
const fn default_checkpoint_enabled() -> bool {
    true
}

/// 单个 run 的运行预算与信号阈值。所有字段均可持久化（`serde(default)` 兼容旧
/// 配置），加载后必须再过一遍 [`RunBudgetPolicy::normalized`] 收紧到安全范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunBudgetPolicy {
    /// 最多工具轮数（模型回合产出 >=1 个工具调用即 +1）。
    #[serde(default = "default_max_tool_rounds")]
    pub max_tool_rounds: u32,
    /// 单次 run 的墙钟上限（秒）。
    #[serde(default = "default_max_run_seconds")]
    pub max_run_seconds: u64,
    /// 累计思考内容预算（Unicode 字符）。
    #[serde(default = "default_reasoning_budget_chars")]
    pub reasoning_budget_chars: u64,
    /// 同一错误指纹连败多少次即停。
    #[serde(default = "default_same_error_limit")]
    pub same_error_limit: u8,
    /// 连续多少工具轮没有任何进展即停。
    #[serde(default = "default_no_progress_rounds")]
    pub no_progress_rounds: u32,
    /// 相邻两轮工具请求集合与结果完全一致时视为 replay。
    #[serde(default = "default_replay_detection")]
    pub replay_detection: bool,
    /// 累计被修改的不同文件数上限。
    #[serde(default = "default_diff_file_limit")]
    pub diff_file_limit: u32,
    /// 累计变更字节上限（old + new 正文的近似长度）。
    #[serde(default = "default_diff_byte_limit")]
    pub diff_byte_limit: u64,
    /// 测试连续失败多少次即停。
    #[serde(default = "default_test_fail_limit")]
    pub test_fail_limit: u8,
    /// 测试全绿后是否创建 git checkpoint。
    #[serde(default = "default_checkpoint_enabled")]
    pub checkpoint_enabled: bool,
}

impl Default for RunBudgetPolicy {
    fn default() -> Self {
        Self {
            max_tool_rounds: default_max_tool_rounds(),
            max_run_seconds: default_max_run_seconds(),
            reasoning_budget_chars: default_reasoning_budget_chars(),
            same_error_limit: default_same_error_limit(),
            no_progress_rounds: default_no_progress_rounds(),
            replay_detection: default_replay_detection(),
            diff_file_limit: default_diff_file_limit(),
            diff_byte_limit: default_diff_byte_limit(),
            test_fail_limit: default_test_fail_limit(),
            checkpoint_enabled: default_checkpoint_enabled(),
        }
    }
}

impl RunBudgetPolicy {
    /// 把损坏/极端配置收紧到安全范围。product 层不提供“仅常量”旁路。
    pub fn normalized(self) -> Self {
        Self {
            max_tool_rounds: self.max_tool_rounds.clamp(4, 200),
            max_run_seconds: self.max_run_seconds.clamp(300, 86_400),
            reasoning_budget_chars: self.reasoning_budget_chars.clamp(20_000, 4_000_000),
            same_error_limit: self.same_error_limit.clamp(1, 10),
            no_progress_rounds: self.no_progress_rounds.clamp(2, 200),
            replay_detection: self.replay_detection,
            diff_file_limit: self.diff_file_limit.clamp(1, 1_000),
            diff_byte_limit: self.diff_byte_limit.clamp(65_536, 1_073_741_824),
            test_fail_limit: self.test_fail_limit.clamp(1, 10),
            checkpoint_enabled: self.checkpoint_enabled,
        }
    }
}

/// 触发原因。由 `llm_runtime` 经 [`trip_reason_to_dto`] 映射为
/// `r-code-core::dto::GuardTripReason` 后发事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TripReason {
    ToolRoundBudget,
    WallClockBudget,
    ReasoningBudget,
    SameError,
    NoProgress,
    DiffDivergence,
    TestFailures,
}

impl TripReason {
    pub const fn label(self) -> &'static str {
        match self {
            Self::ToolRoundBudget => "工具轮数预算耗尽",
            Self::WallClockBudget => "运行时长预算耗尽",
            Self::ReasoningBudget => "思考预算耗尽",
            Self::SameError => "同一错误连续失败",
            Self::NoProgress => "工具持续调用无进展",
            Self::DiffDivergence => "变更范围发散",
            Self::TestFailures => "测试连续失败",
        }
    }
}

/// 一次护栏触发及其面向用户的中文说明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardTrip {
    pub reason: TripReason,
    pub detail: String,
}

/// 把护栏内部原因映射到 IPC 事件使用的枚举。两个枚举的 serde 命名一一对应。
pub fn trip_reason_to_dto(reason: TripReason) -> r_code_core::dto::GuardTripReason {
    match reason {
        TripReason::ToolRoundBudget => r_code_core::dto::GuardTripReason::ToolRoundsExceeded,
        TripReason::WallClockBudget => r_code_core::dto::GuardTripReason::WallClockExceeded,
        TripReason::ReasoningBudget => r_code_core::dto::GuardTripReason::ReasoningBudgetExceeded,
        TripReason::SameError => r_code_core::dto::GuardTripReason::SameErrorLimit,
        TripReason::NoProgress => r_code_core::dto::GuardTripReason::NoProgress,
        TripReason::DiffDivergence => r_code_core::dto::GuardTripReason::DiffDivergence,
        TripReason::TestFailures => r_code_core::dto::GuardTripReason::TestFailStreak,
    }
}

/// 一轮工具调用后宿主喂给护栏的观察值。与公共层（agent-contract）的 `PendingToolCall` /
/// `ToolCallOutcome` 解耦，方便纯逻辑单测。
#[derive(Debug, Clone)]
pub struct ToolObservation {
    pub name: String,
    pub input: serde_json::Value,
    pub is_error: bool,
    /// 结构化错误码（例如 `old_string_not_found`）。
    pub error_code: Option<String>,
    /// 命令类工具的退出码；纯文本工具无退出码。
    pub exit_code: Option<i32>,
    /// 输出文本片段，用于退出码缺失时的失败/成功文本判定。
    pub output_snippet: String,
}

/// 需要从指纹中剔除的易变输入键（任意对象层级）。
const VOLATILE_INPUT_KEYS: &[&str] = &[
    "timestamp",
    "time",
    "created_at",
    "updated_at",
    "request_id",
    "nonce",
    "id",
    "call_id",
    "session_id",
    "run_id",
];

/// 成功的写类工具：视为“有进展”，并计入 diff 范围统计。
const MUTATING_TOOLS: &[&str] = &[
    "edit",
    "create_file",
    "write_file",
    "write",
    "apply_patch",
    "delete_file",
    "move_file",
    "replace_in_file",
    "multi_edit",
    "patch",
];

/// 命令类工具：需要解析退出码并识别测试命令。
const COMMAND_TOOLS: &[&str] = &["bash", "run", "execute_command", "shell", "terminal"];

/// 测试命令前缀（trim 后不区分大小写）。
const TEST_COMMAND_PREFIXES: &[&str] = &[
    "cargo test",
    "pytest",
    "npm test",
    "npm run test",
    "pnpm test",
    "pnpm run test",
    "yarn test",
    "go test",
    "dotnet test",
];

fn stable_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut cleaned = serde_json::Map::new();
            for (key, item) in map {
                if VOLATILE_INPUT_KEYS.contains(&key.as_str()) {
                    continue;
                }
                cleaned.insert(key.clone(), stable_json(item));
            }
            serde_json::Value::Object(cleaned)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(stable_json).collect())
        }
        other => other.clone(),
    }
}

fn stable_input(value: &serde_json::Value) -> String {
    // serde_json::Map 默认按 BTreeMap 排序，重建后即得到键序稳定的序列化。
    stable_json(value).to_string()
}

fn observation_command(input: &serde_json::Value) -> Option<String> {
    for key in ["command", "cmd", "script", "shell_command", "code", "text"] {
        if let Some(text) = input.get(key).and_then(serde_json::Value::as_str) {
            return Some(text.to_string());
        }
    }
    None
}

fn is_test_command(command: &str) -> bool {
    let trimmed = command.trim().trim_start_matches('\u{feff}');
    let lowered = trimmed.to_ascii_lowercase();
    TEST_COMMAND_PREFIXES
        .iter()
        .any(|prefix| lowered.starts_with(prefix))
}

fn is_mutating_tool(name: &str) -> bool {
    MUTATING_TOOLS.contains(&name.to_ascii_lowercase().as_str())
}

fn is_command_tool(name: &str) -> bool {
    COMMAND_TOOLS.contains(&name.to_ascii_lowercase().as_str())
}

fn observation_is_test_command(observation: &ToolObservation) -> bool {
    is_command_tool(&observation.name)
        && observation_command(&observation.input)
            .as_deref()
            .is_some_and(is_test_command)
}

fn observation_path(input: &serde_json::Value) -> Option<String> {
    for key in ["path", "file_path", "target", "destination"] {
        if let Some(text) = input.get(key).and_then(serde_json::Value::as_str) {
            return Some(text.to_string());
        }
    }
    None
}

fn observation_bytes(input: &serde_json::Value) -> u64 {
    let mut total = 0u64;
    if let Some(old) = input.get("old_string").and_then(serde_json::Value::as_str) {
        total += old.len() as u64;
    }
    if let Some(new) = input.get("new_string").and_then(serde_json::Value::as_str) {
        total += new.len() as u64;
    }
    if total > 0 {
        return total;
    }
    for key in ["content", "code", "text", "source"] {
        if let Some(text) = input.get(key).and_then(serde_json::Value::as_str) {
            total += text.len() as u64;
        }
    }
    total
}

/// 文本判定测试结果；退出码缺失时回退到这里。
fn text_test_result(snippet: &str) -> Option<bool> {
    let lowered = snippet.to_ascii_lowercase();
    let passed = lowered.contains("test result: ok")
        || lowered.contains("0 failed")
        || lowered.contains("all tests passed")
        || lowered.contains("passed!");
    if passed {
        return Some(true);
    }
    let failed = lowered.contains("tests failed")
        || lowered.contains("failures:")
        || lowered.contains("error: test failed")
        || lowered.contains("=== failed")
        || lowered.contains("failed with");
    if failed {
        return Some(false);
    }
    None
}

/// Per-run 护栏状态。`run_loop` 与 `run_child` 各持一个实例，子代理不共享父
/// 计数但继承同一阈值。
#[derive(Debug)]
pub struct RunLoopGuard {
    policy: RunBudgetPolicy,
    started_at: Instant,
    tool_rounds: u32,
    reasoning_chars: u64,
    /// 同错指纹 -> 连败次数。
    same_error_streaks: HashMap<String, u32>,
    rounds_since_progress: u32,
    previous_round_signature: Option<Vec<String>>,
    touched_files: HashSet<String>,
    touched_bytes: u64,
    test_failures_in_a_row: u32,
    last_round_tests_green: bool,
    tripped: bool,
}

impl RunLoopGuard {
    pub fn new(policy: RunBudgetPolicy) -> Self {
        Self {
            policy: policy.normalized(),
            started_at: Instant::now(),
            tool_rounds: 0,
            reasoning_chars: 0,
            same_error_streaks: HashMap::new(),
            rounds_since_progress: 0,
            previous_round_signature: None,
            touched_files: HashSet::new(),
            touched_bytes: 0,
            test_failures_in_a_row: 0,
            last_round_tests_green: false,
            tripped: false,
        }
    }

    /// 每轮发送请求前调用：只检查与“模型回合”无关的累计预算。工具轮预算在
    /// `observe_tool_round` 内随计数一起检查，纯文本轮不消耗轮数。
    pub fn before_iteration(&mut self) -> Option<GuardTrip> {
        if self.tripped {
            return None;
        }
        let elapsed = self.started_at.elapsed();
        if elapsed >= Duration::from_secs(self.policy.max_run_seconds) {
            return self.trip(
                TripReason::WallClockBudget,
                format!(
                    "运行时长 {} 秒已达到上限 {} 秒",
                    elapsed.as_secs(),
                    self.policy.max_run_seconds
                ),
            );
        }
        if self.reasoning_chars >= self.policy.reasoning_budget_chars {
            return self.trip(
                TripReason::ReasoningBudget,
                format!(
                    "累计思考 {} 字符已达到上限 {} 字符",
                    self.reasoning_chars, self.policy.reasoning_budget_chars
                ),
            );
        }
        None
    }

    /// 累计流式思考内容（Unicode 字符）。宿主在无 reasoning usage 时按
    /// 4 字符/token 估算后调用本方法。
    pub fn note_reasoning_chars(&mut self, chars: u64) {
        self.reasoning_chars = self.reasoning_chars.saturating_add(chars);
    }

    /// 一轮工具调用执行完毕后调用；`round` 为空时应由宿主直接跳过。
    pub fn observe_tool_round(&mut self, round: &[ToolObservation]) -> Option<GuardTrip> {
        if self.tripped || round.is_empty() {
            return None;
        }
        self.tool_rounds = self.tool_rounds.saturating_add(1);

        if self.tool_rounds >= self.policy.max_tool_rounds {
            return self.trip(
                TripReason::ToolRoundBudget,
                format!(
                    "已完成 {} 个工具轮，达到上限 {}",
                    self.tool_rounds, self.policy.max_tool_rounds
                ),
            );
        }

        let mut round_signature = Vec::with_capacity(round.len());
        let mut round_progress = false;
        let mut round_test_failed = false;
        let mut round_test_passed = false;

        for observation in round {
            let name = observation.name.to_ascii_lowercase();
            let input_stable = stable_input(&observation.input);
            // 同错指纹 = 工具名 + 稳定参数 + 错误码/退出码；三处一致才算同一错误。
            let error_key = observation.error_code.clone().or_else(|| {
                observation
                    .exit_code
                    .map(|code| format!("exit:{code}"))
                    .or_else(|| observation.is_error.then(|| "tool_error".to_string()))
            });
            let outcome_key = match &error_key {
                Some(code) => format!("error:{code}"),
                None => "ok".to_string(),
            };
            round_signature.push(format!("{name}|{input_stable}|{outcome_key}"));

            // 测试命令的连败归「测试连败」信号统计，不占用同错指纹；其余错误
            // 按「工具名 + 稳定参数 + 错误码」识别同一错误。成功清零同工具同
            // 路径的记录（无路径的命令工具按工具名清零）。
            let test_command = observation_is_test_command(observation);
            if let Some(error_key) = &error_key {
                if !test_command {
                    let path_key = observation_path(&observation.input).unwrap_or_default();
                    let counter = self
                        .same_error_streaks
                        .entry(format!("{name}|{path_key}|{input_stable}|{error_key}"))
                        .or_insert(0);
                    *counter += 1;
                    if *counter >= u32::from(self.policy.same_error_limit) {
                        return self.trip(
                            TripReason::SameError,
                            format!(
                                "工具 {name} 的同一错误已连续失败 {} 次",
                                self.policy.same_error_limit
                            ),
                        );
                    }
                }
            } else {
                let clear_prefix = format!(
                    "{name}|{}|",
                    observation_path(&observation.input).unwrap_or_default()
                );
                self.same_error_streaks
                    .retain(|fingerprint, _| !fingerprint.starts_with(&clear_prefix));
            }

            // 进展：成功的写工具，或任意命令退出码 0。
            if (!observation.is_error && is_mutating_tool(&name))
                || observation.exit_code == Some(0)
            {
                round_progress = true;
            }

            // diff 发散：只统计成功写工具。
            if !observation.is_error && is_mutating_tool(&name) {
                if let Some(path) = observation_path(&observation.input) {
                    if !path.is_empty() {
                        self.touched_files.insert(path);
                    }
                }
                self.touched_bytes = self
                    .touched_bytes
                    .saturating_add(observation_bytes(&observation.input));
                if self.touched_files.len() as u32 > self.policy.diff_file_limit
                    || self.touched_bytes > self.policy.diff_byte_limit
                {
                    return self.trip(
                        TripReason::DiffDivergence,
                        format!(
                            "已修改 {} 个文件 / {} 字节，超过范围上限",
                            self.touched_files.len(),
                            self.touched_bytes
                        ),
                    );
                }
            }

            // 测试连败。
            if is_command_tool(&name) {
                if let Some(command) = observation_command(&observation.input) {
                    if is_test_command(&command) {
                        let failure = observation.is_error
                            || matches!(observation.exit_code, Some(code) if code != 0)
                            || text_test_result(&observation.output_snippet) == Some(false);
                        let success = !observation.is_error
                            && (observation.exit_code == Some(0)
                                || text_test_result(&observation.output_snippet) == Some(true));
                        if failure {
                            round_test_failed = true;
                        } else if success {
                            round_test_passed = true;
                        }
                    }
                }
            }
        }

        // replay：相邻两轮的工具集合 + 结果完全相同。
        // 含错误结果的轮次不参与 replay：它们交给同错连败 / 测试连败计数，否则
        // “同错 3 次”会在第 2 轮就被 replay 抢断，同错阈值永远无法生效。
        let round_has_error = round.iter().any(|observation| {
            observation.is_error
                || observation.error_code.is_some()
                || matches!(observation.exit_code, Some(code) if code != 0)
        });
        round_signature.sort_unstable();
        if self.policy.replay_detection && !round_has_error {
            if let Some(previous) = &self.previous_round_signature {
                if previous == &round_signature {
                    return self.trip(
                        TripReason::NoProgress,
                        "相邻两轮工具调用与结果完全相同（replay 检测）".to_string(),
                    );
                }
            }
        }
        self.previous_round_signature = Some(round_signature);

        if round_test_failed {
            self.test_failures_in_a_row = self.test_failures_in_a_row.saturating_add(1);
            if self.test_failures_in_a_row >= u32::from(self.policy.test_fail_limit) {
                return self.trip(
                    TripReason::TestFailures,
                    format!(
                        "测试已连续失败 {} 次，达到上限 {}",
                        self.test_failures_in_a_row, self.policy.test_fail_limit
                    ),
                );
            }
        } else if round_test_passed {
            self.test_failures_in_a_row = 0;
        }
        self.last_round_tests_green = round_test_passed && !round_test_failed;

        if round_progress || round_test_passed {
            self.rounds_since_progress = 0;
        } else {
            self.rounds_since_progress = self.rounds_since_progress.saturating_add(1);
            if self.rounds_since_progress >= self.policy.no_progress_rounds {
                return self.trip(
                    TripReason::NoProgress,
                    format!(
                        "连续 {} 个工具轮没有成功变更或通过的测试/构建",
                        self.rounds_since_progress
                    ),
                );
            }
        }

        None
    }

    /// 最近观察到的工具轮是否包含“测试通过”。`llm_runtime` 据此创建绿灯 checkpoint。
    pub fn last_round_tests_green(&self) -> bool {
        self.last_round_tests_green
    }

    fn trip(&mut self, reason: TripReason, detail: String) -> Option<GuardTrip> {
        if self.tripped {
            return None;
        }
        self.tripped = true;
        Some(GuardTrip { reason, detail })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_with(configure: impl FnOnce(&mut RunBudgetPolicy)) -> RunBudgetPolicy {
        let mut policy = RunBudgetPolicy::default();
        configure(&mut policy);
        policy
    }

    fn edit(path: &str, old: &str, new: &str, error: bool) -> ToolObservation {
        ToolObservation {
            name: "edit".to_string(),
            input: serde_json::json!({
                "path": path,
                "old_string": old,
                "new_string": new,
                "timestamp": 1234,
                "request_id": "volatile"
            }),
            is_error: error,
            error_code: None,
            exit_code: None,
            output_snippet: String::new(),
        }
    }

    fn bash(command: &str, exit_code: Option<i32>, snippet: &str, error: bool) -> ToolObservation {
        ToolObservation {
            name: "bash".to_string(),
            input: serde_json::json!({ "command": command }),
            is_error: error,
            error_code: None,
            exit_code,
            output_snippet: snippet.to_string(),
        }
    }

    #[test]
    fn defaults_and_serde_are_stable() {
        let policy = RunBudgetPolicy::default();
        assert_eq!(policy.max_tool_rounds, 60);
        assert_eq!(policy.max_run_seconds, 14_400);
        assert_eq!(policy.reasoning_budget_chars, 120_000);
        assert_eq!(policy.same_error_limit, 3);
        assert_eq!(policy.no_progress_rounds, 24);
        assert!(policy.replay_detection);
        assert_eq!(policy.diff_file_limit, 60);
        assert_eq!(policy.diff_byte_limit, 262_144);
        assert_eq!(policy.test_fail_limit, 3);
        assert!(policy.checkpoint_enabled);

        // 旧 JSON（无任何新字段）必须反序列化为默认值。
        let old_json = serde_json::json!({});
        let loaded: RunBudgetPolicy = serde_json::from_value(old_json).unwrap();
        assert_eq!(loaded, RunBudgetPolicy::default());
    }

    #[test]
    fn normalization_clamps_every_extreme() {
        let extreme = RunBudgetPolicy {
            max_tool_rounds: 1,
            max_run_seconds: 1,
            reasoning_budget_chars: 1,
            same_error_limit: 1,
            no_progress_rounds: 1,
            replay_detection: false,
            diff_file_limit: 0,
            diff_byte_limit: 1,
            test_fail_limit: 99,
            checkpoint_enabled: false,
        }
        .normalized();
        assert_eq!(extreme.max_tool_rounds, 4);
        assert_eq!(extreme.max_run_seconds, 300);
        assert_eq!(extreme.reasoning_budget_chars, 20_000);
        assert_eq!(extreme.same_error_limit, 1);
        assert_eq!(extreme.no_progress_rounds, 2);
        assert_eq!(extreme.diff_file_limit, 1);
        assert_eq!(extreme.diff_byte_limit, 65_536);
        assert_eq!(extreme.test_fail_limit, 10);
        assert!(!extreme.replay_detection);
        assert!(!extreme.checkpoint_enabled);
    }

    #[test]
    fn tool_round_budget_triggers_exactly_at_boundary() {
        let mut guard = RunLoopGuard::new(policy_with(|policy| policy.max_tool_rounds = 4));
        for command in [
            "cargo test --all",
            "cargo test --lib",
            "cargo test --bin app",
        ] {
            let round = [bash(command, None, "no output", false)];
            let trip = guard.observe_tool_round(&round);
            assert!(trip.is_none(), "{trip:?}");
        }
        let round = [bash("cargo test --doc", None, "no output", false)];
        let trip = guard.observe_tool_round(&round).unwrap();
        assert_eq!(trip.reason, TripReason::ToolRoundBudget);
    }

    #[test]
    fn wall_clock_and_reasoning_budgets_trip_before_iteration() {
        let mut wall = RunLoopGuard::new(policy_with(|policy| policy.max_run_seconds = 300));
        wall.started_at = Instant::now() - Duration::from_secs(301);
        assert_eq!(
            wall.before_iteration().unwrap().reason,
            TripReason::WallClockBudget
        );

        let mut reasoning = RunLoopGuard::new(policy_with(|policy| {
            policy.reasoning_budget_chars = 20_000;
        }));
        reasoning.note_reasoning_chars(19_999);
        assert!(reasoning.before_iteration().is_none());
        reasoning.note_reasoning_chars(1);
        assert_eq!(
            reasoning.before_iteration().unwrap().reason,
            TripReason::ReasoningBudget
        );
    }

    #[test]
    fn same_error_fingerprint_counts_across_tools_and_success_resets() {
        let mut guard = RunLoopGuard::new(policy_with(|policy| policy.same_error_limit = 3));
        let failing = ToolObservation {
            name: "edit".to_string(),
            input: serde_json::json!({ "path": "a.rs", "old_string": "x", "new_string": "y" }),
            is_error: true,
            error_code: Some("old_string_not_found".to_string()),
            exit_code: None,
            output_snippet: String::new(),
        };
        assert!(guard
            .observe_tool_round(std::slice::from_ref(&failing))
            .is_none());
        assert!(guard
            .observe_tool_round(std::slice::from_ref(&failing))
            .is_none());
        assert_eq!(
            guard
                .observe_tool_round(std::slice::from_ref(&failing))
                .unwrap()
                .reason,
            TripReason::SameError
        );

        // 改动参数后指纹不同，重新计数。
        let mut second = RunLoopGuard::new(policy_with(|policy| policy.same_error_limit = 2));
        let failing_a = ToolObservation {
            input: serde_json::json!({ "path": "a.rs", "old_string": "x", "new_string": "y" }),
            ..failing.clone()
        };
        let failing_b = ToolObservation {
            input: serde_json::json!({ "path": "a.rs", "old_string": "x", "new_string": "z" }),
            ..failing.clone()
        };
        assert!(second
            .observe_tool_round(std::slice::from_ref(&failing_a))
            .is_none());
        assert!(second
            .observe_tool_round(std::slice::from_ref(&failing_b))
            .is_none());
        // 成功一次清零同工具记录。
        assert!(second
            .observe_tool_round(&[edit("a.rs", "x", "z", false)])
            .is_none());
        assert!(second
            .observe_tool_round(std::slice::from_ref(&failing_a))
            .is_none());
        assert_eq!(
            second.observe_tool_round(&[failing_a]).unwrap().reason,
            TripReason::SameError
        );
    }

    #[test]
    fn no_progress_window_counts_only_barren_rounds() {
        let mut guard = RunLoopGuard::new(policy_with(|policy| policy.no_progress_rounds = 4));
        for command in ["ls", "ls -la", "ls src"] {
            let barren = [bash(command, None, "", false)];
            assert!(guard.observe_tool_round(&barren).is_none());
        }
        let barren = [bash("ls target", None, "", false)];
        assert_eq!(
            guard.observe_tool_round(&barren).unwrap().reason,
            TripReason::NoProgress
        );

        // 成功写工具重置窗口。
        let mut resets = RunLoopGuard::new(policy_with(|policy| policy.no_progress_rounds = 3));
        for command in ["ls", "ls -la"] {
            let barren = [bash(command, None, "", false)];
            assert!(resets.observe_tool_round(&barren).is_none());
        }
        assert!(resets
            .observe_tool_round(&[edit("a.rs", "x", "y", false)])
            .is_none());
        for command in ["ls src", "ls target"] {
            let barren = [bash(command, None, "", false)];
            assert!(resets.observe_tool_round(&barren).is_none());
        }
        let barren = [bash("ls docs", None, "", false)];
        assert_eq!(
            resets.observe_tool_round(&barren).unwrap().reason,
            TripReason::NoProgress
        );
    }

    #[test]
    fn replay_detection_compares_stable_round_signature() {
        let mut guard = RunLoopGuard::new(RunBudgetPolicy::default());
        let first = [edit("a.rs", "x", "y", false)];
        let same_but_volatile = [ToolObservation {
            input: serde_json::json!({
                "path": "a.rs",
                "old_string": "x",
                "new_string": "y",
                "timestamp": 9999
            }),
            ..first[0].clone()
        }];
        assert!(guard.observe_tool_round(&first).is_none());
        let trip = guard.observe_tool_round(&same_but_volatile).unwrap();
        assert_eq!(trip.reason, TripReason::NoProgress);
        assert!(trip.detail.contains("replay"));
    }

    #[test]
    fn diff_divergence_counts_distinct_files_and_bytes() {
        let mut guard = RunLoopGuard::new(policy_with(|policy| {
            policy.diff_file_limit = 2;
            policy.diff_byte_limit = 1_000_000;
        }));
        assert!(guard
            .observe_tool_round(&[edit("a.rs", "x", "y", false)])
            .is_none());
        assert!(guard
            .observe_tool_round(&[edit("a.rs", "x", "z", false)])
            .is_none());
        assert!(guard
            .observe_tool_round(&[edit("b.rs", "x", "y", false)])
            .is_none());
        assert_eq!(
            guard
                .observe_tool_round(&[edit("c.rs", "x", "y", false)])
                .unwrap()
                .reason,
            TripReason::DiffDivergence
        );

        let mut bytes = RunLoopGuard::new(policy_with(|policy| policy.diff_byte_limit = 65_536));
        let long_old = "a".repeat(40_000);
        let long_new = "b".repeat(40_000);
        assert_eq!(
            bytes
                .observe_tool_round(&[edit("a.rs", &long_old, &long_new, false)])
                .unwrap()
                .reason,
            TripReason::DiffDivergence
        );
    }

    #[test]
    fn test_failure_streak_trips_and_pass_resets() {
        let mut guard = RunLoopGuard::new(policy_with(|policy| policy.test_fail_limit = 3));
        let failed = bash("cargo test", Some(1), "2 tests failed", false);
        assert!(guard
            .observe_tool_round(std::slice::from_ref(&failed))
            .is_none());
        assert!(guard
            .observe_tool_round(std::slice::from_ref(&failed))
            .is_none());
        assert_eq!(
            guard.observe_tool_round(&[failed]).unwrap().reason,
            TripReason::TestFailures
        );

        let mut resets = RunLoopGuard::new(policy_with(|policy| policy.test_fail_limit = 2));
        assert!(resets
            .observe_tool_round(&[bash("pytest", None, "tests failed", false)])
            .is_none());
        assert!(resets
            .observe_tool_round(&[bash("cargo test", Some(0), "test result: ok", false)])
            .is_none());
        assert!(resets
            .observe_tool_round(&[bash("go test ./...", None, "FAIL", true)])
            .is_none());
        assert_eq!(
            resets
                .observe_tool_round(&[bash("dotnet test", Some(1), "", false)])
                .unwrap()
                .reason,
            TripReason::TestFailures
        );
    }

    #[test]
    fn non_test_commands_do_not_affect_test_streak() {
        let mut guard = RunLoopGuard::new(policy_with(|policy| policy.test_fail_limit = 2));
        assert!(guard
            .observe_tool_round(&[bash("npm run build", Some(1), "", false)])
            .is_none());
        assert!(guard
            .observe_tool_round(&[bash("npm run build", Some(1), "", false)])
            .is_none());
        let trip = guard.observe_tool_round(&[bash("npm run build:watch", Some(1), "", false)]);
        assert!(trip.is_none(), "{trip:?}");
    }
}

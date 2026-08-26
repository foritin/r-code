//! 命令失败诊断提示引擎（PRD windows-command-reliability §4.3 / R-DX-01）。
//!
//! 对 `bash` 工具输出与 Codex `commandExecution` 错误投影做**错误签名**分类，
//! 命中后追加有界「诊断提示」（只追加、不改写原文、总追加段 ≤400 字符、只匹配
//! 签名不回显正文之外的内容）。`bash` 工具与 codex 投影共用本同源实现。
//!
//! 命中计数走进程内旁路计数器（只记类别与次数，不记任何正文——R-MET-02），
//! 经 `diagnosis_counters()` 供宿主暴露。

use std::sync::atomic::{AtomicU64, Ordering};

use crate::tools_command::ShellDialect;

/// 诊断提示的稳定标记（金集 fail-with-hint 断言依据）。
pub const HINT_MARKER: &str = "[诊断]";
/// 追加段总长上限（含标记与建议文本）。
const HINT_MAX_CHARS: usize = 400;

/// 诊断类别（§4.3 模式表五行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosisKind {
    /// PowerShell 5.1 不支持 `&&`。
    ParserErrorAndChain,
    /// 相对路径可执行文件需要调用操作符（PS `&` / bash `./` 前缀）。
    RelativeExeInvocation,
    /// 委派只读档位拒绝了命令（codex "blocked by policy" / gateway "pre-rejected by policy"）。
    PolicyBlocked,
    /// 命令未安装或不在 PATH（cmd 风格措辞）。
    NotRecognized,
    /// cmd 链语法错误。
    CmdUnexpected,
}

impl DiagnosisKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::ParserErrorAndChain => "parser-error-chain",
            Self::RelativeExeInvocation => "relative-exe-invocation",
            Self::PolicyBlocked => "policy-blocked",
            Self::NotRecognized => "not-recognized",
            Self::CmdUnexpected => "cmd-unexpected",
        }
    }
}

/// Codex 子进程内部 shell 的方言估计：codex-rs `shell_detect` 在 Windows 上
/// 自主选择 pwsh → powershell → cmd（无公开覆盖键），据此挑选提示措辞。
pub fn codex_shell_dialect() -> ShellDialect {
    #[cfg(windows)]
    {
        let on_path = |name: &str| {
            std::env::var_os("PATH")
                .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
                .unwrap_or(false)
        };
        if on_path("pwsh.exe") {
            ShellDialect::Pwsh
        } else if on_path("powershell.exe") {
            ShellDialect::Powershell
        } else {
            ShellDialect::Cmd
        }
    }
    #[cfg(not(windows))]
    {
        ShellDialect::PosixSh
    }
}

/// 相对路径可执行文件 token（`./tool.exe`、`.\tool.exe`，引号内亦可）。
fn mentions_relative_exe(output_lower: &str) -> bool {
    // 无正则依赖的轻量判定：逐字符扫描 `./` 或 `.\` 后跟 [A-Za-z0-9_.-]+\.exe
    let bytes = output_lower.as_bytes();
    for index in 0..bytes.len() {
        if bytes[index] != b'.' || index + 1 >= bytes.len() {
            continue;
        }
        if bytes[index + 1] != b'/' && bytes[index + 1] != b'\\' {
            continue;
        }
        let mut cursor = index + 2;
        let mut name_len = 0usize;
        while cursor < bytes.len() {
            let ch = bytes[cursor];
            if ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'-' || ch == b'.' {
                cursor += 1;
                name_len += 1;
            } else {
                break;
            }
        }
        if name_len >= 4 && output_lower[index + 2..cursor].ends_with(".exe") {
            return true;
        }
    }
    false
}

/// 按签名分类失败输出（§4.3 五行，首中即返；大小写不敏感）。
pub fn classify_failure(output: &str) -> Option<DiagnosisKind> {
    let output_lower = output.to_ascii_lowercase();
    // 行 3：policy 拒绝（两种来源措辞都算）。
    if output_lower.contains("blocked by policy") || output_lower.contains("pre-rejected by policy")
    {
        return Some(DiagnosisKind::PolicyBlocked);
    }
    // 行 1：ParserError 且输出涉及 `&&`（PS 的报错会引用出错 token）。
    if output_lower.contains("parsererror") && output_lower.contains("&&") {
        return Some(DiagnosisKind::ParserErrorAndChain);
    }
    // 行 4：cmd 措辞的 not recognized（完整短语，与行 2 的 PS/bash 措辞区分）。
    if output_lower.contains("is not recognized as an internal or external command") {
        return Some(DiagnosisKind::NotRecognized);
    }
    // 行 2：not recognized / command not found / No such file or directory 且提及相对路径 .exe。
    let not_found = output_lower.contains("is not recognized")
        || output_lower.contains("command not found")
        || output_lower.contains("no such file or directory");
    if not_found && mentions_relative_exe(&output_lower) {
        return Some(DiagnosisKind::RelativeExeInvocation);
    }
    // 行 5：cmd 链语法。
    if output_lower.contains("was unexpected at this time") {
        return Some(DiagnosisKind::CmdUnexpected);
    }
    None
}

fn hint_text(kind: DiagnosisKind, dialect: ShellDialect) -> &'static str {
    match kind {
        DiagnosisKind::ParserErrorAndChain => {
            "Windows PowerShell 5.1 不支持 `&&` 链接命令。改用分号顺序执行，或用 \
`if ($?) { next }` 仅在上一步成功时继续；pwsh 7 与 bash 档均支持 `&&`。"
        }
        DiagnosisKind::RelativeExeInvocation => {
            if matches!(dialect, ShellDialect::GitBash | ShellDialect::PosixSh) {
                "相对路径可执行文件在 bash 语法下用 `./tool.exe` 形式调用即可；若文件确实\
存在仍报错，检查文件名拼写与所在目录。PowerShell 回落档则需要 `&` 调用操作符：\
`& .\\tool.exe`。"
            } else {
                "PowerShell 直接书写 `./tool.exe` 需要调用操作符：`& .\\tool.exe`\
（或 `& \".\\tool.exe\"` 带引号形式处理空格路径）。"
            }
        }
        DiagnosisKind::PolicyBlocked => {
            "该命令被当前子代理的只读（read-only）档位策略拒绝。如需写类/联网/执行类\
操作，请委派时使用 access=\"full_access\"（受审批矩阵约束），或在宿主侧调整委派权限。"
        }
        DiagnosisKind::NotRecognized => {
            "命令未安装或不在 PATH 上。R-Code 已启用注册表实时 PATH（新装工具无需重启\
即可被找到）；请确认工具名拼写，或用 `where <命令>` / `Get-Command <命令>` 核对安装。"
        }
        DiagnosisKind::CmdUnexpected => {
            "cmd.exe 不支持这类链式/引号语法。改用单条简单命令，或切到受支持的 shell 档\
（本产品的 bash 工具在 Windows 优先经 Git Bash 执行）。"
        }
    }
}

fn count_hint(kind: DiagnosisKind) {
    COUNTERS[kind as usize].fetch_add(1, Ordering::Relaxed);
}

static COUNTERS: [AtomicU64; 5] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

const ALL_KINDS: [DiagnosisKind; 5] = [
    DiagnosisKind::ParserErrorAndChain,
    DiagnosisKind::RelativeExeInvocation,
    DiagnosisKind::PolicyBlocked,
    DiagnosisKind::NotRecognized,
    DiagnosisKind::CmdUnexpected,
];

/// 读取旁路命中计数（request_audit 式：只含类别与次数）。
pub fn diagnosis_counters() -> Vec<(&'static str, u64)> {
    ALL_KINDS
        .iter()
        .map(|kind| {
            (
                kind.label(),
                COUNTERS[*kind as usize].load(Ordering::Relaxed),
            )
        })
        .collect()
}

/// 清零计数（测试用）。
pub fn reset_diagnosis_counters() {
    for counter in &COUNTERS {
        counter.store(0, Ordering::Relaxed);
    }
}

/// 对失败输出追加诊断提示：只追加、不改写原文；无命中时原样返回。
///
/// `exit_code` 语义上表示"这是一次失败"（`Some(0)` 视为成功，不做分类——
/// 正常输出零污染）。bash 工具输出与 codex commandExecution 错误投影共用。
pub fn append_diagnosis(output: &str, exit_code: Option<i32>, dialect: ShellDialect) -> String {
    if exit_code == Some(0) {
        return output.to_string();
    }
    let Some(kind) = classify_failure(output) else {
        return output.to_string();
    };
    count_hint(kind);
    let hint = format!("\n{HINT_MARKER} {}", hint_text(kind, dialect));
    let hint = if hint.chars().count() > HINT_MAX_CHARS {
        hint.chars().take(HINT_MAX_CHARS).collect::<String>()
    } else {
        hint
    };
    let mut annotated = String::with_capacity(output.len() + hint.len());
    annotated.push_str(output.trim_end());
    annotated.push('\n');
    annotated.push_str(&hint);
    annotated.push('\n');
    annotated
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 计数器是进程级静态——诊断测试组之间串行，避免并行相互污染计数断言。
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn dialect() -> ShellDialect {
        #[cfg(windows)]
        {
            crate::win_shell::resolve_windows_shell(None)
                .map(|resolved| resolved.dialect)
                .unwrap_or(ShellDialect::Cmd)
        }
        #[cfg(not(windows))]
        {
            ShellDialect::PosixSh
        }
    }

    #[test]
    fn append_diagnosis_samples_parser_error_chain() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sample =
            "ParserError (\u{7b}1\u{7d}): The token '&&' is not a valid statement separator.\n\
+ a && b\n    +       ~";
        let annotated = append_diagnosis(sample, Some(1), ShellDialect::Powershell);
        assert!(
            annotated.contains(HINT_MARKER),
            "annotated was: {annotated}"
        );
        assert!(annotated.contains("&&"), "must explain && usage");
        assert!(annotated.contains("PowerShell 5.1"));
        // 原文保留（只追加）。
        assert!(annotated.starts_with("ParserError"));
    }

    #[test]
    fn append_diagnosis_samples_relative_exe_bash_and_powershell() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // bash 档措辞（Git Bash：No such file or directory + 相对 exe）。
        let bash_sample = "bash: ./no-such-tool.exe: No such file or directory";
        let annotated = append_diagnosis(bash_sample, Some(127), ShellDialect::GitBash);
        assert!(annotated.contains(HINT_MARKER));
        assert!(annotated.contains("./tool.exe"));

        // PowerShell 档措辞（is not recognized + 相对 exe）。
        let ps_sample = "The term './missing-beta.exe' is not recognized as a name of a cmdlet";
        let annotated = append_diagnosis(ps_sample, Some(1), ShellDialect::Pwsh);
        assert!(annotated.contains(HINT_MARKER));
        assert!(
            annotated.contains("& .\\tool.exe"),
            "must explain & operator"
        );
    }

    #[test]
    fn append_diagnosis_samples_policy_blocked_both_wordings() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // codex 措辞。
        let codex = "rejected: blocked by policy (read-only sandbox)";
        assert!(append_diagnosis(codex, None, dialect()).contains("read-only"));
        // gateway R4 措辞。
        let gateway = "PermissionError: risk level R4: pre-rejected by policy";
        let annotated = append_diagnosis(gateway, None, dialect());
        assert!(annotated.contains("full_access"));
        assert!(annotated.contains("只读"));
    }

    #[test]
    fn append_diagnosis_samples_not_recognized_cmd_wording() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sample = "'xyz-cli' is not recognized as an internal or external command,\noperable program or batch file.";
        let annotated = append_diagnosis(sample, Some(1), ShellDialect::Cmd);
        assert!(annotated.contains(HINT_MARKER));
        assert!(annotated.contains("PATH"));
        assert!(annotated.contains("注册表实时 PATH"));
    }

    #[test]
    fn diagnosis_boundary_clean_output_is_untouched() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 正常成功输出零污染；无签名的失败输出也原样返回。
        let ok = "$ cargo test\nexit: 0\n--- stdout ---\nok. 3 passed\n";
        assert_eq!(append_diagnosis(ok, Some(0), dialect()), ok);
        let clean_failure = "error: could not compile `my-crate` due to 12 previous errors";
        assert_eq!(
            append_diagnosis(clean_failure, Some(101), dialect()),
            clean_failure
        );
    }

    #[test]
    fn diagnosis_boundary_hint_is_bounded() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sample = "ParserError: the token '&&' …";
        let annotated = append_diagnosis(sample, Some(1), ShellDialect::Powershell);
        let appended = &annotated[annotated.find(HINT_MARKER).unwrap()..];
        assert!(
            appended.chars().count() <= 400,
            "hint block must stay within 400 chars, got {}",
            appended.chars().count()
        );
        // 原文仍是前缀。
        assert!(annotated.starts_with("ParserError"));
    }

    #[test]
    fn diagnosis_counters_count_kinds_only() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_diagnosis_counters();
        append_diagnosis("rejected: blocked by policy", None, dialect());
        append_diagnosis("risk level R4: pre-rejected by policy", None, dialect());
        append_diagnosis(
            "'x' is not recognized as an internal or external command",
            Some(1),
            dialect(),
        );
        let counters = diagnosis_counters();
        // 只含类别与次数：每项都是 (静态标签, 数字)。
        assert!(counters
            .iter()
            .all(|(label, _)| label.chars().all(|ch| ch.is_ascii_lowercase() || ch == '-')));
        let policy = counters
            .iter()
            .find(|(label, _)| *label == "policy-blocked")
            .unwrap();
        assert_eq!(policy.1, 2);
        let not_recognized = counters
            .iter()
            .find(|(label, _)| *label == "not-recognized")
            .unwrap();
        assert_eq!(not_recognized.1, 1);
        // 计数内容不含任何正文（结构上只有标签与数字）。
        let rendered = format!("{counters:?}");
        assert!(!rendered.contains("blocked by policy"));
        reset_diagnosis_counters();
    }
}

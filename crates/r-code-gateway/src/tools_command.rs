//! 命令执行工具 -- `bash`（平台自适应 shell）。
//!
//! ## 平台策略
//!
//! | 平台 | 解释器 | 传递方式 |
//! |------|--------|----------|
//! | Windows | `pwsh.exe` → `powershell.exe` → `cmd.exe`（按 PATH 探测顺序） | 临时 `.ps1` 脚本文件 |
//! | macOS / Linux | `/bin/sh -lc` | 直接作为 argv 传入 |
//!
//! Windows 上**不用** `-Command "<字符串>"`：PowerShell 会重新解析 `-Command`
//! 之后的原始命令行，而 Rust 的 `std::process` 按 CRT 规则转义参数（内嵌 `"`
//! 变成 `\"`），两者规则不一致——`git commit -m "fix: x"` 这类命令会被拆坏。
//! 落成临时脚本再 `-File` 执行可以完全绕开引号转义问题。
//!
//! ## 风险分级
//!
//! 静态等级为 R3（工具规格里标记"需确认"），实际等级由
//! [`crate::classifier::classify_shell_command`] 按**本次命令内容**决定：
//! `cargo test` 与 `sudo rm -rf /` 不会同级。见 `classifier` 模块文档。
//!
//! ## 关于跨平台命令差异
//!
//! 模型很容易在 Windows 上写出 `grep`、`sed`、`ls -la` 这类 Unix 命令。
//! 我们的应对不是去 shim 或替换命令，而是两条：
//!
//! 1. 工具描述按平台变化（`#[cfg]`），明确告知当前是 PowerShell 还是 sh；
//! 2. 文件读取 / 搜索 / 编辑一律引导到 `read_file` / `search` / `glob` / `edit`
//!    这些**进程内、跨平台行为一致**的工具，从根上不需要 shell 命令。
//!
//! 若命令头是当前平台确实不存在的 Unix 工具，直接返回带指引的错误，省掉一轮往返。

use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use r_code_core::dto::RiskLevel;
use r_code_core::error::ProductError;
use r_code_core::process::hide_background_console;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::classifier::classify_shell_command;
use crate::gateway::{PathBinding, Tool};

/// 默认超时（毫秒）。
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
/// 超时上限（毫秒）。构建与测试可能很慢，但不能无限挂着。
const MAX_TIMEOUT_MS: u64 = 600_000;
/// stdout / stderr 各自的输出上限（字符）。
const MAX_STREAM_CHARS: usize = 30_000;

/// `bash` 的路径绑定：只有 `cwd`，缺省时回落到工作区根。
///
/// 必须是具名 const —— `&[PathBinding::default_root("cwd")]` 写在返回
/// `&'static [_]` 的函数体里不会被提升为 'static（rvalue 静态提升不覆盖
/// `const fn` 调用），会报 "temporary value dropped while borrowed"。
const BASH_PATH_BINDINGS: &[PathBinding] = &[PathBinding::default_root("cwd")];

/// Windows 上没有、且模型很容易误用的 Unix 工具 -> 对应的建议。
const UNIX_ONLY_HINTS: &[(&str, &str)] = &[
    (
        "grep",
        "改用 `search` 工具做内容搜索（正则、尊重 .gitignore、跨平台一致）",
    ),
    ("egrep", "改用 `search` 工具"),
    ("fgrep", "改用 `search` 工具并设 literal=true"),
    (
        "rg",
        "改用 `search` 工具——它内嵌了同一个 ripgrep 引擎，无需外部二进制",
    ),
    ("ack", "改用 `search` 工具"),
    ("sed", "改用 `edit` 工具做精确替换"),
    ("awk", "先用 `read_file` 取内容，再在回答里处理"),
    ("head", "改用 `read_file` 的 offset / limit 参数"),
    ("tail", "改用 `read_file` 的 offset / limit 参数"),
    (
        "wc",
        "改用 `read_file`，或 `search` 的 output_mode=\"count\"",
    ),
    ("touch", "改用 `create_file` 工具"),
    ("xargs", "把命令展开写成显式的多条命令"),
    ("which", "PowerShell 里用 `Get-Command`"),
    ("chmod", "Windows 上无对应概念"),
    ("ln", "Windows 上用 `New-Item -ItemType SymbolicLink`"),
    ("tr", "先用 `read_file` 取内容，再在回答里处理"),
    ("cut", "先用 `read_file` 取内容，再在回答里处理"),
    ("uniq", "PowerShell 里用 `Get-Unique`"),
    ("basename", "PowerShell 里用 `Split-Path -Leaf`"),
    ("dirname", "PowerShell 里用 `Split-Path -Parent`"),
    ("realpath", "PowerShell 里用 `Resolve-Path`"),
    ("mktemp", "PowerShell 里用 `New-TemporaryFile`"),
    (
        "du",
        "PowerShell 里用 `Get-ChildItem | Measure-Object -Sum Length`",
    ),
    ("df", "PowerShell 里用 `Get-PSDrive`"),
];

/// 探测某个可执行文件是否在 PATH 上。
///
/// Windows 下先试**原名**再逐个补 `PATHEXT` 后缀——顺序很关键：调用方传进来的
/// 可能已经带了扩展名（`powershell.exe`），只补后缀会去找不存在的
/// `powershell.exe.EXE`，把本来装着的 PowerShell 判成缺失。
///
/// 注意 PowerShell 的**别名**（`ls`、`cat`）不是 PATH 上的文件，
/// 所以这个函数只用来判断"真二进制"是否存在。
fn executable_on_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    // 空后缀代表"按原名直接找"，两个平台都需要。
    let mut extensions: Vec<String> = vec![String::new()];
    if cfg!(windows) {
        extensions.extend(
            std::env::var("PATHEXT")
                .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".to_string())
                .split(';')
                .filter(|e| !e.is_empty())
                .map(|e| e.to_string()),
        );
    }
    for dir in std::env::split_paths(&paths) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for ext in &extensions {
            if dir.join(format!("{name}{ext}")).is_file() {
                return true;
            }
        }
    }
    false
}

/// 取命令串首个 token 的 basename（小写）。
fn command_head(command: &str) -> String {
    command
        .split_whitespace()
        .next()
        .unwrap_or("")
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Windows 上若命令头是不存在的 Unix 工具，返回带指引的错误文本。
fn unix_only_rejection(command: &str) -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    let head = command_head(command);
    let (_, hint) = UNIX_ONLY_HINTS.iter().find(|(name, _)| *name == head)?;
    // Git Bash / MSYS 装了同名二进制时照常放行。
    if executable_on_path(&head) {
        return None;
    }
    Some(format!(
        "当前 shell 是 PowerShell，`{head}` 不存在（PATH 上也找不到同名程序）。{hint}。"
    ))
}

/// 已解析的 shell 调用方式。
enum ShellPlan {
    /// 直接把命令作为单个 argv 传给解释器（Unix：execve，无二次解析）。
    Inline { program: String, args: Vec<String> },
    /// 命令落成临时脚本文件后执行（Windows：绕开 PowerShell 的引号重解析）。
    ///
    /// 非 Windows 平台不会构造这个变体，但类型仍需存在以保持 `plan_shell`
    /// 的返回类型跨平台一致。
    #[cfg_attr(not(windows), allow(dead_code))]
    Script {
        program: String,
        /// `-File` 之前的固定参数。
        leading: Vec<String>,
        script_path: PathBuf,
    },
}

impl ShellPlan {
    fn program(&self) -> &str {
        match self {
            Self::Inline { program, .. } | Self::Script { program, .. } => program.as_str(),
        }
    }
    fn cleanup(&self) {
        if let Self::Script { script_path, .. } = self {
            let _ = std::fs::remove_file(script_path);
        }
    }
}

/// 选择解释器并准备调用方式。
#[cfg(windows)]
fn plan_shell(command: &str) -> Result<ShellPlan, ProductError> {
    let program = ["pwsh.exe", "powershell.exe"]
        .into_iter()
        .find(|candidate| executable_on_path(candidate));

    match program {
        Some(program) => {
            // 三段固定前言：
            // 1. 强制 UTF-8 输出。中文 Windows 的控制台默认是 GBK(936)，
            //    直接 from_utf8_lossy 会把中文输出全变成 `�`。用 try/catch 包住是因为
            //    stdout 被重定向时这个赋值可能抛异常，而下一行就把错误设成终止性的。
            // 2. `$ErrorActionPreference = 'Stop'` 让 cmdlet 的非终止错误也中断脚本。
            // 3. 末尾透传原生命令的退出码，否则 pwsh 只报告脚本自身的 0。
            let script = format!(
                "try {{ $OutputEncoding = [Console]::OutputEncoding = \
[System.Text.UTF8Encoding]::new($false) }} catch {{}}\n\
$ErrorActionPreference = 'Stop'\n\
{command}\n\
if ($null -ne $LASTEXITCODE) {{ exit $LASTEXITCODE }}\n"
            );
            let script_path = std::env::temp_dir()
                .join(format!("r-code-bash-{}.ps1", uuid::Uuid::new_v4().simple()));
            std::fs::write(&script_path, script).map_err(|e| {
                ProductError::Other(format!(
                    "failed to stage command script {}: {e}",
                    script_path.display()
                ))
            })?;
            Ok(ShellPlan::Script {
                program: program.to_string(),
                leading: vec![
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-ExecutionPolicy".to_string(),
                    "Bypass".to_string(),
                    "-File".to_string(),
                ],
                script_path,
            })
        }
        // 没有 PowerShell 的极端情况（精简版 Windows）：退到 cmd.exe。
        None => Ok(ShellPlan::Inline {
            program: "cmd.exe".to_string(),
            args: vec!["/D".to_string(), "/C".to_string(), command.to_string()],
        }),
    }
}

/// 选择解释器并准备调用方式。
#[cfg(not(windows))]
fn plan_shell(command: &str) -> Result<ShellPlan, ProductError> {
    // `-c` 而非 `-lc`：不加载登录配置，避免用户 profile 改写 PATH / 别名
    // 导致同一条命令在 Agent 里和终端里行为不同。
    Ok(ShellPlan::Inline {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), command.to_string()],
    })
}

/// 把流输出截断到 `MAX_STREAM_CHARS`：保留头尾，中间省略。
///
/// 只留头部会丢掉最有价值的错误尾巴（编译器的 error 摘要通常在最后）。
fn clip_stream(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let total = text.chars().count();
    if total <= MAX_STREAM_CHARS {
        return text.into_owned();
    }
    let head_len = MAX_STREAM_CHARS / 2;
    let tail_len = MAX_STREAM_CHARS - head_len;
    let head: String = text.chars().take(head_len).collect();
    let tail: String = text.chars().skip(total - tail_len).collect();
    let omitted = total - head_len - tail_len;
    format!("{head}\n\n… [中间省略 {omitted} 个字符] …\n\n{tail}")
}

/// `bash` 工具 -- 在工作区内执行 shell 命令。
///
/// R3（静态）：实际等级由 `classify_shell_command` 按命令内容决定。
pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        #[cfg(windows)]
        {
            "Run a shell command inside the workspace. On this machine the shell is \
PowerShell (pwsh -NoProfile), so use PowerShell syntax — Unix tools like grep, sed, \
head and awk are not available. \
Do NOT use shell commands to read, search, or edit files: use read_file, search, glob, \
create_file and edit instead — they behave identically on every platform and need no approval \
for reads. Reserve this tool for builds, tests, linters, git, and package managers. \
cwd defaults to the workspace root and cannot escape it."
        }
        #[cfg(not(windows))]
        {
            "Run a shell command inside the workspace (/bin/sh, no login profile). \
Do NOT use shell commands to read, search, or edit files: use read_file, search, glob, \
create_file and edit instead — they are faster, respect .gitignore, and need no approval \
for reads. Reserve this tool for builds, tests, linters, git, and package managers. \
cwd defaults to the workspace root and cannot escape it."
        }
    }

    fn risk_level(&self) -> RiskLevel {
        // 静态兜底：让 ToolSpec.requires_confirmation 为 true。
        RiskLevel::R3
    }

    fn risk_for(&self, input: &serde_json::Value) -> RiskLevel {
        match input.get("command").and_then(|v| v.as_str()) {
            Some(command) => classify_shell_command(command).level,
            // 缺参数就无法定级 -> fail-closed 到前置拒绝。
            None => RiskLevel::R4,
        }
    }

    fn path_bindings(&self) -> &'static [PathBinding] {
        // 没有 `path`；`cwd` 缺省时回落到工作区根。
        BASH_PATH_BINDINGS
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to run."
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory, must stay inside the workspace. Defaults to the workspace root."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Kill the command after this many milliseconds. Default 120000, max 600000."
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        let command = input
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProductError::Other("missing 'command' parameter".to_string()))?;
        if command.trim().is_empty() {
            return Err(ProductError::Other(
                "'command' must not be empty".to_string(),
            ));
        }
        if let Some(rejection) = unix_only_rejection(command) {
            return Err(ProductError::Other(rejection));
        }

        // cwd 由运行时的 `PathBinding::default_root("cwd")` 注入（经 PathGuard 解析）。
        // 缺失说明调用没走工作区绑定路径 —— fail-closed，绝不回落到进程 CWD。
        let cwd = input.get("cwd").and_then(|v| v.as_str()).ok_or_else(|| {
            ProductError::Other(
                "missing 'cwd': bash must run inside a bound workspace directory".to_string(),
            )
        })?;
        let cwd_path = Path::new(cwd);
        if !cwd_path.is_dir() {
            return Err(ProductError::Other(format!(
                "cwd is not a directory: {cwd}"
            )));
        }

        let timeout_ms = input
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(1, MAX_TIMEOUT_MS);

        let plan = plan_shell(command)?;
        let mut cmd = Command::new(plan.program());
        match &plan {
            ShellPlan::Inline { args, .. } => {
                cmd.args(args);
            }
            ShellPlan::Script {
                leading,
                script_path,
                ..
            } => {
                cmd.args(leading).arg(script_path);
            }
        }
        cmd.current_dir(cwd_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        hide_background_console(cmd.as_std_mut());

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) => {
                plan.cleanup();
                return Err(ProductError::Other(format!(
                    "failed to spawn {}: {err}",
                    plan.program()
                )));
            }
        };

        // 不用 `wait_with_output()`：超时时必须保留 Child 句柄，Windows 才能用
        // taskkill 结束整棵进程树，而不是留下 node / cargo 等后代继续跑。
        let stdout_task = child.stdout.take().map(|mut pipe| {
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let _ = pipe.read_to_end(&mut bytes).await;
                bytes
            })
        });
        let stderr_task = child.stderr.take().map(|mut pipe| {
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let _ = pipe.read_to_end(&mut bytes).await;
                bytes
            })
        });

        let timeout = std::time::Duration::from_millis(timeout_ms);
        let wait_result = tokio::time::timeout(timeout, child.wait()).await;

        let (exit_code, timed_out, wait_error) = match wait_result {
            Ok(Ok(status)) => (status.code(), false, None),
            Ok(Err(err)) => (None, false, Some(format!("wait failed: {err}"))),
            Err(_) => {
                kill_tree(&mut child).await;
                let _ = child.wait().await;
                (None, true, None)
            }
        };

        let stdout = match stdout_task {
            Some(task) => task.await.unwrap_or_default(),
            None => Vec::new(),
        };
        let stderr = match stderr_task {
            Some(task) => task.await.unwrap_or_default(),
            None => Vec::new(),
        };
        plan.cleanup();

        if let Some(err) = wait_error {
            return Err(ProductError::Other(err));
        }

        Ok(render_output(
            command, exit_code, timed_out, timeout_ms, &stdout, &stderr,
        ))
    }
}

/// 结束子进程及其后代。
///
/// Windows 上 `child.kill()` 只结束解释器本身，`cargo`/`node` 等孙进程会留下来
/// 继续占用端口和文件锁；必须用 `taskkill /T` 杀整棵树。
async fn kill_tree(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    {
        if let Some(pid) = child.id() {
            let mut terminate_tree = Command::new("taskkill");
            terminate_tree.args(["/PID", &pid.to_string(), "/T", "/F"]);
            hide_background_console(terminate_tree.as_std_mut());
            let killed = terminate_tree.output().await;
            if killed.is_ok() {
                return;
            }
        }
    }
    let _ = child.kill().await;
}

/// 渲染给模型看的执行结果。
///
/// 用纯文本而非 JSON：同样的信息量下 token 更省，且模型读 shell 输出更自然。
fn render_output(
    command: &str,
    exit_code: Option<i32>,
    timed_out: bool,
    timeout_ms: u64,
    stdout: &[u8],
    stderr: &[u8],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("$ {command}\n"));
    if timed_out {
        out.push_str(&format!(
            "[超时] 命令运行超过 {timeout_ms}ms，进程树已被结束。\n"
        ));
    } else {
        match exit_code {
            Some(0) => out.push_str("exit: 0\n"),
            Some(code) => out.push_str(&format!("exit: {code}（非零，命令失败）\n")),
            None => out.push_str("exit: 未知（进程被信号结束）\n"),
        }
    }

    let stdout_text = clip_stream(stdout);
    let stderr_text = clip_stream(stderr);
    if stdout_text.trim().is_empty() && stderr_text.trim().is_empty() {
        out.push_str("(无输出)\n");
        return out;
    }
    if !stdout_text.trim().is_empty() {
        out.push_str("\n--- stdout ---\n");
        out.push_str(stdout_text.trim_end());
        out.push('\n');
    }
    if !stderr_text.trim().is_empty() {
        out.push_str("\n--- stderr ---\n");
        out.push_str(stderr_text.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn json_input(command: &str, cwd: &Path) -> serde_json::Value {
        serde_json::json!({ "command": command, "cwd": cwd.to_str().unwrap() })
    }

    #[test]
    fn static_risk_requires_confirmation() {
        assert_eq!(BashTool.risk_level(), RiskLevel::R3);
        assert!(BashTool.risk_level().requires_confirmation());
    }

    #[test]
    fn risk_is_derived_from_the_command() {
        assert_eq!(
            BashTool.risk_for(&serde_json::json!({"command": "cargo test"})),
            RiskLevel::R2
        );
        assert_eq!(
            BashTool.risk_for(&serde_json::json!({"command": "npm install x"})),
            RiskLevel::R3
        );
        assert_eq!(
            BashTool.risk_for(&serde_json::json!({"command": "sudo rm -rf /"})),
            RiskLevel::R4
        );
        // 缺 command 无法定级 -> fail-closed
        assert_eq!(BashTool.risk_for(&serde_json::json!({})), RiskLevel::R4);
    }

    #[test]
    fn cwd_is_the_only_path_binding() {
        let bindings = BashTool.path_bindings();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].key, "cwd");
        assert_eq!(bindings[0].arity, crate::gateway::PathArity::DefaultRoot);
    }

    #[test]
    fn command_head_strips_directories() {
        assert_eq!(command_head("/usr/bin/GREP -n x"), "grep");
        assert_eq!(command_head("C:\\tools\\rg.exe x"), "rg.exe");
        assert_eq!(command_head("cargo test"), "cargo");
        assert_eq!(command_head(""), "");
    }

    #[test]
    fn clip_stream_keeps_head_and_tail() {
        let short = b"hello";
        assert_eq!(clip_stream(short), "hello");

        let long = "a".repeat(MAX_STREAM_CHARS) + "TAIL_MARKER";
        let clipped = clip_stream(long.as_bytes());
        assert!(clipped.contains("中间省略"));
        assert!(clipped.ends_with("TAIL_MARKER"));
    }

    #[test]
    fn render_output_reports_exit_and_streams() {
        let text = render_output("ls", Some(0), false, 1000, b"a.txt\n", b"");
        assert!(text.contains("$ ls"));
        assert!(text.contains("exit: 0"));
        assert!(text.contains("--- stdout ---"));
        assert!(!text.contains("--- stderr ---"));

        let text = render_output("false", Some(1), false, 1000, b"", b"boom");
        assert!(text.contains("exit: 1"));
        assert!(text.contains("--- stderr ---"));

        let text = render_output("sleep 9", None, true, 500, b"", b"");
        assert!(text.contains("[超时]"));
        assert!(text.contains("500ms"));

        let text = render_output("true", Some(0), false, 1000, b"", b"");
        assert!(text.contains("(无输出)"));
    }

    #[tokio::test]
    async fn missing_command_is_rejected() {
        let dir = TempDir::new().unwrap();
        let result = BashTool
            .execute(serde_json::json!({ "cwd": dir.path().to_str().unwrap() }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn empty_command_is_rejected() {
        let dir = TempDir::new().unwrap();
        assert!(BashTool
            .execute(json_input("   ", dir.path()))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn missing_cwd_is_rejected() {
        // cwd 由运行时的 PathBinding 注入；直接调用缺省时必须报错而非落到进程 CWD。
        let result = BashTool
            .execute(serde_json::json!({ "command": "echo hi" }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn nonexistent_cwd_is_rejected() {
        let result = BashTool
            .execute(serde_json::json!({
                "command": "echo hi",
                "cwd": "/definitely/not/a/real/directory"
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn echo_roundtrips() {
        let dir = TempDir::new().unwrap();
        let out = BashTool
            .execute(json_input("echo r-code-marker", dir.path()))
            .await
            .unwrap();
        assert!(out.contains("r-code-marker"), "output was: {out}");
        assert!(out.contains("exit: 0"), "output was: {out}");
    }

    #[tokio::test]
    async fn nonzero_exit_is_reported_not_errored() {
        let dir = TempDir::new().unwrap();
        // 命令失败是"结果"，不是工具故障——必须返回 Ok 让模型读到 stderr。
        #[cfg(windows)]
        let command = "cmd /c exit 3";
        #[cfg(not(windows))]
        let command = "exit 3";
        let out = BashTool
            .execute(json_input(command, dir.path()))
            .await
            .unwrap();
        assert!(out.contains("exit: 3"), "output was: {out}");
    }

    #[tokio::test]
    async fn quotes_in_command_survive() {
        let dir = TempDir::new().unwrap();
        let out = BashTool
            .execute(json_input("echo \"fix: a b\"", dir.path()))
            .await
            .unwrap();
        assert!(out.contains("fix: a b"), "output was: {out}");
    }

    #[tokio::test]
    async fn runs_inside_the_given_cwd() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("marker.txt"), "x").unwrap();
        #[cfg(windows)]
        let command = "Get-ChildItem -Name";
        #[cfg(not(windows))]
        let command = "ls";
        let out = BashTool
            .execute(json_input(command, dir.path()))
            .await
            .unwrap();
        assert!(out.contains("marker.txt"), "output was: {out}");
    }

    #[tokio::test]
    async fn timeout_kills_the_command() {
        let dir = TempDir::new().unwrap();
        #[cfg(windows)]
        let command = "Start-Sleep -Seconds 30";
        #[cfg(not(windows))]
        let command = "sleep 30";
        let out = BashTool
            .execute(serde_json::json!({
                "command": command,
                "cwd": dir.path().to_str().unwrap(),
                "timeout_ms": 400
            }))
            .await
            .unwrap();
        assert!(out.contains("[超时]"), "output was: {out}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn unix_only_commands_get_redirected() {
        let dir = TempDir::new().unwrap();
        // 只在 PATH 上真的没有 grep 时才断言（装了 Git Bash 的机器应放行）。
        if executable_on_path("grep") {
            return;
        }
        let err = BashTool
            .execute(json_input("grep -rn foo .", dir.path()))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("PowerShell"), "message was: {msg}");
        assert!(msg.contains("search"), "message was: {msg}");
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_only_rejection_is_windows_only() {
        assert!(unix_only_rejection("grep -rn foo .").is_none());
    }

    #[test]
    fn executable_on_path_finds_the_interpreter() {
        assert!(!executable_on_path("r-code-definitely-not-a-real-binary"));

        // 回归：曾经把 PATHEXT 追加到已带扩展名的名字上（找 `powershell.exe.EXE`），
        // 导致 plan_shell 误判 PowerShell 不存在、静默退到 cmd.exe。
        // 带扩展名和不带扩展名两种写法都必须能找到同一个程序。
        #[cfg(windows)]
        {
            assert!(
                executable_on_path("powershell.exe"),
                "powershell.exe 在每个 Windows 上都存在，探测不到说明 PATHEXT 逻辑有问题"
            );
            assert!(executable_on_path("powershell"));
            assert!(executable_on_path("cmd"));
        }
        #[cfg(not(windows))]
        assert!(executable_on_path("sh"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_plans_powershell_not_cmd() {
        // 有 PowerShell 时必须走 Script 方案（临时 .ps1），否则引号会被拆坏。
        let plan = plan_shell("echo hi").unwrap();
        // 用 `&plan` 匹配：`matches!` 会 move 掉表达式，之后还要用 plan。
        assert!(
            matches!(&plan, ShellPlan::Script { .. }),
            "planned {} instead of a PowerShell script",
            plan.program()
        );
        assert!(plan.program().starts_with("pwsh") || plan.program().starts_with("powershell"));
        plan.cleanup();
    }
}

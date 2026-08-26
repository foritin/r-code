//! 命令执行工具 -- `bash`（平台自适应 shell）。
//!
//! ## 平台策略
//!
//! | 平台 | 解释器 | 传递方式 |
//! |------|--------|----------|
//! | Windows | 五级解析链（Git Bash 优先，见 [`win_shell`]）：设置覆盖 → 已知位置 → git.exe 反推 → PATH bash.exe（排除 WSL）→ pwsh/powershell/cmd 回落 | bash 档 `bash -c` argv 直传；PowerShell 回落档临时 `.ps1` 脚本文件 |
//! | macOS / Linux | `/bin/sh -c` | 直接作为 argv 传入 |
//!
//! Windows 上 PowerShell 回落档**不用** `-Command "<字符串>"`：PowerShell 会重新
//! 解析 `-Command` 之后的原始命令行，而 Rust 的 `std::process` 按 CRT 规则转义
//! 参数（内嵌 `"` 变成 `\"`），两者规则不一致——`git commit -m "fix: x"` 这类
//! 命令会被拆坏。落成临时脚本再 `-File` 执行可以完全绕开引号转义问题。
//! Git Bash 档 `bash -c` 单 argv 直传，与 Unix 档同一执行模型。
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

use async_trait::async_trait;
use r_code_core::dto::RiskLevel;
use r_code_core::error::ProductError;
use r_code_core::process::hide_background_console;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::classifier::classify_shell_command;
use crate::gateway::{PathBinding, Tool, ToolExecutionContext, ToolExecutionResult};

/// 默认超时（毫秒）。
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
/// 超时上限（毫秒）。构建与测试可能很慢，但不能无限挂着。
const MAX_TIMEOUT_MS: u64 = 600_000;
/// stdout / stderr 各自的输出上限（字符）。
const MAX_STREAM_CHARS: usize = 30_000;
const ABORT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);
/// 进程退出后，等待 stdout/stderr 读端冲刷的宽限时间。个别后代进程可能继承了
/// 管道写端且迟迟不退，`read_to_end` 会一直等 EOF；这个宽限保证命令一旦正常退出
/// 就立即返回已读到的输出，而不是干等整个 `timeout_ms`。
const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(1);

enum CommandWaitResult {
    Exited(std::io::Result<std::process::ExitStatus>),
    TimedOut,
    Cancelled,
}

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
///
/// 仅在 PowerShell/cmd 回落档生效：Git Bash 档自带 Unix 工具，`grep`/`sed`
/// 是一等公民，直接放行（PRD R-SHELL-03）。
fn unix_only_rejection(command: &str, dialect: ShellDialect) -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    if matches!(dialect, ShellDialect::GitBash) {
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

/// shell 方言档（跨平台类型；Windows 值由 `win_shell` 五级解析产生）。
///
/// 金集报告的 `dialect` 字段、诊断提示的方言参数与工具描述的平台分支共用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellDialect {
    /// macOS / Linux 的 `/bin/sh -c`。
    PosixSh,
    /// Windows 五级解析链命中的 Git Bash（第一方言，PRD 决策 1）。
    GitBash,
    /// 回落档：PowerShell 7。
    Pwsh,
    /// 回落档：Windows PowerShell 5.1。
    Powershell,
    /// 回落档：cmd.exe。
    Cmd,
}

impl ShellDialect {
    pub fn label(self) -> &'static str {
        match self {
            Self::PosixSh => "posix-sh",
            Self::GitBash => "git-bash",
            Self::Pwsh => "pwsh",
            Self::Powershell => "powershell",
            Self::Cmd => "cmd",
        }
    }
}

/// 已解析的 shell 调用方式。
#[derive(Debug)]
enum ShellPlan {
    /// 直接把命令作为单个 argv 传给解释器（Unix execve / Windows Git Bash `-c`，
    /// 均无二次解析）。
    Inline {
        dialect: ShellDialect,
        program: String,
        args: Vec<String>,
    },
    /// 命令落成临时脚本文件后执行（Windows PowerShell 回落档：绕开 `-Command`
    /// 的引号重解析）。
    ///
    /// 非 Windows 平台不会构造这个变体，但类型仍需存在以保持 `plan_shell`
    /// 的返回类型跨平台一致。
    #[cfg_attr(not(windows), allow(dead_code))]
    Script {
        dialect: ShellDialect,
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
    fn dialect(&self) -> ShellDialect {
        match self {
            Self::Inline { dialect, .. } | Self::Script { dialect, .. } => *dialect,
        }
    }
    fn cleanup(&self) {
        if let Self::Script { script_path, .. } = self {
            let _ = std::fs::remove_file(script_path);
        }
    }
}

/// 当前解析档的稳定标签（金集报告的 `dialect` 字段与诊断元数据使用）。
///
/// 只读自省：与 `plan_shell` 同源（Windows 经 `win_shell` 五级解析，TTL 缓存），
/// 但绝不产生副作用，也不改变执行行为。
pub fn current_shell_dialect_label() -> &'static str {
    current_shell_dialect().label()
}

/// 当前解析档（与 `current_shell_dialect_label` 同源；诊断提示的方言参数）。
pub fn current_shell_dialect() -> ShellDialect {
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

/// 选择解释器并准备调用方式（Windows：方言 + 暂存方案由五级解析结果决定）。
#[cfg(windows)]
fn plan_shell(command: &str, override_path: Option<&str>) -> Result<ShellPlan, ProductError> {
    let resolved = crate::win_shell::resolve_windows_shell(override_path)?;
    match resolved.dialect {
        // Git Bash 档：`bash -c <command>` 单 argv 直传——不经临时脚本、不加载
        // login profile，从根上绕开引号重解析（与 Unix 档同一执行模型）。
        ShellDialect::GitBash => Ok(ShellPlan::Inline {
            dialect: ShellDialect::GitBash,
            program: resolved.program.to_string_lossy().into_owned(),
            args: vec!["-c".to_string(), command.to_string()],
        }),
        dialect @ (ShellDialect::Pwsh | ShellDialect::Powershell) => {
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
                dialect,
                program: resolved.program.to_string_lossy().into_owned(),
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
        ShellDialect::Cmd => Ok(ShellPlan::Inline {
            dialect: ShellDialect::Cmd,
            program: "cmd.exe".to_string(),
            args: vec!["/D".to_string(), "/C".to_string(), command.to_string()],
        }),
        // win_shell 在 Windows 上不会产出 PosixSh。
        ShellDialect::PosixSh => Ok(ShellPlan::Inline {
            dialect: ShellDialect::Cmd,
            program: "cmd.exe".to_string(),
            args: vec!["/D".to_string(), "/C".to_string(), command.to_string()],
        }),
    }
}

/// 选择解释器并准备调用方式。
#[cfg(not(windows))]
fn plan_shell(command: &str, override_path: Option<&str>) -> Result<ShellPlan, ProductError> {
    // override 是 Windows 专属语义（execution.bash_shell_path），Unix 忽略。
    let _ = override_path;
    // `-c` 而非 `-lc`：不加载登录配置，避免用户 profile 改写 PATH / 别名
    // 导致同一条命令在 Agent 里和终端里行为不同。
    Ok(ShellPlan::Inline {
        dialect: ShellDialect::PosixSh,
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), command.to_string()],
    })
}

/// 把流输出截断到 `MAX_STREAM_CHARS`：保留头尾，中间省略。
///
/// 只留头部会丢掉最有价值的错误���巴（编译器的 error 摘要通常在最后）。
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

/// 后台流式排空一根输出管道。
///
/// 与 `read_to_end` 的区别：字节是边读边存进共享缓冲的，所以即使读端被提前
/// 中止，已经读到的内容也不会丢；这用于「进程已退出、但某个继承管道句柄的
/// 后代还没退」的场景——我们只给 `DRAIN_GRACE` 宽限，随后带部分输出返回。
struct StreamDrain {
    buffer: Arc<Mutex<Vec<u8>>>,
    task: tokio::task::JoinHandle<()>,
}

fn spawn_stream_drain<R>(mut reader: R) -> StreamDrain
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let shared = Arc::clone(&buffer);
    let task = tokio::spawn(async move {
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => shared.lock().await.extend_from_slice(&chunk[..n]),
            }
        }
    });
    StreamDrain { buffer, task }
}

impl StreamDrain {
    /// 等待读端在 `DRAIN_GRACE` 内自然收尾；超时则中止读任务并取走已读字节。
    async fn finish(mut self) -> Vec<u8> {
        match tokio::time::timeout(DRAIN_GRACE, &mut self.task).await {
            Ok(Ok(())) | Ok(Err(_)) => {}
            Err(_) => {
                self.task.abort();
                let _ = self.task.await;
            }
        }
        self.buffer.lock().await.clone()
    }
}

/// Windows Git Bash 档工具描述（诚实声明方言与 Unix 工具可用性）。
const GIT_BASH_TIER_DESCRIPTION: &str = "Run a shell command inside the workspace. On this machine the shell is Git Bash (bash -c, no login profile): use bash/POSIX syntax — grep, sed, awk and other Unix tools are available. Do NOT use shell commands to read, search, or edit files: use read_file, search, glob, create_file and edit instead — they behave identically on every platform and need no approval for reads. Reserve this tool for builds, tests, linters, git, and package managers. Keep commands short and single-purpose: break a multi-step build/test/package pipeline into separate calls, one step at a time, and check each step's output before the next. A command finishes and returns as soon as its process exits; timeout_ms only caps a still-running command. cwd defaults to the workspace root and cannot escape it.";

/// Windows PowerShell/cmd 回落档工具描述（保持既有方言警告）。
const POWERSHELL_FALLBACK_DESCRIPTION: &str = "Run a shell command inside the workspace. On this machine the shell is PowerShell (pwsh -NoProfile), so use PowerShell syntax — Unix tools like grep, sed, head and awk are not available. Do NOT use shell commands to read, search, or edit files: use read_file, search, glob, create_file and edit instead — they behave identically on every platform and need no approval for reads. Reserve this tool for builds, tests, linters, git, and package managers. Keep commands short and single-purpose: break a multi-step build/test/package pipeline into separate calls, one step at a time, and check each step's output before the next. A command finishes and returns as soon as its process exits; timeout_ms only caps a still-running command. cwd defaults to the workspace root and cannot escape it.";

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
        // 描述按当前解析档声明方言与语法约束（PRD R-SHELL-03）：bash 档明确
        // Git Bash 与可用 Unix 工具；PowerShell/cmd 回落档保持既有警告。
        #[cfg(windows)]
        {
            match crate::win_shell::resolve_windows_shell(None)
                .map(|resolved| resolved.dialect)
                .unwrap_or(ShellDialect::Cmd)
            {
                ShellDialect::GitBash => GIT_BASH_TIER_DESCRIPTION,
                _ => POWERSHELL_FALLBACK_DESCRIPTION,
            }
        }
        #[cfg(not(windows))]
        {
            "Run a shell command inside the workspace (/bin/sh, no login profile). Do NOT use shell commands to read, search, or edit files: use read_file, search, glob, create_file and edit instead — they are faster, respect .gitignore, and need no approval for reads. Reserve this tool for builds, tests, linters, git, and package managers. Keep commands short and single-purpose: break a multi-step build/test/package pipeline into separate calls, one step at a time, and check each step's output before the next. A command finishes and returns as soon as its process exits; timeout_ms only caps a still-running command. cwd defaults to the workspace root and cannot escape it."
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
        execute_bash(input, None, None).await
    }

    async fn execute_with_context_and_abort(
        &self,
        input: serde_json::Value,
        context: &ToolExecutionContext,
        abort_flag: Option<&AtomicBool>,
    ) -> Result<ToolExecutionResult, ProductError> {
        execute_bash(input, abort_flag, context.shell_override.as_deref())
            .await
            .map(ToolExecutionResult::from)
    }
}

async fn execute_bash(
    input: serde_json::Value,
    abort_flag: Option<&AtomicBool>,
    shell_override: Option<&str>,
) -> Result<String, ProductError> {
    let command = input
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProductError::Other("missing 'command' parameter".to_string()))?;
    if command.trim().is_empty() {
        return Err(ProductError::Other(
            "'command' must not be empty".to_string(),
        ));
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

    // 落计划（Windows 经五级解析，解析结果 TTL 缓存；PowerShell 档会暂存 .ps1，
    // 拦截命中时由 cleanup 收走）。方言档是拦截门控的唯一依据：Git Bash 档放行
    // Unix 工具，PowerShell/cmd 回落档保持前置拦截。设置覆盖指向不存在的 bash
    // 时在这里报错，绝不静默回落（PRD §4.1 第 1 级）。
    let plan = plan_shell(command, shell_override)?;
    if let Some(rejection) = unix_only_rejection(command, plan.dialect()) {
        plan.cleanup();
        return Err(ProductError::Other(rejection));
    }

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
    if matches!(plan.dialect(), ShellDialect::GitBash) {
        apply_bash_tier_env(&mut cmd);
    }
    // 注册表实时 PATH（R-ENV-01）：子进程摆脱 GUI 启动时的陈旧 PATH；
    // gateway 侧无 RTK 前缀（那是 codex 拉起路径的拼装），基底即合成值。
    #[cfg(windows)]
    cmd.env("PATH", r_code_core::win_env::synthesized_path());
    // Give every Unix command its own process group. `kill()` only targets the shell process;
    // a group lets cancellation and timeout terminate cargo/node descendants as well.
    #[cfg(unix)]
    cmd.as_std_mut().process_group(0);
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
    let stdout_drain = child.stdout.take().map(spawn_stream_drain);
    let stderr_drain = child.stderr.take().map(spawn_stream_drain);

    let timeout = std::time::Duration::from_millis(timeout_ms);
    let wait_result = {
        let wait = child.wait();
        tokio::pin!(wait);
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        loop {
            if abort_flag.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                break CommandWaitResult::Cancelled;
            }
            tokio::select! {
                result = &mut wait => break CommandWaitResult::Exited(result),
                _ = &mut deadline => break CommandWaitResult::TimedOut,
                _ = tokio::time::sleep(ABORT_POLL_INTERVAL), if abort_flag.is_some() => {}
            }
        }
    };

    let (exit_code, timed_out, cancelled, wait_error) = match wait_result {
        CommandWaitResult::Exited(Ok(status)) => (status.code(), false, false, None),
        CommandWaitResult::Exited(Err(err)) => {
            (None, false, false, Some(format!("wait failed: {err}")))
        }
        CommandWaitResult::TimedOut => {
            kill_tree(&mut child).await;
            let _ = child.wait().await;
            (None, true, false, None)
        }
        CommandWaitResult::Cancelled => {
            // Cancellation is a cleanup protocol, not just a dropped wait future. On Windows
            // kill_on_drop only guarantees the shell process; taskkill /T is required for
            // cargo/node descendants. Always reap the child before reporting cancellation.
            kill_tree(&mut child).await;
            let _ = child.wait().await;
            (None, false, true, None)
        }
    };

    let stdout = match stdout_drain {
        Some(drain) => drain.finish().await,
        None => Vec::new(),
    };
    let stderr = match stderr_drain {
        Some(drain) => drain.finish().await,
        None => Vec::new(),
    };
    plan.cleanup();

    if let Some(err) = wait_error {
        return Err(ProductError::Other(err));
    }
    if cancelled {
        return Err(ProductError::Other(
            "bash command cancelled; process tree was terminated".to_string(),
        ));
    }

    let rendered = render_output(command, exit_code, timed_out, timeout_ms, &stdout, &stderr);
    // 诊断提示（R-DX-01）：失败输出经签名分类后追加有界提示；正常输出零污染。
    Ok(crate::diagnosis::append_diagnosis(
        &rendered,
        exit_code,
        plan.dialect(),
    ))
}

/// Git Bash 档的 MSYS 环境治理（PRD R-SHELL-03）。
///
/// - `MSYS_NO_PATHCONV=1`：禁止 MSYS 把 `/c`、`/d/…` 这类 Unix 风格参数改写为
///   Windows 路径——实测 `cmd /c exit 3` 会被拆成 `cmd C:\ exit 3` 导致 cmd
///   进入交互模式；
/// - `LANG=C.UTF-8`：强制 UTF-8 locale，中文输出不依赖系统代码页（936/GBK）。
fn apply_bash_tier_env(cmd: &mut Command) {
    cmd.env("MSYS_NO_PATHCONV", "1");
    cmd.env("LANG", "C.UTF-8");
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
            if killed.is_ok_and(|output| output.status.success()) {
                return;
            }
        }
    }
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            // SAFETY: the child is spawned into a fresh process group whose id equals its pid.
            // Passing the negative group id to kill(2) targets only that command tree.
            let killed = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
            if killed == 0 {
                return;
            }
        }
    }
    let _ = child.kill().await;
}

/// 渲染给模型看的执行结果。
///
/// 触发串联拆解提示的最低步骤数：两步串联（如 install && build）是常规用法，
/// 三步及以上才值得提醒。
const CHAINED_STEPS_HINT_THRESHOLD: usize = 3;

/// 估算命令串联的步骤数：按引号外的 `&&`/`||`/`;`/`|` 切分。只服务于温和
/// 提示，不做安全判定（风险分级由 classifier 负责）。
fn count_chained_steps(command: &str) -> usize {
    let mut segments = 1_usize;
    let mut quote: Option<char> = None;
    let mut in_escape = false;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_escape {
            in_escape = false;
            continue;
        }
        match quote {
            Some(q) => {
                if ch == '\\' && q != '\'' {
                    in_escape = true;
                } else if ch == q {
                    quote = None;
                }
            }
            None => match ch {
                '\'' | '"' | '`' => quote = Some(ch),
                '&' if chars.peek() == Some(&'&') => {
                    chars.next();
                    segments += 1;
                }
                // `||` 与单管道 `|` 各计一次切分。
                '|' => {
                    if chars.peek() == Some(&'|') {
                        chars.next();
                    }
                    segments += 1;
                }
                ';' => segments += 1,
                _ => {}
            },
        }
    }
    segments
}

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
        // 成功的长串联命令附一次温和提示：引导模型把多步流水线拆成逐次调用，
        // 某一步失败时才容易定位，单次等待也更短。失败/超时本身已是信号，不再叠加。
        if exit_code == Some(0) {
            let steps = count_chained_steps(command);
            if steps >= CHAINED_STEPS_HINT_THRESHOLD {
                out.push_str(&format!(
                    "[提示] 这条命令串联了约 {steps} 个步骤；后续建议拆分为多次调用、逐步检查输出，\
避免单次长时间等待且失败时难以定位。\n"
                ));
            }
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

    /// 按当前解析档选择测试命令（bash 档用 bash 语法，PowerShell 回落档用 PS 语法）。
    /// 保证测试在装/不装 Git Bash 的机器上都成立。
    #[cfg(windows)]
    fn tier_command(bash_cmd: &str, ps_cmd: &str) -> String {
        let dialect = crate::win_shell::resolve_windows_shell(None)
            .map(|resolved| resolved.dialect)
            .unwrap_or(ShellDialect::Cmd);
        if matches!(dialect, ShellDialect::GitBash) {
            bash_cmd.to_string()
        } else {
            ps_cmd.to_string()
        }
    }

    #[test]
    fn static_risk_requires_confirmation() {
        assert_eq!(BashTool.risk_level(), RiskLevel::R3);
        assert!(BashTool.risk_level().requires_confirmation());
    }

    #[test]
    fn chained_steps_counts_separators_outside_quotes() {
        assert_eq!(count_chained_steps("cargo build"), 1);
        assert_eq!(count_chained_steps("npm install && npm run build"), 2);
        assert_eq!(
            count_chained_steps("a && b && c || d; e"),
            CHAINED_STEPS_HINT_THRESHOLD + 2
        );
        // 管道每段算一步。
        assert_eq!(count_chained_steps("cat a | grep b | wc -l"), 3);
        assert_eq!(count_chained_steps("a || b || c"), 3);
        // 引号内的分隔符不算。
        assert_eq!(count_chained_steps("echo \"a && b; c\""), 1);
        assert_eq!(count_chained_steps("echo 'a | b' && echo c"), 2);
    }

    #[test]
    fn render_output_appends_chained_hint_only_on_success() {
        let stdout = b"done";
        let chained = "a && b && c";
        let ok = render_output(chained, Some(0), false, 1_000, stdout, b"");
        assert!(ok.contains("串联了约 3 个步骤"));
        let failed = render_output(chained, Some(1), false, 1_000, stdout, b"");
        assert!(!failed.contains("串联"));
        let short = render_output("a && b", Some(0), false, 1_000, stdout, b"");
        assert!(!short.contains("串联"));
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
        // bash 档直接用内建 exit；PowerShell 回落档经 cmd（bash 档下 `/c` 会被
        // MSYS 路径转换拆坏，MSYS_NO_PATHCONV 由 M1-02 统一注入）。
        #[cfg(windows)]
        let command = tier_command("exit 3", "cmd /c exit 3");
        #[cfg(not(windows))]
        let command = "exit 3";
        let out = BashTool
            .execute(json_input(&command, dir.path()))
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
        let command = tier_command("ls", "Get-ChildItem -Name");
        #[cfg(not(windows))]
        let command = "ls";
        let out = BashTool
            .execute(json_input(&command, dir.path()))
            .await
            .unwrap();
        assert!(out.contains("marker.txt"), "output was: {out}");
    }

    #[tokio::test]
    async fn timeout_kills_the_command() {
        let dir = TempDir::new().unwrap();
        #[cfg(windows)]
        let command = tier_command("sleep 30", "Start-Sleep -Seconds 30");
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

    #[tokio::test]
    async fn cancellation_kills_and_reaps_the_command_tree_before_returning() {
        let dir = TempDir::new().unwrap();
        let started = dir.path().join("bash-cancel-started");
        let finished = dir.path().join("bash-cancel-finished");
        #[cfg(windows)]
        let command = {
            let bash_tier = matches!(
                crate::win_shell::resolve_windows_shell(None)
                    .map(|resolved| resolved.dialect)
                    .unwrap_or(ShellDialect::Cmd),
                ShellDialect::GitBash
            );
            if bash_tier {
                format!(
                    "printf started > '{}'; sleep 30; printf finished > '{}'",
                    started.display(),
                    finished.display()
                )
            } else {
                format!(
                    "Set-Content -LiteralPath '{}' -Value started; Start-Sleep -Seconds 30; Set-Content -LiteralPath '{}' -Value finished",
                    started.display().to_string().replace('\'', "''"),
                    finished.display().to_string().replace('\'', "''"),
                )
            }
        };
        #[cfg(not(windows))]
        let command = format!(
            "printf started > '{}'; sleep 30; printf finished > '{}'",
            started.display().to_string().replace('\'', "'\\''"),
            finished.display().to_string().replace('\'', "'\\''"),
        );
        let abort = std::sync::Arc::new(AtomicBool::new(false));
        let run_abort = abort.clone();
        let input = serde_json::json!({
            "command": command,
            "cwd": dir.path().to_str().unwrap(),
            "timeout_ms": 60_000,
        });
        let context = ToolExecutionContext {
            origin_request_key: None,
            task_id: "task-cancel-bash".to_string(),
            run_id: "run-cancel-bash".to_string(),
            tool_call_id: "call-cancel-bash".to_string(),
            caller: Some("subagent:run-cancel-bash".to_string()),
            access_mode: r_code_core::dto::ProjectAccessMode::FullAccess,
            shell_override: None,
        };
        let run = tokio::spawn(async move {
            BashTool
                .execute_with_context_and_abort(input, &context, Some(run_abort.as_ref()))
                .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !started.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("bash fixture must start before cancellation");
        abort.store(true, Ordering::SeqCst);

        let error = tokio::time::timeout(std::time::Duration::from_secs(5), run)
            .await
            .expect("cancelled bash must reap promptly")
            .expect("bash test task panicked")
            .expect_err("cancelled bash must not report success");
        assert!(error.to_string().contains("cancelled"), "error: {error}");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !finished.exists(),
            "the cancelled command continued and wrote its completion marker"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn unix_only_commands_get_redirected() {
        let dir = TempDir::new().unwrap();
        // bash 档自带 Unix 工具，直接放行（拦截只属于 PowerShell/cmd 回落档）。
        if matches!(
            crate::win_shell::resolve_windows_shell(None)
                .map(|resolved| resolved.dialect)
                .unwrap_or(ShellDialect::Cmd),
            ShellDialect::GitBash
        ) {
            return;
        }
        // 只在 PATH 上真的没有 grep 时才断言（MSYS 装了同名二进制不拦）。
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
        assert!(unix_only_rejection("grep -rn foo .", ShellDialect::PosixSh).is_none());
        // 即使模拟 Windows 档也不受影响（cfg!(windows) 为 false）。
        assert!(unix_only_rejection("grep -rn foo .", ShellDialect::Pwsh).is_none());
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
    #[tokio::test]
    async fn bash_tier_env_markers_visible_in_subprocess() {
        // MSYS_NO_PATHCONV=1 与 LANG=C.UTF-8 必须真实进入 bash 档子进程 env
        //（PRD R-SHELL-03 / M1-02.A1）。非 bash 档机器跳过（回落档无此治理）。
        let resolved = crate::win_shell::resolve_windows_shell(None).unwrap();
        if !matches!(resolved.dialect, ShellDialect::GitBash) {
            return;
        }
        let dir = TempDir::new().unwrap();
        let out = BashTool
            .execute(json_input("echo \"$MSYS_NO_PATHCONV:$LANG\"", dir.path()))
            .await
            .unwrap();
        assert!(
            out.contains("1:C.UTF-8"),
            "env markers missing, output was: {out}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn unix_only_rejection_is_dialect_gated() {
        // bash 档：Git Bash 自带 Unix 工具，一律放行（即使进程 PATH 上没有）。
        assert!(unix_only_rejection("grep -rn foo .", ShellDialect::GitBash).is_none());
        assert!(unix_only_rejection("sed -i s/a/b/ f.txt", ShellDialect::GitBash).is_none());
        // PowerShell/cmd 回落档：保持前置拦截（ack 在 Windows PATH 上不存在）。
        for dialect in [
            ShellDialect::Pwsh,
            ShellDialect::Powershell,
            ShellDialect::Cmd,
        ] {
            let rejection = unix_only_rejection("ack pattern", dialect)
                .expect("PS/cmd tier must intercept ack");
            assert!(
                rejection.contains("PowerShell"),
                "rejection was: {rejection}"
            );
            assert!(rejection.contains("search"), "rejection was: {rejection}");
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn synthesized_path_child_inherits_registry_path() {
        // R-ENV-01 端到端：子进程环境块里的 PATH 必须是 win_env 合成值。
        // 经 PowerShell 回落档做精确比对——pwsh 不重写继承的 PATH，`$env:PATH`
        // 原样回显 spawn 时注入的值（bash 档由 MSYS 根映射重写视角，无法精确
        // 往返，但注入走的是同一条 `cmd.env("PATH", …)` 代码路径）。
        let dir = TempDir::new().unwrap();
        let context = ToolExecutionContext {
            origin_request_key: None,
            task_id: "task-winenv".to_string(),
            run_id: "run-winenv".to_string(),
            tool_call_id: "call-winenv".to_string(),
            caller: None,
            access_mode: r_code_core::dto::ProjectAccessMode::FullAccess,
            // 空串 = 强制回落 PowerShell 链（本机 pwsh 7 或 powershell 5.1）。
            shell_override: Some(String::new()),
        };
        let input = serde_json::json!({
            "command": "$env:PATH",
            "cwd": dir.path().to_str().unwrap(),
        });
        let outcome = BashTool
            .execute_with_context_and_abort(input, &context, None)
            .await
            .expect("fallback tier must execute");
        let output = outcome.content;
        if !output.contains("--- stdout ---") {
            // 无任何 PowerShell 的极端机器（cmd 档）——cmd 不回显 PATH 同样格式，跳过。
            return;
        }
        let stdout = output
            .split("--- stdout ---")
            .nth(1)
            .and_then(|section| section.split("--- stderr ---").next())
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .replace("\r\n", "\n");
        let expected_binding = r_code_core::win_env::synthesized_path();
        let expected = expected_binding.to_string_lossy().to_ascii_lowercase();
        // pwsh 启动时会把自身安装目录前置到 PATH（一次性、非合成来源）——
        // 剥掉这一项后必须与合成值逐项一致。
        let split = |value: &str| {
            value
                .split(';')
                .filter(|entry| !entry.trim().is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let mut child_entries = split(&stdout);
        let expected_entries = split(&expected);
        if child_entries.first() != expected_entries.first() {
            child_entries.remove(0);
        }
        assert_eq!(
            child_entries, expected_entries,
            "child PATH env must equal the synthesized registry PATH entry-by-entry"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_tool_description_matches_dialect() {
        // bash 档描述必须声明 Git Bash 与 bash 语法；回落档描述必须声明 PowerShell
        //（PRD M1-02.A3）。
        let dialect = crate::win_shell::resolve_windows_shell(None)
            .map(|resolved| resolved.dialect)
            .unwrap_or(ShellDialect::Cmd);
        let description = BashTool.description();
        match dialect {
            ShellDialect::GitBash => {
                assert!(
                    description.contains("Git Bash"),
                    "description was: {description}"
                );
                assert!(description.contains("bash/POSIX syntax"));
            }
            _ => {
                assert!(
                    description.contains("PowerShell"),
                    "description was: {description}"
                );
            }
        }
        // 两份常量文案同时存在（回落机器上 bash 档文案由常量断言覆盖）。
        assert!(GIT_BASH_TIER_DESCRIPTION.contains("Git Bash"));
        assert!(POWERSHELL_FALLBACK_DESCRIPTION.contains("PowerShell"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_plan_matches_resolved_dialect() {
        // 计划方案必须与解析档一致：Git Bash 档 Inline `-c` 直传；PowerShell 档
        // 走 .ps1 暂存（否则引号会被 -Command 重解析拆坏）；cmd 档 Inline `/D /C`。
        let resolved = crate::win_shell::resolve_windows_shell(None).unwrap();
        let plan = plan_shell("echo hi", None).unwrap();
        assert_eq!(
            plan.dialect(),
            resolved.dialect,
            "plan dialect must match resolution"
        );
        match (&plan, resolved.dialect) {
            (ShellPlan::Inline { args, program, .. }, ShellDialect::GitBash) => {
                assert_eq!(args.first().map(String::as_str), Some("-c"));
                assert!(
                    program.to_ascii_lowercase().ends_with("bash.exe"),
                    "program was {program}"
                );
            }
            (ShellPlan::Script { .. }, ShellDialect::Pwsh | ShellDialect::Powershell) => {}
            (ShellPlan::Inline { args, program, .. }, ShellDialect::Cmd) => {
                assert_eq!(args.first().map(String::as_str), Some("/D"));
                assert_eq!(program, "cmd.exe");
            }
            (plan, dialect) => panic!("plan/dialect mismatch: {plan:?} vs {dialect:?}"),
        }
        plan.cleanup();
    }

    #[cfg(windows)]
    #[test]
    fn plan_shell_override_empty_forces_fallback() {
        // execution.bash_shell_path="" 表示强制回落：跳过 bash 各级，直接进
        // pwsh → powershell → cmd 链（PRD §4.5 空串语义）。
        let plan = plan_shell("echo hi", Some("")).unwrap();
        assert_ne!(
            plan.dialect(),
            ShellDialect::GitBash,
            "empty override must force fallback"
        );
        plan.cleanup();
    }

    #[cfg(windows)]
    #[test]
    fn plan_shell_override_missing_path_errors_without_fallback() {
        // 设置指向不存在的 bash：报错，绝不静默回落（否则用户以为在用指定 bash）。
        let error = plan_shell("echo hi", Some(r"X:\definitely\not\bash.exe"))
            .expect_err("missing override path must error");
        let message = error.to_string();
        assert!(
            message.contains("execution.bash_shell_path"),
            "message was: {message}"
        );
        assert!(
            message.contains("X:\\definitely\\not\\bash.exe"),
            "message was: {message}"
        );
    }
}

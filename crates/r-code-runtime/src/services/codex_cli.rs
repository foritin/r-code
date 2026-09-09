//! Codex CLI 探测与登录服务（自 `src-tauri/src/commands.rs` 原样搬移，供 GUI/TUI 共用）。
//!
//! 搬移范围：CLI 可用性探测、登录状态探测、`codex_integration_status` 集成状态与
//! 终端登录入口。依赖 CommandState 的安装/更新/MCP 写入流程仍在 r-code-host 的
//! `commands.rs`，并经由 `r_code_runtime::services::codex_cli` 复用本模块的探测核心。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(target_os = "macos")]
use core_foundation::url::CFURL;
#[cfg(target_os = "macos")]
use security_framework::os::macos::code_signing::{
    Flags as CodeSigningFlags, SecRequirement, SecStaticCode,
};
#[cfg(windows)]
use std::os::windows::process::CommandExt;

use serde::Deserialize;
use tokio::process::Command as TokioCommand;
use tokio::time::{timeout, Duration};

use r_code_core::process::{hide_background_console, kill_tree};

use crate::services::skills::SkillManager;

const CODEX_CLI_PROBE_TIMEOUT: Duration = Duration::from_secs(4);

const CODEX_CLI_INSTALL_COMMAND: &str = "npm install -g @openai/codex";

/// Codex CLI 的可用性。不要把 PATH 上一个同名文件的存在误认为 CLI 可运行：
/// Windows App 的受保护安装目录、陈旧 shim 和损坏的 npm 安装都会命中这种误判。
#[derive(Debug, Clone)]
pub struct CodexCliProbe {
    pub available: bool,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub source: Option<CodexCliSource>,
    pub error: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexCliSource {
    Path,
    NpmGlobal,
    MacosDesktopBundle,
}

impl CodexCliSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::NpmGlobal => "npm_global",
            Self::MacosDesktopBundle => "macos_desktop_bundle",
        }
    }

    const fn display_name(self) -> &'static str {
        match self {
            Self::Path => "系统 PATH",
            Self::NpmGlobal => "npm 全局安装",
            Self::MacosDesktopBundle => "Codex 桌面版内置",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexCliCandidate {
    path: PathBuf,
    source: CodexCliSource,
}

#[derive(Debug, Default)]
struct CodexCliProbeFailures {
    checked: usize,
    permission_denied: bool,
    timed_out: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexAuthState {
    Authenticated,
    NotAuthenticated,
    Unknown,
}

impl CodexAuthState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Authenticated => "authenticated",
            Self::NotAuthenticated => "not_authenticated",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexSetupState {
    InstallCli,
    Login,
    Check,
    Configure,
    Ready,
}

impl CodexSetupState {
    fn from_components(
        cli_available: bool,
        auth_state: CodexAuthState,
        skill_status: &str,
        mcp_server_configured: bool,
    ) -> Self {
        if !cli_available {
            Self::InstallCli
        } else if auth_state == CodexAuthState::NotAuthenticated {
            Self::Login
        } else if auth_state == CodexAuthState::Unknown {
            Self::Check
        } else if skill_status != "up_to_date" || !mcp_server_configured {
            Self::Configure
        } else {
            Self::Ready
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::InstallCli => "install_cli",
            Self::Login => "login",
            Self::Check => "check",
            Self::Configure => "configure",
            Self::Ready => "ready",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexAuthProbe {
    pub state: CodexAuthState,
    pub method: Option<&'static str>,
}

#[derive(Debug)]
pub enum CodexCommandError {
    Launch(std::io::ErrorKind),
    Timeout,
}

pub fn executable_paths(candidates: &[&str]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            push_executable_candidates(&mut found, &directory, candidates);
        }
    }
    found
}

fn push_executable_candidates(found: &mut Vec<PathBuf>, directory: &Path, names: &[&str]) {
    for name in names {
        let path = directory.join(name);
        if path.is_file() && !found.iter().any(|candidate| candidate == &path) {
            found.push(path);
        }
    }
}

fn push_codex_cli_candidates(
    found: &mut Vec<CodexCliCandidate>,
    paths: impl IntoIterator<Item = PathBuf>,
    source: CodexCliSource,
) {
    for path in paths {
        if !found.iter().any(|candidate| candidate.path == path) {
            found.push(CodexCliCandidate { path, source });
        }
    }
}

fn codex_cli_names() -> &'static [&'static str] {
    if cfg!(windows) {
        // npm 安装通常留下 .cmd shim；只找 exe 会把正常的 npm CLI 误判为未安装。
        &["codex.exe", "codex.cmd", "codex.bat", "codex"]
    } else {
        &["codex"]
    }
}

fn ordered_initial_codex_cli_candidates(
    user_npm_paths: Vec<PathBuf>,
    path_paths: Vec<PathBuf>,
) -> Vec<CodexCliCandidate> {
    let mut found = Vec::new();
    push_codex_cli_candidates(&mut found, user_npm_paths, CodexCliSource::NpmGlobal);
    push_codex_cli_candidates(&mut found, path_paths, CodexCliSource::Path);
    found
}

fn initial_codex_cli_candidates() -> Vec<CodexCliCandidate> {
    // 仅 Windows 需要向 user_npm_paths 追加用户级 npm 候选；其他平台没有
    // 追加点，mut 由 cfg 分支持有，避免非 Windows 上的 unused_mut。
    #[cfg(windows)]
    let mut user_npm_paths = Vec::new();
    #[cfg(not(windows))]
    let user_npm_paths = Vec::new();
    #[cfg(windows)]
    if let Some(app_data) = std::env::var_os("APPDATA") {
        // 打包应用可能把 WindowsApps / Codex Desktop 的 resources 目录放在用户 PATH
        // 前面。那套内置 CLI 能通过 `--version`，却不一定共享用户已登录的 npm CLI
        // 认证环境。先探测用户级 npm 安装，既避免误报“尚未登录”，也让后续运行、
        // 登录和更新始终落到同一份 CLI。
        let npm_prefix = Path::new(&app_data).join("npm");
        push_executable_candidates(&mut user_npm_paths, &npm_prefix, codex_cli_names());
    }
    ordered_initial_codex_cli_candidates(user_npm_paths, executable_paths(codex_cli_names()))
}

#[cfg(target_os = "macos")]
pub const OPENAI_CODEX_BUNDLE_IDENTIFIER: &str = "com.openai.codex";
#[cfg(target_os = "macos")]
const OPENAI_CODEX_TEAM_IDENTIFIER: &str = "2DC432GLL2";

#[cfg(target_os = "macos")]
pub fn validate_macos_codex_bundle_signature(app_bundle: &Path) -> Result<(), String> {
    let url = CFURL::from_path(app_bundle, true)
        .ok_or_else(|| "无法构造 Codex Desktop bundle URL".to_string())?;
    let code = SecStaticCode::from_path(&url, CodeSigningFlags::NONE)
        .map_err(|error| format!("无法读取 Codex Desktop 代码签名：{error}"))?;
    let requirement = format!(
        "anchor apple generic and identifier \"{OPENAI_CODEX_BUNDLE_IDENTIFIER}\" and certificate leaf[subject.OU] = \"{OPENAI_CODEX_TEAM_IDENTIFIER}\""
    )
    .parse::<SecRequirement>()
    .map_err(|error| format!("无法构造 Codex Desktop 签名要求：{error}"))?;
    let flags = CodeSigningFlags::CHECK_ALL_ARCHITECTURES
        | CodeSigningFlags::STRICT_VALIDATE
        | CodeSigningFlags::RESTRICT_SYMLINKS
        | CodeSigningFlags::NO_NETWORK_ACCESS;
    code.check_validity(flags, &requirement)
        .map_err(|error| format!("Codex Desktop 代码签名不受信任：{error}"))
}

#[cfg(target_os = "macos")]
pub fn verified_macos_codex_bundle_cli_with(
    app_bundle: &Path,
    signature_is_valid: impl FnOnce(&Path) -> bool,
) -> Option<PathBuf> {
    let app_bundle = app_bundle.canonicalize().ok()?;
    let info = plist::Value::from_file(app_bundle.join("Contents/Info.plist")).ok()?;
    let identifier = info
        .as_dictionary()?
        .get("CFBundleIdentifier")?
        .as_string()?;
    if identifier != OPENAI_CODEX_BUNDLE_IDENTIFIER {
        return None;
    }
    if !signature_is_valid(&app_bundle) {
        return None;
    }
    let cli = app_bundle.join("Contents/Resources/codex");
    cli.is_file().then_some(cli)
}

#[cfg(target_os = "macos")]
pub fn verified_macos_codex_bundle_cli(app_bundle: &Path) -> Option<PathBuf> {
    verified_macos_codex_bundle_cli_with(app_bundle, |bundle| {
        validate_macos_codex_bundle_signature(bundle).is_ok()
    })
}

#[cfg(target_os = "macos")]
fn macos_bundle_root_for_codex_cli(cli_path: &Path) -> Option<&Path> {
    if cli_path.file_name()? != "codex" {
        return None;
    }
    let resources = cli_path.parent()?;
    if resources.file_name()? != "Resources" {
        return None;
    }
    let contents = resources.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    let app_bundle = contents.parent()?;
    (app_bundle.extension().and_then(|value| value.to_str()) == Some("app")).then_some(app_bundle)
}

#[cfg(target_os = "macos")]
pub fn validate_macos_bundle_cli_before_launch(cli_path: &Path) -> Result<(), String> {
    let Some(app_bundle) = macos_bundle_root_for_codex_cli(cli_path) else {
        return Ok(());
    };
    validate_macos_codex_bundle_signature(app_bundle)
}

#[cfg(target_os = "macos")]
fn macos_codex_app_cli_paths_from_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    const APP_NAMES: &[&str] = &["Codex.app", "ChatGPT.app"];
    let mut found = Vec::new();
    for root in roots {
        for app_name in APP_NAMES {
            let Some(cli) = verified_macos_codex_bundle_cli(&root.join(app_name)) else {
                continue;
            };
            if !found.contains(&cli) {
                found.push(cli);
            }
        }
    }
    found
}

#[cfg(target_os = "macos")]
fn macos_codex_app_cli_paths() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join("Applications"));
    }
    macos_codex_app_cli_paths_from_roots(&roots)
}

#[cfg(not(target_os = "macos"))]
fn macos_codex_app_cli_paths() -> Vec<PathBuf> {
    Vec::new()
}

fn npm_cli_paths() -> Vec<PathBuf> {
    if cfg!(windows) {
        executable_paths(&["npm.exe", "npm.cmd", "npm.bat", "npm"])
    } else {
        executable_paths(&["npm"])
    }
}

fn codex_home_dir_from(configured: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    configured.unwrap_or_else(|| home.unwrap_or_else(|| PathBuf::from(".")).join(".codex"))
}

pub fn codex_home_dir() -> PathBuf {
    let configured = std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    codex_home_dir_from(configured, dirs::home_dir())
}

/// 只运行由本模块声明的固定 Codex 参数，绝不把 WebView 文字拼到 shell 命令中。
pub async fn run_codex_cli_at(
    cli_path: &Path,
    args: &[&str],
) -> Result<std::process::Output, CodexCommandError> {
    run_codex_cli_at_with_timeout(cli_path, args, CODEX_CLI_PROBE_TIMEOUT).await
}

pub async fn run_codex_cli_at_with_timeout(
    cli_path: &Path,
    args: &[&str],
    deadline: Duration,
) -> Result<std::process::Output, CodexCommandError> {
    #[cfg(target_os = "macos")]
    validate_macos_bundle_cli_before_launch(cli_path)
        .map_err(|_| CodexCommandError::Launch(std::io::ErrorKind::PermissionDenied))?;
    #[cfg(windows)]
    let mut command = if cli_path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
    }) {
        // npm 安装通常是 .cmd shim。命令路径来自 PATH 且先拒绝 cmd 元字符，参数只
        // 来自本模块字面量；这样既能绕开 Windows Store 别名，也不接受 WebView 文本。
        debug_assert!(args.iter().all(|arg| {
            arg.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        }));
        windows_cmd_safe_path(cli_path)
            .map_err(|_| CodexCommandError::Launch(std::io::ErrorKind::InvalidInput))?;
        let mut command = TokioCommand::new("cmd.exe");
        command
            .args(["/D", "/S", "/C", "call"])
            .arg(cli_path)
            .args(args);
        command
    } else {
        let mut command = TokioCommand::new(cli_path);
        command.args(args);
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = TokioCommand::new(cli_path);
        command.args(args);
        command
    };

    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    hide_background_console(command.as_std_mut());
    run_cli_output_with_deadline(command, deadline).await
}

/// 构造 npm 命令。调用方只能传入本模块声明的固定参数；WebView 不能提供包名、
/// registry、脚本或任意 shell 文本。
pub fn npm_command_at(npm_path: &Path, args: &[&str]) -> Result<TokioCommand, String> {
    #[cfg(windows)]
    if npm_path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
    }) {
        windows_cmd_safe_path(npm_path)?;
        let mut command = TokioCommand::new("cmd.exe");
        command
            .args(["/D", "/S", "/C", "call"])
            .arg(npm_path)
            .args(args);
        return Ok(command);
    }

    let mut command = TokioCommand::new(npm_path);
    command.args(args);
    Ok(command)
}

pub async fn run_npm_at(
    npm_path: &Path,
    args: &[&str],
    deadline: Duration,
) -> Result<std::process::Output, CodexCommandError> {
    let mut command = npm_command_at(npm_path, args)
        .map_err(|_| CodexCommandError::Launch(std::io::ErrorKind::InvalidInput))?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    hide_background_console(command.as_std_mut());
    run_cli_output_with_deadline(command, deadline).await
}

/// 一次性 CLI 的有界执行：不用 `output()`——超时 drop future 后 Child 随之 drop，
/// kill_on_drop 只杀 cmd.exe wrapper，npm 安装拉起的 node 后代会泄漏；改为
/// 保住 Child 句柄 + 并发 drain，超时走 kill_tree 树杀整棵后代树后收尸
///（r-code-core 唯一树杀实现，F-robust-03）。
async fn run_cli_output_with_deadline(
    mut command: TokioCommand,
    deadline: Duration,
) -> Result<std::process::Output, CodexCommandError> {
    let mut child = command
        .spawn()
        .map_err(|error| CodexCommandError::Launch(error.kind()))?;
    let stdout_task = child.stdout.take().map(drain_pipe_to_vec);
    let stderr_task = child.stderr.take().map(drain_pipe_to_vec);
    let status = match timeout(deadline, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => return Err(CodexCommandError::Launch(error.kind())),
        Err(_) => {
            kill_tree(&mut child).await;
            let _ = child.wait().await;
            return Err(CodexCommandError::Timeout);
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
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// 把子进程的一路管道读尽为 Vec<u8>（EOF/错误都正常收尾，防写满死锁）。
fn drain_pipe_to_vec<R>(mut pipe: R) -> tokio::task::JoinHandle<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;

        let mut buffer = Vec::new();
        let _ = pipe.read_to_end(&mut buffer).await;
        buffer
    })
}

#[cfg(test)]
mod cli_deadline_tests {
    use super::*;

    /// 超时分支必须保住 Child 句柄并树杀后及时返回（F-robust-03 钉子）：
    /// 旧实现 drop `output()` future 后只剩 kill_on_drop 单杀 wrapper。
    #[tokio::test]
    async fn one_shot_cli_timeout_kills_the_tree_and_returns_promptly() {
        let mut command = if cfg!(windows) {
            let mut command = TokioCommand::new("cmd.exe");
            command.args([
                "/D",
                "/S",
                "/C",
                "ping",
                "-n",
                "60",
                "-w",
                "1000",
                "127.0.0.1",
            ]);
            command
        } else {
            let mut command = TokioCommand::new("sh");
            command.arg("-c").arg("sleep 30");
            command
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_background_console(command.as_std_mut());
        let started = std::time::Instant::now();
        let result = run_cli_output_with_deadline(command, Duration::from_secs(1)).await;
        assert!(matches!(result, Err(CodexCommandError::Timeout)));
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "timeout branch must reap the tree promptly; took {:?}",
            started.elapsed()
        );
    }
}

pub fn first_nonempty_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(120).collect())
}

pub async fn probe_npm_cli() -> Option<PathBuf> {
    for path in npm_cli_paths() {
        if matches!(
            run_npm_at(&path, &["--version"], CODEX_CLI_PROBE_TIMEOUT).await,
            Ok(output) if output.status.success()
        ) {
            return Some(path);
        }
    }
    None
}

async fn npm_global_prefix(npm_path: &Path) -> Option<PathBuf> {
    let output = run_npm_at(npm_path, &["prefix", "-g"], CODEX_CLI_PROBE_TIMEOUT)
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let prefix = PathBuf::from(first_nonempty_line(&output.stdout)?);
    prefix.is_absolute().then_some(prefix)
}

async fn probe_new_codex_cli_candidates(
    candidates: &[CodexCliCandidate],
    failures: &mut CodexCliProbeFailures,
) -> Option<CodexCliProbe> {
    while let Some(candidate) = candidates.get(failures.checked) {
        failures.checked += 1;
        match run_codex_cli_at(&candidate.path, &["--version"]).await {
            Ok(output) if output.status.success() => {
                return Some(CodexCliProbe {
                    available: true,
                    path: Some(candidate.path.clone()),
                    version: first_nonempty_line(&output.stdout),
                    source: Some(candidate.source),
                    error: None,
                });
            }
            Ok(_) => {}
            Err(CodexCommandError::Launch(std::io::ErrorKind::PermissionDenied)) => {
                failures.permission_denied = true;
            }
            Err(CodexCommandError::Timeout) => failures.timed_out = true,
            Err(CodexCommandError::Launch(_)) => {}
        }
    }
    None
}

pub async fn probe_codex_cli() -> CodexCliProbe {
    let mut candidates = initial_codex_cli_candidates();
    let mut failures = CodexCliProbeFailures::default();
    // 先验证 PATH 和 Windows 默认 npm 目录中的 Codex。此前在验证任何 Codex
    // 候选前都会启动 `npm --version` 与 `npm prefix -g`，使每次冷探测平白多两个
    // cmd/Node 进程；只有直接候选全部失败时才需要 npm 的自定义全局 prefix。
    if let Some(probe) = probe_new_codex_cli_candidates(&candidates, &mut failures).await {
        return probe;
    }

    if let Some(npm_path) = probe_npm_cli().await {
        if let Some(prefix) = npm_global_prefix(&npm_path).await {
            let bin = if cfg!(windows) {
                prefix
            } else {
                prefix.join("bin")
            };
            let mut npm_candidates = Vec::new();
            push_executable_candidates(&mut npm_candidates, &bin, codex_cli_names());
            push_codex_cli_candidates(&mut candidates, npm_candidates, CodexCliSource::NpmGlobal);
        }
        if let Some(probe) = probe_new_codex_cli_candidates(&candidates, &mut failures).await {
            return probe;
        }
    }

    // 独立 CLI 始终优先。macOS 最后才检查 OpenAI 签名产品使用的固定 bundle id 与
    // 固定 Resources 相对路径；候选仍必须实际通过 `codex --version` 才算可用。
    push_codex_cli_candidates(
        &mut candidates,
        macos_codex_app_cli_paths(),
        CodexCliSource::MacosDesktopBundle,
    );
    if let Some(probe) = probe_new_codex_cli_candidates(&candidates, &mut failures).await {
        return probe;
    }

    let error = if candidates.is_empty() {
        "未检测到可运行的 Codex CLI。请安装 Codex CLI，macOS 也可安装包含内置 CLI 的 Codex 桌面版。"
    } else if failures.permission_denied {
        "检测到 Codex 命令，但当前用户没有执行权限。请检查文件权限或重新安装 Codex CLI。"
    } else if failures.timed_out {
        "Codex CLI 启动超时。请在系统终端运行 `codex doctor` 排查。"
    } else {
        "检测到 Codex 命令，但无法正常启动。请在系统终端运行 `codex --version` 排查。"
    };
    CodexCliProbe {
        available: false,
        path: None,
        version: None,
        source: None,
        error: Some(error),
    }
}

/// 解析 `codex login status` 的公开、人类可读状态。只归纳状态和登录方式，绝不把
/// stdout/stderr、账户名或凭据传回前端或写入日志。
fn parse_codex_login_status(success: bool, stdout: &[u8], stderr: &[u8]) -> CodexAuthProbe {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    )
    .to_ascii_lowercase();
    let method = if text.contains("chatgpt") {
        Some("ChatGPT")
    } else if text.contains("api key") || text.contains("api-key") {
        Some("API Key")
    } else if text.contains("access token") {
        Some("访问令牌")
    } else {
        None
    };
    let explicitly_signed_out = [
        "not logged in",
        "not authenticated",
        "no active authentication",
        "no active login",
        "signed out",
    ]
    .iter()
    .any(|needle| text.contains(needle));
    let state = if explicitly_signed_out {
        CodexAuthState::NotAuthenticated
    } else if success {
        // `codex login status` 的公开契约是：存在有效登录时退出码为 0。
        // 输出文本用于识别认证方式，不应反过来把成功退出误判为 unknown。
        CodexAuthState::Authenticated
    } else {
        CodexAuthState::Unknown
    };
    CodexAuthProbe { state, method }
}

pub async fn probe_codex_login(cli_path: Option<&Path>) -> CodexAuthProbe {
    let Some(cli_path) = cli_path else {
        return CodexAuthProbe {
            state: CodexAuthState::Unknown,
            method: None,
        };
    };
    match run_codex_cli_at(cli_path, &["login", "status"]).await {
        Ok(output) => {
            parse_codex_login_status(output.status.success(), &output.stdout, &output.stderr)
        }
        Err(_) => CodexAuthProbe {
            state: CodexAuthState::Unknown,
            method: None,
        },
    }
}

/// 返回 Codex CLI 协作入口的状态。它不会读取、修改或回传认证令牌，也不把
/// `auth.json` 是否存在当作登录结论，因为 Codex 可将凭据保存到系统密钥库。
pub async fn codex_integration_status() -> Result<serde_json::Value, String> {
    let manager = SkillManager::new();
    let skill_path = manager.codex_install_path();
    let skill_status = match std::fs::read(&skill_path) {
        Ok(contents) if contents == SkillManager::skill_content().as_bytes() => "up_to_date",
        Ok(_) => "update_available",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "not_installed",
        Err(error) => return Err(format!("读取 Codex Skill 状态失败：{error}")),
    };
    let codex_dir = codex_home_dir();
    let config_path = codex_dir.join("config.toml");
    let auth_path = codex_dir.join("auth.json");
    let cli = probe_codex_cli().await;
    let npm_path = if cli.available {
        None
    } else {
        probe_npm_cli().await
    };
    let (auth, mcp_server_configured) = if cli.available {
        tokio::join!(
            probe_codex_login(cli.path.as_deref()),
            codex_mcp_server_configured(cli.path.as_deref())
        )
    } else {
        (
            CodexAuthProbe {
                state: CodexAuthState::Unknown,
                method: None,
            },
            false,
        )
    };
    let setup_state = CodexSetupState::from_components(
        cli.available,
        auth.state,
        skill_status,
        mcp_server_configured,
    );
    Ok(serde_json::json!({
        "cli_available": cli.available,
        "cli_path": cli.path,
        "cli_version": cli.version,
        "cli_source": cli.source.map(CodexCliSource::as_str),
        "cli_source_label": cli.source.map(CodexCliSource::display_name),
        "cli_error": cli.error,
        "installer_available": npm_path.is_some(),
        "installer_command": CODEX_CLI_INSTALL_COMMAND,
        "installer_error": if npm_path.is_some() {
            None
        } else {
            Some("未检测到 npm。请先安装 Node.js，或在系统终端手动安装 Codex CLI。")
        },
        "config_path": config_path,
        "config_exists": config_path.exists(),
        "auth_path": auth_path,
        "authenticated": auth.state == CodexAuthState::Authenticated,
        "auth_status": auth.state.as_str(),
        "auth_method": auth.method,
        "skill_path": skill_path,
        "skill_status": skill_status,
        "mcp_server_configured": mcp_server_configured,
        "mcp_server_name": "r-code",
        "integration_ready": setup_state == CodexSetupState::Ready,
        "setup_state": setup_state.as_str(),
        "wire_api": "responses",
    }))
}

pub const CODEX_MCP_CONFIG_TIMEOUT: Duration = Duration::from_secs(12);
const CODEX_MCP_GET_MAX_BYTES: usize = 64 * 1024;
pub const CODEX_MCP_SERVER_NAME: &str = "r-code";
pub const CODEX_MCP_HOST_DIR: &str = "mcp-host";
pub const CODEX_MCP_HOST_PREFIX: &str = "r-code-mcp-host-";

#[derive(Debug, Deserialize)]
struct CodexMcpRegistrationWire {
    name: String,
    enabled: bool,
    transport: CodexMcpTransportWire,
}

#[derive(Debug, Deserialize)]
struct CodexMcpTransportWire {
    #[serde(rename = "type")]
    transport_type: String,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexMcpRegistration {
    pub enabled: bool,
    pub command: PathBuf,
    pub args: Vec<String>,
}

impl CodexMcpRegistration {
    pub fn data_dir(&self) -> Option<PathBuf> {
        match self.args.as_slice() {
            [subcommand, flag, data_dir] if subcommand == "mcp-server" && flag == "--data-dir" => {
                Some(PathBuf::from(data_dir))
            }
            _ => None,
        }
    }

    pub fn is_managed_for(&self, data_dir: &Path) -> bool {
        let Some(file_name) = self.command.file_name().and_then(|value| value.to_str()) else {
            return false;
        };
        let Some(parent) = self.command.parent() else {
            return false;
        };
        file_name.starts_with(CODEX_MCP_HOST_PREFIX)
            && paths_equal(parent, &data_dir.join(CODEX_MCP_HOST_DIR))
    }
}

pub fn paths_equal(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        left.to_string_lossy()
            .replace('/', "\\")
            .eq_ignore_ascii_case(&right.to_string_lossy().replace('/', "\\"))
    } else {
        left == right
    }
}

pub fn parse_codex_mcp_registration(stdout: &[u8]) -> Result<CodexMcpRegistration, String> {
    if stdout.len() > CODEX_MCP_GET_MAX_BYTES {
        return Err("Codex MCP 配置响应过大，已停止读取。".to_string());
    }
    let wire: CodexMcpRegistrationWire =
        serde_json::from_slice(stdout).map_err(|_| "Codex MCP 配置格式无法识别。".to_string())?;
    if wire.name != CODEX_MCP_SERVER_NAME || wire.transport.transport_type != "stdio" {
        return Err("Codex 中的 r-code 条目不是受支持的本地 MCP 配置。".to_string());
    }
    let command = wire
        .transport
        .command
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "Codex MCP 配置缺少启动命令。".to_string())?;
    Ok(CodexMcpRegistration {
        enabled: wire.enabled,
        command,
        args: wire.transport.args,
    })
}

pub async fn codex_mcp_registration(
    cli_path: &Path,
) -> Result<Option<CodexMcpRegistration>, String> {
    let output = run_codex_cli_at_with_timeout(
        cli_path,
        &["mcp", "get", CODEX_MCP_SERVER_NAME, "--json"],
        CODEX_MCP_CONFIG_TIMEOUT,
    )
    .await
    .map_err(|error| match error {
        CodexCommandError::Timeout => "读取 Codex MCP 配置超时。".to_string(),
        CodexCommandError::Launch(_) => "无法启动 Codex CLI 读取 MCP 配置。".to_string(),
    })?;
    if !output.status.success() {
        return Ok(None);
    }
    parse_codex_mcp_registration(&output.stdout).map(Some)
}

pub fn file_fingerprint(path: &Path) -> Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|error| format!("读取 R-Code MCP 主机失败：{error}"))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("读取 R-Code MCP 主机失败：{error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub fn managed_codex_mcp_file_name(source: &Path, fingerprint: &str) -> String {
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    format!("{CODEX_MCP_HOST_PREFIX}{fingerprint}{extension}")
}

pub fn registration_uses_current_host(
    registration: &CodexMcpRegistration,
    current_executable: &Path,
) -> bool {
    let Some(data_dir) = registration.data_dir() else {
        return false;
    };
    if !registration.enabled
        || !registration.is_managed_for(&data_dir)
        || !registration.command.is_file()
    {
        return false;
    }
    let Ok(fingerprint) = file_fingerprint(current_executable) else {
        return false;
    };
    let expected = managed_codex_mcp_file_name(current_executable, &fingerprint);
    if registration
        .command
        .file_name()
        .and_then(|value| value.to_str())
        == Some(expected.as_str())
    {
        return true;
    }
    file_fingerprint(&registration.command).is_ok_and(|configured| configured == fingerprint)
}

/// 只有内容与当前 R-Code 构建一致、且位于应用数据目录中的独立副本才算就绪。
/// 旧版直接指向 `target/debug/r-code-host.exe` 的配置会被判定为待迁移，避免 Codex
/// 长驻进程锁住 Cargo 下一次需要覆盖的热编译产物。
pub async fn codex_mcp_server_configured(cli_path: Option<&Path>) -> bool {
    let Some(cli_path) = cli_path else {
        return false;
    };
    let Ok(Some(registration)) = codex_mcp_registration(cli_path).await else {
        return false;
    };
    let Ok(current_executable) = std::env::current_exe() else {
        return false;
    };
    registration_uses_current_host(&registration, &current_executable)
}

#[cfg(windows)]
pub fn windows_cmd_safe_path(path: &Path) -> Result<String, String> {
    let text = path
        .to_str()
        .ok_or_else(|| "命令路径不是有效的 Unicode 文本。".to_string())?;
    if text.chars().any(|character| {
        matches!(
            character,
            '\0' | '\r' | '\n' | '"' | '&' | '|' | '<' | '>' | '^' | '%' | '!'
        )
    }) {
        return Err("命令路径包含 cmd 不支持的字符；请改用不含特殊字符的安装目录。".to_string());
    }
    Ok(format!("\"{text}\""))
}

#[derive(Debug, Clone, Copy)]
pub enum CodexLoginMode {
    Browser,
    DeviceCode,
}

impl CodexLoginMode {
    pub fn args(self) -> &'static [&'static str] {
        match self {
            Self::Browser => &["login"],
            Self::DeviceCode => &["login", "--device-auth"],
        }
    }
}

#[cfg(windows)]
pub fn configure_windows_codex_login_command(
    command: &mut Command,
    executable: &Path,
    mode: CodexLoginMode,
) -> Result<(), String> {
    // `cmd.exe` does not use CommandLineToArgvW parsing. Passing one script argument that contains
    // a quoted `.cmd` path makes Rust escape the inner quotes as `\"`; cmd then treats those
    // backslashes as literal characters and never starts Codex. Keep every command token separate
    // so the standard Windows process builder quotes only the executable path when needed.
    windows_cmd_safe_path(executable)?;
    command
        .args(["/D", "/S", "/C", "call"])
        .arg(executable)
        .args(mode.args())
        .args([
            "&",
            "if",
            "errorlevel",
            "1",
            "(",
            "echo.",
            "&",
            "echo",
            "Codex",
            "login",
            "did",
            "not",
            "complete.",
            "&",
            "echo",
            "This",
            "window",
            "stays",
            "open",
            "for",
            "diagnostics.",
            "Press",
            "any",
            "key",
            "to",
            "close",
            "it.",
            "&",
            "pause",
            ")",
        ]);
    Ok(())
}

#[cfg(any(test, target_os = "macos"))]
fn posix_shell_quote(value: &str) -> Result<String, String> {
    if value
        .chars()
        .any(|character| matches!(character, '\0' | '\r' | '\n'))
    {
        return Err("命令路径包含终端脚本不支持的控制字符。".to_string());
    }
    Ok(format!("'{}'", value.replace('\'', "'\"'\"'")))
}

#[cfg(any(test, target_os = "macos"))]
fn macos_codex_login_shell_script(
    executable: &Path,
    mode: CodexLoginMode,
) -> Result<String, String> {
    let executable_text = executable
        .to_str()
        .ok_or_else(|| "Codex 命令路径不是有效的 Unicode 文本。".to_string())?;
    let quoted_executable = posix_shell_quote(executable_text)?;
    let path_prefix = Path::new(executable_text)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .and_then(Path::to_str)
        .map(posix_shell_quote)
        .transpose()?
        .map(|directory| format!("PATH={directory}:\"$PATH\"; export PATH; "))
        .unwrap_or_default();
    let arguments = mode.args().join(" ");
    Ok(format!(
        "{path_prefix}{quoted_executable} {arguments}; status=$?; \
if [ \"$status\" -eq 0 ]; then exit 0; fi; \
printf '\\nCodex login did not complete (exit %s).\\nThis window stays open for diagnostics. Press Return to close it.\\n' \"$status\"; \
IFS= read -r _; exit \"$status\""
    ))
}

#[cfg(target_os = "macos")]
pub fn create_macos_codex_login_command_file(
    executable: &Path,
    mode: CodexLoginMode,
) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;

    let command_path = std::env::temp_dir().join(format!(
        "r-code-codex-login-{}.command",
        uuid::Uuid::new_v4()
    ));
    let shell = macos_codex_login_shell_script(executable, mode)?;
    let source = format!("#!/bin/sh\n/bin/rm -f \"$0\"\n{shell}\n");
    std::fs::write(&command_path, source)
        .map_err(|_| "无法创建临时 Codex 登录脚本。".to_string())?;
    if let Err(error) =
        std::fs::set_permissions(&command_path, std::fs::Permissions::from_mode(0o700))
    {
        let _ = std::fs::remove_file(&command_path);
        return Err(format!("无法授权临时 Codex 登录脚本：{error}"));
    }
    Ok(command_path)
}

/// 在用户可见的系统终端中启动 Codex 登录。它不接收任何来自 WebView 的命令文本，
/// 也不读取登录输出或 auth.json；OAuth 交互完全由 Codex CLI 处理。成功后终端会话
/// 干净退出（窗口是否关闭由系统终端偏好决定），失败时保留诊断输出等待用户关闭。
async fn codex_start_login_with_mode(mode: CodexLoginMode) -> Result<(), String> {
    let cli = probe_codex_cli().await;
    if !cli.available {
        return Err(cli
            .error
            .unwrap_or("未检测到可运行的 Codex CLI。")
            .to_string());
    }

    // The settings view can hold a stale signed-out snapshot while another terminal or Codex
    // Desktop has already completed ChatGPT authentication. Re-read the exact selected CLI before
    // opening OAuth; an existing login is already shared through Codex's normal credential store.
    if probe_codex_login(cli.path.as_deref()).await.state == CodexAuthState::Authenticated {
        return Ok(());
    }

    #[cfg(windows)]
    {
        let executable = cli.path.unwrap_or_else(|| PathBuf::from("codex"));
        // R-Code 是 GUI 进程；新控制台确保设备码和 OAuth 提示始终对用户可见。
        // `/C` 在成功时自然退出，参数化的失败分支只在 Codex 非零退出时执行 `pause`。
        let mut command = Command::new("cmd.exe");
        configure_windows_codex_login_command(&mut command, &executable, mode)?;
        command.creation_flags(0x0000_0010); // CREATE_NEW_CONSOLE
        command
            .spawn()
            .map_err(|_| "无法启动 Codex 登录终端。请在系统终端运行 `codex login`。".to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        let executable = cli.path.unwrap_or_else(|| PathBuf::from("codex"));
        validate_macos_bundle_cli_before_launch(&executable)?;
        let command_path = create_macos_codex_login_command_file(&executable, mode)?;
        // 通过 Launch Services 打开 `.command`，不申请控制 Terminal 的 Apple Events
        // 权限；脚本启动后会立即自删，成功时 shell 干净退出，失败时保留诊断输出。
        if Command::new("/usr/bin/open")
            .args(["-a", "Terminal"])
            .arg(&command_path)
            .spawn()
            .is_err()
        {
            let _ = std::fs::remove_file(command_path);
            return Err(
                "无法启动 macOS Terminal 登录窗口。请在系统终端运行 `codex login`。".to_string(),
            );
        }
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let executable = cli.path.unwrap_or_else(|| PathBuf::from("codex"));
        let mut command = Command::new(executable);
        command.args(mode.args());
        command
            .spawn()
            .map_err(|_| "无法启动 Codex 登录。请在系统终端运行 `codex login`。".to_string())?;
    }
    Ok(())
}

pub async fn codex_start_login() -> Result<(), String> {
    codex_start_login_with_mode(CodexLoginMode::Browser).await
}

/// 适合远程桌面、无浏览器回调或 localhost callback 被拦截的设备码登录。
pub async fn codex_start_device_login() -> Result<(), String> {
    codex_start_login_with_mode(CodexLoginMode::DeviceCode).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use tempfile::TempDir;

    #[test]
    fn codex_login_status_uses_official_exit_code_contract() {
        let status = parse_codex_login_status(true, b"Logged in using ChatGPT\n", b"");
        assert_eq!(status.state, CodexAuthState::Authenticated);
        assert_eq!(status.method, Some("ChatGPT"));

        let status = parse_codex_login_status(false, b"", b"Not logged in");
        assert_eq!(status.state, CodexAuthState::NotAuthenticated);
        assert_eq!(status.method, None);

        let status = parse_codex_login_status(true, b"status unavailable", b"");
        assert_eq!(status.state, CodexAuthState::Authenticated);
        assert_eq!(status.method, None);

        let status = parse_codex_login_status(false, b"", b"temporary transport error");
        assert_eq!(status.state, CodexAuthState::Unknown);
    }

    #[test]
    fn codex_setup_state_follows_prerequisite_order() {
        assert_eq!(
            CodexSetupState::from_components(
                false,
                CodexAuthState::Unknown,
                "not_installed",
                false,
            ),
            CodexSetupState::InstallCli
        );
        assert_eq!(
            CodexSetupState::from_components(
                true,
                CodexAuthState::NotAuthenticated,
                "up_to_date",
                true,
            ),
            CodexSetupState::Login
        );
        assert_eq!(
            CodexSetupState::from_components(true, CodexAuthState::Unknown, "up_to_date", true,),
            CodexSetupState::Check
        );
        assert_eq!(
            CodexSetupState::from_components(
                true,
                CodexAuthState::Authenticated,
                "update_available",
                true,
            ),
            CodexSetupState::Configure
        );
        assert_eq!(
            CodexSetupState::from_components(
                true,
                CodexAuthState::Authenticated,
                "up_to_date",
                true,
            ),
            CodexSetupState::Ready
        );
    }

    #[test]
    fn codex_login_status_detects_supported_auth_methods() {
        let api = parse_codex_login_status(true, b"Authenticated with API Key", b"");
        assert_eq!(api.state, CodexAuthState::Authenticated);
        assert_eq!(api.method, Some("API Key"));

        let token = parse_codex_login_status(true, b"Signed in with access token", b"");
        assert_eq!(token.state, CodexAuthState::Authenticated);
        assert_eq!(token.method, Some("访问令牌"));
    }

    #[test]
    fn codex_home_prefers_explicit_codex_home() {
        let home = codex_home_dir_from(
            Some(PathBuf::from("D:/isolated/codex-home")),
            Some(PathBuf::from("C:/Users/example")),
        );
        assert_eq!(home, PathBuf::from("D:/isolated/codex-home"));

        let default = codex_home_dir_from(None, Some(PathBuf::from("C:/Users/example")));
        assert_eq!(default, PathBuf::from("C:/Users/example/.codex"));
    }

    #[test]
    fn codex_candidate_order_prefers_the_user_npm_auth_environment() {
        let npm = PathBuf::from("C:/Users/example/AppData/Roaming/npm/codex.cmd");
        let bundled = PathBuf::from("C:/Program Files/WindowsApps/OpenAI.Codex/codex.exe");
        let candidates = ordered_initial_codex_cli_candidates(
            vec![npm.clone()],
            vec![bundled.clone(), npm.clone()],
        );

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].path, npm);
        assert_eq!(candidates[0].source, CodexCliSource::NpmGlobal);
        assert_eq!(candidates[1].path, bundled);
        assert_eq!(candidates[1].source, CodexCliSource::Path);
    }

    #[cfg(unix)]
    fn codex_probe_test_shim(directory: &Path, name: &str, version: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = directory.join(name);
        std::fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{version}'\n")).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_probe_accepts_desktop_only_candidate() {
        let directory = TempDir::new().unwrap();
        let desktop = codex_probe_test_shim(directory.path(), "desktop-codex", "desktop 1.0");
        let candidates = vec![CodexCliCandidate {
            path: desktop.clone(),
            source: CodexCliSource::MacosDesktopBundle,
        }];
        let mut failures = CodexCliProbeFailures::default();

        let probe = probe_new_codex_cli_candidates(&candidates, &mut failures)
            .await
            .unwrap();
        assert_eq!(probe.path.as_deref(), Some(desktop.as_path()));
        assert_eq!(probe.source, Some(CodexCliSource::MacosDesktopBundle));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_probe_accepts_cli_only_candidate() {
        let directory = TempDir::new().unwrap();
        let cli = codex_probe_test_shim(directory.path(), "path-codex", "path 1.0");
        let candidates = vec![CodexCliCandidate {
            path: cli.clone(),
            source: CodexCliSource::Path,
        }];
        let mut failures = CodexCliProbeFailures::default();

        let probe = probe_new_codex_cli_candidates(&candidates, &mut failures)
            .await
            .unwrap();
        assert_eq!(probe.path.as_deref(), Some(cli.as_path()));
        assert_eq!(probe.source, Some(CodexCliSource::Path));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_probe_prefers_cli_over_desktop_when_both_are_available() {
        let directory = TempDir::new().unwrap();
        let cli = codex_probe_test_shim(directory.path(), "path-codex", "path 1.0");
        let desktop = codex_probe_test_shim(directory.path(), "desktop-codex", "desktop 1.0");
        let candidates = vec![
            CodexCliCandidate {
                path: cli.clone(),
                source: CodexCliSource::Path,
            },
            CodexCliCandidate {
                path: desktop,
                source: CodexCliSource::MacosDesktopBundle,
            },
        ];
        let mut failures = CodexCliProbeFailures::default();

        let probe = probe_new_codex_cli_candidates(&candidates, &mut failures)
            .await
            .unwrap();
        assert_eq!(probe.path.as_deref(), Some(cli.as_path()));
        assert_eq!(probe.source, Some(CodexCliSource::Path));
    }

    #[test]
    fn codex_login_modes_are_fixed_commands() {
        assert_eq!(CodexLoginMode::Browser.args(), ["login"]);
        assert_eq!(
            CodexLoginMode::DeviceCode.args(),
            ["login", "--device-auth"]
        );
    }

    #[test]
    fn macos_login_terminal_uses_fixed_arguments_and_quotes_executable_paths() {
        let executable = Path::new("/Applications/Codex Tool's/bin/codex");
        let browser = macos_codex_login_shell_script(executable, CodexLoginMode::Browser).unwrap();
        let device =
            macos_codex_login_shell_script(executable, CodexLoginMode::DeviceCode).unwrap();

        assert!(browser.contains("PATH='/Applications/Codex Tool'\"'\"'s/bin':\"$PATH\""));
        assert!(browser.contains("'/Applications/Codex Tool'\"'\"'s/bin/codex' login"));
        assert!(!browser.contains("--device-auth"));
        assert!(device.contains("login --device-auth"));
        assert!(device.contains("if [ \"$status\" -eq 0 ]; then exit 0; fi"));
        assert!(device.contains("IFS= read -r _"));
    }

    #[test]
    fn macos_login_terminal_rejects_control_characters_in_paths() {
        assert!(macos_codex_login_shell_script(
            Path::new("/tmp/codex\nmalicious"),
            CodexLoginMode::Browser,
        )
        .is_err());
    }
}

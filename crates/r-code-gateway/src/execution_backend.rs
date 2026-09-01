//! CommandExecutionBackend trait（docs/pi-alignment PRD §4.1 R-SBX-01 / M7-01）。
//!
//! 命令执行后端抽象面：spawn / kill_tree / 输出收集。默认实现
//! [`LocalShellBackend`] = 现有五级 shell 链（`plan_shell` 解析 + tokio
//! spawn + `kill_tree` 进程组终止），未启用其他后端时**零行为变化**——
//! 既有 BashTool 路径不经过本抽象，本 trait 为 M7-02（DockerBackend）等
//! 可插拔后端提供与宿主审批链（R0-R4 + PathGuard）对接的统一形状。
//!
//! 注意安全边界：backend 只负责"怎么执行"，"能不能执行"永远留在
//! ToolGateway 审批矩阵——任何后端实现都不得绕过调用方的审批前置。

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use async_trait::async_trait;
use r_code_core::error::ProductError;

/// 一次命令执行的规格（backend 无权解释语义——只是执行载体）。
#[derive(Debug, Clone)]
pub struct CommandSpec {
    /// shell 命令行（经审批后的原文）。
    pub command: String,
    /// 工作目录（审批链已验证不越界）。
    pub cwd: std::path::PathBuf,
    /// 超时上限。
    pub timeout: Duration,
}

/// 运行中的命令句柄。
pub struct CommandHandle {
    pub child: tokio::process::Child,
    /// 平台清理物（如 PowerShell 暂存脚本）；kill/收尾时释放。
    pub cleanup: Option<Box<dyn FnOnce() + Send>>,
}

impl CommandHandle {
    /// 终止整个进程树（进程组语义；对齐 r_code_core::process::kill_tree）。
    pub async fn kill_tree(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
        r_code_core::process::kill_tree(&mut self.child).await;
    }
}

/// 收集完成后的输出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// 命令执行后端。
#[async_trait]
pub trait CommandExecutionBackend: Send + Sync {
    /// 后端标识（审计/设置页展示）。
    fn backend_id(&self) -> &'static str;

    /// 启动命令（不等待完成；审批已由调用方完成）。
    async fn spawn(
        &self,
        spec: &CommandSpec,
        abort_flag: Option<&AtomicBool>,
    ) -> Result<CommandHandle, ProductError>;

    /// 收集输出直到退出/超时/中止；超时与中止时先 kill_tree 再收尾。
    async fn collect(
        &self,
        handle: CommandHandle,
        spec: &CommandSpec,
        abort_flag: Option<&AtomicBool>,
    ) -> Result<CollectedOutput, ProductError>;
}

/// 默认后端：现有五级 shell 链的包装（plan_shell → tokio Command →
/// kill_on_drop + kill_tree）。与 BashTool::execute 同一解析与 spawn 语义；
/// 抽象出来仅为可插拔，不改变默认路径行为。
pub struct LocalShellBackend {
    /// shell 覆盖（设置页 shell 路径覆盖；None = 五级解析）。
    shell_override: Option<String>,
}

impl LocalShellBackend {
    pub fn new() -> Self {
        Self {
            shell_override: None,
        }
    }

    pub fn with_shell_override(shell_override: Option<String>) -> Self {
        Self { shell_override }
    }
}

impl Default for LocalShellBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CommandExecutionBackend for LocalShellBackend {
    fn backend_id(&self) -> &'static str {
        "local"
    }

    async fn spawn(
        &self,
        spec: &CommandSpec,
        _abort_flag: Option<&AtomicBool>,
    ) -> Result<CommandHandle, ProductError> {
        use std::process::Stdio;
        use tokio::process::Command;

        let plan = crate::tools_command::plan_shell(&spec.command, self.shell_override.as_deref())?;
        let mut cmd = Command::new(plan.program());
        match &plan {
            crate::tools_command::ShellPlan::Inline { args, .. } => {
                cmd.args(args);
            }
            crate::tools_command::ShellPlan::Script {
                leading,
                script_path,
                ..
            } => {
                cmd.args(leading).arg(script_path);
            }
        }
        cmd.current_dir(&spec.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.env("PATH", r_code_core::win_env::synthesized_path());
        #[cfg(unix)]
        cmd.as_std_mut().process_group(0);
        let child = cmd.spawn().map_err(|error| {
            ProductError::Other(format!("failed to spawn {}: {error}", plan.program()))
        })?;
        Ok(CommandHandle {
            child,
            cleanup: Some(Box::new(move || plan.cleanup())),
        })
    }

    async fn collect(
        &self,
        mut handle: CommandHandle,
        spec: &CommandSpec,
        abort_flag: Option<&AtomicBool>,
    ) -> Result<CollectedOutput, ProductError> {
        let timeout = tokio::time::timeout(spec.timeout, async {
            loop {
                if handle.child.try_wait().ok().flatten().is_some() {
                    return;
                }
                if abort_flag.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
                    handle.kill_tree().await;
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await;
        // 超时：终止进程树再等待退出。
        if timeout.is_err() {
            handle.kill_tree().await;
        }
        let output = handle
            .child
            .wait_with_output()
            .await
            .map_err(|error| ProductError::Other(format!("collect output: {error}")))?;
        if let Some(cleanup) = handle.cleanup.take() {
            cleanup();
        }
        Ok(CollectedOutput {
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Docker 容器后端（PRD §4.1 R-SBX-02 / M7-02）：命令在 `docker run` 容器内
/// 执行，工作区只读挂载。**审批/风险分级/审计全部留在 Host 侧 ToolGateway**
/// ——本 backend 只替换执行载体，与 LocalShellBackend 实现同一 trait 面。
/// 默认不启用（execution.container 未设置时零行为变化）。
pub struct DockerBackend {
    /// 容器镜像（如 "alpine:3"）。
    pub image: String,
}

impl DockerBackend {
    pub fn new(image: impl Into<String>) -> Self {
        Self {
            image: image.into(),
        }
    }

    /// docker run 参数模板：镜像 + 只读工作区挂载 + 内存/CPU 限额（防资源
    /// 失控）+ 网络 none（沙箱临时目录无网络，PRD R-EVL-04 同一原则）。
    fn base_args(&self, cwd: &Path) -> Vec<String> {
        vec![
            "run".to_string(),
            "--rm".to_string(),
            "--network".to_string(),
            "none".to_string(),
            "--memory".to_string(),
            "512m".to_string(),
            "--cpus".to_string(),
            "1".to_string(),
            "-v".to_string(),
            format!("{}:/workspace:ro", cwd.to_string_lossy()),
            "-w".to_string(),
            "/workspace".to_string(),
        ]
    }
}

#[async_trait]
impl CommandExecutionBackend for DockerBackend {
    fn backend_id(&self) -> &'static str {
        "docker"
    }

    async fn spawn(
        &self,
        spec: &CommandSpec,
        _abort_flag: Option<&AtomicBool>,
    ) -> Result<CommandHandle, ProductError> {
        use std::process::Stdio;
        use tokio::process::Command;
        // 容器内经 sh -c 执行（镜像内解释器由 docker 提供；Host 侧五级解析
        // 不适用于容器）。
        let mut cmd = Command::new("docker");
        cmd.args(self.base_args(&spec.cwd))
            .arg(&self.image)
            .arg("sh")
            .arg("-c")
            .arg(&spec.command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = cmd
            .spawn()
            .map_err(|error| ProductError::Other(format!("docker spawn: {error}")))?;
        Ok(CommandHandle {
            child,
            cleanup: None,
        })
    }

    async fn collect(
        &self,
        handle: CommandHandle,
        spec: &CommandSpec,
        abort_flag: Option<&AtomicBool>,
    ) -> Result<CollectedOutput, ProductError> {
        // 与 LocalShellBackend 同一收尾语义（超时/中止 kill_tree）。
        LocalShellBackend::new()
            .collect(handle, spec, abort_flag)
            .await
    }
}

/// 工作区存在性预检（backend 契约的一部分：cwd 必须已存在，spawn 前置）。
pub fn ensure_cwd_exists(cwd: &Path) -> Result<(), ProductError> {
    if cwd.is_dir() {
        Ok(())
    } else {
        Err(ProductError::Other(format!(
            "cwd does not exist: {}",
            cwd.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M7-01.A1：trait 抽象面完整 + 默认实现 = 五级链（backend_id/命令执行）。
    #[tokio::test]
    async fn local_backend_runs_through_five_tier_chain() {
        let backend: Box<dyn CommandExecutionBackend> = Box::new(LocalShellBackend::new());
        assert_eq!(backend.backend_id(), "local");
        let cwd = tempfile::tempdir().unwrap();
        let spec = CommandSpec {
            command: "echo hello-backend".to_string(),
            cwd: cwd.path().to_path_buf(),
            timeout: Duration::from_secs(30),
        };
        ensure_cwd_exists(&spec.cwd).unwrap();
        let handle = backend.spawn(&spec, None).await.unwrap();
        let output = backend.collect(handle, &spec, None).await.unwrap();
        assert_eq!(output.exit_code, Some(0));
        assert!(
            output.stdout.contains("hello-backend"),
            "stdout={}",
            output.stdout
        );
    }

    /// 超时路径：kill_tree 生效（命令被终止、不悬挂）。
    #[tokio::test]
    async fn timeout_kills_process_tree() {
        let backend = LocalShellBackend::new();
        let cwd = tempfile::tempdir().unwrap();
        let spec = CommandSpec {
            command: "sleep 30".to_string(),
            cwd: cwd.path().to_path_buf(),
            timeout: Duration::from_millis(400),
        };
        let handle = backend.spawn(&spec, None).await.unwrap();
        let started = std::time::Instant::now();
        let output = backend.collect(handle, &spec, None).await.unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "超时必须及时终止，实际 {:?}",
            started.elapsed()
        );
        assert_ne!(output.exit_code, Some(0));
    }

    /// 中止旗标路径：abort 即终止（旗标预设 true，collect 首轮轮询即中止）。
    #[tokio::test]
    async fn abort_flag_terminates_command() {
        let backend = LocalShellBackend::new();
        let cwd = tempfile::tempdir().unwrap();
        let spec = CommandSpec {
            command: "sleep 30".to_string(),
            cwd: cwd.path().to_path_buf(),
            timeout: Duration::from_secs(30),
        };
        let flag = AtomicBool::new(true);
        let handle = backend.spawn(&spec, None).await.unwrap();
        let started = std::time::Instant::now();
        let output = backend.collect(handle, &spec, Some(&flag)).await.unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "中止必须及时生效，实际 {:?}",
            started.elapsed()
        );
        assert_ne!(output.exit_code, Some(0));
    }

    /// M7-02.A1：DockerBackend 路由进容器（命令进 docker run argv）。
    /// docker 不在场的环境用 dry 语义验证：spawn 报错携带 "docker spawn"
    /// （证明确实路由到 docker 可执行文件而非本地 shell）。
    #[tokio::test]
    async fn docker_backend_routes_to_docker_run() {
        let backend: Box<dyn CommandExecutionBackend> = Box::new(DockerBackend::new("alpine:3"));
        assert_eq!(backend.backend_id(), "docker");
        let cwd = tempfile::tempdir().unwrap();
        let spec = CommandSpec {
            command: "echo hi".to_string(),
            cwd: cwd.path().to_path_buf(),
            timeout: Duration::from_secs(15),
        };
        match backend.spawn(&spec, None).await {
            Ok(handle) => {
                // docker 在场：命令必须真的跑在容器里（--rm + alpine 的 sh 语义）。
                let output = backend.collect(handle, &spec, None).await.unwrap();
                assert!(output.stdout.contains("hi"), "stdout={}", output.stdout);
            }
            Err(error) => {
                // docker 缺席：失败必须来自 docker 可执行文件本身（路由证明）。
                let text = error.to_string();
                assert!(
                    text.contains("docker spawn"),
                    "失败必须证明走了 docker 路由：{text}"
                );
            }
        }
    }

    /// M7-02.A3：启用 Docker 后审批/风险分级语义不变——分类器与 R4 红线
    /// 与执行后端无关（backend 只换执行载体）。
    #[test]
    fn docker_backend_does_not_touch_approval_semantics() {
        use crate::classify_shell_command;
        use r_code_core::dto::RiskLevel;
        // 容器后端在场与否，命令分级一致（backend 类型系统上与分类器无交集）。
        assert_eq!(classify_shell_command("sudo rm -rf /").level, RiskLevel::R4);
        assert_ne!(classify_shell_command("ls").level, RiskLevel::R4);
        assert_ne!(classify_shell_command("ls").level, RiskLevel::R3);
        // backend trait 不暴露任何"跳过审批"入口（编译面保证）。
        let _ = DockerBackend::new("alpine:3").backend_id();
    }

    /// docker run 参数模板：网络 none + 只读挂载 + 限额。
    #[test]
    fn docker_args_sandbox_the_command() {
        let backend = DockerBackend::new("alpine:3");
        let cwd = Path::new("D:/ws");
        let args = backend.base_args(cwd);
        assert!(args.contains(&"--network".to_string()));
        let network_index = args.iter().position(|a| a == "--network").unwrap();
        assert_eq!(args[network_index + 1], "none");
        assert!(args.contains(&format!("{}:/workspace:ro", cwd.to_string_lossy())));
        assert!(args.contains(&"--memory".to_string()));
    }

    /// cwd 预检：不存在即拒绝。
    #[test]
    fn missing_cwd_is_rejected() {
        let missing = Path::new("Z:/definitely/not/here");
        assert!(ensure_cwd_exists(missing).is_err());
    }
}

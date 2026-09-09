//! 共享 daemon 测试环境（T35）：构建 r-code-service 与 r-code-harness-native、
//! staging 内置插件包、为每个测试分配隔离的 data-dir + ipc-name、结束清理
//! 守护进程。各 PTY/集成测试经 `mod daemon_common;` 复用。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

/// 构建（stdio 继承：编译进度实时可见——捕获式 `.output()` 会让几分钟的
/// 重编译看起来像测试卡死；cargo 自身有文件锁，并行测试调用安全）。
fn build(package: &str, bin: Option<&str>) {
    let mut args = vec!["build".to_string(), "-p".to_string(), package.to_string()];
    if let Some(bin) = bin {
        args.push("--bin".into());
        args.push(bin.into());
    }
    let status = Command::new(cargo())
        .args(&args)
        .status()
        .unwrap_or_else(|error| panic!("run cargo build {package}: {error}"));
    assert!(
        status.success(),
        "building {package} failed（若错误含“拒绝访问/os error 5”：多为上次取消的运行\
         残留 r-code-service.exe 占住文件锁——结束它后重跑）"
    );
}

/// 只清理本仓库 target 目录下残留的 r-code-service（测试产物）。被取消的
/// 测试运行会留下存活的守护进程：占住 exe 文件锁让后续嵌套 build 报
/// os error 5，也可能干扰后续用例。绝不触碰安装目录里的用户守护进程。
pub fn kill_stale_target_daemons() {
    // 词法路径即可（不要 canonicalize：Windows 会加 \\?\ 前缀，PowerShell
    // -like 模式永远匹配不上）。
    let target_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target");
    let target_text = target_root.display().to_string();
    #[cfg(windows)]
    {
        let script = format!(
            "Get-CimInstance Win32_Process -Filter \"Name='r-code-service.exe'\" | \
             Where-Object {{ $_.ExecutablePath -like '{}*' }} | \
             ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }}",
            // PowerShell 单引号串里反斜杠不是转义符，只需处理单引号本身。
            target_text.replace('\'', "''")
        );
        let _ = Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .output();
    }
    #[cfg(unix)]
    {
        let _ = Command::new("sh")
            .args([
                "-c",
                &format!(
                    "pkill -f '{}/target/debug/r-code-service' 2>/dev/null; true",
                    target_text
                ),
            ])
            .output();
    }
}

/// 守护进程子进程守卫：panic/提前返回时杀掉子进程，防止残留进程把
/// exe 文件锁留给下一次运行。（本测试用 bridge 自起 daemon，不经此守卫；
/// 保留供后续用例复用。）
#[allow(dead_code)]
pub struct DaemonGuard(pub std::process::Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn target_debug(name: &str) -> PathBuf {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/debug")
        .join(exe);
    assert!(path.is_file(), "missing {}", path.display());
    path
}

/// staging 内置 native 插件包（conversation_engine.rs 模式），缓存于
/// target/debug/t35-builtin/plugins/native.r-code。返回 R_CODE_BUILTIN_PLUGINS_DIR
/// 的取值（即 "plugins" 的父目录）。
pub fn stage_builtin_plugins() -> PathBuf {
    build("r-code-runtime", Some("r-code-service"));
    build("r-code-harness-native", None);
    let binary = target_debug("r-code-harness-native");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/t35-builtin");
    let package = root.join("plugins").join("native.r-code");
    let bin_dir = package.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create builtin package dirs");
    let platform = match r_code_harness_protocol::Platform::current() {
        r_code_harness_protocol::Platform::WindowsX64 => "windows-x64",
        r_code_harness_protocol::Platform::MacosArm64 => "macos-arm64",
        r_code_harness_protocol::Platform::MacosX64 => "macos-x64",
        r_code_harness_protocol::Platform::LinuxX64 => "linux-x64",
    };
    // 内容不变则跳过重写（避免与并行测试/正在安装的守护进程互相干扰）。
    let manifest = serde_json::json!({
        "schema_version": "1",
        "id": "native.r-code",
        "version": "1.0.0",
        "apiMajor": 1,
        "apiMinor": 0,
        "displayName": "Native",
        "supportedPlatforms": [{"platform": platform, "executable": "bin/r-code-harness-native"}],
        "requestedHostServices": [
            "host.model.stream",
            "host.tools.list",
            "host.tools.call",
            "host.checkpoint.save",
            "host.completion.propose"
        ],
        "configSchema": {"type": "object"}
    })
    .to_string();
    let manifest_path = package.join("harness.json");
    if std::fs::read_to_string(&manifest_path).ok().as_deref() != Some(manifest.as_str()) {
        std::fs::write(&manifest_path, manifest).expect("write harness.json");
    }
    let staged_binary = bin_dir.join(binary.file_name().unwrap());
    let same = std::fs::metadata(&staged_binary)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .zip(
            std::fs::metadata(&binary)
                .ok()
                .and_then(|meta| meta.modified().ok()),
        )
        .is_some_and(|(staged, source)| staged == source);
    if !same {
        std::fs::copy(&binary, &staged_binary).expect("stage native binary");
    }
    root
}

/// 一个测试的隔离 daemon 环境（唯一 data-dir + 唯一 ipc-name → 唯一端点）。
pub struct DaemonEnv {
    /// TUI --data-dir 取值（tempdir 已 forget，路径进程内有效）。
    pub data_dir: PathBuf,
    /// TUI --ipc-name 取值。
    pub ipc_name: String,
}

/// 分配隔离环境并返回子进程需要的 env 键值（R_CODE_SERVICE_BIN /
/// R_CODE_BUILTIN_PLUGINS_DIR）。
pub fn daemon_env(tag: &str) -> (DaemonEnv, Vec<(&'static str, String)>) {
    // 先清掉历史取消运行留下的 target 目录守护进程（释放 exe 文件锁）。
    kill_stale_target_daemons();
    let builtin = stage_builtin_plugins();
    let service = target_debug("r-code-service");
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().to_path_buf();
    std::mem::forget(dir);
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ipc_name = format!("tui-{tag}-{}-{unique}", std::process::id());
    let env = vec![
        ("R_CODE_SERVICE_BIN", service.display().to_string()),
        ("R_CODE_BUILTIN_PLUGINS_DIR", builtin.display().to_string()),
    ];
    // 同时写进本测试进程：引擎 connect 走 ensure_daemon，若在测试自起守护
    // 进程绑定前抢先 spawn 竞争者，子进程继承这些 env，同样注册内置插件
    //（否则竞态赢家可能是一个没有 native.r-code 的守护进程）。
    for (key, value) in &env {
        std::env::set_var(key, value);
    }
    (
        DaemonEnv {
            data_dir,
            ipc_name: ipc_name.clone(),
        },
        env,
    )
}

/// 测试收尾清理：优先 service.shutdown（优雅），失败按 owner.json 的 pid
/// 硬杀（Windows: taskkill /F；Unix: kill），避免残留守护进程。
pub fn shutdown_daemon(env: &DaemonEnv) {
    let profile = r_code_runtime::RuntimeProfile::resolve(
        &r_code_runtime::LaunchOptions::new(r_code_runtime::ProfileFlavor::Development)
            .with_data_root(&env.data_dir)
            .with_ipc_name(env.ipc_name.clone()),
    )
    .expect("profile");
    // 优雅路径：owner.json 的 token 直连发 service.shutdown。独立线程 +
    // 自有 runtime：调用方可能是 #[tokio::test]（运行时内不能再 block_on）。
    let graceful = {
        let profile = profile.clone();
        std::thread::spawn(move || {
            tokio::runtime::Runtime::new().map(|runtime| {
                runtime.block_on(async {
                    let Some(info) = r_code_client::read_owner_token(&profile.harness_v2_root())
                    else {
                        return false;
                    };
                    let Ok(mut client) = r_code_client::DaemonClient::connect(
                        &profile.ipc_endpoint(),
                        &profile.profile_id(),
                        &info.token,
                        "test-teardown",
                    )
                    .await
                    else {
                        return false;
                    };
                    client
                        .call("service.shutdown", serde_json::json!({}))
                        .await
                        .is_ok()
                })
            })
        })
        .join()
        .unwrap_or(Ok(false))
        .unwrap_or(false)
    };
    if graceful {
        // 给守护进程一点退出时间（shutdown 是 notify 不是 join）。
        std::thread::sleep(std::time::Duration::from_millis(300));
        return;
    }
    // 硬杀兜底。
    let pid = std::fs::read_to_string(profile.harness_v2_root().join("owner.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|owner| owner["pid"].as_u64());
    if let Some(pid) = pid {
        #[cfg(windows)]
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output();
        #[cfg(unix)]
        let _ = Command::new("kill").arg(pid.to_string()).output();
    }
}

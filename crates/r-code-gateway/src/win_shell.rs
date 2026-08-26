//! Windows shell 五级解析链（PRD §4.1，顺序冻结）。
//!
//! 1. 设置覆盖 `execution.bash_shell_path`（存在即用；指向缺失报错不静默回落；
//!    空串=强制回落，跳过 1-4 级）；
//! 2. 已知安装位置（Program Files / Program Files(x86) / LOCALAPPDATA / scoop）；
//! 3. PATH 上 `git.exe` 反推 `<git根>\bin\bash.exe` 与 `<git根>\usr\bin\bash.exe`；
//! 4. PATH 上 `bash.exe`（大小写不敏感，**跳过 `C:\Windows\System32\bash.exe`
//!    及其它解析为 WSL 启动器的命中**）；
//! 5. 回落：`pwsh.exe` → `powershell.exe` → `cmd.exe`。
//!
//! 解析结果带 TTL 缓存（默认 5 分钟）：探测是进程内 PATH 扫描，缓存命中后 O(1)。
//! 设置覆盖不进缓存（覆盖值随时可变，且校验本身只是单次 stat）。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use r_code_core::error::ProductError;

use crate::tools_command::ShellDialect;

/// 解析结果：方言档 + 具体程序。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedShell {
    pub dialect: ShellDialect,
    /// bash 档为绝对路径；PowerShell 档为 PATH 名（`pwsh.exe`）；cmd 档 `cmd.exe`。
    pub program: PathBuf,
}

/// 解析输入（生产环境从进程 env 收集；单测注入假值）。
pub(crate) struct ResolveInputs<'a> {
    /// `execution.bash_shell_path` 设置值（None=未设置；Some("")=强制回落）。
    pub override_path: Option<&'a str>,
    /// 进程 PATH 拆分后的目录（顺序保持）。
    pub path_entries: &'a [PathBuf],
    /// 第 2 级已知安装位置（已展开环境变量）。
    pub known_bash_locations: &'a [PathBuf],
    /// WSL 启动器所在系统目录（System32 / SysWOW64），第 4 级跳过用。
    pub system_dirs: &'a [PathBuf],
    /// 文件存在判定（单测注入内存集合）。
    pub file_exists: Box<dyn Fn(&Path) -> bool + 'a>,
}

fn same_dir_normalized(candidate: &Path, dir: &Path) -> bool {
    let normalize = |path: &Path| {
        path.to_string_lossy()
            .to_ascii_lowercase()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_string()
    };
    normalize(candidate) == normalize(dir)
}

/// 纯函数解析核心（五级顺序冻结，PRD §4.1 逐字一致）。
pub(crate) fn resolve_shell(inputs: &ResolveInputs) -> Result<ResolvedShell, ProductError> {
    // 空串覆盖 = 强制回落：跳过 1-4 级全部 bash 探测，直接进回落链。
    if matches!(inputs.override_path, Some(raw) if raw.trim().is_empty()) {
        return fallback_shell(inputs);
    }

    // 第 1 级：设置覆盖（存在即用；指向缺失报错不静默回落）。
    if let Some(raw) = inputs.override_path {
        let path = Path::new(raw);
        if !(inputs.file_exists)(path) {
            return Err(ProductError::ConfigError(format!(
                "execution.bash_shell_path 指向的 bash 不存在：{raw}\
（不会静默回落；请修正路径，或清空该值以强制回落 PowerShell）"
            )));
        }
        return Ok(ResolvedShell {
            dialect: ShellDialect::GitBash,
            program: path.to_path_buf(),
        });
    }

    // 第 2 级：已知安装位置。
    for candidate in inputs.known_bash_locations {
        if (inputs.file_exists)(candidate) {
            return Ok(ResolvedShell {
                dialect: ShellDialect::GitBash,
                program: candidate.clone(),
            });
        }
    }

    // 第 3 级：PATH 上 git.exe 反推。
    for dir in inputs.path_entries {
        let git_exe = dir.join("git.exe");
        if (inputs.file_exists)(&git_exe) {
            if let Some(git_root) = dir.parent() {
                for sub in ["bin", "usr\\bin"] {
                    let candidate = git_root.join(sub).join("bash.exe");
                    if (inputs.file_exists)(&candidate) {
                        return Ok(ResolvedShell {
                            dialect: ShellDialect::GitBash,
                            program: candidate,
                        });
                    }
                }
            }
        }
    }

    // 第 4 级：PATH 上 bash.exe，跳过 WSL 启动器（System32/SysWOW64 下的命中）。
    for dir in inputs.path_entries {
        let candidate = dir.join("bash.exe");
        if (inputs.file_exists)(&candidate)
            && !inputs
                .system_dirs
                .iter()
                .any(|system_dir| same_dir_normalized(dir, system_dir))
        {
            return Ok(ResolvedShell {
                dialect: ShellDialect::GitBash,
                program: candidate,
            });
        }
    }

    // 第 5 级：回落 pwsh → powershell → cmd。
    fallback_shell(inputs)
}

/// 第 5 级回落链：pwsh → powershell → cmd。
fn fallback_shell(inputs: &ResolveInputs) -> Result<ResolvedShell, ProductError> {
    for (name, dialect) in [
        ("pwsh.exe", ShellDialect::Pwsh),
        ("powershell.exe", ShellDialect::Powershell),
    ] {
        for dir in inputs.path_entries {
            let candidate = dir.join(name);
            if (inputs.file_exists)(&candidate) {
                return Ok(ResolvedShell {
                    dialect,
                    program: PathBuf::from(name),
                });
            }
        }
    }
    Ok(ResolvedShell {
        dialect: ShellDialect::Cmd,
        program: PathBuf::from("cmd.exe"),
    })
}

const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

static RESOLUTION_CACHE: Mutex<Option<(Instant, ResolvedShell)>> = Mutex::new(None);

fn cached_resolution() -> Option<ResolvedShell> {
    let guard = RESOLUTION_CACHE.lock().ok()?;
    match guard.as_ref() {
        Some((at, resolved)) if at.elapsed() < CACHE_TTL => Some(resolved.clone()),
        _ => None,
    }
}

fn store_resolution(resolved: &ResolvedShell) {
    if let Ok(mut guard) = RESOLUTION_CACHE.lock() {
        *guard = Some((Instant::now(), resolved.clone()));
    }
}

/// 清空解析缓存（测试与设置变更后使用）。
pub fn invalidate_shell_cache() {
    if let Ok(mut guard) = RESOLUTION_CACHE.lock() {
        *guard = None;
    }
}

fn production_inputs<'a>(
    override_path: Option<&'a str>,
    path_entries: &'a [PathBuf],
    known_bash_locations: &'a [PathBuf],
    system_dirs: &'a [PathBuf],
) -> ResolveInputs<'a> {
    ResolveInputs {
        override_path,
        path_entries,
        known_bash_locations,
        system_dirs,
        file_exists: Box::new(|path: &Path| path.is_file()),
    }
}

fn gather_path_entries() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .filter(|dir| !dir.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn gather_known_bash_locations() -> Vec<PathBuf> {
    let mut locations = Vec::new();
    let mut push = |value: Option<std::ffi::OsString>, tail: &[&str]| {
        if let Some(base) = value {
            let mut path = PathBuf::from(base);
            for segment in tail {
                path.push(segment);
            }
            locations.push(path);
        }
    };
    push(
        std::env::var_os("ProgramFiles"),
        &["Git", "bin", "bash.exe"],
    );
    push(
        std::env::var_os("ProgramFiles(x86)"),
        &["Git", "bin", "bash.exe"],
    );
    push(
        std::env::var_os("LOCALAPPDATA"),
        &["Programs", "Git", "bin", "bash.exe"],
    );
    push(
        std::env::var_os("USERPROFILE"),
        &["scoop", "apps", "git", "current", "bin", "bash.exe"],
    );
    locations
}

fn gather_system_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(root) = std::env::var_os("SystemRoot") {
        let base = PathBuf::from(root);
        dirs.push(base.join("System32"));
        dirs.push(base.join("SysWOW64"));
    } else {
        dirs.push(PathBuf::from(r"C:\Windows\System32"));
        dirs.push(PathBuf::from(r"C:\Windows\SysWOW64"));
    }
    dirs
}

/// 生产入口：五级解析（无覆盖走 TTL 缓存；覆盖每次实时校验）。
pub fn resolve_windows_shell(override_path: Option<&str>) -> Result<ResolvedShell, ProductError> {
    if override_path.is_some() {
        let path_entries = gather_path_entries();
        let known = gather_known_bash_locations();
        let system_dirs = gather_system_dirs();
        return resolve_shell(&production_inputs(
            override_path,
            &path_entries,
            &known,
            &system_dirs,
        ));
    }
    if let Some(resolved) = cached_resolution() {
        return Ok(resolved);
    }
    let path_entries = gather_path_entries();
    let known = gather_known_bash_locations();
    let system_dirs = gather_system_dirs();
    let resolved = resolve_shell(&production_inputs(
        None,
        &path_entries,
        &known,
        &system_dirs,
    ))?;
    store_resolution(&resolved);
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn inputs<'a>(
        override_path: Option<&'a str>,
        path_entries: &'a [PathBuf],
        known: &'a [PathBuf],
        system_dirs: &'a [PathBuf],
        existing: &'a HashSet<PathBuf>,
    ) -> ResolveInputs<'a> {
        ResolveInputs {
            override_path,
            path_entries,
            known_bash_locations: known,
            system_dirs,
            file_exists: Box::new(move |path: &Path| existing.contains(path)),
        }
    }

    fn pb(path: &str) -> PathBuf {
        PathBuf::from(path)
    }

    const SYS32: &str = r"C:\Windows\System32";

    #[test]
    fn resolve_windows_shell_level1_override_used_and_missing_errors() {
        let existing: HashSet<PathBuf> = [pb(r"C:\custom\bin\bash.exe"), pb(SYS32)]
            .into_iter()
            .collect();
        let paths = [pb(r"C:\Windows\System32"), pb(r"C:\other")];
        let known = [pb(r"C:\Program Files\Git\bin\bash.exe")];
        let system = [pb(SYS32)];

        // 存在即用：第 1 级优先于一切。
        let resolved = resolve_shell(&inputs(
            Some(r"C:\custom\bin\bash.exe"),
            &paths,
            &known,
            &system,
            &existing,
        ))
        .unwrap();
        assert_eq!(resolved.dialect, ShellDialect::GitBash);
        assert_eq!(resolved.program, pb(r"C:\custom\bin\bash.exe"));

        // 指向缺失：报错且信息含设置键，不静默回落。
        let error = resolve_shell(&inputs(
            Some(r"C:\gone\bash.exe"),
            &paths,
            &known,
            &system,
            &existing,
        ))
        .expect_err("missing override must error");
        assert!(error.to_string().contains("execution.bash_shell_path"));
    }

    #[test]
    fn resolve_windows_shell_override_empty_forces_fallback() {
        // 空串=强制回落：即使第 2-4 级全部命中也不选 bash。
        let existing: HashSet<PathBuf> = [
            pb(r"C:\Program Files\Git\bin\bash.exe"),
            pb(r"C:\tools\pwsh.exe"),
        ]
        .into_iter()
        .collect();
        let paths = [pb(r"C:\tools")];
        let known = [pb(r"C:\Program Files\Git\bin\bash.exe")];
        let system = [pb(SYS32)];
        let resolved =
            resolve_shell(&inputs(Some(""), &paths, &known, &system, &existing)).unwrap();
        assert_eq!(resolved.dialect, ShellDialect::Pwsh);
        assert_eq!(resolved.program, pb("pwsh.exe"));
    }

    #[test]
    fn resolve_windows_shell_level2_known_location() {
        let existing: HashSet<PathBuf> = [pb(r"C:\Program Files\Git\bin\bash.exe")]
            .into_iter()
            .collect();
        let paths: [PathBuf; 0] = [];
        let known = [pb(r"C:\Program Files\Git\bin\bash.exe")];
        let system = [pb(SYS32)];
        let resolved = resolve_shell(&inputs(None, &paths, &known, &system, &existing)).unwrap();
        assert_eq!(resolved.dialect, ShellDialect::GitBash);
        assert_eq!(resolved.program, pb(r"C:\Program Files\Git\bin\bash.exe"));
    }

    #[test]
    fn resolve_windows_shell_level2_known_beats_path_bash() {
        // 第 2 级优先于第 4 级。
        let existing: HashSet<PathBuf> = [
            pb(r"C:\Program Files\Git\bin\bash.exe"),
            pb(r"C:\stray\bash.exe"),
        ]
        .into_iter()
        .collect();
        let paths = [pb(r"C:\stray")];
        let known = [pb(r"C:\Program Files\Git\bin\bash.exe")];
        let system = [pb(SYS32)];
        let resolved = resolve_shell(&inputs(None, &paths, &known, &system, &existing)).unwrap();
        assert_eq!(resolved.program, pb(r"C:\Program Files\Git\bin\bash.exe"));
    }

    #[test]
    fn resolve_windows_shell_level3_git_exe_derives_bash() {
        // PATH 上没有 bash.exe，但有 git.exe：反推 <git根>\bin\bash.exe。
        let existing: HashSet<PathBuf> = [
            pb(r"D:\tools\Git\cmd\git.exe"),
            pb(r"D:\tools\Git\bin\bash.exe"),
        ]
        .into_iter()
        .collect();
        let paths = [pb(r"D:\tools\Git\cmd"), pb(r"C:\other")];
        let known: [PathBuf; 0] = [];
        let system = [pb(SYS32)];
        let resolved = resolve_shell(&inputs(None, &paths, &known, &system, &existing)).unwrap();
        assert_eq!(resolved.dialect, ShellDialect::GitBash);
        assert_eq!(resolved.program, pb(r"D:\tools\Git\bin\bash.exe"));
    }

    #[test]
    fn resolve_windows_shell_level3_prefers_bin_over_usr_bin() {
        let existing: HashSet<PathBuf> = [
            pb(r"D:\g\cmd\git.exe"),
            pb(r"D:\g\bin\bash.exe"),
            pb(r"D:\g\usr\bin\bash.exe"),
        ]
        .into_iter()
        .collect();
        let paths = [pb(r"D:\g\cmd")];
        let known: [PathBuf; 0] = [];
        let system = [pb(SYS32)];
        let resolved = resolve_shell(&inputs(None, &paths, &known, &system, &existing)).unwrap();
        assert_eq!(resolved.program, pb(r"D:\g\bin\bash.exe"));
    }

    #[test]
    fn resolve_windows_shell_level4_path_bash() {
        let existing: HashSet<PathBuf> = [pb(r"C:\tools\bash.exe")].into_iter().collect();
        let paths = [pb(r"C:\other"), pb(r"C:\tools")];
        let known: [PathBuf; 0] = [];
        let system = [pb(SYS32)];
        let resolved = resolve_shell(&inputs(None, &paths, &known, &system, &existing)).unwrap();
        assert_eq!(resolved.dialect, ShellDialect::GitBash);
        assert_eq!(resolved.program, pb(r"C:\tools\bash.exe"));
    }

    #[test]
    fn resolve_windows_shell_skips_wsl_bash_launcher_and_continues() {
        // PATH 首位就是 System32\bash.exe（WSL 启动器）：必须跳过并继续到下一级。
        let existing: HashSet<PathBuf> = [
            pb(r"C:\Windows\System32\bash.exe"),
            pb(r"C:\realgit\bash.exe"),
        ]
        .into_iter()
        .collect();
        let paths = [pb(r"C:\Windows\System32"), pb(r"C:\realgit")];
        let known: [PathBuf; 0] = [];
        let system = [pb(SYS32)];
        let resolved = resolve_shell(&inputs(None, &paths, &known, &system, &existing)).unwrap();
        assert_eq!(resolved.dialect, ShellDialect::GitBash);
        assert_eq!(
            resolved.program,
            pb(r"C:\realgit\bash.exe"),
            "System32 WSL launcher must be skipped, not selected"
        );
    }

    #[test]
    fn resolve_windows_shell_wsl_only_falls_back_to_powershell() {
        // 只有 WSL bash.exe 时视为未命中 bash，回落 PowerShell，绝不选 WSL。
        let existing: HashSet<PathBuf> =
            [pb(r"C:\Windows\System32\bash.exe"), pb(r"C:\ps\pwsh.exe")]
                .into_iter()
                .collect();
        let paths = [pb(r"C:\Windows\System32"), pb(r"C:\ps")];
        let known: [PathBuf; 0] = [];
        let system = [pb(SYS32)];
        let resolved = resolve_shell(&inputs(None, &paths, &known, &system, &existing)).unwrap();
        assert_eq!(resolved.dialect, ShellDialect::Pwsh);
    }

    #[test]
    fn resolve_windows_shell_falls_back_powershell_then_cmd() {
        let system = [pb(SYS32)];
        let known: [PathBuf; 0] = [];

        // pwsh 优先于 powershell。
        let existing: HashSet<PathBuf> = [pb(r"C:\ps\pwsh.exe"), pb(r"C:\ps\powershell.exe")]
            .into_iter()
            .collect();
        let paths = [pb(r"C:\ps")];
        let resolved = resolve_shell(&inputs(None, &paths, &known, &system, &existing)).unwrap();
        assert_eq!(resolved.dialect, ShellDialect::Pwsh);

        // 无 pwsh 时用 Windows PowerShell 5.1。
        let existing: HashSet<PathBuf> = [pb(r"C:\ps\powershell.exe")].into_iter().collect();
        let resolved = resolve_shell(&inputs(None, &paths, &known, &system, &existing)).unwrap();
        assert_eq!(resolved.dialect, ShellDialect::Powershell);

        // 两者皆无：cmd.exe 兜底。
        let existing: HashSet<PathBuf> = HashSet::new();
        let resolved = resolve_shell(&inputs(None, &paths, &known, &system, &existing)).unwrap();
        assert_eq!(resolved.dialect, ShellDialect::Cmd);
        assert_eq!(resolved.program, pb("cmd.exe"));
    }

    #[test]
    fn resolve_windows_shell_cache_roundtrip_and_invalidate() {
        invalidate_shell_cache();
        let first = resolve_windows_shell(None).unwrap();
        let second = resolve_windows_shell(None).unwrap();
        assert_eq!(first, second, "cached resolution must be stable");
        invalidate_shell_cache();
        // invalidate 后仍可解析（重新探测）。
        let third = resolve_windows_shell(None).unwrap();
        assert_eq!(second, third);
    }
}

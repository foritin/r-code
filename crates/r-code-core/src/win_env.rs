//! Windows 注册表实时 PATH 合成（PRD windows-command-reliability §4.2）。
//!
//! GUI 启动的进程继承的是登录会话建立时的陈旧 PATH；用户安装新工具后写进
//! 注册表的 PATH 条目（HKLM 系统 + HKCU 用户）对旧进程不可见。本模块在每次
//! 拉起子进程前从注册表合成**实时 PATH**：
//!
//! 顺序 = HKLM 条目 → HKCU 条目 → 进程 PATH 中不在前两者的条目（大小写不
//! 敏感去重，保持进程内相对顺序）。这是 macOS `fix_path_env` 的 Windows 等价物。
//!
//! 合同：注册表读取只读；读失败 fallthrough 进程 PATH 并记录日志（绝不 panic）；
//! 进程内 TTL 缓存 5 分钟，`invalidate()` 可强制刷新。
//!
//! `REG_EXPAND_SZ` 按 `ExpandEnvironmentStringsW` 语义展开（查进程环境块，
//! 大小写不敏感；未知变量原样保留）。
#![cfg(windows)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const HKLM_ENV_SUBKEY: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
const HKCU_ENV_SUBKEY: &str = "Environment";

static PATH_CACHE: Mutex<Option<(Instant, OsString)>> = Mutex::new(None);

/// 合成的实时 PATH（HKLM → HKCU → 进程差集）。注册表读失败时 fallthrough
/// 返回进程 PATH。结果缓存 5 分钟。
pub fn synthesized_path() -> OsString {
    if let Ok(guard) = PATH_CACHE.lock() {
        if let Some((at, cached)) = guard.as_ref() {
            if at.elapsed() < CACHE_TTL {
                return cached.clone();
            }
        }
    }
    let synthesized = synthesize_uncached();
    if let Ok(mut guard) = PATH_CACHE.lock() {
        *guard = Some((Instant::now(), synthesized.clone()));
    }
    synthesized
}

/// 清空 PATH 合成缓存（测试与设置变更后使用）。
pub fn invalidate() {
    if let Ok(mut guard) = PATH_CACHE.lock() {
        *guard = None;
    }
}

fn synthesize_uncached() -> OsString {
    let process = process_path_entries();
    match read_registry_paths(HKLM_ENV_SUBKEY, HKCU_ENV_SUBKEY) {
        Some((hklm, hkcu)) => {
            let entries = synthesize(&hklm, &hkcu, &process);
            join_path_entries(&entries)
        }
        None => {
            tracing::warn!(
                "win_env: 注册表 PATH 读取失败，fallthrough 到进程 PATH（{} 条目）",
                process.len()
            );
            std::env::var_os("PATH").unwrap_or_default()
        }
    }
}

fn process_path_entries() -> Vec<String> {
    std::env::var_os("PATH")
        .map(|paths| split_path_entries(&paths.to_string_lossy()))
        .unwrap_or_default()
}

fn join_path_entries(entries: &[String]) -> OsString {
    let joined = entries.join(";");
    OsString::from(joined)
}

fn split_path_entries(raw: &str) -> Vec<String> {
    raw.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// 纯合成核心：HKLM → HKCU → 进程差集，大小写不敏感去重。
fn synthesize(hklm: &[String], hkcu: &[String], process: &[String]) -> Vec<String> {
    let mut ordered: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for source in [hklm, hkcu, process] {
        for entry in source {
            if seen.insert(normalize_entry(entry)) {
                ordered.push(entry.clone());
            }
        }
    }
    ordered
}

fn normalize_entry(entry: &str) -> String {
    entry.trim_end_matches('\\').to_ascii_lowercase()
}

/// 从注册表读 HKLM 与 HKCU 的 Path 值（REG_SZ 与 REG_EXPAND_SZ 均接受，
/// 后者按进程环境展开）。任一根键读取失败即返回 None（调用方 fallthrough）。
fn read_registry_paths(hklm_subkey: &str, hkcu_subkey: &str) -> Option<(Vec<String>, Vec<String>)> {
    let hklm = read_path_value(windows_registry::LOCAL_MACHINE, hklm_subkey)?;
    let hkcu = read_path_value(windows_registry::CURRENT_USER, hkcu_subkey)?;
    Some((hklm, hkcu))
}

fn read_path_value(root: &windows_registry::Key, subkey: &str) -> Option<Vec<String>> {
    let key = root.open(subkey).ok()?;
    let value = key.get_value("Path").ok()?;
    let ty = value.ty();
    let raw = String::try_from(value).ok()?;
    let expanded = if matches!(ty, windows_registry::Type::ExpandString) {
        expand_registry_string(&raw, &process_env_lookup)
    } else {
        raw
    };
    Some(split_path_entries(&expanded))
}

/// 进程环境查找（大小写不敏感；ExpandEnvironmentStringsW 同义）。
fn process_env_lookup(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    std::env::vars()
        .find(|(key, _)| key.to_ascii_lowercase() == lower)
        .map(|(_, value)| value)
}

/// 展开 `%VAR%` 引用：已知变量替换值，未知原样保留（与
/// ExpandEnvironmentStringsW 行为一致）。`%%` 展开为单个 `%`。
fn expand_registry_string(raw: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '%' {
            out.push(chars[index]);
            index += 1;
            continue;
        }
        // `%%` → 字面 %
        if index + 1 < chars.len() && chars[index + 1] == '%' {
            out.push('%');
            index += 2;
            continue;
        }
        let Some(close) = chars[index + 1..].iter().position(|c| *c == '%') else {
            // 未闭合的 % 原样保留
            out.extend(chars[index..].iter());
            break;
        };
        let name: String = chars[index + 1..index + 1 + close].iter().collect();
        match lookup(&name) {
            Some(value) => out.push_str(&value),
            None => {
                out.push('%');
                out.push_str(&name);
                out.push('%');
            }
        }
        index += close + 2;
    }
    out
}

/// 供测试与诊断：解引用后的进程 PATH 条目（PathBuf 形态）。
pub fn synthesized_path_entries() -> Vec<PathBuf> {
    synthesized_path()
        .to_string_lossy()
        .split(';')
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|e| e.to_string()).collect()
    }

    #[test]
    fn win_env_synthesis_orders_hklm_hkcu_then_process_diff() {
        let hklm = s(&[r"C:\Windows", r"C:\Program Files"]);
        let hkcu = s(&[r"C:\Users\me\bin", r"C:\tools"]);
        let process = s(&[r"c:\windows", r"C:\extra", r"C:\Tools"]);
        let merged = synthesize(&hklm, &hkcu, &process);
        assert_eq!(
            merged,
            s(&[
                r"C:\Windows",
                r"C:\Program Files",
                r"C:\Users\me\bin",
                r"C:\tools",
                r"C:\extra",
            ]),
            "HKLM→HKCU→进程差集，大小写不敏感去重（保留首次出现的大小写形态）"
        );
    }

    #[test]
    fn win_env_synthesis_dedups_trailing_backslash_variants() {
        let hklm = s(&[r"C:\Python312\"]);
        let hkcu = s(&[r"c:\python312"]);
        let process = s(&[]);
        let merged = synthesize(&hklm, &hkcu, &process);
        assert_eq!(merged, s(&[r"C:\Python312\"]));
    }

    #[test]
    fn win_env_expand_registry_string_expands_percent_vars() {
        let lookup = |name: &str| match name.to_ascii_lowercase().as_str() {
            "systemdrive" => Some("C:".to_string()),
            "windir" => Some(r"C:\Windows".to_string()),
            _ => None,
        };
        assert_eq!(
            expand_registry_string(r"%SYSTEMDRIVE%\Tools", &lookup),
            r"C:\Tools",
            "变量名大小写不敏感"
        );
        assert_eq!(
            expand_registry_string(r"%SystemRoot%\System32", &lookup),
            r"%SystemRoot%\System32",
            "未知变量原样保留（ExpandEnvironmentStringsW 语义）"
        );
        assert_eq!(expand_registry_string("100%%", &lookup), "100%");
        assert_eq!(
            expand_registry_string(r"%A%and%WINDIR%", &lookup),
            r"%A%andC:\Windows",
            "未知变量保留、已知变量展开"
        );
        // 未闭合 % 保留原文。
        assert_eq!(expand_registry_string("a%oops", &lookup), "a%oops");
    }

    #[test]
    fn win_env_fallthrough_uses_process_path_when_registry_unreadable() {
        // 指向不存在的子键：read_registry_paths 必须返回 None，synthesized_path
        // 的 fallthrough 语义由此驱动（不 panic、非空）。
        assert!(read_registry_paths(
            r"SOFTWARE\r-code-definitely-not-here",
            r"SOFTWARE\r-code-definitely-not-here"
        )
        .is_none());
        // 真实键（HKCU Environment 不存在的变体）单侧失败也必须整体 fallthrough。
        assert!(read_registry_paths(HKLM_ENV_SUBKEY, r"SOFTWARE\r-code-nope").is_none());
    }

    #[test]
    fn win_env_synthesized_path_returns_real_registry_value() {
        // 真实注册表只读冒烟：合成结果非空，且包含 System32（任何 Windows 的
        // HKLM PATH 都有它或 Windows 目录；至少条目数 ≥1）。
        invalidate();
        let entries = synthesized_path_entries();
        assert!(!entries.is_empty(), "synthesized PATH must not be empty");
        assert!(
            entries.iter().any(|entry| entry
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("windows")),
            "synthesized PATH should contain a Windows system dir, got {entries:?}"
        );
    }

    #[test]
    fn win_env_cache_roundtrip_and_invalidate() {
        invalidate();
        let first = synthesized_path();
        let second = synthesized_path();
        assert_eq!(first, second, "cached value must be stable");
        invalidate();
        let third = synthesized_path();
        assert_eq!(second, third);
    }
}

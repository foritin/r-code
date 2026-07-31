#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use r_code_installer::{extract_payload, inspect_payload, ExtractControl, OverlayError};
use serde::Serialize;
use std::ffi::OsString;
use std::path::{Component, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::{Emitter, Manager, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

const APP_BINARY: &str = "r-code-host.exe";
#[cfg(windows)]
const PRODUCT_REGISTRY_KEY: &str = r"Software\R-Code Team\R-Code";
const LICENSE_TEXT: &str = include_str!("../../LICENSE");
const PRIVACY_TEXT: &str = include_str!("../../PRIVACY.md");

#[derive(Clone, Default)]
struct InstallerState(Arc<InstallerStateInner>);

#[derive(Default)]
struct InstallerStateInner {
    active: AtomicBool,
    cancelable: AtomicBool,
    cancel_requested: AtomicBool,
    last_install_path: Mutex<Option<PathBuf>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallerInfo {
    version: &'static str,
    default_install_path: String,
    existing_install: bool,
    package_size_mb: u64,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallRequest {
    install_path: String,
    create_shortcuts: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallProgress {
    stage: &'static str,
    percent: u8,
    message: String,
    cancelable: bool,
    install_path: Option<String>,
    error_code: Option<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CancelResponse {
    accepted: bool,
    message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CloseRequest {
    cancelable: bool,
}

enum WorkerOutcome {
    Completed(PathBuf),
    Cancelled,
    Failed { code: &'static str, message: String },
}

#[tauri::command]
fn installer_info() -> InstallerInfo {
    let default_path = preferred_install_path();
    let existing_install = default_path.join(APP_BINARY).is_file();
    let package_size_mb = std::env::current_exe()
        .ok()
        .and_then(|path| inspect_payload(&path).ok())
        .map(|metadata| metadata.length.div_ceil(1024 * 1024))
        .unwrap_or(0);

    InstallerInfo {
        version: env!("CARGO_PKG_VERSION"),
        default_install_path: default_path.to_string_lossy().into_owned(),
        existing_install,
        package_size_mb,
    }
}

#[tauri::command]
async fn choose_directory(
    app: tauri::AppHandle,
    current: String,
) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app.dialog().file();
        let current = PathBuf::from(current);
        if current.is_absolute() {
            dialog = dialog.set_directory(current);
        }
        dialog
            .blocking_pick_folder()
            .and_then(|path| path.into_path().ok())
            .map(|path| path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|error| format!("无法打开文件夹选择器：{error}"))
}

#[tauri::command]
fn legal_document(kind: &str) -> Result<&'static str, String> {
    match kind {
        "license" => Ok(LICENSE_TEXT),
        "privacy" => Ok(PRIVACY_TEXT),
        _ => Err("未知文档".to_string()),
    }
}

#[tauri::command]
fn start_install(
    app: tauri::AppHandle,
    state: State<'_, InstallerState>,
    request: InstallRequest,
) -> Result<(), String> {
    let install_path = validate_install_path(&request.install_path)?;
    let shared = state.inner().clone();
    if shared
        .0
        .active
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("安装任务已经在运行".to_string());
    }

    shared.0.cancel_requested.store(false, Ordering::Release);
    shared.0.cancelable.store(true, Ordering::Release);
    emit_progress(&app, "extracting", 2, "正在验证安装包", true, None, None);

    let worker_state = shared.clone();
    let worker_app = app.clone();
    let create_shortcuts = request.create_shortcuts;
    let spawn_result = thread::Builder::new()
        .name("r-code-installer-worker".to_string())
        .spawn(move || {
            let outcome =
                perform_install(&worker_app, &worker_state, install_path, create_shortcuts);

            worker_state.0.cancelable.store(false, Ordering::Release);
            worker_state.0.active.store(false, Ordering::Release);
            match outcome {
                WorkerOutcome::Completed(path) => {
                    if let Ok(mut last_path) = worker_state.0.last_install_path.lock() {
                        *last_path = Some(path.clone());
                    }
                    emit_progress(
                        &worker_app,
                        "complete",
                        100,
                        "R-Code 已安装完成",
                        false,
                        Some(path.to_string_lossy().into_owned()),
                        None,
                    );
                }
                WorkerOutcome::Cancelled => emit_progress(
                    &worker_app,
                    "cancelled",
                    0,
                    "安装已取消，临时文件已清理",
                    false,
                    None,
                    None,
                ),
                WorkerOutcome::Failed { code, message } => {
                    emit_progress(&worker_app, "error", 0, message, false, None, Some(code))
                }
            }
        });

    if let Err(error) = spawn_result {
        shared.0.active.store(false, Ordering::Release);
        shared.0.cancelable.store(false, Ordering::Release);
        return Err(format!("无法启动安装任务：{error}"));
    }
    Ok(())
}

#[tauri::command]
fn cancel_install(state: State<'_, InstallerState>) -> CancelResponse {
    if !state.0.active.load(Ordering::Acquire) {
        return CancelResponse {
            accepted: false,
            message: "当前没有正在运行的安装任务".to_string(),
        };
    }
    if !state.0.cancelable.load(Ordering::Acquire) {
        return CancelResponse {
            accepted: false,
            message: "正在安全写入应用文件，请等待当前阶段完成".to_string(),
        };
    }

    state.0.cancel_requested.store(true, Ordering::Release);
    CancelResponse {
        accepted: true,
        message: "正在停止安装并清理临时文件".to_string(),
    }
}

#[tauri::command]
fn minimize_window(window: WebviewWindow) -> Result<(), String> {
    window.minimize().map_err(|error| error.to_string())
}

#[tauri::command]
fn close_window(window: WebviewWindow, state: State<'_, InstallerState>) -> Result<bool, String> {
    if state.0.active.load(Ordering::Acquire) {
        window
            .emit(
                "installer-close-requested",
                CloseRequest {
                    cancelable: state.0.cancelable.load(Ordering::Acquire),
                },
            )
            .map_err(|error| error.to_string())?;
        return Ok(false);
    }
    window.close().map_err(|error| error.to_string())?;
    Ok(true)
}

#[tauri::command]
fn launch_installed_app(
    app: tauri::AppHandle,
    state: State<'_, InstallerState>,
) -> Result<(), String> {
    let install_path = state
        .0
        .last_install_path
        .lock()
        .map_err(|_| "无法读取安装结果".to_string())?
        .clone()
        .unwrap_or_else(preferred_install_path);
    let executable = install_path.join(APP_BINARY);
    if !executable.is_file() {
        return Err(format!("未找到已安装的 R-Code：{}", executable.display()));
    }

    let mut command = Command::new(&executable);
    command.current_dir(&install_path);
    hide_child_console(&mut command);
    command
        .spawn()
        .map_err(|error| format!("无法启动 R-Code：{error}"))?;
    app.exit(0);
    Ok(())
}

fn perform_install(
    app: &tauri::AppHandle,
    state: &InstallerState,
    install_path: PathBuf,
    create_shortcuts: bool,
) -> WorkerOutcome {
    let current_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            return WorkerOutcome::Failed {
                code: "RCI-101",
                message: format!("无法定位安装程序：{error}"),
            };
        }
    };
    let temporary = match tempfile::Builder::new()
        .prefix("r-code-installer-")
        .tempdir()
    {
        Ok(directory) => directory,
        Err(error) => {
            return WorkerOutcome::Failed {
                code: "RCI-102",
                message: format!("无法创建安装临时目录：{error}"),
            };
        }
    };
    let payload_path = temporary.path().join("r-code-nsis-payload.exe");
    let mut last_percent = 0_u8;
    let extraction = extract_payload(&current_exe, &payload_path, |done, total| {
        if state.0.cancel_requested.load(Ordering::Acquire) {
            return ExtractControl::Cancel;
        }
        let percent = 4 + ((done.saturating_mul(24) / total.max(1)) as u8);
        if percent != last_percent {
            last_percent = percent;
            emit_progress(
                app,
                "extracting",
                percent,
                "正在校验并准备安装组件",
                true,
                None,
                None,
            );
        }
        ExtractControl::Continue
    });

    match extraction {
        Ok(_) => {}
        Err(OverlayError::Cancelled) => return WorkerOutcome::Cancelled,
        Err(error) => {
            return WorkerOutcome::Failed {
                code: "RCI-103",
                message: error.to_string(),
            };
        }
    }
    if state.0.cancel_requested.load(Ordering::Acquire) {
        return WorkerOutcome::Cancelled;
    }

    state.0.cancelable.store(false, Ordering::Release);
    emit_progress(
        app,
        "preparing",
        32,
        "安装组件已就绪，准备写入应用文件",
        false,
        None,
        None,
    );

    let marker_path = temporary.path().join("progress.txt");
    let mut command = Command::new(&payload_path);
    command.arg("/S");
    command.arg(format!("/RC_PROGRESS={}", marker_path.display()));
    if !create_shortcuts {
        command.arg("/NS");
    }
    // NSIS requires /D to be the final command-line argument.
    command.arg(format!("/D={}", install_path.display()));
    hide_child_console(&mut command);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return WorkerOutcome::Failed {
                code: "RCI-104",
                message: format!("无法启动系统安装组件：{error}"),
            };
        }
    };
    emit_progress(
        app,
        "installing",
        42,
        "正在关闭旧版本并写入 R-Code",
        false,
        None,
        None,
    );

    let mut marker = String::new();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                return WorkerOutcome::Failed {
                    code: "RCI-105",
                    message: format!("无法读取安装状态：{error}"),
                };
            }
        }

        if let Ok(next_marker) = std::fs::read_to_string(&marker_path) {
            let next_marker = next_marker.trim().to_string();
            if next_marker != marker {
                marker = next_marker;
                match marker.as_str() {
                    "installing" => emit_progress(
                        app,
                        "installing",
                        62,
                        "正在写入应用文件和卸载组件",
                        false,
                        None,
                        None,
                    ),
                    "finalizing" => emit_progress(
                        app,
                        "finalizing",
                        90,
                        "正在创建快捷方式并完成系统集成",
                        false,
                        None,
                        None,
                    ),
                    _ => {}
                }
            }
        }
        thread::sleep(Duration::from_millis(120));
    };

    if !status.success() {
        return WorkerOutcome::Failed {
            code: "RCI-106",
            message: format!(
                "系统安装组件未能完成（退出码 {}）",
                status.code().unwrap_or(-1)
            ),
        };
    }

    let installed_binary = install_path.join(APP_BINARY);
    if !installed_binary.is_file() {
        return WorkerOutcome::Failed {
            code: "RCI-107",
            message: "安装组件已退出，但没有找到 R-Code 应用文件".to_string(),
        };
    }
    WorkerOutcome::Completed(install_path)
}

fn emit_progress(
    app: &tauri::AppHandle,
    stage: &'static str,
    percent: u8,
    message: impl Into<String>,
    cancelable: bool,
    install_path: Option<String>,
    error_code: Option<&'static str>,
) {
    let _ = app.emit(
        "installer-progress",
        InstallProgress {
            stage,
            percent,
            message: message.into(),
            cancelable,
            install_path,
            error_code,
        },
    );
}

fn validate_install_path(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim().trim_matches('"');
    if trimmed.is_empty() {
        return Err("请选择安装位置".to_string());
    }
    if trimmed
        .chars()
        .any(|value| matches!(value, '\0' | '"' | '*' | '?' | '<' | '>' | '|'))
    {
        return Err("安装位置包含 Windows 不支持的字符".to_string());
    }
    if trimmed.chars().count() > 220 {
        return Err("安装路径过长，请选择更短的目录".to_string());
    }

    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err("安装位置必须是完整的绝对路径".to_string());
    }
    if path.file_name().is_none() {
        return Err("不能直接安装到磁盘根目录".to_string());
    }
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("安装位置不能包含 .. 路径片段".to_string());
    }
    Ok(path)
}

fn preferred_install_path() -> PathBuf {
    stored_install_path()
        .filter(|path| path.is_absolute())
        .or_else(|| dirs::data_local_dir().map(|path| path.join("R-Code")))
        .unwrap_or_else(|| PathBuf::from(r"C:\R-Code"))
}

#[cfg(windows)]
fn stored_install_path() -> Option<PathBuf> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let key = current_user.open_subkey(PRODUCT_REGISTRY_KEY).ok()?;
    let value: String = key.get_value("").ok()?;
    let value = value.trim().trim_matches('"');
    (!value.is_empty()).then(|| PathBuf::from(value))
}

#[cfg(not(windows))]
fn stored_install_path() -> Option<PathBuf> {
    None
}

fn is_passthrough_invocation(args: &[OsString]) -> bool {
    args.iter().any(|argument| {
        let normalized = argument.to_string_lossy().to_ascii_uppercase();
        matches!(
            normalized.as_str(),
            "/S" | "/P" | "/UPDATE" | "/UPDATER" | "--SILENT" | "--QUIET"
        )
    })
}

fn run_passthrough(args: &[OsString]) -> i32 {
    let current_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) => return 21,
    };
    let temporary = match tempfile::Builder::new()
        .prefix("r-code-installer-")
        .tempdir()
    {
        Ok(path) => path,
        Err(_) => return 22,
    };
    let payload = temporary.path().join("r-code-nsis-payload.exe");
    if extract_payload(&current_exe, &payload, |_done, _total| {
        ExtractControl::Continue
    })
    .is_err()
    {
        return 23;
    }

    let mut command = Command::new(payload);
    command.args(args);
    hide_child_console(&mut command);
    match command.status() {
        Ok(status) => status.code().unwrap_or(24),
        Err(_) => 25,
    }
}

#[cfg(windows)]
fn hide_child_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_child_console(_command: &mut Command) {}

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if is_passthrough_invocation(&args) {
        std::process::exit(run_passthrough(&args));
    }

    let state = InstallerState::default();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            installer_info,
            choose_directory,
            legal_document,
            start_install,
            cancel_install,
            minimize_window,
            close_window,
            launch_installed_app,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<InstallerState>();
                if state.0.active.load(Ordering::Acquire) {
                    api.prevent_close();
                    let _ = window.emit(
                        "installer-close-requested",
                        CloseRequest {
                            cancelable: state.0.cancelable.load(Ordering::Acquire),
                        },
                    );
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("failed to run R-Code installer");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_relative_or_root_install_paths() {
        assert!(validate_install_path("R-Code").is_err());
        assert!(validate_install_path(r"C:\").is_err());
        assert!(validate_install_path(r"C:\Users\demo\..\Windows").is_err());
    }

    #[test]
    #[cfg(windows)]
    fn accepts_normal_windows_install_path() {
        let path = validate_install_path(r"C:\Users\demo\AppData\Local\R-Code").unwrap();
        assert_eq!(
            path,
            std::path::Path::new(r"C:\Users\demo\AppData\Local\R-Code")
        );
    }

    #[test]
    fn detects_silent_and_updater_invocations() {
        assert!(is_passthrough_invocation(&[OsString::from("/S")]));
        assert!(is_passthrough_invocation(&[OsString::from("/UPDATE")]));
        assert!(!is_passthrough_invocation(&[OsString::from(
            "/D=C:\\R-Code"
        )]));
    }
}

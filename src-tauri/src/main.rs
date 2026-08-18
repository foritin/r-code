#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

//! R-Code Host 二进制入口 -- Tauri 应用壳。
//!
//! 启动流程：
//! 1. 初始化结构化日志（stdout + 内存尾部 + 7 天滚动文件）
//! 2. 启动 Tauri 应用壳（加载前端 WebView）
//! 3. 在 setup hook 中创建持久化 CommandState（AppData/r-code）+ 后台启动 IPC server
//! 4. 注册 Tauri 命令（前端通过 invoke 调用）

use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::Arc;

use agent_ipc::IpcServer;
use r_code_host::commands::CommandState;
use tauri::{Emitter, Manager};

#[cfg(target_os = "windows")]
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

mod tauri_commands;

/// Tauri 命令：ping -- 验证前后端 IPC 通道。
#[tauri::command]
fn ping() -> bool {
    true
}

/// An independent companion keeps the event loop alive, so closing main must exit the full app
/// whenever the platform has no remaining restore surface (Dock reopen or a working tray icon).
#[cfg(any(target_os = "windows", target_os = "linux", test))]
fn main_close_requires_full_exit(is_main_window: bool, restore_available: bool) -> bool {
    is_main_window && !restore_available
}

#[cfg(target_os = "windows")]
const MAIN_TRAY_ID: &str = "r-code-main-tray";
#[cfg(target_os = "windows")]
const TRAY_SHOW_ID: &str = "r-code-tray-show";
#[cfg(target_os = "windows")]
const TRAY_HIDE_ID: &str = "r-code-tray-hide";
#[cfg(target_os = "windows")]
const TRAY_QUIT_ID: &str = "r-code-tray-quit";

#[cfg(target_os = "windows")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowsLifecycleAction {
    Restore,
    HideToTray,
    Quit,
    None,
}

#[cfg(target_os = "windows")]
fn windows_close_action(is_main_window: bool, tray_available: bool) -> WindowsLifecycleAction {
    if !is_main_window {
        return WindowsLifecycleAction::None;
    }
    if main_close_requires_full_exit(is_main_window, tray_available) {
        // The independent companion keeps Tauri's event loop alive after the main window closes.
        // Without a tray restore affordance, exit both windows explicitly instead of leaving an
        // unreachable background process.
        WindowsLifecycleAction::Quit
    } else {
        WindowsLifecycleAction::HideToTray
    }
}

#[cfg(target_os = "windows")]
fn windows_tray_menu_action(menu_id: &str) -> WindowsLifecycleAction {
    match menu_id {
        TRAY_SHOW_ID => WindowsLifecycleAction::Restore,
        TRAY_HIDE_ID => WindowsLifecycleAction::HideToTray,
        TRAY_QUIT_ID => WindowsLifecycleAction::Quit,
        _ => WindowsLifecycleAction::None,
    }
}

#[cfg(target_os = "windows")]
fn windows_tray_click_action(
    is_left_button: bool,
    is_button_release: bool,
) -> WindowsLifecycleAction {
    if is_left_button && is_button_release {
        WindowsLifecycleAction::Restore
    } else {
        WindowsLifecycleAction::None
    }
}

/// Restore the hidden main window from the notification area. Showing, unminimizing and focusing
/// are deliberately separate best-effort operations: Windows can report a stale minimized state
/// after a display or virtual-desktop transition, but one failed step must not block the others.
#[cfg(target_os = "windows")]
fn show_main_window(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        tracing::warn!("cannot restore R-Code because the main window is missing");
        return;
    };
    if let Err(error) = window.show() {
        tracing::warn!(%error, "failed to show the R-Code main window");
    }
    if let Err(error) = window.unminimize() {
        tracing::warn!(%error, "failed to restore the R-Code main window");
    }
    if let Err(error) = window.set_focus() {
        tracing::warn!(%error, "failed to focus the R-Code main window");
    }
    if let Some(tray) = app.tray_by_id(MAIN_TRAY_ID) {
        let _ = tray.set_tooltip(Some(r_code_host::app_paths::product_name()));
    }
}

#[cfg(target_os = "windows")]
fn hide_main_window(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        tracing::warn!("cannot hide R-Code because the main window is missing");
        return;
    };
    match window.hide() {
        Ok(()) => {
            if let Some(tray) = app.tray_by_id(MAIN_TRAY_ID) {
                let _ = tray.set_tooltip(Some(format!(
                    "{} — 正在后台运行",
                    r_code_host::app_paths::product_name()
                )));
            }
        }
        Err(error) => tracing::warn!(%error, "failed to hide the R-Code main window"),
    }
}

#[cfg(target_os = "windows")]
fn apply_windows_lifecycle_action(app: &tauri::AppHandle, action: WindowsLifecycleAction) {
    match action {
        WindowsLifecycleAction::Restore => show_main_window(app),
        WindowsLifecycleAction::HideToTray => hide_main_window(app),
        WindowsLifecycleAction::Quit => app.exit(0),
        WindowsLifecycleAction::None => {}
    }
}

/// Windows follows the familiar Discord/WeChat/OneDrive tray contract:
/// left click restores the window, right click exposes explicit show/hide/quit actions.
/// Tray setup is non-fatal; the close handler checks that this icon exists before hiding so a
/// shell restriction or icon failure can never leave the user with an unreachable application.
#[cfg(target_os = "windows")]
fn setup_windows_tray(app: &mut tauri::App) -> tauri::Result<()> {
    let product_name = r_code_host::app_paths::product_name();
    let show = MenuItem::with_id(
        app,
        TRAY_SHOW_ID,
        format!("打开 {product_name}"),
        true,
        None::<&str>,
    )?;
    let hide = MenuItem::with_id(app, TRAY_HIDE_ID, "隐藏到系统托盘", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(
        app,
        TRAY_QUIT_ID,
        format!("退出 {product_name}"),
        true,
        None::<&str>,
    )?;
    let menu = Menu::with_items(app, &[&show, &hide, &separator, &quit])?;

    let mut tray = TrayIconBuilder::with_id(MAIN_TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip(product_name)
        .on_menu_event(|app, event| {
            apply_windows_lifecycle_action(app, windows_tray_menu_action(event.id().as_ref()));
        })
        .on_tray_icon_event(|tray, event| {
            let action = match event {
                TrayIconEvent::Click {
                    button,
                    button_state,
                    ..
                } => windows_tray_click_action(
                    matches!(button, MouseButton::Left),
                    matches!(button_state, MouseButtonState::Up),
                ),
                _ => WindowsLifecycleAction::None,
            };
            apply_windows_lifecycle_action(tray.app_handle(), action);
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

const COMPANION_WINDOW_LABEL: &str = "companion";
const COMPANION_MIN_INNER_SIZE: (f64, f64) = (108.0, 116.0);
const COMPANION_INITIAL_INNER_SIZE: (f64, f64) = (168.0, 196.0);
// Two visible task rows plus the explicit overflow affordance need 520 logical px. The same
// shared limit is used by WebView2 and WKWebView, so Windows and macOS keep identical tracking.
const COMPANION_MAX_INNER_SIZE: (f64, f64) = (420.0, 520.0);

fn is_companion_window(label: &str) -> bool {
    label == COMPANION_WINDOW_LABEL
}

/// Create the process-wide companion window once, but keep it hidden until the persisted frontend
/// preference has loaded. The window intentionally has no parent so it can remain available when
/// the main window is minimized or moved to another workspace.
fn setup_companion_window(app: &tauri::AppHandle) -> tauri::Result<()> {
    if app.get_webview_window(COMPANION_WINDOW_LABEL).is_some() {
        return Ok(());
    }

    let builder = tauri::WebviewWindowBuilder::new(
        app,
        COMPANION_WINDOW_LABEL,
        tauri::WebviewUrl::App(PathBuf::from("index.html?window=companion")),
    )
    .title(format!(
        "{} Companion",
        r_code_host::app_paths::product_name()
    ))
    .inner_size(
        COMPANION_INITIAL_INNER_SIZE.0,
        COMPANION_INITIAL_INNER_SIZE.1,
    )
    .min_inner_size(COMPANION_MIN_INNER_SIZE.0, COMPANION_MIN_INNER_SIZE.1)
    .max_inner_size(COMPANION_MAX_INNER_SIZE.0, COMPANION_MAX_INNER_SIZE.1)
    .resizable(false)
    // `resizable(false)` already disables Windows left/right Aero Snap; `maximizable(false)`
    // disables the top-edge maximize trigger so dragging the assistant to the screen top no
    // longer previews/maximizes and bounces back down.
    .maximizable(false)
    .decorations(false)
    .shadow(false)
    .transparent(true)
    .background_color(tauri::webview::Color(0, 0, 0, 0))
    .always_on_top(true)
    .focusable(true)
    .focused(false)
    // The frameless assistant has an explicit, recoverable Close action. Prevent Alt+F4 from
    // destroying its only WebView behind the Settings toggle on Windows.
    .closable(false)
    .accept_first_mouse(true)
    .visible(false)
    .devtools(false);

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let builder = builder.visible_on_all_workspaces(true);

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    let builder = builder.skip_taskbar(true);

    builder.build().map(|_| ())
}

fn main() {
    let flavor = r_code_host::app_paths::AppFlavor::current();
    if let Err(error) = flavor.prepare_process_environment() {
        eprintln!(
            "{} could not initialize its isolated environment: {error}",
            flavor.product_name()
        );
        std::process::exit(2);
    }

    // MCP stdio is a protocol endpoint: it must run before the normal JSON logger is
    // initialized, otherwise a single log line would corrupt Codex's JSON-RPC stream.
    match mcp_server_data_dir_from_args() {
        Ok(Some(data_dir)) => {
            let runtime = tokio::runtime::Runtime::new().expect("create MCP Tokio runtime");
            if let Err(error) = runtime.block_on(r_code_host::mcp_server::serve_stdio(data_dir)) {
                eprintln!("R-Code MCP server failed: {error}");
                std::process::exit(1);
            }
            return;
        }
        Ok(None) => {}
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }

    // Finder/Dock 启动的 macOS GUI 不会读取用户的登录 shell 配置。Codex、Node、
    // npm 和项目工具常由 Homebrew/nvm 安装；在任何 CLI 探测前恢复同一份 PATH。
    // 失败不阻断 R-Code 启动，设置页的诊断仍会给出具体的 CLI 缺失提示。
    #[cfg(target_os = "macos")]
    if let Err(error) = fix_path_env::fix() {
        eprintln!("R-Code could not import the macOS login-shell PATH: {error}");
    }

    let mut context = tauri::generate_context!();
    if let Err(error) = flavor.apply_to_tauri_config(context.config_mut()) {
        eprintln!("{error}");
        std::process::exit(2);
    }

    r_code_host::init_logging();
    tracing::info!(
        flavor = flavor.as_str(),
        identifier = flavor.bundle_identifier(),
        "R-Code Host starting (Tauri shell)..."
    );

    let builder = tauri::Builder::default();
    // A tray application must remain single-instance: launching R-Code again while its window is
    // hidden restores the existing process instead of opening a second SQLite/MCP owner. The
    // single-instance plugin must be registered first so it can intercept before other plugins.
    #[cfg(target_os = "windows")]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        show_main_window(app);
    }));

    builder
        .plugin(
            tauri::plugin::Builder::<tauri::Wry, ()>::new("navigation-guard")
                .on_navigation(|_, url| {
                    let allow = !r_code_host::should_block_navigation(url.as_str());
                    if !allow {
                        tracing::warn!(url = %url, "blocked dangerous WebView navigation");
                    }
                    allow
                })
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        // `<a target="_blank">` 交给系统默认浏览器，避免 WebView 内部静默吞掉外链。
        .plugin(tauri_plugin_opener::init())
        .on_window_event(|_window, _event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = _event {
                if is_companion_window(_window.label()) {
                    // Alt+F4 / Cmd+W is not the companion's product-level Close action. Keep the
                    // reusable WebView alive; its context menu and Settings toggle hide it while
                    // synchronizing the persisted preference.
                    api.prevent_close();
                    return;
                }
            }

            #[cfg(target_os = "windows")]
            if let tauri::WindowEvent::CloseRequested { api, .. } = _event {
                // Hide only when a restore tray exists. Without one, explicitly exit both native
                // windows so the independent companion cannot strand a background process.
                let action = windows_close_action(
                    _window.label() == "main",
                    _window.app_handle().tray_by_id(MAIN_TRAY_ID).is_some(),
                );
                if action != WindowsLifecycleAction::None {
                    api.prevent_close();
                    apply_windows_lifecycle_action(_window.app_handle(), action);
                }
            }

            #[cfg(target_os = "macos")]
            if let tauri::WindowEvent::CloseRequested { api, .. } = _event {
                if _window.label() == "main" {
                    // Keep the main WebView (and therefore the companion navigation controller)
                    // alive behind the conventional macOS close-to-hide behavior. Dock reopen and
                    // a companion session click can restore the same window without a stale ACK.
                    api.prevent_close();
                    if let Err(error) = _window.hide() {
                        tracing::warn!(%error, "failed to hide the R-Code main window");
                    }
                }
            }

            #[cfg(target_os = "linux")]
            if let tauri::WindowEvent::CloseRequested { api, .. } = _event {
                if main_close_requires_full_exit(_window.label() == "main", false) {
                    // Linux has no tray/reopen controller in this build. The companion is an
                    // independent top-level window, so merely closing main would keep the process
                    // alive. Exit the application atomically and let Tauri close both WebViews.
                    api.prevent_close();
                    _window.app_handle().exit(0);
                }
            }
        })
        .setup(|app| {
            // 持久化状态：AppData/r-code/{db,blobs,sessions,config}
            let base = app.path().app_data_dir()?.join("r-code");
            let db_dir = base.join("db");
            let blobs_dir = base.join("blobs");
            let sessions_dir = base.join("sessions");
            let config_dir = base.join("config");
            let logs_dir = base.join("logs");
            for dir in [&db_dir, &blobs_dir, &sessions_dir, &config_dir, &logs_dir] {
                std::fs::create_dir_all(dir)?;
            }
            let retention_dir = logs_dir.clone();
            tauri::async_runtime::spawn(async move {
                let mut interval =
                    tokio::time::interval(std::time::Duration::from_secs(60 * 60));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    interval.tick().await;
                    if let Err(error) =
                        r_code_host::log_buffer::prune_expired_logs(&retention_dir)
                    {
                        tracing::warn!(%error, "failed to prune expired diagnostic logs");
                    }
                }
            });
            // 旧版 config.toml 可能仍含明文 api_key：尽早迁入当前平台凭据后端。
            // macOS 只写应用数据目录的加密凭据文件，绝不尝试读取或迁移旧 Keychain 项。
            // 凭据后端暂不可用不阻断启动，设置页会给出可见错误而不会泄露密钥。
            match r_code_host::settings::SettingsService::new(config_dir.clone())
                .migrate_legacy_provider_secrets()
            {
                Ok(count) if count > 0 => {
                    tracing::info!(count, "migrated provider secrets to credential backend")
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "provider secret migration skipped"),
            }
            match r_code_host::settings::SettingsService::new(config_dir.clone())
                .migrate_legacy_provider_kinds()
            {
                Ok(count) if count > 0 => {
                    tracing::info!(count, "persisted stable provider identities")
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "provider identity migration skipped"),
            }
            let db_path = db_dir.join("r-code.db");
            // Migration is deliberately completed before the connection pool is created. This
            // gives the upgrade path exclusive ownership of SQLite handles so it can make a
            // WAL-safe pre-migration snapshot and restore it if migration or integrity checks
            // fail. Do not replace this with `Database::open`, which is retained for isolated
            // callers and tests but has no product-level recovery boundary.
            let migration = r_code_host::MigrationManager::new(db_path.clone());
            let migration_result = tauri::async_runtime::block_on(migration.migrate()).map_err(
                |error| {
                    tracing::error!(%error, db_path = %db_path.display(), "database migration failed; desktop startup aborted");
                    error
                },
            )?;
            if let Some(backup_path) = migration_result.backup_path {
                tracing::info!(%backup_path, "created pre-migration database backup");
            }
            let db = Arc::new(r_code_store::Database::open_after_migration(&db_path)?);

            // 项目根：开发期取进程 cwd（工作区选择/切换后续里程碑）
            let project_root = std::env::current_dir()?;

            let state = CommandState::new(
                db,
                blobs_dir,
                sessions_dir,
                config_dir,
                project_root,
                Some(db_path),
            );
            // Only the authoritative desktop startup performs crash recovery. MCP sibling
            // processes also construct CommandState against the same database and must not mark
            // a live desktop continuation as interrupted.
            match state.reconcile_durable_steer_queue_claims() {
                Ok(reconciled) if reconciled > 0 => tracing::info!(
                    reconciled,
                    "reconciled durable steer queue claims without replay"
                ),
                Err(error) => {
                    tracing::warn!("failed to reconcile durable steer queue claims: {error}")
                }
                _ => {}
            }
            match state.plan_store.recover_interrupted_continuations() {
                Ok(recovered) if recovered > 0 => tracing::warn!(
                    recovered,
                    "recovered interrupted Plan continuations as visible retryable failures"
                ),
                Err(error) => tracing::warn!("failed to recover Plan continuations: {error}"),
                _ => {}
            }
            match state
                .plan_store
                .recover_interrupted_implementation_dispatches()
            {
                Ok(recovered) if recovered > 0 => tracing::warn!(
                    recovered,
                    "recovered interrupted Plan implementation handoffs as visible retryable failures"
                ),
                Err(error) => {
                    tracing::warn!("failed to recover Plan implementation handoffs: {error}")
                }
                _ => {}
            }
            // Rejection recovery is desktop-authoritative for the same reason: an MCP sibling can
            // share this database, but must never roll back paths while the desktop is active.
            match tauri::async_runtime::block_on(
                r_code_host::plan_review_tools::recover_plan_review_rejections(&state.plan_review),
            ) {
                Ok(report)
                    if !report.recovered_operation_ids.is_empty()
                        || !report.conflicted_operation_ids.is_empty()
                        || !report.retryable_operation_ids.is_empty() =>
                {
                    tracing::warn!(
                        recovered = report.recovered_operation_ids.len(),
                        conflicted = report.conflicted_operation_ids.len(),
                        retryable = report.retryable_operation_ids.len(),
                        "recovered pending Plan enhanced-review rejections"
                    );
                }
                Err(error) => {
                    tracing::warn!("failed to recover Plan enhanced-review rejections: {error}")
                }
                _ => {}
            }
            // A committed task/project deletion is authoritative even when Windows temporarily
            // keeps an AppData file open. Retry only cryptographically named Blob files and
            // canonical UUID Plan directories; database-provided paths are never deletion roots.
            let blob_store = r_code_store::BlobStore::new(&state.db, state.blobs_dir.clone());
            match blob_store.prune_unreferenced_files() {
                Ok(report) => {
                    if report.removed > 0 {
                        tracing::info!(removed = report.removed, "pruned orphan Blob files");
                    }
                    for warning in report.warnings {
                        tracing::warn!(%warning, "orphan Blob cleanup will retry at next startup");
                    }
                }
                Err(error) => tracing::warn!(%error, "failed to prune orphan Blob files"),
            }
            match state.plan_store.prune_orphan_projection_directories() {
                Ok(report) => {
                    if report.removed > 0 {
                        tracing::info!(removed = report.removed, "pruned orphan Plan projections");
                    }
                    for warning in report.warnings {
                        tracing::warn!(%warning, "orphan Plan cleanup will retry at next startup");
                    }
                }
                Err(error) => tracing::warn!(%error, "failed to prune orphan Plan projections"),
            }
            // agent 事件出口：drain 循环 → WebView（"agent-event" 信封 {task_id, event}）
            let app_handle = app.handle().clone();
            state.set_agent_event_sink(Arc::new(move |task_id, event| {
                let payload = serde_json::json!({ "task_id": task_id, "event": event });
                if let Err(e) = app_handle.emit("agent-event", payload) {
                    tracing::warn!("emit agent-event failed: {e}");
                }
            }));
            // PTY reader 只发轻量“有输出”信号；WebView 收到后按绝对游标拉取有界
            // 增量。相比固定轮询，这既消除键入回显延迟，也不会把大段输出复制进事件。
            let mut terminal_output = state.terminal_manager.subscribe_output();
            let terminal_app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match terminal_output.recv().await {
                        Ok(terminal_id) => {
                            if let Err(error) =
                                terminal_app_handle.emit("terminal-output", terminal_id)
                            {
                                tracing::warn!(%error, "emit terminal-output failed");
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(skipped, "terminal output notifications lagged");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
            // MCP settings are live. Forward only redacted lifecycle state so the settings UI can
            // reflect connecting/ready/error without polling or revealing credentials.
            let mut mcp_statuses = state.mcp_manager.subscribe_statuses();
            let mcp_app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                while mcp_statuses.changed().await.is_ok() {
                    let payload = mcp_statuses
                        .borrow_and_update()
                        .values()
                        .cloned()
                        .collect::<Vec<_>>();
                    if let Err(error) = mcp_app_handle.emit("mcp-status", payload) {
                        tracing::warn!(%error, "emit mcp-status failed");
                    }
                }
            });
            // 生产路径使用真实 provider runtime（配置缺失时 agent_send 直接报错）
            state.agent.enable_real_mode();
            match tauri::async_runtime::block_on(
                r_code_host::commands::resume_queued_dispatches(&state),
            ) {
                Ok(resumed) if resumed > 0 => {
                    tracing::info!(resumed, "resumed durable queued tasks after startup")
                }
                Err(error) => tracing::warn!("failed to resume durable queued tasks: {error}"),
                _ => {}
            }
            app.manage(state);

            if let Err(error) = setup_companion_window(app.handle()) {
                // A compositor may reject transparent/always-on-top windows. Keep the primary
                // workspace usable and expose the failure in diagnostics instead of aborting boot.
                tracing::warn!(%error, "native companion window is unavailable");
            }

            tracing::info!(data_dir = %base.display(), "CommandState initialized (persistent)");

            #[cfg(target_os = "windows")]
            if let Err(error) = setup_windows_tray(app) {
                tracing::warn!(%error, "system tray is unavailable; close will exit normally");
            }

            // 后台启动 IPC server（保留外部 CLI / agent worker 通道）
            tauri::async_runtime::spawn(async {
                if let Err(e) = run_ipc_server().await {
                    tracing::error!("IPC server exited: {e:#}");
                }
            });
            tracing::info!("Tauri setup complete; IPC server spawned in background");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ping,
            tauri_commands::cmd_app_quit,
            tauri_commands::cmd_companion_ensure,
            tauri_commands::cmd_platform_capabilities,
            tauri_commands::cmd_task_create,
            tauri_commands::cmd_project_conversation_create,
            tauri_commands::cmd_task_prepare,
            tauri_commands::cmd_task_list,
            tauri_commands::cmd_task_archive,
            tauri_commands::cmd_task_restore,
            tauri_commands::cmd_task_delete,
            tauri_commands::cmd_task_set_workspace,
            tauri_commands::cmd_task_choose_workspace,
            tauri_commands::cmd_task_set_agent_engine,
            tauri_commands::cmd_task_set_provider,
            tauri_commands::cmd_task_set_model,
            tauri_commands::cmd_task_set_inference,
            tauri_commands::cmd_task_rename,
            tauri_commands::cmd_task_update_goal,
            tauri_commands::cmd_task_set_mode,
            tauri_commands::cmd_plan_get,
            tauri_commands::cmd_plan_create,
            tauri_commands::cmd_plan_answer,
            tauri_commands::cmd_plan_retry_continuation,
            tauri_commands::cmd_plan_approve,
            tauri_commands::cmd_plan_retry_implementation,
            tauri_commands::cmd_plan_cancel,
            tauri_commands::cmd_plan_repair_projection,
            tauri_commands::cmd_plan_update_item,
            tauri_commands::cmd_plan_review_status,
            tauri_commands::cmd_plan_review_accept_file,
            tauri_commands::cmd_plan_review_accept_feature,
            tauri_commands::cmd_plan_review_reject_file,
            tauri_commands::cmd_plan_review_reject_feature,
            tauri_commands::cmd_task_fork_context,
            tauri_commands::cmd_task_clear_context,
            tauri_commands::cmd_task_compact_context,
            tauri_commands::cmd_task_detail,
            tauri_commands::cmd_task_detail_batch,
            tauri_commands::cmd_agent_send,
            tauri_commands::cmd_agent_attachment_preview,
            tauri_commands::cmd_agent_abort,
            tauri_commands::cmd_agent_abort_subagent,
            tauri_commands::cmd_agent_delegate_codex,
            tauri_commands::cmd_agent_delegate_codex_mcp,
            tauri_commands::cmd_agent_queue_list,
            tauri_commands::cmd_agent_queue_remove,
            tauri_commands::cmd_agent_queue_reorder,
            tauri_commands::cmd_agent_queue_update,
            tauri_commands::cmd_agent_queue_steer,
            tauri_commands::cmd_agent_resend,
            tauri_commands::cmd_permission_approve,
            tauri_commands::cmd_permission_pending,
            tauri_commands::cmd_notification_list,
            tauri_commands::cmd_notification_mark_read,
            tauri_commands::cmd_notification_mark_all_read,
            tauri_commands::cmd_changes_list,
            tauri_commands::cmd_change_diff,
            tauri_commands::cmd_rollback_file,
            tauri_commands::cmd_rollback_task,
            tauri_commands::cmd_rollback_task_to_checkpoint,
            tauri_commands::cmd_accept_task,
            tauri_commands::cmd_review_git_status,
            tauri_commands::cmd_review_accept_line,
            tauri_commands::cmd_review_accept_file,
            tauri_commands::cmd_review_accept_all,
            tauri_commands::cmd_review_reject_file,
            tauri_commands::cmd_git_delivery_status,
            tauri_commands::cmd_git_stage_accepted,
            tauri_commands::cmd_git_suggest_commit_message,
            tauri_commands::cmd_git_commit_task,
            tauri_commands::cmd_git_push_task,
            tauri_commands::cmd_workflow_skills_list,
            tauri_commands::cmd_workflow_skill_save,
            tauri_commands::cmd_workflow_skill_reset,
            tauri_commands::cmd_workflow_skill_delete,
            tauri_commands::cmd_workflow_skill_sync_to_global,
            tauri_commands::cmd_knowledge_prompts_get,
            tauri_commands::cmd_knowledge_prompts_save,
            tauri_commands::cmd_knowledge_prompts_reset,
            tauri_commands::cmd_change_request,
            tauri_commands::cmd_run_verification,
            tauri_commands::cmd_verification_list,
            tauri_commands::cmd_verification_output,
            tauri_commands::cmd_workspace_list,
            tauri_commands::cmd_workspace_open,
            tauri_commands::cmd_workspace_forget,
            tauri_commands::cmd_workspace_choose,
            tauri_commands::cmd_workspace_set_access_mode,
            tauri_commands::cmd_workspace_set_memory_mode,
            tauri_commands::cmd_workspace_dashboard,
            tauri_commands::cmd_project_activity_list,
            tauri_commands::cmd_activity_list,
            tauri_commands::cmd_quick_open,
            tauri_commands::cmd_global_search,
            tauri_commands::cmd_terminal_list,
            tauri_commands::cmd_terminal_create,
            tauri_commands::cmd_terminal_create_codex,
            tauri_commands::cmd_terminal_send,
            tauri_commands::cmd_terminal_read,
            tauri_commands::cmd_terminal_snapshot,
            tauri_commands::cmd_terminal_raw_snapshot,
            tauri_commands::cmd_terminal_raw_since,
            tauri_commands::cmd_terminal_kill,
            tauri_commands::cmd_terminal_resize,
            tauri_commands::cmd_recovery_data,
            tauri_commands::cmd_recovery_cleanup,
            tauri_commands::cmd_support_bundle,
            tauri_commands::cmd_support_bundle_choose,
            tauri_commands::cmd_support_preview,
            tauri_commands::cmd_file_list,
            tauri_commands::cmd_file_read,
            tauri_commands::cmd_file_write,
            tauri_commands::cmd_local_file_target,
            tauri_commands::cmd_local_image_preview,
            tauri_commands::cmd_reveal_local_path,
            tauri_commands::cmd_prepare_workbench_window,
            tauri_commands::cmd_replay,
            tauri_commands::cmd_session_messages,
            tauri_commands::cmd_session_messages_for_branch,
            tauri_commands::cmd_subagent_session_message_page,
            tauri_commands::cmd_subagent_session_messages,
            tauri_commands::cmd_memory_overview,
            tauri_commands::cmd_memory_update_settings,
            tauri_commands::cmd_memory_review_now,
            tauri_commands::cmd_memory_retry_job,
            tauri_commands::cmd_memory_cancel_job,
            tauri_commands::cmd_memory_add_entry,
            tauri_commands::cmd_memory_edit_entry,
            tauri_commands::cmd_memory_delete_entry,
            tauri_commands::cmd_memory_approve_candidate,
            tauri_commands::cmd_memory_reject_candidate,
            tauri_commands::cmd_memory_clear_all,
            tauri_commands::cmd_legacy_memory_status,
            tauri_commands::cmd_logs_tail,
            tauri_commands::cmd_settings_get,
            tauri_commands::cmd_mcp_snapshot,
            tauri_commands::cmd_mcp_upsert,
            tauri_commands::cmd_mcp_remove,
            tauri_commands::cmd_mcp_toggle,
            tauri_commands::cmd_mcp_test_connection,
            tauri_commands::cmd_mcp_credential_status,
            tauri_commands::cmd_mcp_set_credential,
            tauri_commands::cmd_mcp_delete_credential,
            tauri_commands::cmd_mcp_market_search,
            tauri_commands::cmd_mcp_market_prepare_install,
            tauri_commands::cmd_mcp_market_install,
            tauri_commands::cmd_provider_catalog,
            tauri_commands::cmd_provider_catalog,
            tauri_commands::cmd_subagent_provider_catalog,
            tauri_commands::cmd_subagent_provider_test,
            tauri_commands::cmd_subagent_provider_test_batch,
            tauri_commands::cmd_subagent_pool_snapshot,
            tauri_commands::cmd_subagent_pool_save,
            tauri_commands::cmd_provider_models,
            tauri_commands::cmd_provider_balance,
            tauri_commands::cmd_settings_set,
            tauri_commands::cmd_settings_save_provider,
            tauri_commands::cmd_settings_select_provider,
            tauri_commands::cmd_settings_delete_provider,
            tauri_commands::cmd_rtk_status,
            tauri_commands::cmd_rtk_set_enabled,
            tauri_commands::cmd_request_audit_counters,
            tauri_commands::cmd_rtk_open_security_exclusions,
            tauri_commands::cmd_codex_integration_status,
            tauri_commands::cmd_codex_install_cli,
            tauri_commands::cmd_codex_start_login,
            tauri_commands::cmd_codex_start_device_login,
            tauri_commands::cmd_codex_install_skill,
            tauri_commands::cmd_codex_install_mcp_server,
            tauri_commands::cmd_codex_setup_collaboration,
            tauri_commands::cmd_codex_cli_preferences,
            tauri_commands::cmd_codex_save_cli_preferences,
        ])
        .build(context)
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            #[cfg(target_os = "macos")]
            if matches!(event, tauri::RunEvent::Reopen { .. }) {
                if let Some(window) = app_handle.get_webview_window("main") {
                    if let Err(error) = window.show() {
                        tracing::warn!(%error, "failed to reopen the R-Code main window");
                    }
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
            }
            if matches!(event, tauri::RunEvent::Exit) {
                let state = app_handle.state::<CommandState>();
                let manager = state.mcp_manager.clone();
                let codex_app_server = state.codex_app_server.clone();
                let outcome = tauri::async_runtime::block_on(async move {
                    tokio::time::timeout(std::time::Duration::from_secs(2), async move {
                        let (mcp_result, ()) =
                            tokio::join!(manager.shutdown(), codex_app_server.shutdown());
                        mcp_result
                    })
                    .await
                });
                match outcome {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::warn!(%error, "MCP shutdown reported an error"),
                    Err(_) => tracing::warn!("MCP/Codex transport shutdown timed out after two seconds"),
                }
            }
        });
}

/// 解析 `r-code-host mcp-server [--data-dir <path>]`。
///
/// 该小型 parser 不引入 CLI 框架，且只在第一个参数明确为 `mcp-server` 时接管
/// 启动；普通 Tauri 命令行参数完全保持原状。
fn mcp_server_data_dir_from_args() -> Result<Option<Option<PathBuf>>, String> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(OsStr::new("mcp-server")) {
        return Ok(None);
    }
    let mut data_dir = None;
    while let Some(argument) = args.next() {
        if argument == OsStr::new("--data-dir") {
            let path = args
                .next()
                .ok_or_else(|| "--data-dir requires a path".to_string())?;
            if data_dir.replace(PathBuf::from(path)).is_some() {
                return Err("--data-dir can only be specified once".to_string());
            }
        } else {
            return Err("usage: r-code-host mcp-server [--data-dir <path>]".to_string());
        }
    }
    Ok(Some(data_dir))
}

/// 启动后台 IPC server（保留 P0 行为：内存 SQLite + ping / task.create）。
async fn run_ipc_server() -> anyhow::Result<()> {
    let _db = r_code_store::Database::open_in_memory()?;
    tracing::info!("Database initialized");

    let socket_path = std::env::temp_dir().join(format!("r-code-{}.sock", std::process::id()));
    let mut server = IpcServer::bind(socket_path.clone())?;

    server.register("ping", Arc::new(r_code_host::ipc::PingHandler));
    server.register("task.create", Arc::new(r_code_host::ipc::TaskCreateHandler));

    tracing::info!("IPC server listening on {}", socket_path.display());
    server.serve().await?;

    Ok(())
}

#[cfg(test)]
mod companion_window_tests {
    use super::*;

    #[test]
    fn companion_dynamic_sizes_stay_inside_the_native_contract() {
        assert_eq!(COMPANION_MIN_INNER_SIZE, (108.0, 116.0));
        assert_eq!(COMPANION_INITIAL_INNER_SIZE, (168.0, 196.0));
        assert_eq!(COMPANION_MAX_INNER_SIZE, (420.0, 520.0));
        assert!(COMPANION_MIN_INNER_SIZE.0 <= COMPANION_INITIAL_INNER_SIZE.0);
        assert!(COMPANION_MIN_INNER_SIZE.1 <= COMPANION_INITIAL_INNER_SIZE.1);
        assert!(COMPANION_INITIAL_INNER_SIZE.0 <= COMPANION_MAX_INNER_SIZE.0);
        assert!(COMPANION_INITIAL_INNER_SIZE.1 <= COMPANION_MAX_INNER_SIZE.1);
    }

    #[test]
    fn independent_companion_never_strands_mainless_processes() {
        assert!(main_close_requires_full_exit(true, false));
        assert!(!main_close_requires_full_exit(true, true));
        assert!(!main_close_requires_full_exit(false, false));
    }
}

#[cfg(all(test, target_os = "windows"))]
mod windows_tests {
    use super::*;

    #[test]
    fn main_close_hides_only_when_a_restore_tray_is_available() {
        assert_eq!(
            windows_close_action(true, true),
            WindowsLifecycleAction::HideToTray
        );
        assert_eq!(
            windows_close_action(true, false),
            WindowsLifecycleAction::Quit,
            "without a tray affordance both native windows must exit"
        );
        assert_eq!(
            windows_close_action(false, true),
            WindowsLifecycleAction::None,
            "secondary windows must retain their native close behavior"
        );
    }

    #[test]
    fn tray_menu_keeps_restore_hide_and_explicit_quit_distinct() {
        assert_eq!(
            windows_tray_menu_action(TRAY_SHOW_ID),
            WindowsLifecycleAction::Restore
        );
        assert_eq!(
            windows_tray_menu_action(TRAY_HIDE_ID),
            WindowsLifecycleAction::HideToTray
        );
        assert_eq!(
            windows_tray_menu_action(TRAY_QUIT_ID),
            WindowsLifecycleAction::Quit
        );
        assert_eq!(
            windows_tray_menu_action("unknown-menu-item"),
            WindowsLifecycleAction::None
        );
    }

    #[test]
    fn only_a_released_left_tray_click_restores_the_window() {
        assert_eq!(
            windows_tray_click_action(true, true),
            WindowsLifecycleAction::Restore
        );
        assert_eq!(
            windows_tray_click_action(true, false),
            WindowsLifecycleAction::None
        );
        assert_eq!(
            windows_tray_click_action(false, true),
            WindowsLifecycleAction::None
        );
    }
}

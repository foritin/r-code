//! R-Code Host 二进制入口 -- Tauri 应用壳。
//!
//! 启动流程：
//! 1. 初始化结构化日志（stdout + 内存环形缓冲）
//! 2. 启动 Tauri 应用壳（加载前端 WebView）
//! 3. 在 setup hook 中创建持久化 CommandState（AppData/r-code）+ 后台启动 IPC server
//! 4. 注册 Tauri 命令（前端通过 invoke 调用）

use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::Arc;

use hermes_ipc::IpcServer;
use r_code_host::commands::CommandState;
use tauri::{Emitter, Manager};

mod tauri_commands;

/// Tauri 命令：ping -- 验证前后端 IPC 通道。
#[tauri::command]
fn ping() -> bool {
    true
}

fn main() {
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

    r_code_host::init_logging();
    tracing::info!("R-Code Host starting (Tauri shell)...");

    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // 持久化状态：AppData/r-code/{db,blobs,sessions,config}
            let base = app.path().app_data_dir()?.join("r-code");
            let db_dir = base.join("db");
            let blobs_dir = base.join("blobs");
            let sessions_dir = base.join("sessions");
            let config_dir = base.join("config");
            for dir in [&db_dir, &blobs_dir, &sessions_dir, &config_dir] {
                std::fs::create_dir_all(dir)?;
            }
            // 旧版 config.toml 可能仍含明文 api_key：尽早迁入系统凭据库。
            // Keychain 暂不可用不阻断启动，设置页会给出可见错误而不会泄露密钥。
            match r_code_host::settings::SettingsService::new(config_dir.clone())
                .migrate_legacy_provider_secrets()
            {
                Ok(count) if count > 0 => {
                    tracing::info!(count, "migrated provider secrets to OS keychain")
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "provider secret migration skipped"),
            }
            let db_path = db_dir.join("r-code.db");
            let db = Arc::new(r_code_store::Database::open(&db_path)?);

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
            // agent 事件出口：drain 循环 → WebView（"agent-event" 信封 {task_id, event}）
            let app_handle = app.handle().clone();
            state.set_agent_event_sink(Arc::new(move |task_id, event| {
                let payload = serde_json::json!({ "task_id": task_id, "event": event });
                if let Err(e) = app_handle.emit("agent-event", payload) {
                    tracing::warn!("emit agent-event failed: {e}");
                }
            }));
            // 生产路径使用真实 provider runtime（配置缺失时 agent_send 直接报错）
            state.agent.blocking_lock().enable_real_mode();
            app.manage(state);

            tracing::info!(data_dir = %base.display(), "CommandState initialized (persistent)");

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
            tauri_commands::cmd_task_create,
            tauri_commands::cmd_task_list,
            tauri_commands::cmd_task_archive,
            tauri_commands::cmd_task_delete,
            tauri_commands::cmd_task_set_workspace,
            tauri_commands::cmd_task_set_agent_engine,
            tauri_commands::cmd_task_set_provider,
            tauri_commands::cmd_task_set_model,
            tauri_commands::cmd_task_set_inference,
            tauri_commands::cmd_task_rename,
            tauri_commands::cmd_task_fork_context,
            tauri_commands::cmd_task_compact_context,
            tauri_commands::cmd_task_detail,
            tauri_commands::cmd_task_detail_batch,
            tauri_commands::cmd_agent_send,
            tauri_commands::cmd_agent_abort,
            tauri_commands::cmd_agent_abort_subagent,
            tauri_commands::cmd_agent_delegate_codex,
            tauri_commands::cmd_agent_delegate_codex_mcp,
            tauri_commands::cmd_agent_queue_list,
            tauri_commands::cmd_agent_queue_remove,
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
            tauri_commands::cmd_accept_task,
            tauri_commands::cmd_change_request,
            tauri_commands::cmd_run_verification,
            tauri_commands::cmd_verification_list,
            tauri_commands::cmd_verification_output,
            tauri_commands::cmd_workspace_list,
            tauri_commands::cmd_workspace_open,
            tauri_commands::cmd_workspace_choose,
            tauri_commands::cmd_workspace_set_access_mode,
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
            tauri_commands::cmd_support_preview,
            tauri_commands::cmd_file_list,
            tauri_commands::cmd_file_read,
            tauri_commands::cmd_file_write,
            tauri_commands::cmd_replay,
            tauri_commands::cmd_session_messages,
            tauri_commands::cmd_subagent_session_messages,
            tauri_commands::cmd_memory_get,
            tauri_commands::cmd_memory_set,
            tauri_commands::cmd_logs_tail,
            tauri_commands::cmd_settings_get,
            tauri_commands::cmd_provider_catalog,
            tauri_commands::cmd_provider_models,
            tauri_commands::cmd_settings_set,
            tauri_commands::cmd_settings_save_provider,
            tauri_commands::cmd_settings_select_provider,
            tauri_commands::cmd_settings_delete_provider,
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
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
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

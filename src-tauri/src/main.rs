#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

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

    // Finder/Dock 启动的 macOS GUI 不会读取用户的登录 shell 配置。Codex、Node、
    // npm 和项目工具常由 Homebrew/nvm 安装；在任何 CLI 探测前恢复同一份 PATH。
    // 失败不阻断 R-Code 启动，设置页的诊断仍会给出具体的 CLI 缺失提示。
    #[cfg(target_os = "macos")]
    if let Err(error) = fix_path_env::fix() {
        eprintln!("R-Code could not import the macOS login-shell PATH: {error}");
    }

    r_code_host::init_logging();
    tracing::info!("R-Code Host starting (Tauri shell)...");

    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        // `<a target="_blank">` 交给系统默认浏览器，避免 WebView 内部静默吞掉外链。
        .plugin(tauri_plugin_opener::init())
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
            // Only the authoritative desktop startup performs crash recovery. MCP sibling
            // processes also construct CommandState against the same database and must not mark
            // a live desktop continuation as interrupted.
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
            tauri_commands::cmd_task_restore,
            tauri_commands::cmd_task_delete,
            tauri_commands::cmd_task_set_workspace,
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
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                let manager = app_handle.state::<CommandState>().mcp_manager.clone();
                let outcome = tauri::async_runtime::block_on(async move {
                    tokio::time::timeout(std::time::Duration::from_secs(2), manager.shutdown())
                        .await
                });
                match outcome {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::warn!(%error, "MCP shutdown reported an error"),
                    Err(_) => tracing::warn!("MCP shutdown timed out after two seconds"),
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

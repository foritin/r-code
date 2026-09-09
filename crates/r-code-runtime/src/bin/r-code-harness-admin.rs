//! r-code-harness-admin: headless admin CLI over the same ApplicationService
//! the daemon serves (real catalog/store/task surface — UIs and scripts ride
//! the identical operations).
//!
//! Usage: r-code-harness-admin --profile development --data-root <dir>
//!        [--ipc-name <name>] <command> [args]
//! Commands: install <pkg-dir> | list | enable <id> <digest> |
//!           disable <id> <digest> | remove <id> <digest> |
//!           task-create <task-id> <objective> | select <task-id> <harness-id>

use r_code_runtime::application::ApplicationService;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::path::Path;
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut flavor = None;
    let mut data_root: Option<std::path::PathBuf> = None;
    let mut ipc_name: Option<String> = None;
    let mut command: Vec<String> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--profile" => {
                flavor = Some(match args.get(index + 1).map(String::as_str) {
                    Some("production") | Some("prod") => ProfileFlavor::Production,
                    _ => ProfileFlavor::Development,
                });
                index += 2;
            }
            "--data-root" => {
                data_root = args.get(index + 1).map(std::path::PathBuf::from);
                index += 2;
            }
            "--ipc-name" => {
                ipc_name = args.get(index + 1).cloned();
                index += 2;
            }
            other => {
                command.push(other.to_string());
                index += 1;
            }
        }
    }
    let mut options = LaunchOptions::new(flavor.unwrap_or(ProfileFlavor::Development));
    if let Some(root) = data_root {
        options = options.with_data_root(root);
    }
    if let Some(name) = ipc_name {
        options = options.with_ipc_name(name);
    }
    let profile = match RuntimeProfile::resolve(&options) {
        Ok(profile) => profile,
        Err(error) => {
            eprintln!("admin: {error}");
            std::process::exit(2);
        }
    };
    let service = match ApplicationService::compose(
        &profile,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
    ) {
        Ok(service) => service,
        Err(error) => {
            eprintln!("admin: {error}");
            std::process::exit(3);
        }
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async move {
        let outcome = run(&service, &command).await;
        match outcome {
            Ok(json) => {
                println!("{json}");
            }
            Err(message) => {
                eprintln!("admin: {message}");
                std::process::exit(1);
            }
        }
    });
}

async fn run(service: &ApplicationService, command: &[String]) -> Result<String, String> {
    match (
        command.first().map(String::as_str),
        command.get(1).map(String::as_str),
    ) {
        (Some("install"), Some(path)) => {
            let installed = service
                .install_package_from_directory(Path::new(path))
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({
                "id": installed.manifest.id.0,
                "version": installed.manifest.version.to_string(),
                "contentDigest": installed.package_ref.content_digest,
            })
            .to_string())
        }
        (Some("list"), _) => {
            let entries = service.list_plugins().map_err(|e| e.to_string())?;
            Ok(serde_json::to_string(&entries).unwrap_or_default())
        }
        (Some("enable"), Some(id)) | (Some("disable"), Some(id)) => {
            let digest = command.get(2).ok_or("missing digest")?;
            let enabled = command.first().map(String::as_str) == Some("enable");
            service
                .set_plugin_enabled(id, digest, enabled)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"id": id, "enabled": enabled}).to_string())
        }
        (Some("remove"), Some(id)) => {
            let digest = command.get(2).ok_or("missing digest")?;
            service
                .remove_package(id, digest)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"removed": id}).to_string())
        }
        (Some("task-create"), Some(task_id)) => {
            let objective = command.get(2).map(String::as_str).unwrap_or("");
            let state = service
                .create_task(
                    task_id,
                    objective,
                    r_code_kernel::task::TaskKind::Conversation,
                    vec![],
                )
                .await
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"taskId": state.contract.task_id}).to_string())
        }
        (Some("select"), Some(task_id)) => {
            let harness = command.get(2).ok_or("missing harness id")?;
            let package = service
                .select_harness(task_id, harness)
                .await
                .map_err(|e| e.to_string())?;
            Ok(
                serde_json::json!({"pinned": package.id.0, "digest": package.content_digest})
                    .to_string(),
            )
        }
        (Some(other), _) => Err(format!("unknown command {other:?}")),
        (None, _) => Err("missing command".into()),
    }
}

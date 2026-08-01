use std::path::Path;

fn production_source(name: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(name))
        .unwrap_or_else(|error| panic!("failed to read production source {name}: {error}"))
}

fn frontend_production_source() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/src");
    let mut pending = vec![root];
    let mut source = String::new();

    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("ts" | "tsx" | "css")
            ) {
                source.push_str(&std::fs::read_to_string(path).unwrap());
                source.push('\n');
            }
        }
    }
    source
}

#[test]
fn retired_tauri_commands_are_unregistered_and_have_no_backend_surface() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        !manifest.join("src/project_memory.rs").exists(),
        "the legacy ProjectMemory implementation must stay retired"
    );

    let main = production_source("main.rs");
    let tauri = production_source("tauri_commands.rs");
    let commands = production_source("commands.rs");
    let library = production_source("lib.rs");
    let detector = production_source("legacy_memory.rs");
    let detector_production = detector
        .split_once("#[cfg(test)]")
        .map_or(detector.as_str(), |(production, _)| production);

    assert!(main.contains("tauri_commands::cmd_legacy_memory_status"));
    assert!(tauri.contains("pub async fn cmd_legacy_memory_status"));
    assert!(commands.contains("pub async fn legacy_memory_status"));

    for retired in ["cmd_memory_get", "cmd_memory_set"] {
        assert!(
            !main.contains(retired),
            "retired command {retired} is still registered; an unknown invoke would not be command-not-found"
        );
        assert!(
            !tauri.contains(retired),
            "retired Tauri wrapper {retired} is still callable"
        );
    }

    for retired in [
        "ProjectMemory",
        "pub async fn memory_get",
        "pub async fn memory_set",
        "generate_preamble",
        "sync_to_gitignore",
        "sync_to_claude",
        "sync_to_agents",
    ] {
        assert!(
            !commands.contains(retired),
            "retired workspace-memory production surface remains in commands.rs: {retired}"
        );
        assert!(
            !library.contains(retired),
            "retired workspace-memory production surface remains exported by lib.rs: {retired}"
        );
    }

    assert!(detector_production.contains("std::fs::symlink_metadata"));
    assert!(detector_production
        .contains(".args([\"ls-files\", \"--error-unmatch\", \"--\", LEGACY_MEMORY_PATH])"));
    for forbidden in [
        "std::fs::read(",
        "std::fs::read_to_string",
        "File::open",
        "OpenOptions",
        "std::fs::write",
        "remove_file",
        "remove_dir",
        "git add",
        "git rm",
    ] {
        assert!(
            !detector_production.contains(forbidden),
            "metadata-only detector contains a file-content or write surface: {forbidden}"
        );
    }
}

#[test]
fn frontend_keeps_only_the_read_only_legacy_status_surface() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let frontend = frontend_production_source();
    let ipc = std::fs::read_to_string(manifest.join("frontend/src/lib/ipc.ts")).unwrap();
    let projects =
        std::fs::read_to_string(manifest.join("frontend/src/components/scenes/ProjectsScene.tsx"))
            .unwrap();

    assert!(ipc.contains("cmd_legacy_memory_status"));
    for retired in [
        "cmd_memory_get",
        "cmd_memory_set",
        "memoryGet",
        "memorySet",
        "memoryByWorkspace",
    ] {
        assert!(
            !frontend.contains(retired),
            "retired frontend memory surface remains reachable: {retired}"
        );
    }

    for retired_editor_surface in [
        "<textarea",
        "memory-editor",
        "保存记忆",
        "保存在当前附加工作区",
        "记录架构约定、开发偏好与重要上下文",
    ] {
        assert!(
            !projects.contains(retired_editor_surface),
            "the Projects memory notice still exposes the retired editor: {retired_editor_surface}"
        );
    }
}

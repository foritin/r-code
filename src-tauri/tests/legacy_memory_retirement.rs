use std::path::Path;

fn production_source(name: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(name))
        .unwrap_or_else(|error| panic!("failed to read production source {name}: {error}"))
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

use std::{fs, path::Path};

#[test]
fn tauri_watcher_ignores_vite_generated_directories() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ignore_file = manifest_dir.join(".taurignore");
    let contents = fs::read_to_string(&ignore_file)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", ignore_file.display()));

    let rules: Vec<_> = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();

    assert!(
        rules.contains(&"frontend/dist/"),
        "Vite build output must not restart the Tauri host"
    );
    assert!(
        rules.contains(&"frontend/node_modules/"),
        "Vite dependency cache writes must not restart the Tauri host"
    );
}

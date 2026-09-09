//! T04a — explicit isolated RuntimeProfile.
//!
//! Acceptance: fresh v2 startup and both client flavors leave old
//! DB/config/JSONL unchanged and resolve identical intended profile
//! identities.

use r_code_runtime::{IpcEndpoint, LaunchOptions, ProfileError, ProfileFlavor, RuntimeProfile};
use std::path::Path;

fn hash_of(path: &Path) -> u128 {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    // FNV-1a is plenty for change detection in tests.
    bytes
        .iter()
        .fold(1_469_598_103_934_665_603_128u128, |acc, byte| {
            (acc ^ *byte as u128).wrapping_mul(1099511628211)
        })
}

fn legacy_layout(root: &Path) {
    std::fs::create_dir_all(root).expect("legacy root");
    std::fs::write(root.join("db.sqlite3"), b"legacy-sqlite-bytes").unwrap();
    std::fs::write(root.join("config.json"), br#"{"provider":"legacy"}"#).unwrap();
    std::fs::write(root.join("history.jsonl"), b"{\"a\":1}\n{\"a\":2}\n").unwrap();
}

#[test]
fn fresh_v2_startup_leaves_legacy_data_untouched() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("data-root");
    legacy_layout(&root);

    let before: Vec<_> = ["db.sqlite3", "config.json", "history.jsonl"]
        .iter()
        .map(|name| (name.to_string(), hash_of(&root.join(name))))
        .collect();

    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development).with_data_root(&root),
    )
    .expect("resolve");
    profile.ensure_layout().expect("fresh v2 layout");

    // All v2 state is strictly below <data_root>/harness-v2.
    let v2 = profile.harness_v2_root();
    assert!(v2.starts_with(&root));
    assert!(v2.ends_with("harness-v2"));
    assert!(profile.database_path().starts_with(&v2));
    assert!(profile.plugins_root().starts_with(&v2));
    assert!(profile.blobs_root().starts_with(&v2));
    assert!(profile.checkpoints_root().starts_with(&v2));
    assert!(v2.is_dir(), "v2 layout created");
    assert!(profile.database_path().parent().unwrap().is_dir());

    // Legacy bytes are bit-identical.
    for (name, digest) in before {
        assert_eq!(hash_of(&root.join(&name)), digest, "{name} mutated");
    }

    // No v2 path ever escapes the harness-v2 subtree of the data root.
    assert!(profile.database_path().starts_with(&v2));
}

#[test]
fn both_flavors_resolve_distinct_but_stable_identities() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dev = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development).with_data_root(temp.path().join("dev")),
    )
    .unwrap();
    let prod = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Production).with_data_root(temp.path().join("prod")),
    )
    .unwrap();

    assert_ne!(dev.profile_id(), prod.profile_id());
    assert_ne!(dev.harness_v2_root(), prod.harness_v2_root());
    assert_ne!(dev.credential_service(), prod.credential_service());
    assert_ne!(dev.ipc_endpoint(), prod.ipc_endpoint());
    assert_eq!(dev.profile_id(), "harness-v2/development");
    assert_eq!(prod.profile_id(), "harness-v2/production");
    assert_eq!(dev.credential_service(), "r-code-harness-v2-dev");
    assert_eq!(prod.credential_service(), "r-code-harness-v2");

    // Repeated resolution is deterministic (same identity every launch).
    let dev_again = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development).with_data_root(temp.path().join("dev")),
    )
    .unwrap();
    assert_eq!(dev, dev_again);

    // Endpoints are user-scoped per flavor.
    match (dev.ipc_endpoint(), prod.ipc_endpoint()) {
        (IpcEndpoint::NamedPipe { name: a }, IpcEndpoint::NamedPipe { name: b }) => {
            assert!(a.contains("development"));
            assert!(b.contains("production"));
            assert!(a.starts_with(r"\\.\pipe\r-code-harness-v2-"));
        }
        (IpcEndpoint::UnixSocket { path: a }, IpcEndpoint::UnixSocket { path: b }) => {
            assert!(a.to_string_lossy().contains("development"));
            assert!(b.to_string_lossy().contains("production"));
        }
        _ => panic!("mixed endpoint kinds"),
    }
}

#[test]
fn flavor_is_explicit_and_never_inferred() {
    // Without --profile and without a default, construction fails loudly.
    let args: Vec<String> = vec![];
    assert_eq!(
        LaunchOptions::parse_args(&args),
        Err(ProfileError::AmbiguousFlavor)
    );

    // Unknown values are rejected with the valid set.
    let bad = ["--profile".to_string(), "tauri-dev".to_string()];
    assert!(matches!(
        LaunchOptions::parse_args(&bad),
        Err(ProfileError::UnknownFlavor(value)) if value == "tauri-dev"
    ));

    // Explicit values parse, including short spellings.
    for (value, flavor) in [
        ("development", ProfileFlavor::Development),
        ("dev", ProfileFlavor::Development),
        ("production", ProfileFlavor::Production),
        ("prod", ProfileFlavor::Production),
        ("PRODUCTION", ProfileFlavor::Production),
    ] {
        let args = ["--profile".to_string(), value.to_string()];
        let options = LaunchOptions::parse_args(&args).expect("parse");
        assert_eq!(options.flavor, flavor);
    }

    // Unknown extra flags are refused rather than ignored.
    let extra = [
        "--profile".to_string(),
        "dev".to_string(),
        "--tauri-infer".to_string(),
    ];
    assert!(matches!(
        LaunchOptions::parse_args(&extra),
        Err(ProfileError::UnexpectedArgument(_))
    ));

    // data-root override participates in identity resolution.
    let args = [
        "--profile".to_string(),
        "dev".to_string(),
        "--data-root".to_string(),
        "X:/custom/root".to_string(),
    ];
    let options = LaunchOptions::parse_args(&args).expect("parse");
    let profile = RuntimeProfile::resolve(&options).expect("resolve");
    assert_eq!(profile.data_root(), Path::new("X:/custom/root"));
    assert_eq!(profile.profile_id(), "harness-v2/development");
}

#[test]
fn default_data_roots_match_flavor_identifiers() {
    // The default-root table mirrors the desktop flavor identifiers so the
    // v2 tree grows inside the same per-flavor AppData root.
    let root = Path::new("appdata");
    let dev = ProfileFlavor::Development.data_root_under(root);
    let prod = ProfileFlavor::Production.data_root_under(root);
    assert_ne!(dev, prod);
    let dev_text = dev.to_string_lossy().replace('\\', "/");
    let prod_text = prod.to_string_lossy().replace('\\', "/");
    assert!(dev_text.ends_with("dev/r-code") || dev_text.ends_with("app.dev/r-code"));
    assert!(prod_text.ends_with("app/r-code") || prod_text.ends_with("desktop/r-code"));
}

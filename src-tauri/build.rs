//! Tauri build script。
//!
//! `tauri.conf.json` 与本 `build.rs` 同处 `src-tauri/` 目录，
//! `tauri-build` 默认即可找到。若未来将 config 移回 workspace 根目录，
//! 需要升级 tauri-build 到 2.7+ 并改用 `Attributes::config_path`。
//!
//! 历史说明：这里曾把 `eval/plan-eval/artifacts/manifest.json` 嵌入 `OUT_DIR`
//! 作为规划建议的证据门；证据门已于 2026-08-22 移除（见
//! docs/support/archive/implementation/settings-ux-and-image-understanding.md A3），构建不再读取评估产物，
//! `eval/plan-eval/` 降级为可选的事后质量回归工具。

mod build_support;

/// M8-04（R-DST-01）：bundle.externalBin 声明的 `binaries/r-code-tui` 需要
/// `<name>-<target-triple>.exe` 产物存在于 src-tauri/binaries/。普通开发构建
/// （cargo build / cargo test）不先构建 CLI 时，这里放一个占位文件满足
/// tauri-build 的存在性检查。打包入口必须设置 `R_CODE_TAURI_PACKAGING=1`；
/// 该模式拒绝占位文件，并校验真实产物的大小与目标平台魔数。
/// Harness v2 sidecars shipped beside the app (T38): the shared daemon and
/// the two built-in harness plugin executables.
const HARNESS_SIDECARS: &[&str] = &[
    "r-code-service",
    "r-code-harness-native",
    "r-code-harness-codex",
];

fn prepare_external_bin_placeholder() {
    use std::path::PathBuf;
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let binaries = manifest.join("binaries");
    std::fs::create_dir_all(&binaries).expect("create src-tauri/binaries");
    let triple = std::env::var("TARGET").expect("Cargo must provide TARGET to build.rs");
    let suffix = if triple.contains("-windows-") {
        ".exe"
    } else {
        ""
    };
    println!("cargo:rerun-if-env-changed=R_CODE_TAURI_PACKAGING");

    let packaging = matches!(
        std::env::var("R_CODE_TAURI_PACKAGING").as_deref(),
        Ok("1" | "true")
    );
    let mut sidecars: Vec<&str> = vec!["r-code-tui"];
    sidecars.extend(HARNESS_SIDECARS);
    for name in sidecars {
        let exe = binaries.join(format!("{name}-{triple}{suffix}"));
        println!("cargo:rerun-if-changed={}", exe.display());
        if packaging {
            build_support::validate_external_binary(&exe, &triple).unwrap_or_else(|error| {
                panic!(
                    "refusing to package an invalid {name} sidecar: {error}. Build the target-specific binary and copy it to src-tauri/binaries before cargo tauri build"
                )
            });
        } else if !exe.exists() {
            std::fs::write(
                &exe,
                format!(
                    "# placeholder for tauri-build existence check; replaced by the real {name} binary during packaging\n"
                )
                .as_bytes(),
            )
            .expect("write development external-bin placeholder");
        }
    }
    // 占位/产物都不入库（真实二进制由打包脚本生成）。
    let gitignore = binaries.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, b"*\n!.gitignore\n")
            .expect("write src-tauri/binaries/.gitignore");
    }
}

fn main() {
    // `tauri-build` embeds the Windows icon in the executable, but Cargo does not
    // otherwise know that this file lives outside the package directory. Without
    // an explicit dependency edge, `cargo tauri dev` can keep linking the old
    // icon after `icons/icon.ico` is regenerated.
    println!("cargo:rerun-if-changed=../icons/icon.ico");

    prepare_external_bin_placeholder();
    tauri_build::build();
}

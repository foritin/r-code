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

/// M8-04（R-DST-01）：bundle.externalBin 声明的 `binaries/r-code-tui` 需要
/// `<name>-<target-triple>.exe` 产物存在于 src-tauri/binaries/。开发机构建
/// （cargo build / cargo test）不先构建 CLI 时，这里放一个占位文件满足
/// tauri-build 的存在性检查；真实发布管线（release.mjs / CI）先构建
/// r-code-tui 再打包，占位文件被真实产物覆盖——占位内容是文本而非可执行
/// 镜像，误打包会在安装期立即暴露而非静默运行。
fn prepare_external_bin_placeholder() {
    use std::path::PathBuf;
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let binaries = manifest.join("binaries");
    let _ = std::fs::create_dir_all(&binaries);
    // 目标三元组优先取 tauri 注入（打包路径的权威值），缺省退回 Cargo TARGET
    // （只决定占位文件名；真实打包时由发布管线覆盖）。
    let triple = std::env::var("TAURI_ENV_TARGET_TRIPLE")
        .or_else(|_| std::env::var("TARGET"))
        .unwrap_or_else(|_| "unknown-target".to_string());
    let exe = binaries.join(format!(
        "r-code-tui-{triple}{}",
        std::env::consts::EXE_SUFFIX
    ));
    if !exe.exists() {
        let _ = std::fs::write(
            &exe,
            b"# placeholder for tauri-build existence check; replaced by the real r-code-tui binary during packaging\n",
        );
    }
    // 占位/产物都不入库（真实二进制由打包脚本生成）。
    let gitignore = binaries.join(".gitignore");
    if !gitignore.exists() {
        let _ = std::fs::write(&gitignore, b"*\n!.gitignore\n");
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

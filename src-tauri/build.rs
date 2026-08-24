//! Tauri build script。
//!
//! `tauri.conf.json` 与本 `build.rs` 同处 `src-tauri/` 目录，
//! `tauri-build` 默认即可找到。若未来将 config 移回 workspace 根目录，
//! 需要升级 tauri-build 到 2.7+ 并改用 `Attributes::config_path`。
//!
//! 历史说明：这里曾把 `eval/plan-eval/artifacts/manifest.json` 嵌入 `OUT_DIR`
//! 作为规划建议的证据门；证据门已于 2026-08-22 移除（见
//! docs/archive/implementation/settings-ux-and-image-understanding.md A3），构建不再读取评估产物，
//! `eval/plan-eval/` 降级为可选的事后质量回归工具。

fn main() {
    // `tauri-build` embeds the Windows icon in the executable, but Cargo does not
    // otherwise know that this file lives outside the package directory. Without an
    // explicit dependency edge, `cargo tauri dev` can keep linking the old icon
    // after `icons/icon.ico` is regenerated.
    println!("cargo:rerun-if-changed=../icons/icon.ico");

    tauri_build::build();
}

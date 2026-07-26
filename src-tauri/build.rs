//! Tauri build script。
//!
//! `tauri.conf.json` 与本 `build.rs` 同处 `src-tauri/` 目录，
//! `tauri-build` 默认即可找到。若未来将 config 移回 workspace 根目录，
//! 需要升级 tauri-build 到 2.7+ 并改用 `Attributes::config_path`。

fn main() {
    tauri_build::build();
}

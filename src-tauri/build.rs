//! Tauri build script。
//!
//! `tauri.conf.json` 与本 `build.rs` 同处 `src-tauri/` 目录，
//! `tauri-build` 默认即可找到。若未来将 config 移回 workspace 根目录，
//! 需要升级 tauri-build 到 2.7+ 并改用 `Attributes::config_path`。

fn main() {
    // `tauri-build` embeds the Windows icon in the executable, but Cargo does not
    // otherwise know that this file lives outside the package directory. Without
    // an explicit dependency edge, `cargo tauri dev` can keep linking the old icon
    // after `icons/icon.ico` is regenerated.
    println!("cargo:rerun-if-changed=../icons/icon.ico");

    embed_plan_evidence_manifest();
    tauri_build::build();
}

/// Plan 证据门（docs/plan-mode-dual-track-gate.md §14.1、§16）：build 时把匹配的
/// DeepSeek 证据 manifest 复制进 `OUT_DIR`，宿主 `plan_policy` 再用 include_str!
/// 嵌入。manifest 缺失或结构不完整时嵌入 `null` 并给出告警——发布资格在运行时
/// fail closed，绝不让构建产物携带半份证据。
fn embed_plan_evidence_manifest() {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../eval/plan-eval/artifacts/manifest.json");
    println!(
        "cargo:rerun-if-changed={}",
        source.to_string_lossy().replace('\\', "/")
    );
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let target = out_dir.join("plan_evidence_manifest.json");
    let payload = match std::fs::read_to_string(&source) {
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) => {
                if value.is_null() {
                    println!("cargo:warning=plan evidence manifest is null; validated planning stays disabled");
                    "null".to_string()
                } else {
                    text
                }
            }
            Err(error) => {
                println!(
                        "cargo:warning=plan evidence manifest is not valid JSON ({error}); embedding null"
                    );
                "null".to_string()
            }
        },
        Err(_) => {
            // 首发默认路径：真实三臂证据尚未产生，validated 恒为关闭。
            "null".to_string()
        }
    };
    if let Err(error) = std::fs::write(&target, payload) {
        println!("cargo:warning=failed to embed plan evidence manifest: {error}");
    }
}

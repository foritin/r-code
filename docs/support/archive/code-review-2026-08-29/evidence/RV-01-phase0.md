# RV-01 evidence（阶段0 快照）

- 时间：2026-08-29
- 分支：feat/code-review-2026-08-29（自 main 49f9193 新建）
- git 快照：见 phase0-git-status.txt
- 内部依赖边抽取命令：
  `for f in crates/*/Cargo.toml src-tauri/Cargo.toml installer/Cargo.toml; do grep -E '^(r-code|agent)-' $f; done`
  结果：无循环依赖（vendor <- core <- store/gateway/mcp/terminal <- agent-worker <- host）
- LOC：`find <dir> -name '*.rs' | xargs wc -l`，数值见 findings/01-baseline 表格
- CI：ci.yml 8 job（frontend/npm-audit/secrets/fmt/clippy/test×3OS/audit/deny/submodule-pin），test 用 --test-threads=1 串行

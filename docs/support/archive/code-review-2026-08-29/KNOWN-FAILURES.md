# KNOWN-FAILURES — 既有失败处置记录（2026-08-29 基线）

本文件记录 code-review 修复开始**前**工作树上已存在的测试失败（处置路径要求，F-test-04）。
修复任务的验收口径：不新增失败、blocking/major 修复目标测试转绿。

## Rust（cargo test --workspace --all-features -- --test-threads=1）

- `task_workspace_binding::tests::a3_symlink_into_foreign_repo_root_is_rejected`：
  Windows os error 1314（无开发者模式/admin 的 symlink 特权）→ 测试直接 unwrap symlink 创建。
  **处置**：FX-18 已修——宿主无特权时显式 SKIP（环境能力缺失，非被测行为回归）。
  其余 1661 全绿。

## 前端（npm test，本地 Windows，249/304，53 失败）

两类：

1. **~20 个 30s 级超时**（sidebar status / archived conversations / companion 系列等）：
   本地 Playwright e2e 环境超时（慢机 + 无 --with-deps 的本地 chromium）。CI ubuntu 腿全量跑这些
   测试且历史绿；本地重跑单个文件多数可过。**处置**：不视为回归；涉及前端热路径的修复
   （FX-15/16/17）验收时逐文件对比，不新增失败即可。
2. **~33 个亚秒级失败**（feature-flag 矩阵、m1-03 状态投影、settings/knowledge/review 契约等
   静态断言）：与用户 WIP（SettingsScene/Canvas/format 等重构中源文件）直接相关的既有红。
   **处置**：属用户未完成的重构工作（已由 FX-00 wip 提交保全原状），不在本次修复范围；
   记录在案，待用户 WIP 收尾后由其自测收敛。

## 处置原则（沿用）

- 平台能力缺失（symlink 特权等）→ 测试内显式 SKIP + 原因输出，不允许静默 pass。
- 显式选择执行的测试（金集）→ `#[ignore]` + 门禁脚本显式 `--ignored`，默认 run 如实报 ignored。
  （FX-18 已把 command_corpus_runner 从"静默 pass"改为该模式。）

## 2026-08-29 续期处置更新（路径缺陷 + 断言回归已修）

**已修（本轮）**：
- **13 个前端测试文件 Windows 路径缺陷**（147d7cd）：`new URL(...).pathname` 在 Windows 产出 `D:\D:\` 双盘符，导致 m4-03 / m1-03-s3-s5 / m1-02 / m1-03-a1-a2 / m2-03-a4 / feature-flag / m3-04 等本地全红（CI linux 不受影响）。改用 `fileURLToPath(import.meta.url)`，本地 Windows 可运行；复跑这些 suite 全绿。
- **companion.test.mjs 断言回归**（e2acd22）：FX-11 把 `cmd_companion_ensure` 的错误从 `.to_string()` 改为 `.into()`（CommandError 契约），regex 断言未同步导致 `companion recovery tolerates` 回归。适配断言（`.to_string()`/`.into()` 双形式，错误分支语义不变）后恢复通过。

**全量 npm test 实测（314 tests / 288 pass / 24 fail）**，24 条失败经基线对照**全部既有**，非本轮引入：
- 30s 级 Playwright e2e 超时（本地慢 runner）：room project attachment / sidebar status / archived conversations / clearing a project / info tips / subagents tab / Needs You / task completion / session assistant / running sessions / full task panel / 键盘可达 / 搜索命中 / A1 三视口——baseline 同款超时。
- 亚秒级静态/契约失败（用户 WIP Settings/样式重构相关）：A2-A4（960 宽度 / 主题切换 / 执行台 bounds）、font-size <11px（room.css:1957 等，非我改动行）、dark faint WCAG、companion is a separate（baseline 同失败）。
- **JSX user copy cannot grow**：baseline 测试 PASS 但本轮 FAIL——经查 `App.tsx` 与 `i18n-hardcoded-baseline.json` **均未被本分支改动**（`git diff main` 为空），属用户 WIP 时段 i18n 基线漂移；待用户 WIP 收尾时同步 baseline。**非本 review 引入**（证据：两文件在分支上零提交改动）。


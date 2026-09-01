# RV-07 可维护性维度 Evidence（2026-08-29）

工作树快照：`git status --short` 显示 24 个 M + 14 个 ??（WIP），review 按工作树现状进行。所有命令在仓库根 `D:\project\rust\r-code` 执行（rg 均带 `-g '!target' -g '!vendor'`）。

## E1 进程树 kill 三份实现（F-maint-01）

```
$ rg -n "taskkill|/T |/PID" --type rust
crates\r-code-gateway\src\tools_command.rs:597:    // taskkill 结束整棵进程树…
crates\r-code-gateway\src\tools_command.rs:686:            let mut terminate_tree = Command::new("taskkill");
crates\r-code-gateway\src\tools_command.rs:687:            terminate_tree.args(["/PID", &pid.to_string(), "/T", "/F"]);
crates\r-code-store\src\verification.rs:356:                    let mut terminate_tree = Command::new("taskkill");
crates\r-code-store\src\verification.rs:357:                    terminate_tree.args(["/PID", &pid.to_string(), "/T", "/F"]);
src-tauri\src\commands.rs:20745:        let mut terminate_tree = TokioCommand::new("taskkill");
src-tauri\src\commands.rs:20747:            .args(["/PID", pid.as_str(), "/T", "/F"])

$ rg -n "terminate_codex_child|async fn kill_tree" --type rust
src-tauri\src\commands.rs:20741:async fn terminate_codex_child(child: &mut tokio::process::Child) {
crates\r-code-gateway\src\tools_command.rs:682:async fn kill_tree(child: &mut tokio::process::Child) {
```

分叉差异（Read 原文比对）：
- commands.rs:20752 `let _ = timeout(Duration::from_secs(5), terminate_tree.status()).await;`（唯一带 5s 超时）
- tools_command.rs:689-692 `let killed = terminate_tree.output().await; if killed.is_ok_and(…) { return; }`（无超时）
- verification.rs:353-364 `Err(_) =>` 超时分支内联，`#[cfg(not(windows))]` 只 `child.kill().await` 不发组信号

## E2 厂商身份判定三份（F-maint-02）

```
$ rg -n "fn is_deepseek_provider|fn is_deepseek_native_provider" --type rust
src-tauri\src\commands.rs:14888:fn is_deepseek_provider(provider: &agent_config::ProviderConfig) -> bool {
crates\r-code-agent-worker\src\llm_runtime.rs:6287:fn is_deepseek_native_provider(provider_name: &str) -> bool {

$ rg -n '"deepseek" \| "deepseek_responses" \| "deepseek_anthropic"' --type rust
crates\r-code-agent-worker\src\llm_runtime.rs:366   （reasoning governor 内联 matches!）
crates\r-code-agent-worker\src\llm_runtime.rs:6290  （is_deepseek_native_provider 体内）
```
口径差异：commands.rs:14888-14893 判 `provider_kind`（config 字段）；llm_runtime 判 provider 名字别名。`is_kimi_coding_provider`（commands.rs:14895）同 kind 口径，llm_runtime.rs:352-358 用名字清单 `ark_coding|ark_agent|kimi_coding`。

## E3 allow(dead_code) 全清单 16 处（F-maint-03）

```
$ rg -n "allow\(dead_code\)" --type rust      # 16 处
crates\r-code-core\tests\user_error_contract.rs:24      （fixture struct 字段，合理）
crates\r-code-core\src\security.rs:118                   （cfg_attr 平台，合理）
crates\r-code-gateway\src\tools_command.rs:231           （cfg_attr(not(windows))，合理）
src-tauri\src\commands.rs:20576,20857,20877,22126,22242,22253,22316,22380,22405,22417,24007   （11 处，Codex 簇）
src-tauri\src\replay.rs:461
src-tauri\src\rtk.rs:620                                （cfg_attr(not(windows))，合理）
```

引用计数验证（rg -w 全仓，含测试文件）：

| 符号 | 定义 | 引用点 | 结论 |
| --- | --- | --- | --- |
| wait_for_codex_app_server_response | 22316 | 无 | 死 |
| codex_app_server_thread_id | 22405 | 无 | 死 |
| codex_app_server_turn_id | 22417 | 无 | 死 |
| meta_to_summary | replay.rs:461 | 无 | 死 |
| write_codex_app_server_value | 22242 | 22354（在死函数 22316 内） | 传递死 |
| read_bounded_line | 22253 | 22331（同上） | 传递死 |
| codex_app_server_startup_progress | 22380 | 22341（同上） | 传递死 |
| codex_exec_command_with_permissions | 20576 | 37943、40647（均在 26169 `#[cfg(test)] mod tests` 之后） | 仅测试 |
| CodexLineEvent | 22126 | 36593（测试内） | 仅测试 |
| run_codex_exec_process / _with_options / …and_permissions / …and_images | 20857/20877/20901/20926 | 37730、37792、40523、40569（全在测试模块） | 仅测试 |
| run_codex_app_server_process | 24009 | 24020 → 24084 → 24116；24084 被 25050 调用，25050 位于 `run_codex_delegation_process_with_images`(25037)，链回 `spawn_codex_main`(25111, 被 20344 生产调用) | **生产活，allow 过时** |

测试模块边界证据：`rg -n '^#\[cfg\(test\)\]' src-tauri/src/commands.rs` → 1871、15834、19226、19281、21565、21567、22125、22186、22549、26169；37730/37792/40523/40569/36593/37943/40647 均 > 26169。

## E4 未引用 pub fn 全量清单（F-maint-07）

方法：Python 脚本提取 git 跟踪的 1259 个 `pub fn` 名 → 统计每个名字在全部 Rust 文本中的词匹配数 → 计数 ≤ 定义处数 即零外部引用。结果 **21 个**：

```
apply_cached_patch -> crates/r-code-store/src/git_service.rs
blobs_dir_path -> crates/r-code-core/src/testing.rs            （cfg(any(test, feature="testing")) 模块）
create_large_text_fixture -> crates/r-code-core/src/testing.rs
disarm -> src-tauri/src/plan_policy.rs
enqueue_manual -> crates/r-code-store/src/memory_store.rs
expire_pending_offer -> crates/r-code-store/src/plan_entry_store.rs
gate_armed -> src-tauri/src/plan_tools.rs
has_conflict -> crates/r-code-store/src/git_service.rs
has_staged_change -> crates/r-code-store/src/git_service.rs
has_worktree_change -> crates/r-code-store/src/git_service.rs
intent_to_add -> crates/r-code-store/src/git_service.rs
is_decided -> crates/r-code-core/src/dto.rs
is_decline -> crates/r-code-core/src/plan_entry.rs
is_indexed -> crates/r-code-store/src/git_service.rs
label_zh -> crates/r-code-core/src/dto.rs
list_hashes_for_task -> crates/r-code-store/src/attachment_store.rs
mark_current_corrupt -> src-tauri/src/browser/installer.rs
quiet_reason_for -> src-tauri/src/plan_policy.rs
uses_utc_interval -> crates/r-code-core/src/automation.rs
with_external_agent_runner -> crates/r-code-agent-worker/src/llm_runtime.rs
worktree_patch -> crates/r-code-store/src/git_service.rs
```

## E5 超长函数完整清单（F-maint-05）

脚本：`$TMP/rv07_longfns.py`（fn 声明+大括号深度，剥离字符串/正则；排除 *tests* 文件与 `#[cfg(test)] mod` 之后内容；TSX 数字经人工边界校准，`langFromInfo` 类正则字面量误报已剔除）。>150 行共 39 个，全量输出（test=True 行为脚本近似标记，前 6 项与标 ★ 项已 sed/awk 手工复核）：

```
file                                                                     line fn                                              len test
src-tauri\frontend\src\components\room\Composer.tsx                       242 Composer                                       1602 False ★(人工1587)
crates\r-code-agent-worker\src\llm_runtime.rs                            4375 run_loop                                       1567 False ★
src-tauri\frontend\src\components\companion\CompanionWindow.tsx           443 CompanionWindow                                1304 False
src-tauri\frontend\src\lib\browser-mock-runtime.ts                       2073 browserMockInvoke                              1092 False (mock 设施)
src-tauri\frontend\src\components\scenes\HomeScene.tsx                     93 HomeScene                                        818 False
src-tauri\src\main.rs                                                     265 main                                            726 False
src-tauri\frontend\src\components\scenes\RoomScene.tsx                    104 RoomScene                                       723 False
src-tauri\frontend\src\components\plan\PlanPanel.tsx                      359 PlanPanel                                       649 False
src-tauri\frontend\src\components\room\Canvas.tsx                        1092 NormalChangesPanel                              621 False
src-tauri\frontend\src\components\room\Canvas.tsx                        2694 TerminalPanel                                   607 False
src-tauri\frontend\src\components\onboarding\OnboardingCampaign.tsx        67 OnboardingCampaign                               585 False
src-tauri\frontend\src\components\scenes\SubagentProvidersPanel.tsx       187 SubagentProvidersPanel                          585 False
src-tauri\frontend\src\components\room\Canvas.tsx                        1810 FilesPanel                                       476 False
src-tauri\frontend\src\components\scenes\MemoryPanel.tsx                   78 MemoryPanel                                      402 False
src-tauri\frontend\src\components\room\ModelSwitcher.tsx                   53 ModelSwitcher                                    394 False
src-tauri\frontend\src\components\room\EnhancedReviewPanel.tsx            144 EnhancedReviewPanel                             374 False
src-tauri\frontend\src\components\room\Canvas.tsx                         237 Canvas                                          351 False
src-tauri\frontend\src\components\codex\CodexCliGate.tsx                   65 CodexCliGateProvider                             335 False
src-tauri\frontend\src\components\room\Canvas.tsx                         598 SummaryPanel                                     333 False
src-tauri\frontend\src\components\room\SubagentWorkbench.tsx              726 SubagentInspector                               332 False
src-tauri\frontend\src\components\room\Markdown.tsx                        78 Block                                            327 False
src-tauri\src\commands.rs                                                9110 dispatch_next_queued                           321 False
crates\r-code-agent-worker\src\llm_runtime.rs                            7974 handle_plan_subagents                          319 False
src-tauri\frontend\src\components\scenes\SettingsScene.tsx               2275 OrchestrationSection                           348 False
src-tauri\frontend\src\components\scenes\SettingsScene.tsx                564 SettingsScene                                   295 False
src-tauri\frontend\src\components\deck\FleetRows.tsx                       41 FleetRows                                        271 False
src-tauri\frontend\src\components\settings\ApplicationUpdaterSettings.tsx  36 ApplicationUpdaterSettings                      268 False
src-tauri\src\commands.rs                                               12778 parse_session_messages                        260 False
src-tauri\frontend\src\components\ui\Toast.tsx                            304 useTaskCompletionToasts                         256 False
src-tauri\src\migration.rs                                               365 known_steps                                     256 False
src-tauri\frontend\src\components\scenes\McpPanel.tsx                      54 McpPanel                                        255 False
src-tauri\frontend\src\components\room\Canvas.tsx                        2443 TerminalViewport                               250 False
src-tauri\frontend\src\components\scenes\InboxScene.tsx                    75 InboxScene                                      242 False
src-tauri\frontend\src\components\shell\Rail.tsx                           38 Rail                                            245 False
src-tauri\frontend\src\components\SearchOverlay.tsx                       17 SearchOverlay                                    232 False
src-tauri\frontend\src\components\room\SessionRunSummary.tsx               49 SessionRunSummary                               226 False
SettingsScene.tsx:862 ProviderSection ~1003 ★(人工: 下一顶层声明在 1865)
llm_runtime.rs:9271 run_child 987 ★(人工: 9271→10368)
llm_runtime.rs:8604 spawn_with_run_id 399
commands.rs:5738 persist_runtime_event 543
commands.rs:24116 run_codex_app_server_process_with_images_and_registry ~823（生产链见 E3）
```
（Rust 侧 commands.rs 若干 `test=True` 条目为测试 helper，如 `scoped_subagent_lifecycle_persists…`:28165、`dynamic_delegate_handler_flows…`:35655，已排除。）

人工复核命令样例：
```
$ awk 'NR>=4375 && /^}/ {print NR; exit}' crates/r-code-agent-worker/src/llm_runtime.rs   → 5941  (=1567 行)
$ awk 'NR>=9271 && /^}/ {print NR; exit}' crates/r-code-agent-worker/src/llm_runtime.rs   → 10368 (=987 行)
$ rg -n "^function " src-tauri/frontend/src/components/scenes/SettingsScene.tsx | awk -F: '$1>862' | head -1
  → 1865:function imageModelsOfProvider(...)   (=1003 行)
```

## E6 前端时间格式化使用计数（F-maint-04）

```
$ rg -o "elapsedSince|elapsedMinutes|relativeAgo|formatDateTime|formatRelativeTime" src-tauri/frontend/src | 排序计数
     18 elapsedSince
     14 elapsedMinutes
      3 relativeAgo
      1 formatRelativeTime      ← i18n 入口
      1 formatDateTime          ← i18n 入口
```
硬编码中文样本：format.ts:26 `if (minutes === 0) return "刚刚";`、:41-48 「刚刚/分钟前/小时前/昨天/天前」；audit.ts:444 `` return `${minutes}分${String(seconds).padStart(2, "0")}秒`; ``。
本地副本：ApplicationUpdaterSettings.tsx:17-25 `formatDate`（Intl.DateTimeFormat）、:28 `Intl.NumberFormat`；MemoryPanel.tsx:51-58 `formatTime`（toLocaleString）。
i18n 治理基线存在：`src-tauri/frontend/scripts/i18n-hardcoded-baseline.json`（按文件冻结 count+sha256，如 App.tsx count=10）。

## E7 ConfigRow 复制（F-maint-06）

```
$ rg -n "^function ConfigRow" src-tauri/frontend/src/components/room/
CodexModelConfiguration.tsx:240:function ConfigRow({ label, value, onSelect }: { label: string; value: string; onSelect: () => void }) {
ModelSwitcher.tsx:448:function ConfigRow({ label, value, onSelect }: { label: string; value: string; onSelect: () => void }) {
```
Read 全文比对：两处 JSX（`model-config-row ring-inset` 按钮 + `<span>{label}</span><strong title={value}>{value}</strong><span aria-hidden="true">›</span>`）逐字相同；`ConfigBack`（248/456）同。

## E8 遗留物清点原始输出（F-maint-10）

```
$ git ls-files | awk -F/ '{print $1}' | sort | uniq -c | sort -rn   （头部）
    389 src-tauri / 341 docs / 175 artifacts / 125 eval / 104 crates / 97 scripts
      16 icons / 12 installer / 12 design / 10 .github / 6 fixtures / 2 design-proto / 1 vendor …

$ for d in artifacts design-proto sandbox eval fixtures .reference .reasonix target-qa; do git ls-files $d | wc -l; du -sh $d; done
artifacts     175 tracked   3.8M   （ai-tasks/ knowledge-*.png knowledge-settings-audit/ metrics/）
design-proto     2 tracked   248K   （settings-card-lite.png settings-flat.png）
sandbox          0 tracked   422M
eval           125 tracked   541K   （plan-eval/ 语料）
fixtures         6 tracked   620K
.reference       0 tracked    51M
.reasonix        0 tracked   318K
target-qa        0 tracked   2.6G   （debug/ room-parity/ room-dom-probe.mjs room-todo-probe.mjs tmp/）

$ ls *.log | wc -l → 13；git ls-files '*.log' | wc -l → 0
  ci-clippy2.log(228K) ci-frontend-meta.log(132K) ci-ubuntu-test.log(420K) clippy-linux.log(12K)
  clippy-run.log clippy-run2.log clippy-run3.log clippy-run4.log clippy-run5.log(16K) clippy-run6.log
  fix-verify.log plan-eval-test.log rust-tests.log

$ git check-ignore -v .reasonix
.git/info/exclude:7:.reasonix/   .reasonix        ← 本地 exclude，.gitignore 无此条目

$ git ls-files artifacts | grep -c "\.yaml$" → 71；grep -c "\.json$" → 94；grep -c "\.log$" → 0
$ git ls-files artifacts/metrics | head
  artifacts/metrics/command-corpus/replay-eval-69ab1637c1.json
  artifacts/metrics/command-corpus/report-61a7e630537027263a98a4b30c49e95ce3d03907-windows-m0-01.json
  artifacts/metrics/command-corpus/report-69ab1637c1ea346e0241a52ba4d939626dce9958-windows-baseline.json …

$ rg -rn "design-proto" --glob '!design-proto/**' -l → 无命中（零引用）
$ rg -n "dev\.ps1|dev\.sh" README.md → 66,69,74,77 行（README↔dev 脚本无漂移）
```

## E9 命名统计（F-maint-11）

```
$ rg -ci conversation|session|task|\brun\b  src-tauri/frontend/src（按词计数求和）
  task 2028 / session 539 / run 363 / conversation 108
$ useCreateConversation.ts:24-28 → taskCreate(null, "新对话", …) → ipc.ts:228 "cmd_task_create"
$ rg -c "\bvendor\b" --type rust → 21 处（多为注释，如 agent_loop.rs:842 “vendor 层 send_with_retry”）
$ rg -c "provider_kind" --type rust → 145 处
$ rg -o "_and_(images|permissions|options|registry|retry)" --type rust | 计数
  _and_registry 11 / _and_images 9 / _and_permissions 7
  极端样本：run_codex_exec_process_with_options_and_permissions_and_images（commands.rs:20926）
```

## E10 零负债项验证（正面结论）

```
$ rg -n "TODO|FIXME|HACK|XXX" --type rust → 0；前端/脚本/docs → 0
$ 连续 >20 行注释块扫描（Python，全仓 .rs/.ts/.tsx）→ 0 块
$ rg -n "cfg\(feature" --type rust → 0（features 仅 custom-protocol/testing，由 Cargo.toml/cfg(any(test,…)) 消费）
$ rg -c "cfg!\(windows\)" --type rust → 16 处，分布 commands.rs 9 / rtk.rs 3 / tools_command.rs 3 / 测试 1（集中度可接受）
$ rg -n "fn hide_background_console" → 仅 r-code-core/src/process.rs:11（win）+ :19（unix stub），10 文件复用
$ 组件层 invoke() 绕过 ipc.ts → 0 处（唯一 invoke 字面量在 lib/poll.ts:40 为 registration.invoke()）
```

## E11 跨侧映射（F-maint-08）

```
$ rg -n "DEEPSEEK_RESPONSES_MODELS" src-tauri/frontend/src
  src-tauri\frontend\src\components\scenes\SettingsScene.tsx:170: const DEEPSEEK_RESPONSES_MODELS = new Set(["deepseek-v4-flash", "deepseek-v4-pro"]);
  src-tauri/frontend/src\components\scenes\SettingsScene.tsx:1271: !DEEPSEEK_RESPONSES_MODELS.has(fields.model.trim().toLowerCase());
权威源：src-tauri/src/provider_catalog.rs:372-410（deepseek Preset，native: P_CR = [OpenAiChat, OpenAiResponses]，
note: "Responses 已支持 V4-Flash（0731）与 V4-Pro…"）
IPC：src-tauri/frontend/src/lib/ipc.ts:1180 "cmd_provider_catalog" → provider.ts:42-54 loadCatalog() 缓存 presets（含 presetModels）
```

## E12 PLAN_TOOL_NAMES 子集重复（F-maint-09）

format.ts:161-177 `TOOL_DISPLAY_NAMES` 16 键（enter_plan_mode…send_agent_message）vs :191-202 `PLAN_TOOL_NAMES` 10 键 = 前者 plan_* 前缀键手抄（`plan_publish`…`plan_cancel`）。

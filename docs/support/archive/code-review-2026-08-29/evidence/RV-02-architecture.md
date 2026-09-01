# RV-02 架构与模块边界 — 证据记录

- 审查日期：2026-08-29
- 审查者：code-review 代理（RV-02）
- 仓库状态：工作树含未提交 WIP，按现状静态审查；未执行 cargo build/test/clippy
- 所有命令在 `D:\project\rust\r-code` 下执行（Git Bash）；rg 输出计数摘要见各条

## E1. commands.rs / tauri_commands.rs 命令面

```
$ rg -c '#\[tauri::command\]' src-tauri/src/commands.rs
1                                                        # 唯一命中是第 5 行 doc 注释
$ rg -n 'tauri::command' src-tauri/src/commands.rs
5://! - `#[tauri::command] cmd_*` 薄包装在 **bin 侧** `tauri_commands.rs`

$ for f in src-tauri/src/*.rs; do rg -c '#\[tauri::command\]' "$f"; done   # 按文件计数
190  tauri_commands.rs
4    lifecycle_commands.rs
1    main.rs
1    commands.rs（doc 注释，非实际属性）
0    其余全部文件

$ rg -c 'pub (async )?fn ' src-tauri/src/commands.rs      → 201
$ rg -n 'pub (async )?fn ' src-tauri/src/commands.rs | awk -F: '$1<26169' | wc -l
196                                                      # 生产区 pub fn（mod tests 起于 26169）
$ rg -n 'fn ' src-tauri/src/commands.rs | awk -F: '$1<26169' | wc -l
708                                                      # 生产区全部 fn（含私有/helper）
$ wc -l src-tauri/src/commands.rs                        → 41299

$ rg -c 'commands::' src-tauri/src/tauri_commands.rs      → 199（含类型 re-import）
$ rg -n 'commands::cmd_' src-tauri/src/main.rs | wc -l    → 195（generate_handler 注册）
$ rg -n 'generate_handler' src-tauri/src/main.rs         → 760

# commands.rs 内部 mod 结构（生产区无任何 mod 声明，纯平铺）
$ rg -n '^(pub )?(mod|struct|enum) ' src-tauri/src/commands.rs | rg 'mod '
（无输出）                                                # 内嵌 mod 全部是 #[cfg(test)] 测试模块
$ rg -n '#\[cfg\(test\)\]' src-tauri/src/commands.rs
866 977 1871 10305 15834 19226 19281 21565 21567 22125 22186 22549 26169
# 26169 起为 `mod tests`（至文件尾约 15k 行测试同文件）
$ rg -n '^mod |^#\[cfg\(test\)\]\s*$' src-tauri/src/commands.rs -A1 | rg 'mod '
19227: mod policy_rejection_system_hint_tests
19282: mod codex_diagnosis_projection_tests
26170: mod tests
```

## E2. commands.rs 内嵌 SQL / HTTP / 文件 IO

```
$ rg -n 'SELECT |INSERT INTO|UPDATE ' src-tauri/src/commands.rs | awk -F: '$1 < 26169' | wc -l
23                                                      # SQL 语句起始行（生产区）
# 其中位于 1871(单测试 fn task_workspace_binding_from_db)/10305(cfg(test) 块) 内的为 0 处
# 逐条人工核对后生产区裸 SQL 语句约 20 处，代表性行号：
#   1099, 1382-1408, 1429, 5274, 5383, 5679, 7416, 10199, 11982, 11996,
#   12049, 12064, 12082, 12104, 12115, 12130, 12160, 14600
$ rg -c 'rusqlite' src-tauri/src/commands.rs            → 21

# 涉及表：queued_messages / tool_calls / agent_runs / permission_requests /
#         tasks / task_events / notifications / verifications / attachments / memory_review_turns

$ rg -c 'reqwest|HttpClient|http::' src-tauri/src/commands.rs
（exit=1，0 命中）                                        # commands.rs 无内嵌 HTTP

$ rg -n 'std::fs::|fs::write|fs::read|fs::create_dir|fs::remove|File::' \
    src-tauri/src/commands.rs | awk -F: '$1 < 26169' | wc -l
47                                                      # 生产区文件 IO 调用点
```

## E3. host 各模块 rusqlite / 裸 SQL 分布

```
$ for f in src-tauri/src/*.rs; do rg -c 'rusqlite' "$f"; done | sort -rn
21 commands.rs
8  recovery.rs        # 头部注释自认"直接通过 rusqlite::Connection 访问数据库"
8  migration.rs       # 复用 r_code_store::migrations，SQL 用于锁/test（recovery.rs:7 注释）
3  support_bundle.rs  # 生产只读统计 + 测试插入
2  attachment_migration.rs
1  plan_review_tools.rs
1  mcp_server.rs      # 628 行，测试代码

$ rg -n '"(SELECT|INSERT|UPDATE|DELETE|CREATE)' recovery.rs support_bundle.rs \
    plan_review_tools.rs attachment_migration.rs commands.rs migration.rs | awk -F: '{print $1}' | sort | uniq -c
41 commands.rs
11 migration.rs
10 attachment_migration.rs
7  recovery.rs
4  support_bundle.rs
1  plan_review_tools.rs

# store 侧同表已有封装（r-code-store/src/repositories.rs 1690-1798 行对 queued_messages
# 的 UPDATE/INSERT），即同一张表存在 crate 两侧双写路径。
```

## E4. src-tauri/src 模块依赖图（crate::X:: 全路径引用，含 use 与 inline path）

```
$ cd src-tauri/src && for f in *.rs; do rg -o "crate::([a-z_0-9]+)::" "$f" -r '$1' | sort -u; done
# 归一化后非空边（A -> B 表示 A 引用 crate::B）：
codex_app_server     -> codex_interaction, rtk
codex_interaction_tests -> codex_interaction        # 经 #[path] 由 codex_interaction.rs:2178 挂载
codex_mcp            -> codex_permissions, rtk
commands             -> browser, codex_app_server, codex_interaction, codex_mcp,
                        codex_permissions, legacy_memory, log_buffer, logging, mac_ocr,
                        mcp_manager, mcp_settings, memory_runtime, model_capabilities,
                        native_notification, plan_entry_commands, plan_policy,
                        plan_review_tools, plan_tools, provider_catalog, provider_models,
                        replay, rtk, search, settings, skills, subagent_providers,
                        support_bundle, task_workspace_binding, windows_ocr, workflow_skills
                       （30 个兄弟模块，全仓扇入最大）
lifecycle_commands   -> close_gate, shutdown_coordinator
logging              -> app_paths, log_buffer
mcp_manager          -> mcp_settings
mcp_server           -> app_paths, commands, migration, settings
mcp_settings         -> app_paths, security_config
memory_runtime       -> commands, settings                        # 环①与 commands
model_capabilities   -> provider_catalog
packaging            -> app_paths
plan_entry_commands  -> commands, plan_policy, settings           # 环②与 commands
plan_tools           -> plan_policy
provider_models      -> provider_catalog
settings             -> app_paths, commands(仅 #[cfg(test)], settings.rs:1000), mcp_settings, provider_catalog
subagent_providers   -> settings
support_bundle       -> log_buffer

# 环① commands <-> memory_runtime（均为生产代码）
$ rg -n 'crate::commands' src-tauri/src/memory_runtime.rs
13: use crate::commands::{build_provider_config, provider_readiness_error};
$ rg -n 'crate::memory_runtime' src-tauri/src/commands.rs | head -3
296, 13776, 13922, 13934（spawn_memory_review_worker 调用）

# 环② commands <-> plan_entry_commands（均为生产代码）
$ rg -n 'crate::commands' src-tauri/src/plan_entry_commands.rs | head -5
127: crate::commands::resolve_effective_protocol(...)
421/460/474: fn(state: &crate::commands::CommandState, ...)
435: crate::commands::provider_readiness_error(...)
$ rg -n 'crate::plan_entry_commands' src-tauri/src/commands.rs | head -5
41, 332, 1473, 4232, 8037, 8382, 8687, 14076, 25332

# 伪环③ settings -> commands 仅测试（settings.rs:910 #[cfg(test)] 内 1000 行）
```

## E5. lib「不依赖 tauri」声明与实际

```
$ rg -n 'use tauri::' src-tauri/src/lifecycle_commands.rs src-tauri/src/plan_entry_commands.rs \
    src-tauri/src/commands.rs src-tauri/src/tauri_commands.rs
lifecycle_commands.rs:8: use tauri::{AppHandle, Manager, State};   # lib.rs:25 pub mod 无 cfg 门
tauri_commands.rs:7:    use tauri::{...}                            # bin 侧，符合声明
$ rg -n '^tauri = ' src-tauri/Cargo.toml
76: tauri = { workspace = true }            # [dependencies]，lib 目标无条件链接
# 对照 commands.rs:4-6 doc：「lib 不依赖 tauri —— 保持单元测试二进制无 GUI/comctl32 链接」
```

## E6. codex_* 文件族

```
$ wc -l src-tauri/src/codex_*.rs
1855 codex_app_server.rs     # 传输/进程 registry，自述"只持有已初始化的 transport"
2179 codex_interaction.rs    # JSON-RPC 帧 -> CodexTimelineEventV1 归一化（纯函数、无 IO）
1607 codex_interaction_tests.rs  # 经 codex_interaction.rs:2178 #[path] #[cfg(test)] 挂载
574  codex_mcp.rs            # codex mcp-server 客户端桥（自述刻意保持小）
558  codex_permissions.rs    # config.toml 权限枚举映射

# 但 codex CLI 生命周期管理在 commands.rs：
$ rg -n 'pub (async )?fn codex_' src-tauri/src/commands.rs
17324 codex_integration_status    17585 codex_cli_preferences
17705 codex_save_cli_preferences  17795 codex_install_cli
17856 codex_sync_cli              18171 codex_install_mcp_server
18506 codex_start_login           18511 codex_start_device_login
25948 codex_install_skill         25956 codex_setup_collaboration
$ rg -n 'codex' src-tauri/src/commands.rs -i | awk -F: '$1>=17324 && $1<=18700' | wc -l
312                                    # codex 段代码行命中
$ rg -n 'Command::new|Stdio' src-tauri/src/commands.rs | awk -F: '$1>=17324 && $1<=18700' | wc -l
9                                      # 含 npm 安装等进程派生
```

## E7. llm_runtime.rs（r-code-agent-worker）

```
$ wc -l crates/r-code-agent-worker/src/llm_runtime.rs   → 11921（534KB）
$ wc -l crates/r-code-agent-worker/src/*.rs | sort -rn
11921 llm_runtime.rs
9967  llm_runtime_tests.rs        # 测试拆分到独立文件（经 #[path]? 由 lib.rs 声明）
4266  agent_loop.rs
 946  run_guard.rs
 628  delegation_tree.rs
 470  cache_shape.rs
 345  mock_runtime.rs
 236  checkpoint.rs
 174  runtime.rs
  69  recovery.rs
  47  lib.rs

$ rg -n '^(pub )?(struct|enum) ' crates/r-code-agent-worker/src/llm_runtime.rs | wc -l   → 68
$ rg -n 'fn ' crates/r-code-agent-worker/src/llm_runtime.rs | awk -F: '$1<10613' | wc -l → 258
$ rg -c 'pub (async )?fn ' crates/r-code-agent-worker/src/llm_runtime.rs                → 29
# 生产区（<10613 首个大型 cfg(test)）类型/职责抽样：
# 95-204  SubagentNameAllocator（子代理命名）
# 252-505 DeepSeek V4 / Ark / Kimi ReasoningGovernor（provider 特化策略）
# 506-600 DelegationRouterMode / OrchestrationPolicy / PlanNativeCatalog*
# 602-780 ContextInjectionProfile / ContextSource / ResolvedAttachment
# 787+   AgentPromptPolicy
# 1455-1910 ExternalAgentId / FrozenSubagentSlot* / SubagentCandidate* / CodexSubagent*
# 1820-1868 CodexExternalAgentAdapter（impl ExternalAgentRunner）
# 1910+   LlmAgentRuntime / SessionState / impl AgentRuntime(2283+)
```

## E8. MCP：host 与 r-code-mcp 分工

```
$ wc -l src-tauri/src/mcp_manager.rs src-tauri/src/mcp_server.rs src-tauri/src/mcp_settings.rs
2959 / 688 / 612
$ rg -c 'fn ' src-tauri/src/mcp_manager.rs    → 103
$ rg -n 'rmcp|StreamableHttp|stdio' src-tauri/src/mcp_manager.rs -i | head
# 命中均为视图模型映射（McpTransportView）与 McpTransportConfig 转换，
# 协议实现委托 r_code_mcp::{RmcpConnector, RegistryClient, WebClient}
$ rg -l 'reqwest' src-tauri/src/*.rs
rtk.rs provider_models.rs                 # host 层仅这两处直用 HTTP，MCP 域无
$ wc -l crates/r-code-mcp/src/*.rs | sort -rn | head -5
854 web.rs / 774 registry.rs / 773 client.rs / 462 runtime.rs / 430 model.rs（共 4176）
# mcp_server.rs:20-22 use crate::commands::{agent_abort, agent_send, session_messages,
# task_create_with_agent, task_detail, CommandState}  —— stdio 端点直依赖上帝模块
```

## E9. 前端分层抽样

```
$ rg -l "from ['\"].*lib/ipc['\"]" src-tauri/frontend/src --glob '*.tsx' --glob '*.ts' | wc -l  → 48
$ rg -c 'ipc\.' src-tauri/frontend/src/App.tsx        → 0（App.tsx 只用 store + lib/keys）
$ rg -l '@tauri-apps/api' src-tauri/frontend/src | tr '\n' ' '
components/companion/CompanionWindowController.tsx  components/companion/CompanionWindow.tsx
lib/ipc.ts  components/companion/bridge.ts  components/shell/MenuBar.tsx
components/shell/ClosePromptDialog.tsx
# 直接引 @tauri-apps 的组件仅 5 个，且均为窗口/事件 API（window/listen），非 invoke 命令
$ wc -l src-tauri/frontend/src/store/*.ts
app.ts 592 / tasks.ts 439 / toast.ts 153 / companion.ts 126 / sync-health.ts 48
# 混合模式示例：
$ rg -n 'from "\.\./\.\./lib/ipc"' src-tauri/frontend/src/components/scenes/DashboardScene.tsx
2: import { permissionApprove, taskDelete, taskRestore } from "../../lib/ipc";
# 同时 ConversationsScene.tsx 既 useTasksStore(s=>s.tasks) 又直接调 taskList() 刷新
```

## E10. vendor/agent-contracts 快扫（只记录）

```
$ ls vendor/agent-contracts/crates
agent-compaction agent-config agent-contract agent-error agent-ipc
agent-llm agent-mcp agent-store agent-tauri
$ rg -ln 'r_code|RCode|r-code' vendor/agent-contracts/crates/agent-contract/src/
（无输出）                              # 合同 crate 无产品类型泄漏
$ rg -n 'r_code|RCode|r-code' vendor/agent-contracts/crates/agent-ipc/src/
client.rs:21/204, server.rs:73          # Windows 管道名前缀硬编码 r-code-（产品自持 submodule，可接受）
```

## 覆盖范围声明（扫描了什么 / 没扫什么）

已扫：
- src-tauri/src 全部 55 个顶层 .rs 文件的 crate 内依赖边（E4）；
- commands.rs 全量函数名/域分布（rg 定位 + 关键区段 Read 局部：164-1953 状态区、
  982-1060 CommandState、1099/5679/7416/10199/17795 SQL 与安装流程、26169 测试边界）；
- llm_runtime.rs 类型/impl 分布（未通读，按行号抽样 266-300、1820-1844）；
- codex_*/mcp_*/lifecycle/plan_entry 模块头部与交叉引用；
- 前端 App.tsx 全读、store/ 5 文件计数、scenes/room 抽样（DashboardScene/ConversationsScene）。

未扫（下述结论不覆盖）：
- commands.rs 与 llm_runtime.rs 的函数体逐行语义（仅 rg 定位 + 局部抽样）；
- crates/r-code-{core,store,gateway,mcp,terminal,agent-worker} 内部模块级环（阶段0 仅验 crate 级）；
- browser/ automation/ updater/ 子目录内部结构（仅入库到 E4 依赖边一层）；
- vendor/agent-contracts 各 crate 内部设计（仅 E10 快扫）；
- 任何运行时行为（未编译未测试）。

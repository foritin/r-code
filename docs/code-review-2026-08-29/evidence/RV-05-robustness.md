# RV-05 健壮性维度证据 — 2026-08-29

工作树现状审查（含未提交 WIP）；未运行 build/test。以下为 rg 命令原文与关键输出计数（rtk 过滤不改计数）。

## 1. HTTP 客户端构造点与超时

```
$ rg -n 'Client::builder|Client::new|reqwest::' --type rust -g '!target' | wc -l
18
```

命中文件与定性：
- vendor/agent-contracts/crates/agent-llm/src/{anthropic,openai,responses}.rs — 仅 `connect_timeout(15s)`，无总超时（流式靠 watchdog / 调用方超时；anthropic 线路缺 watchdog，见 F-robust-04）
- vendor/agent-contracts/crates/agent-mcp/src/transport.rs:232-235 — `.timeout(timeout)`（传入）
- src-tauri/src/provider_models.rs:62-66,160-164 — connect 6s + total 15s
- src-tauri/src/rtk.rs:300-304 — connect 12s + DOWNLOAD_TIMEOUT 90s（:29）
- crates/r-code-mcp/src/registry.rs:50-55 — total 15s
- crates/r-code-mcp/src/web.rs:124-130 — 每请求 `.timeout(request.timeout)`；WebLimits::default timeout_ms=15000（model.rs:399-408）

SSE 空闲保护：
```
$ rg -n 'watch_sse_idle|DEFAULT_STREAM_IDLE_TIMEOUT' vendor/agent-contracts/crates/agent-llm/src/openai.rs | head
openai.rs:46  DEFAULT_STREAM_IDLE_TIMEOUT = 120s
openai.rs:394 do_stream → parse_openai_sse_with_idle_timeout
# anthropic.rs 无任何命中（do_stream:264 直接 parse_sse_stream） → F-robust-04
$ rg -n 'LLM_PROVIDER_IDLE_TIMEOUT' crates/r-code-agent-worker/src/agent_loop.rs
agent_loop.rs:35  = 600s；:917/:931 包住 stream.next()
```

rmcp streamable-http：
```
$ rg -n 'timeout' crates/r-code-mcp/src/client.rs | head
:37 SESSION_CLOSE_TIMEOUT=3s；:40 MCP_INITIALIZE_TIMEOUT=15s；:43 MCP_LIST_TOOLS_TIMEOUT=15s
:91 MCP_CALL_TIMEOUT=300s；:92 ABORT_POLL=25ms；:93 CANCEL_NOTIFY=500ms
$ rg -n 'const MCP_' crates/r-code-mcp/src/runtime.rs
:21 MCP_CONNECT_TIMEOUT=20s；:25 MCP_SUPERVISOR_CLOSE_TIMEOUT=5s
```

## 2. 重试与退避

```
$ rg -c 'retry|backoff|Retry|Backoff' -g '*.rs'（top）
crates/r-code-agent-worker/src/agent_loop.rs: 71
src-tauri/src/commands.rs: 44
vendor/.../anthropic.rs: 39 / openai.rs: 36
```
- vendor send_with_retry：openai.rs:30 `MAX_RETRIES=10`、:483-485 `retryable_status = 408|429|500..=599`、:490-496 指数退避 500ms×2^n 封顶 15s、Retry-After 封顶 60s（:35）、错误体读取 10s 超时（:511-516）；**无抖动**（:489 注释自认）→ F-robust-09
- anthropic.rs:275-328 复用同函数，仅响应头前重试、body 字节复用
- agent 层：agent_loop.rs:87 `MAX_STREAM_RECOVERIES=5`、:89 `STREAM_RECOVERY_BASE_MS=500`、:1112-1155 仅零输出时冻结重放（幂等安全）
- 降级链：commands.rs:15994-16078 build_runtime_subagent_candidate_pool（20s 探测预算 + 25s 单槽上限 + 槽位剔除 + Degraded 空池回退）— 实现完好

## 3. 子进程生命周期

```
$ rg -n 'kill_on_drop' -g '*.rs' src-tauri crates | wc -l
14
$ rg -n 'taskkill' -g '*.rs' src-tauri crates | wc -l
9   # tools_command.rs、verification.rs、commands.rs(terminate_codex_child + 其余为注释/测试)
```
树杀已实现：tools_command.rs:682-703（kill_tree）、verification.rs:353-360、commands.rs:20741-20760（terminate_codex_child）。
树杀缺失：codex_app_server.rs:600（`self.child.kill()`；spawn 经 cmd.exe wrapper :790-797）→ F-robust-01；codex_mcp.rs:396（同型 :425-429）→ F-robust-02；commands.rs:17054/17095 `timeout(deadline, command.output())` drop 后仅 kill_on_drop 杀 cmd.exe → F-robust-03。
MCP stdio 安全：client.rs:281-311 reject_unsafe_windows_launcher（拒绝 cmd/ps1/bat/npx/npm）+ :218 kill_on_drop。
终端 PTY：manager.rs:128-135 Drop guard（kill+wait，killed 防重杀）。
退出钩子：main.rs:971-988 仅 MCP + codex app_server（2s 预算）；终端未收束 → F-robust-08。

## 4. wait()/管道

```
$ rg -n '\.wait\(\)\.await' -g '*.rs' src-tauri crates | wc -l
14
```
全部定性：kill 后 reap（tools_command:626,634 / verification:365 / codex_app_server:601 / codex_mcp:397 / commands:21219-21258 terminate 后）；commands.rs:21221-21260 的裸 wait 均在 idle/deadline/cancel select 内。无「无超时等活进程」的裸等。
管道：tools_command.rs:598-599、verification.rs:320-333、codex_app_server.rs:366-401、codex_mcp.rs:216-222 均先 take 管道并 drain stderr/stdout 再 wait；codex_app_server reader/writer 有界队列 + shutdown join_bounded（:598-604,643）。

## 5. tokio 任务治理

```
$ rg -n 'tokio::spawn' crates/r-code-agent-worker/src/llm_runtime.rs crates/r-code-agent-worker/src/agent_loop.rs | wc -l
6
$ rg -n 'tokio::spawn' src-tauri/src/mcp_manager.rs src-tauri/src/codex_interaction.rs src-tauri/src/codex_app_server.rs src-tauri/src/shutdown_coordinator.rs | wc -l
9   # codex_interaction.rs 0 命中（纯协议投影模块，无进程/异步）
```
- commands.rs:8691 spawn_drain_loop_with_resources：JoinHandle 丢弃，终态收敛全在任务内 → F-robust-06
- 锁中毒：
```
$ rg -n 'expect\(".*poisoned"\)' crates/r-code-agent-worker/src/llm_runtime.rs | wc -l
13   # :128,:2141,:2169,:2388,:7345,:7353,:7367,:7399,:8030,:8261,:8883,:8974,:10466 → F-robust-07
```
- watch 阻塞点：llm_runtime.rs:10536-10549 wait_for_subagent（closed 时降级 Failed，正确）；挂死上游为 F-robust-05 的无超时 complete()。

## 6. 状态一致性

```
$ rg -n 'transaction\(\)' -g '*.rs' crates/r-code-store/src | wc -l
26   # plan_store/review_git/repositories/change_service/plan_review 广泛使用
```
- migrations.rs:117-169：schema_version Immediate 事务 + 每迁移一个事务 + FK off/on 恢复矩阵；失败停在上一版本。
- 启动恢复：commands.rs:1378-1398 reconcile_tool_calls、:1404-1442 capture_startup_recovery、:12034-12140 cleanup_recovery_snapshot（单事务：run 收束 + tool_calls 收束 + task→interrupted + 双事件 + 权限 deny）；recovery.rs:59-133 扫描/孤儿权限。
- 运行中失败收敛：commands.rs:8946-9030 native_run_terminal_outcome 四分支（Aborted/PartialSuccess/CompletedWithError/Completed*）均落终态；`let _ =` 吞错为已知取舍（DB 故障时由启动恢复兜底）。
- complete() 超时覆盖核对：commands.rs:3325(120s)、llm_runtime.rs:3205(120s)、:3922(VISUAL_CHECKPOINT_TIMEOUT)、commands.rs:7118(60s)、memory_runtime.rs:84(120s)、commands.rs:16293(PROBE)；**唯一无超时：llm_runtime.rs:10298** → F-robust-05。

## 7. 前端

```
$ rg -g '*.ts' -g '*.tsx' -o 'catch \(' src-tauri/frontend/src | wc -l
200
$ rg -g '*.ts' -g '*.tsx' -o '} finally \{|\.finally\(' src-tauri/frontend/src | wc -l
151
$ rg -g '*.ts' -g '*.tsx' -o 'invoke[<(]' src-tauri/frontend/src | wc -l
4   # 全部收敛于 lib/ipc.ts（:172-185 统一错误规范化包装）
```
抽样：MemoryPanel.tsx:106-116（try/catch/finally + error state）、KnowledgeSettingsPane.tsx:303-304、poll.ts:36-49（防重入 + 失败上报）。未发现"await 无错误分支导致永久 loading"的系统性模式。

## 环境备注

- `rg` 对 codex_interaction.rs 的 'timeout'/'spawn'/'kill' 均 0 命中（file 为 UTF-8；该文件为纯协议/投影模块，无进程管理与 async fn），子进程面实际在 codex_app_server.rs / codex_mcp.rs / commands.rs。
- rtk hook 会将 rg 输出中的 "taskkill" 字样在部分管道下渲染异常；核实文件真实内容以 Read 工具为准（tools_command.rs:597/631/686、commands.rs:20745 等均确认为 `Command::new("taskkill")`）。

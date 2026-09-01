# RV-06 性能维度审查（2026-08-29）

## 扫描方法与覆盖声明

- 方式：`rg` 定位热路径模式（`.clone()`、`to_string()`、`emit`、`Regex::new`、`setInterval`/`usePoll`、`Mutex<HashMap>`）后用 `sed`/`Read` 局部精读；全程只读，未运行 build/test/benchmark。大文件（commands.rs 1.63MB、llm_runtime.rs 534KB 等）未通读，只精读 rg 命中上下文。
- 覆盖的链路：① 原生 agent 事件链（`agent_loop.rs` 流式循环 → `llm_runtime.rs` run_turn → mpsc → `commands.rs` drain 循环 → `main.rs` sink → Tauri emit → 前端 `onAgentEvent`/coalescer/Timeline/Markdown）；② Codex 事件链（`codex_app_server.rs` stdout → `commands.rs` observe_codex_app_server_event → `codex_interaction.rs` projector/buffer → sink）；③ 工具调用链（`agent_loop.rs` execute_pending_tools → `gateway.rs` execute_call/execute_with_wait → 审计 ledger）；④ 持久化（`SessionStore::append_batch`、`persist_runtime_event`、rusqlite 池配置）；⑤ 前端轮询/渲染（`usePoll` 全部 19 处订阅、Canvas/Timeline/Markdown、zustand 订阅、localStorage）。
- 审计过但**未形成 finding** 的点（避免误报）：
  - `log_buffer.rs` `BUFFER: OnceLock<Mutex<VecDeque>>`：临界区仅 `pop_front+push_back`，且生产默认 `info` 级过滤（`logging.rs:63`），非每 token 路径；`redact_text` 的 11 个 regex 已 `LazyLock` 缓存（`secret.rs:614`）。仅 dev 构建（默认 `debug`，`logging.rs:93`）下每条日志付 11 次 regex 扫描，记 minor。
  - 仓库 regex 均为静态缓存；`r-code-terminal/control_service.rs:236` 的 `Regex::new` 是每次 wait 调用一次（非循环内），可接受。
  - `repositories.rs` 未发现循环内逐条 query 的 N+1（排序 UPDATE 循环在事务内且为用户触发）。
  - 前端 zustand 全部为 selector 订阅（未发现整店 `useAppStore()` 订阅）；Rail 拖拽用 CSS 变量预览、localStorage 仅在拖拽结束时写一次；composer 草稿 250ms 防抖——这三处实现质量良好。
  - 前端有 100ms 事件 coalescer（`model.ts:1153`）与 Timeline 80 轮窗口（`Timeline.tsx:807/1104`），说明流式渲染已有意识优化；剩余问题见 F-perf-09。
- severity 口径：blocking=用户可感知卡顿（每消息 O(会话大小) 或 O(n²) 工作）；major=明确可观的浪费；minor=小幅机会。

## 汇总表

| F-perf-NN | 位置 | severity | 根因描述 | 修复方向 |
|---|---|---|---|---|
| F-perf-01 | src-tauri/src/codex_interaction.rs:741-773 | blocking | Codex 工具输出缓冲 `push` 逐字符 `chars().count()` + tail 溢出时逐字符重建 String，O(n²) CPU + 每字符 ~64KB 分配 | 改字节/字符计数器增量维护 + 环形 Vec 或 `String::drain(..overflow)` 一次截断 |
| F-perf-02 | crates/r-code-agent-worker/src/llm_runtime.rs:4510-4541, 5023, 5067; agent_loop.rs:831-837,881 | blocking | 每轮 provider 请求对整个会话历史做 5~7 次深拷贝（canonical/projection/request_messages/dispatch_ref/request/attempt_request×重试） | 会话历史 `Arc<Vec<Message>>`/COW；provider.stream 借用 &CompletionRequest 或 Arc；重试复用冻结请求 |
| F-perf-03 | src-tauri/src/main.rs:641-656; commands.rs:8806 | major | 每个 agent 事件（含每个文本 delta）单独构造 `serde_json::json!` Value + 单独 emit：双重序列化且无后端批内合并 | 借用型 Envelope 结构体直发；drain 循环内合并同批文本 delta 为一条消息 |
| F-perf-04 | src-tauri/src/commands.rs:5429,5756 | major | `ensure_session_log` 在**每个**事件（含每个 delta）上做文件系统 `.exists()` stat（Windows 上是真实 syscall） | 会话粒度 HashSet 缓存"已确保"标记 |
| F-perf-05 | vendor/agent-contracts/crates/agent-store/src/session_store.rs:38-48,87-103 | major | `SessionStore::append` 单事件即开文件-写-flush-关；且全局 `Mutex<HashMap>` + `retain` 全表扫描在每次 append 上执行 | 热路径改持句柄 `BufWriter`/追加句柄缓存；锁注册表去 retain；drain 循环用 append_batch 批量 |
| F-perf-06 | llm_runtime.rs:4632,4657,9669; gateway.rs:827-845; mcp_manager.rs:1328-1337 | major | 每轮重建整个工具目录：gateway `tool_specs()` 全量 String/schema 克隆 + MCP `direct_catalog.specs.clone()` + `serde_json::to_string(&tools)` 仅为取长度 + agent_loop 再 `to_vec` + `request.clone()` 再拷一层 | 目录按注册表版本缓存 Arc；长度用增量维护或 Option 松弛 |
| F-perf-07 | RoomScene.tsx:354; Canvas.tsx:1145,2678,2744,2981; commands.rs:4199-4251,10651-10672 | major | Room 常驻轮询矩阵：2s task_detail（8 次查询）、2s `git status` 子进程、1.2s terminalList、2s verificationList；sessionMessages 全量 JSONL 重读由 auditStamp 变化触发近乎 2s 一次 | 空闲降频/事件驱动；git 状态只在面板可见+有变更事件时刷新；会话读取走游标增量（subagent 已有先例） |
| F-perf-08 | gateway.rs:930-975,1114; agent_loop.rs:989-1002,461 | major | 单次工具调用的 input Value 被深拷贝 ~3 次 + `input.to_string()` 全量 JSON 序列化 2~3 次（审计 + 权限检查） | 权限检查传摘要；审计存 Arc<str>/延迟序列化；agent_loop 末次使用改 move |
| F-perf-09 | frontend Markdown.tsx:44-60 | major | 流式期间 text 每 ~100ms 变化使 `parseMarkdown(全文)` 全量重解析 + 全部 Block 重渲染（memo 在 text 变化时无效，注释自知"最贵"） | 增量解析最后段落/按块 diff；只重渲染末块 |
| F-perf-10 | commands.rs:23615,23657; codex_interaction.rs:801-804 | major | Codex 每帧 `value.get("params").cloned()` 深拷整帧参数；每条工具输出 delta `accumulate_tool_output` 返回值（render 全量拼接，最大 ~128KB）被 `let _ =` 丢弃 | 借用 params；accumulate 返回 () 或按需 render |
| F-perf-11 | llm_runtime.rs:4480-4521 | minor | 全局 `sessions: tokio::Mutex<HashMap>` 持锁期间执行 O(历史) `messages.clone()`，阻塞其它会话的 steer/abort/快照 | 锁内仅 `Arc::clone` 快照，深拷贝移出锁外 |
| F-perf-12 | codex_interaction.rs:634-672 | minor | 投影器每条 delta 线性扫 `items`（≤1024）+ 每条 delta 两次 String 分配（item_id/delta to_string） | HashMap 索引 item；Delta 事件借用 |
| F-perf-13 | main.rs:716-722 | minor | `mcp-status` 每次变更 `values().cloned().collect()` 全量克隆所有服务器状态再 emit | watch 通道只发变更项，前端合并 |
| F-perf-14 | logging.rs:93; log_buffer.rs:91-113; secret.rs:614-665 | minor | dev 构建默认 debug 级，每条日志（含热路径 debug!）付 11 个 redaction regex 扫描 + entry.clone() + JSONL 序列化 | BufferLayer 仅对 warn+ 走 redact 全量，debug 级走轻量路径或延迟脱敏 |
| F-perf-15 | commands.rs:20685,23987 | minor | `emit_codex_*` 双出口场景对每个 AgentEvent 先 `event.clone()`（大 delta 文本整段复制） | sink 改收 &AgentEvent 或 Arc<AgentEvent> |
| F-perf-16 | llm_runtime.rs:4097-4130,5100-5131 | minor | journal 模式下每轮 fingerprint 对 system+tools+全量消息做 `to_value`（深拷入 Value 树）再 `to_vec` 再哈希：额外 2 次全历史物化 + `dispatch_ref_messages` 额外一整份克隆 | 流式哈希（serde_json::Serializer into Hasher）或仅哈希逐条消息拼接 |

---

## 逐条展开

### F-perf-01（blocking）Codex 工具输出缓冲 O(n²) 推进 + 每字符 64KB 分配

**位置**：`src-tauri/src/codex_interaction.rs:741-773`（`CodexToolOutputBuffer::push`）；触发点 `commands.rs:23657`（`accumulate_tool_output`）。

**证据**：
```rust
fn push(&mut self, delta: &str) {
    for ch in delta.chars() {
        if !self.head_full {
            if self.head.chars().count() >= Self::HEAD_CHARS {   // O(head.len()) 每字符
                self.head_full = true;
            } else { self.head.push(ch); continue; }
        }
        self.tail.push(ch);
        if self.tail.chars().count() > Self::TAIL_CHARS {        // O(tail.len()) 每字符
            let overflow = self.tail.chars().count() - Self::TAIL_CHARS;
            let keep: String = self.tail.chars().skip(overflow).collect(); // 每字符一次 ~64KB 分配
            self.tail = keep;
        }
    }
}
```
`HEAD_CHARS = TAIL_CHARS = 65_536`（codex_interaction.rs:733-734）。

**为什么热**：该缓冲挂在 `CodexRunProjection::tool_outputs`，Codex 子进程的每条 `item/*/outputDelta` 帧都会推进（commands.rs:23657）。head 填充到 64K 字符的过程中 `chars().count()` 的总代价是 65536²/2 ≈ 2.1×10⁹ 次字符解码——单次 64KB 工具输出就要秒级 CPU；超过 head 上限后每来一个字符：一次 65537 次 count + 一次 `skip(1).collect()` 的 ~64KB String 重建。一个 1MB 输出的工具（常见于 build/测试日志）≈ 940K 次溢出循环 × ~130K 字符操作 ≈ 10¹¹ 级操作 + ~60GB 累计分配。这是 UI 线程可感知的卡死级热点。叠加 `render()`（:762-768）在每次 `accumulate_tool_output` 调用时全量 `format!("{}{}", head, tail)` 再被丢弃（见 F-perf-10）。

**根因**：作者关注的是 §6 内存有界（头尾保留 + 截断标记），正确性测试（codex_interaction_tests.rs:1551 的 "big" 用例）只验证语义没验证耗时；O(n²) 只在输出跨过 64K 阈值后爆发，短输出测试全绿。

**修复方向**：增量维护 `head_chars_count`/`tail_chars_count`；溢出用 `String::drain(..overflow_bytes)`（UTF-8 边界安全截断）一次完成而非重建；`accumulate_tool_output` 不再返回 render 全量。

---

### F-perf-02（blocking）每轮 provider 请求对全历史的 5~7 次深拷贝

**位置**：
- `crates/r-code-agent-worker/src/llm_runtime.rs:4509-4511`：锁内 `session.messages.clone()` + `session.model_projection.clone()`
- `llm_runtime.rs:4533/4540`：`model_projection.clone().unwrap_or_else(|| canonical_messages.clone())`（unwrap 分支对刚 clone 的 canonical 再 clone 一次）
- `llm_runtime.rs:5023`：`request_messages.extend(messages.iter().cloned())`（整历史再拷一份）
- `llm_runtime.rs:5067`：`let dispatch_ref_messages = request_messages.clone();`（无条件整份克隆，仅为了探测 Attachment 引用）
- `llm_runtime.rs:5037`：`system: Some(system_prompt.clone())`（数十 KB 系统提示每轮复制）+ `active_hosted_tools.clone()`
- `crates/r-code-agent-worker/src/agent_loop.rs:831-832`：`request.messages = messages.clone(); request.tools = tools.to_vec();`
- `agent_loop.rs:837`：`let attempt_request = request.clone();`（含全部 messages+tools 的整份克隆）
- `agent_loop.rs:881`：`provider.stream(attempt_request.clone())` —— 在 `'attempt` 重试循环**内部**，每次重试再克隆一份完整请求

**为什么热**：一次 agent 任务 = 多个工具轮，每轮都走上述全部拷贝。会话到 200KB 文本（几十轮工具调用后很常见）时，单轮 ≈ 6×200KB ≈ 1.2MB 深拷贝（含每条消息每块的 String/Value 递归分配）；30 轮任务 ≈ 36MB 分配 + 递归 drop，且随会话增长线性恶化——每轮成本 O(会话大小)，即"越长越慢"的用户可感知模式。`Message`/`ContentBlock` 无 Arc 字段（crates/r-code-core dto），工具输出（read_file 100KB 上限）进历史后单条消息即可放大全部系数。克隆计数佐证：llm_runtime.rs 全文 `.clone()` 288 处、agent_loop.rs 77 处（见 evidence）。

**根因**：三套正确性约束叠加——(a) canonical/model_projection 双轨历史需要快照隔离；(b) P1-E 流中断重放要求"重试字节逐字节一致"，于是冻结 `attempt_request` 又因 `provider.stream` 吃所有权再 clone；(c) `CompletionRequest` 按值传递的所有权链没有借用/Arc 通道。没人发现是因为每轮毫秒级浪费在短会话测试里不可见，成本随会话长度超线性累积。

**修复方向**：`SessionState.messages: Arc<Vec<Message>>`（写时复制追加）；`CompletionRequest` 支持 `Arc<[Message]>`/`Cow`；`provider.stream` 接收 `&CompletionRequest` 或 Arc；`dispatch_ref_messages` 仅在 `has_attachment_refs` 探测时需要——先用迭代器探测再决定是否物化克隆。

---

### F-perf-03（major）每事件单独 IPC emit：serde Value 双重序列化 + 无批内合并

**位置**：`src-tauri/src/main.rs:641-656`（sink 实现）；`src-tauri/src/commands.rs:8806`（drain 循环逐事件调用 `sink(&task_id, event)`）。

**证据**：
```rust
// main.rs:654-656
let payload = serde_json::json!({ "task_id": task_id, "event": event });
if let Err(e) = app_handle.emit("agent-event", payload) { ... }
```
drain 循环 40ms 一批（`AGENT_EVENT_DRAIN_INTERVAL`，commands.rs:8671），但批内**每个事件**仍各自走一次 `json!`（把 event 的字符串再拷进 Value 树）+ 一次 `emit`（Tauri 再把 Value serde 序列化成 JSON 字符串过 IPC）——同一 payload 序列化两次，且文本 delta 不做批内合并（注释声称"避免 WebView 被每个 token 的 IPC 淹没"，但 40ms 只限制了批频率，没减少消息条数：一次 burst 内 N 个 delta 仍是 N 条 IPC 消息）。

**为什么热**：流式回答每秒数十条 `Message{delta}` 事件，每条付：Value 树分配（String 拷贝）→ serde 序列化 → WebView 解析 → JS 对象分配。前端虽有 100ms coalescer 兜底渲染（model.ts:1153），IPC 通道与 JSON.parse 成本照付。同文件终端通道已示范了正确做法（main.rs:690-698 注释："PTY 只发轻量信号，WebView 按游标拉增量"）。

**根因**：sink 是全局单例闭包，签名 `Fn(&str, &AgentEvent)` 限制了批处理能力；"25 FPS 排空"的注释让作者误以为节流已解决消息量问题。

**修复方向**：sink 增加 `events: &[AgentEvent]` 批接口；drain 循环把同 task 的连续 `Message{delta:true}` 合并成一条（文本拼接）；payload 用 `#[derive(Serialize)] struct Envelope<'a>` 借用直发，消掉中间 Value。

---

### F-perf-04（major）每事件文件系统 stat：`ensure_session_log`

**位置**：`src-tauri/src/commands.rs:5429`（`session_file_path(...).exists()`）；调用点 `commands.rs:5756` —— 在 `persist_runtime_event` **入口、每个事件**（含每个文本 delta、每条 tool 输出 delta）执行。

**为什么热**：Windows 上 `Path::exists()` 是一次真实 syscall（CreateFile/GetFileAttributes），流式期间每秒数十事件 × 每事件 1 stat；且发生在 tokio 异步线程上的**同步**阻塞调用。结果是恒定 "文件永远存在" 的重复探测——典型的可用 HashSet 记忆化场景。

**根因**：`ensure_session_log` 是幂等防御（会话文件可能被删），作者选择了无状态实现；没有意识到它被塞进了每 delta 路径。

**修复方向**：`HashSet<String> ensured` 挂在 drain 循环状态；或 SessionStore 返回 `ensure_once` 句柄。

---

### F-perf-05（major）SessionStore 单事件 append：每事件开文件 + 全局锁注册表全表扫描

**位置**：`vendor/agent-contracts/crates/agent-store/src/session_store.rs:38-48`（`append_lock_for`：全局 `Mutex<HashMap<PathBuf, Weak>>` + `locks.retain(...)` 全表扫描每次调用执行）；`:87-103`（`append_batch`：每次 `OpenOptions::open → write_all → flush`，未缓存句柄）。

**证据**：
```rust
fn append_lock_for(path: &Path) -> Arc<SessionAppendLock> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<SessionAppendLock>>>> = ...;
    let mut locks = LOCKS...lock()...;
    locks.retain(|_, lock| lock.strong_count() > 0);   // 每次 append 全表扫描
    ...
}
```
`SessionStore::append` = `append_batch(&[event])`（:77-79）。热路径调用：`persist_runtime_event` 对每条非 delta 事件（ToolCall/ToolResult/State/完整 Message，commands.rs:5874-5920 等）各调一次 `session_store.append`。

**为什么热**：每条落盘事件 = 1 次全局锁获取 + O(活跃会话数) retain + 1 次 per-path tokio Mutex + 1 次 open/close + 1 次 flush。工具密集型任务每秒多条 ToolCall/ToolResult，叠加多任务并发（每个 drain 循环独立 SessionStore 实例共享同一全局锁注册表）时互相放大。

**根因**：注释自述"desktop 与 runtime drain 故意构造独立句柄"，为防 JSONL 交错引入按路径锁——正确性优先；`append_batch` 已存在但宿主热路径没用它（只在 steer/journal 批量场景用了）。retain 全表扫描是防 Weak 泄漏的懒惰清理，被放进了每次调用的关键路径。

**修复方向**：drain 循环每 40ms 批末 flush 一次 `append_batch`（delta 缓冲已有 PendingRuntimeText 先例）；`retain` 改为定期/容量阈值触发；可选：进程内缓存 append 句柄（`std::fs::OpenOptions::append` 句柄可长期持有）。

---

### F-perf-06（major）工具目录每轮全量重建 + 仅为取长度的全量 JSON 序列化

**位置**：
- `crates/r-code-gateway/src/gateway.rs:827-845`：`tool_specs()` 每次调用重建 `Vec<ToolSpec>`，逐工具 `name/description/input_schema` String/Value 克隆
- `src-tauri/src/mcp_manager.rs:1328-1337`：`tool_specs()` = `external_tool_specs()` + `direct_catalog...specs.clone()`（**整个 MCP 目录** Vec 深拷贝）
- `crates/r-code-agent-worker/src/llm_runtime.rs:4632`：run_turn 每轮调 `tool_host.tool_specs()`（SessionToolHost 再 filter/extend 一遍）；`:4657` 与 `:9669`：`serde_json::to_string(&tools).map(|json| json.len())` —— 把整个目录序列化成 JSON 字符串只为取长度
- `agent_loop.rs:832`：`request.tools = tools.to_vec()`（再整份克隆）；`agent_loop.rs:837`：`request.clone()` 里再拷一层

**为什么热**：接了 MCP 服务器的会话目录轻松达到几十上百个工具、每个 schema 数 KB（总计量级 100KB+）。每轮 = 目录重建 1 次 + MCP 目录深拷 1 次 + 全量序列化丢字符串 1 次 + to_vec 1 次 + request.clone 内再 1 次 ≈ 5 次目录级物化/轮。工具目录在两轮之间几乎从不变化（注册是启动/会话级事件）。

**根因**：注册表用 `HashMap<String, Vec<ToolEntry>>`（gateway.rs:636，栈式覆盖语义），快照语义下最简单的正确实现就是重建；`tools_json_len` 是 tokPerChar 校准输入（docs §6.1），作者选择了"序列化再量长度"的直接写法。

**修复方向**：注册表加版本号，`tool_specs` 返回 `Arc<[ToolSpec]>` 缓存（栈变更时失效）；`tools_json_len` 缓存于目录版本，或序列化进 `len()` 计数器。

---

### F-perf-07（major）Room 常驻轮询矩阵 + git 子进程每 2s + 会话全量重读

**位置与频率**（均挂 `usePoll`，窗口聚焦时常驻）：
- `src-tauri/frontend/src/components/scenes/RoomScene.tsx:354-357`：`refreshDetail` 2s——后端 `task_detail`（commands.rs:4199-4251）跑 ~8 组查询（task/ensure_active[含写]/branches/runs/400 events/changes/permissions/verifications/queued）
- `src-tauri/frontend/src/components/room/Canvas.tsx:1145-1147`：`refreshGitStatus` 2s——后端 `review_git_status`（commands.rs:10651-10672）→ `workspace_git_changes` → `GitService::status()` **spawn git 子进程**（git_service.rs:96,149），加上 `repo_root`/canonicalize 再 1-2 次进程/IO
- `Canvas.tsx:2678-2681`：终端输出 pullOutput 1s；`Canvas.tsx:2744`：terminalList 1.2s；`Canvas.tsx:2981-2985`：verificationList 2s；`Canvas.tsx:3005`：gitDeliveryStatus 10s（自注释"runs several subprocesses"）
- 连带：`Canvas.tsx:660-677` SummaryPanel 以 `auditStamp`（events/changes/verifications 计数）为依赖重取 `sessionMessages`，运行中 detail 每 2s 变化 ⇒ 近乎每 2s 全量 `read_to_string` + 逐行 parse 整个会话 JSONL（commands.rs:12428-12441）并全量 IPC 序列化

**为什么热**：空闲任务挂在 Room 上也全速轮询（active 条件只是 `currentTaskId != null`）；Windows 上 git 子进程 spawn 10-50ms，2s 一次 = 恒定 1-3% CPU + 大仓库下更贵的 status；会话 JSONL 到 MB 级后每 2s 的全量读+parse+序列化成为最大单项。多场景叠加（InboxScene 2s、Dashboard 2.5s）后 WebView 与后端双端付费。

**根因**：轮询是健壮性最简单方案（后端事件源已有 agent-event，但 detail 聚合数据无推送通道）；`usePoll` 做了失焦停表与签名对比（tasks.ts:64-74 注释明说避免二次序列化）说明作者优化过"渲染"侧，但没优化"请求"侧频率；git status 无文件系统 watcher。

**修复方向**：running=false 时 detail/git 轮询退避到 10-30s；detail 改为后端在 drain 循环里已知的变更时推 `task-detail-changed` 事件（permission/run/event 计数后端全都知道）；`cmd_session_messages` 加 since-cursor 增量（subagent 分页读取 `subagentSessionMessagePage` 已是现成先例，ipc.ts:1015-1026 注释明说"避免重读解析完整 JSONL"）。

---

### F-perf-08（major）工具 input 的多次深拷与 2~3 次全量 JSON 序列化

**位置**：
- `crates/r-code-agent-worker/src/agent_loop.rs:989-1002`（ToolUseComplete）：`input.clone()` ×2（assistant_blocks + emit ToolCall）+ `name/id.clone()` ×3-4
- `agent_loop.rs:461`（execute_pending_tool）：`call.input.clone()` 第 3 次深拷
- `crates/r-code-gateway/src/gateway.rs:930/941 + 975`：`execute_call` 路径 `input.to_string()` ×2（审计 ToolCall::new + `check_detailed_with_access_mode` 入参）；`:1114`：`execute_with_wait` 路径审计再 1 次（权限侧用 input_summary 已优化）

**为什么热**：`write_file` 携带整个文件内容（可达数十至上百 KB）时，单次调用的 input 承受：3 次 Value 深拷 + 2 次全量 JSON 序列化（每次都是 O(input) 的字符串构建）。工具调用是 agent 主循环最高频动作（每轮 1-N 个），长编辑会话中该成本线性叠加。

**根因**：`ToolHost::call_with_id(&self, ..., input: Value)` 按值接口迫使上游保留副本；审计 `input_json: String` 字段（dto.rs:551）要求序列化；权限引擎签名收 `&str` 又要求再序列化一次。各层接口都"各自正确"，串联起来重复付费。

**修复方向**：`PendingToolCall` 执行阶段 move input；审计记录 `Arc<str>` 延迟序列化或直接引用 Value；权限检查复用审计已序列化的字符串（一次 to_string 传两处）。

---

### F-perf-09（major）流式 Markdown 全文重解析（每 ~100ms 一次）

**位置**：`src-tauri/frontend/src/components/room/Markdown.tsx:44-60`。

**证据**：`useMemo(() => parseMarkdown(text), [text])` + `memo(Markdown)`。文件头注释自知："流式下每个 token 都会重渲染，重复解析整段是这里最贵的一笔开销"——但 memo 只对**非流式**场景有效：流式中 text 每个合并批（100ms，model.ts:1164 `intervalMs = 100`）都变，`parseMarkdown` 每次解析**整条已累积消息**，且 `nodes` 数组全新导致所有 `Block`（未 memo 的函数组件）全部重渲染（代码块的 `highlight` 有 `useMemo` 缓存可命中，Paragraph 类每批重建 ReactNode）。

**为什么热**：长回答（20KB+）流式 60s ⇒ ~600 次全文解析，总解析量 O(text²/tick) ≈ 6MB+ 纯解析 + 数万组件次重渲染，是前端流式掉帧的主要嫌疑。Timeline 侧 `renderTimelineItem` 内联 reduce/findIndex（Timeline.tsx:1105-1240）每渲染遍历全部可见 turn×item，属同一次渲染的次级放大项。

**根因**：自研 markdown 解析器没有增量接口；作者用 memo+useMemo 做了"能做的"，未做分段级缓存（解析结果按块指纹缓存）。

**修复方向**：把消息拆成稳定前缀块 + 活动尾块，仅对尾块重解析（streaming 时常量级）；或以 \n\n 切分后逐块 memo。

---

### F-perf-10（major）Codex 帧处理：整帧 params 深拷 + 每条输出 delta 的全量 render 被丢弃

**位置**：
- `src-tauri/src/commands.rs:23615`（observe_codex_app_server_event 入口）：`let params = value.get("params").cloned().unwrap_or_default();` —— 每帧（每个 notification，含每条 delta 帧）深拷整个 params Value 树
- `commands.rs:23657`：`let _ = projection.accumulate_tool_output(&item_id, &safe_delta);` —— 返回值（`buffer.render()` 全量 head+tail 拼接，最大 ~128KB String）被丢弃；`render` 在**每条** delta 上执行
- `codex_interaction.rs:801-804`：`accumulate_tool_output` 定义（push 后无条件 render 返回）

**为什么热**：Codex App Server 的 agentMessage/工具输出全部走该函数；文本 delta 帧的 params 含 delta 字符串（小），但工具 outputDelta 帧的 render 浪费立即可观：输出 1MB 的工具在 64K+ 区域时，每条 delta 都构建 ~128KB 字符串再立即 drop（分配器压力 + 与 F-perf-01 的 O(n²) 叠乘）。`params.cloned()` 对 completed 帧（含 authoritative 全文）则是整段文本的额外整份拷贝。

**根因**：`accumulate_tool_output` 的返回值签名是为测试方便（tests 直接断言累计文本）；宿主接线时用 `let _ =` 丢弃却没删掉 render 调用。`params` 深拷是因为后续多个分支都要用 `&params`，借用重构未做。

**修复方向**：`accumulate_tool_output` 返回 `()`（终态走 `take_tool_output` 已有权威输出）；`params` 改 `value.get("params")` 借用传递。

---

### F-perf-11（minor）sessions 全局锁内做 O(历史) 深拷

**位置**：`crates/r-code-agent-worker/src/llm_runtime.rs:4486-4521`：`ctx.sessions.lock().await` 持锁期间执行 `session.messages.clone()` + `session.model_projection.clone()`（见 F-perf-02 证据块）。锁对象是 `Arc<Mutex<HashMap<String, SessionState>>>`（:1921）覆盖**所有**会话。

**为什么热**：每轮 run_turn 都在该锁内做毫秒级深拷；期间其它会话的 steer 入队、abort 标记、history_snapshot、状态查询全部排队。桌面单用户通常 1-3 个并发会话，故 minor；但多子代理并发（delegation）时同进程子会话共享此 map，争用放大。

**修复方向**：锁内只做 `Arc` 快照/增量交换，深拷出锁后再做（配合 F-perf-02 的 Arc 化自然消除）。

---

### F-perf-12（minor）Codex 投影器每 delta 线性扫 + 每条 delta 两次分配

**位置**：`src-tauri/src/codex_interaction.rs:649-672`（push_delta）：先 `items.iter().any(...)` 查墓碑再 `items.iter_mut().find(...)` 定位，每条 delta 两次 O(items) 扫描（items 上限 1024，:532）；返回 `Delta { item_id: item_id.to_string(), delta: delta.to_string() }` 每条 delta 两次 String 分配（item_id 已知可复用）。

**修复方向**：`HashMap<String, usize>` 索引；emission 借用或 `Arc<str>` item_id。

### F-perf-13（minor）mcp-status 全量克隆广播

**位置**：`src-tauri/src/main.rs:716-722`：每次状态变更 `values().cloned().collect::<Vec<_>>()` 克隆全部服务器状态（含每个 server 的元数据串）再 emit。变更单一服务器也全量重发。修复：发变更增量，前端按 id 合并（watch 已有 `borrow_and_update` 语义可判断变更集合）。

### F-perf-14（minor）dev 构建下每条 debug 日志付 11 regex 脱敏

**位置**：`src-tauri/src/logging.rs:93`（init_dev 默认 `debug` 级）；`log_buffer.rs:91-113`（on_event：MessageVisitor 格式化 + `redact_text` + `entry.clone()` + JSONL 序列化）；`crates/r-code-core/src/secret.rs:614-665`（11 个正则，已 LazyLock）。生产 `info` 级不受影响；dev 热路径（agent_loop/gateway 的 per-tool debug!）逐条付费。修复：BufferLayer 对 debug 级只进环形缓冲不落盘/或延迟脱敏。

### F-perf-15（minor）双出口事件的整事件克隆

**位置**：`src-tauri/src/commands.rs:20684-20686`（emit_codex_observable_event：`event_sink(event.clone())`）与 `:23986-23987`（emit_codex_frontend_event 同型）。大 delta 文本/工具输出事件被整段复制一次仅为第二个消费者复用。修复：sink 签名改 `&AgentEvent`（main.rs sink 本来就收引用）或事件 `Arc` 化。

### F-perf-16（minor）journal 模式每轮指纹的全历史 Value 往返

**位置**：`crates/r-code-agent-worker/src/llm_runtime.rs:4111-4130`（fingerprint_request_envelope：`to_value(system/tools/messages)` 深拷入 Value 树 → `to_vec` 再序列化 → SHA-256）+ `:5100-5131`（每轮调用，journal 接线时）+ `:5067`（`dispatch_ref_messages` 无条件整历史克隆即使无附件）。为"键序稳定哈希"付了两次全量物化。修复：直接对原类型用 serde 序列化进哈希器（结构体字段序本身就是稳定口径）；dispatch_ref 仅在 `has_attachment_refs` 探测（可先 iter 探测后决定）。

---

## 计数汇总（详见 evidence 文件）

| 模式 | commands.rs | llm_runtime.rs | agent_loop.rs | gateway.rs | codex_interaction.rs | mcp_manager.rs |
|---|---|---|---|---|---|---|
| `.clone()` | 835 | 288 | 77 | 54 | 23 | 47 |
| `format!` | 416 | 174 | — | 16 | — | — |

热路径代表性位置（非全量）：llm_runtime.rs:4510/4511/4533/4540/5023/5067（历史克隆链）、agent_loop.rs:831/837/881（请求冻结链）、main.rs:654（IPC payload）、commands.rs:5756（每事件 stat）、gateway.rs:941/975（input 序列化×2）。

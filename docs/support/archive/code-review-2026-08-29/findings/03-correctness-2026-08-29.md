# 03 正确性维度 Findings（2026-08-29）

审查人：RV-03（正确性：错误处理、panic 面、并发正确性、锁）。全程代码只读，未修改任何代码/配置/测试文件。

## 扫描方法与覆盖声明

- **工具**：rg + 自研 awk 跨度检测器做 `#[cfg(test)]` 精确切分（本仓库测试大量内联在生产文件尾部与中部，按文件计数会高估约 40 倍；commands.rs 1395 处 unwrap 中 1370 处在测试区）。检测器对字符串字面量/原始字符串/注释做了括号平衡处理，commands.rs 结果与人工逐段核对一致。
- **覆盖**：全仓 224 个 .rs 文件（crates/ 6 个 crate + src-tauri 宿主层 + vendor/agent-contracts + installer）。清单六项全部完成：unwrap/expect/panic 面测绘（含四类分桶）、panic 宏、错误传播策略、并发正确性（guard 跨 await / 锁粒度 / r2d2 / spawn）、整数与切片、前端。大文件（commands.rs 41k 行、mcp_manager.rs 117K 等）均 rg 定位后局部 Read，未通读。
- **验证深度**：所有 major/minor finding 的触发条件均通过 Read 源码上下文确认（守卫存在性、锁类型 std/tokio 区分、不变量链条）。
- 完整命令原文与输出见 `../evidence/RV-03-correctness.md`。
- vendor/agent-contracts 为公共合同，只记录不改。

**总体结论：无 blocking 级发现。** 该代码库的并发与 panic 纪律显著高于同规模平均水平：std 锁跨 await 为 0、DB 连接在 await 前显式词法块释放（含注释文档化）、工具 panic 有 CatchUnwind 隔离、前端 0 处 `as any`。发现集中在「poisoning 级联」「错误类型边界拍平」「跨模块不变量同步」三类系统性弱点。

## Findings 总表

| ID | 位置 | severity | 根因描述 | 修复方向 |
| --- | --- | --- | --- | --- |
| F-corr-01 | 全仓（证据 §1） | minor | 生产 unwrap/expect 面 159 处 vs 全仓 6601；CI clippy 仅 `-D warnings` 默认集，无 unwrap_used/expect_used restriction lint，新增退化管理靠人工 review | `[workspace.lints]` 启用 `clippy::unwrap_used`/`expect_used`（allow 列表豁免静态常量类） |
| F-corr-02 | src-tauri/src/log_buffer.rs:51,91,137 | minor | 全局静态 `BUFFER: OnceLock<Mutex<VecDeque>>` 每条 tracing 事件 `lock().unwrap()`；一旦持锁 panic 引发 poison，此后**所有日志事件连锁 panic**（自增强故障） | 改 `unwrap_or_else(|p| p.into_inner())`（日志缓冲无不变量需要毒化保护） |
| F-corr-03 | src-tauri/src/commands.rs:337,1355,1368,8091,8524,8666,20058,25722,25904；native_notification.rs:170,198,240 | minor | `agent_event_sink`/`locale`/`delivered_sources` std Mutex `lock().unwrap()`；clone-and-drop 模式本身正确（无跨 await），但 poisoning 即级联：sink 注入线程 panic 后所有事件广播全部 panic | 同 F-corr-02 恢复策略；或换 parking_lot（无毒化语义） |
| F-corr-04 | llm_runtime.rs:128,2141,2169,2388,7345-7399,8030,8261,8883,8974,10466；delegation_tree.rs:124,178,208,268,328；gateway.rs:718-831（9 处）；browser/installer.rs:27-61；commands.rs:21312,21320,21380,21475,21742,24819；plan_entry_commands.rs:48,64；plan_policy.rs:350-364；mcp/installer.rs:92,114；terminal/manager.rs:196,276,425；session_store.rs:40；log_buffer/native_notification（并入 F-corr-02/03） | minor | `lock().expect("... poisoned")` 家族约 35 处：触发条件 = 任意持锁路径 panic → 该子系统后续全部访问 panic。当前持锁区间全部为短作用域纯内存操作，真实触发概率低 | 统一 `unwrap_or_else(PoisonError::into_inner)` 辅助函数，一次性消除整类 |
| F-corr-05 | vendor/agent-contracts/crates/agent-llm/src/lib.rs:147,159,171,182 ↔ src-tauri/src/commands.rs:14805 | minor | dialect 解构用 `dialect_for(...).unwrap_or_else(\|\| unreachable!())`；安全性依赖 commands.rs:14805 白名单 `matches!(*kind, "ark_coding"\|"ark_agent"\|"ark_coding_openai")` 与 dialect_for 内部分派表**两处列表手工同步**——当前一致，但新增 kind 只改白名单漏改 dialect 表即变成「用户配置直接触发生产 panic」 | 把 kind 建模为枚举（serde 反序列化即校验），白名单与 dialect 分派合一 |
| F-corr-06 | src-tauri/src/mcp_manager.rs:458,461 | minor | `McpSupervisor::new(configs).expect("MCP settings service returns only validated configurations")`：跨模块自证假设（settings 层验证 ⇒ 构造必胜），比同函数守卫弱；未来绕过 settings 注入配置的调用方会踩 panic | `McpSupervisor::new` 返回 Result 并传播 |
| F-corr-07 | 宿主层边界（commands.rs 等 31 文件） | major | 错误类型分层断裂：crate 层有 `ProductError`（thiserror，1918 处使用），但 196 个 tauri command 中 177 个返回 `Result<_, String>`（94%），另有 22 处 `.map_err(\|e\| e.to_string())` 把错误拍平为字符串——源因链、错误种类、定位上下文在 IPC 边界全部丢失；anyhow 仅 main.rs:1018 一处（孤立方言）。前端靠 `toUserFacingIpcError`/`commandErrorPayload` 猜测性恢复结构（仅当 String 恰好是结构化负载时成功） | command 边界统一 `CommandError { code, message, detail }` 序列化错误（仓库已有 CommandError/UserFacingError 雏形，7+3 处使用），ProductError 实现 Into<CommandError> |
| F-corr-08 | vendor/agent-contracts/crates/agent-ipc/src/protocol.rs:104 | minor | `payload.len() as u32` 线框长度写侧静默截断：payload >4GiB 时长度字段回绕、对端解析错位（读侧 protocol.rs:116 有 16MiB cap 可拦截，故实际触发需先绕过 16MiB 上游约束；理论缺陷） | `u32::try_from(len).map_err(...)` 显式拒绝 |
| F-corr-09 | commands.rs:8691,9680,19029,20980,24900,25136,25379,25494；main.rs:440,662,692,713,752；memory_runtime.rs:26；codex_mcp.rs:217；ipc/server.rs:115,140；verification.rs:321,328；updater/mod.rs:76,85 | minor | fire-and-forget `tokio::spawn` 约 15 处丢弃 JoinHandle：spawn 体内部错误均有 tracing 日志（纪律好），但**任务 panic → JoinError 被静默丢弃**，无日志无通知（对比：gateway.rs:549-568 对 in-process 工具 panic 有显式 CatchUnwind 隔离并转错误——主循环 spawn 无同等保护） | 封装 `spawn_supervised(fn)`：包装 JoinHandle 记录 panic 于 tracing::error |
| F-corr-10 | crates/r-code-store/src/change_service.rs（43 个 async fn）、review.rs（20）、verification.rs（13）等 | minor | 同步阻塞 IO（rusqlite 查询、`std::fs::read`）直接在 async fn 体内执行于 tokio worker 线程：不产生数据竞争，但长查询/慢盘会占住 worker 阻塞同核其他任务（含 UI 事件转发）。仓库已有 `spawn_blocking` 先例（tauri_commands.rs:1114,1342、updater、native_notification）但 store 路径未用 | store 阻塞段包 `spawn_blocking`，或 store API 提供 blocking 变体由宿主选择 |
| F-corr-11 | src-tauri/frontend（全量扫描） | 无发现（正面） | `as any` = 0；非空断言仅 12 处且全部为判别联合窄化后使用；轮询（lib/poll.ts 统一 try/catch + reportSyncFailure）、启动刷新（App.tsx void…catch）、IPC（ipc.ts 统一包装）错误路径全覆盖 | 无需修复；维持现状 |

## 逐条展开

### F-corr-01 生产 unwrap/expect 面（minor）

**证据**：全仓 `.unwrap()` 5720 + `.expect(` 883 = 6601；其中 tests/ 目录与 *_tests.rs 文件 954 处（(a) 类可接受）；内联 `#[cfg(test)]` 跨度内约 5488 处；**真实生产面 159 处**（(c)(d) 类）。四类分桶：

- (a) 测试：954 + 内联 ~5488；
- (b) bin 入口（弱接受）：main.rs:279（Runtime::new）、959（tauri build）、379/388、lifecycle_commands.rs:20、installer/src/main.rs:618，共 6 处；
- (c) 库/命令路径：~110 处，其中静态常量自证 ~20（secret.rs:620-664 十一条 `Regex::new(字面量).unwrap()`、web.rs:336/366 静态 URL、native_notification.rs:132/136 内嵌 JSON 目录）＋「checked above」同函数守卫不变量 ~45（llm_runtime 的 candidate slot/descriptor 系列、commands.rs:14821-14872/24161/24283-24310——均已 Read 验证守卫真实存在）＋跨模块自证 2 处（F-corr-06）＋mock.rs 2 处（dev/test runtime）；
- (d) lock().unwrap()/expect()：44 处（F-corr-02/03/04 详述）。

**触发条件分析**：(c) 类中「真正可能 panic」的只有跨模块自证两处（F-corr-05/F-corr-06）与 poisoning 级联（F-corr-02/03/04）；其余为编译期常量或同函数可见守卫。
**为什么存在/未被拦住**：CI（.github/workflows/ci.yml:134）只跑默认 clippy lint 集；无 `[workspace.lints]` restriction 配置。unwrap 治理依赖 review 习惯而非机器门禁，新增代码无强制约束。

### F-corr-02 log_buffer 全局锁 poison 级联（minor）

**代码证据**（src-tauri/src/log_buffer.rs:48-51, 88-97, 135-137）：

```rust
static BUFFER: OnceLock<Mutex<VecDeque<LogEntry>>> = OnceLock::new();
...
pub fn tail(...) { let buf = buffer().lock().unwrap(); ... }          // :51
let mut buf = buffer().lock().unwrap(); buf.push_back(entry.clone()); // :91（每条日志）
let mut buf = buffer().lock().unwrap(); buf.clear(); ...              // :137
```

**触发条件**：任何线程在持锁窗口内 panic（如 `entry.clone()` 分配 OOM、或未来有人在锁内加可 panic 逻辑）→ 锁毒化 → 之后**每一个 tracing 事件**（经 BufferLayer）都 `unwrap()` panic。日志是故障排查的最后通道，此处级联会同时摧毁可观测性。日志环形缓冲无跨调用不变量，`into_inner()` 恢复完全安全。
**为什么存在**：Rust 标准教程惯用法；毒化风险通常被接受，但放在「每事件必经」路径上时放大系数是全进程。

### F-corr-03 agent_event_sink 等状态锁 unwrap（minor）

**代码证据**（commands.rs:1019 声明 `Mutex<Option<AgentEventSink>>`；:337/:1368 等 10 处 `lock().unwrap().clone()`；native_notification.rs:170/198/240）。clone-and-drop 模式本身正确：guard 在语句末释放、sink 回调在锁外执行、无 await。风险仅在毒化级联（set_agent_event_sink 在启动期 panic 会毒化，之后 emit_agent_event 全部 panic）。
**为什么存在**：与 F-corr-02 同根因——std Mutex 毒化语义 + unwrap 惯用法。

### F-corr-04 poisoned-lock expect 家族（minor）

**代码证据**：44 处生产 lock().unwrap()/expect() 中约 35 处为 `.expect("xxx poisoned")` 风格（完整 file:line 清单见总表）。抽验代表：
- gateway.rs:718 `self.tools.write().expect("tool registry poisoned")`（工具注册表 RwLock，9 处同型）；
- llm_runtime.rs:128 `self.used.lock().expect("subagent name allocator poisoned")`（子代理假名分配，12 处同型）；
- delegation_tree.rs:124-328（6 处）。

全部为短作用域（获取→改字段→drop），无跨 await、无嵌套双锁（逐一 Read 确认）。触发条件 = 持锁区间 panic → 该子系统永久 panic（直到重启）。工具注册表若在执行 panic（gateway 已有 CatchUnwind 隔离工具执行期 panic，但注册期无保护）。
**为什么未被拦住**：`expect("poisoned")` 是 Rust 社区对毒化的标准表态（fail fast），无 lint 区分「锁内无不变量可破坏」的场景。

### F-corr-05 dialect 白名单双表同步隐患（minor）

**代码证据**：commands.rs:14803-14808 构造侧白名单——

```rust
let ark_kind = pcfg.provider_kind.as_deref().map(str::trim)
    .filter(|kind| matches!(*kind, "ark_coding" | "ark_agent" | "ark_coding_openai"))
    .map(str::to_string);
```

消费侧 vendor/.../agent-llm/src/lib.rs:147-182——

```rust
let dialect = dialect_for(&kind, &model, DialectPort::AnthropicMessages)
    .unwrap_or_else(|| unreachable!("{kind} anthropic dialect must resolve"));
```

dialect_for（dialect.rs:161-170）只认识 `ark_coding/ark_agent/ark_coding_openai/kimi_coding` 四个字符串。**当前两表一致，不可触发**；但这是一个跨 crate、跨文件的手工同步不变量：给 commands.rs 白名单新增 kind（如 `ark_agent_openai`）而漏改 dialect_for，用户 settings 里填该 kind 即触发生产 `unreachable!()` panic。`kind` 字段类型是裸 `String`，类型系统不设防。
**为什么存在**：vendor 合同层只暴露字符串接口（避免宿主依赖），宿主侧用运行时白名单补校验，两处分离演化。

### F-corr-06 mcp_manager 跨模块自证 expect（minor）

mcp_manager.rs:458 `McpSupervisor::new(connector, configs).expect("MCP settings service returns only validated configurations")`、:461 `RegistryClient::official(...).expect("... valid static URL")`。后者是真静态常量（安全）；前者依赖「mcp_settings 产出的 configs 必然合法」这一**跨模块合同**——settings 模块任何演化（如放宽校验、新增来源）都会在无声中把 panic 面扩大到此。已 Read 确认当前 settings 侧确有校验，故不可触发，定级 minor。

### F-corr-07 错误类型在 IPC 边界拍平（major）

**计数证据**：`Result<_, String>` 580 处/31 文件（非测试）；tauri command 177/188 返回 String（94%），typed 错误仅 UserFacingError 7 + CommandError 3；`.map_err(|e| e.to_string())` 22 处；crate 层 `ProductError`（thiserror 派生、含 `#[error]` 上下文）使用 1918 处——说明类型化错误系统存在且被广泛使用，但**在最靠近用户的一层被统一拍平成字符串**。

**缺陷后果**（错误处理缺陷，非崩溃）：
1. 源因链丢失：`ProductError::DatabaseError(format!(...))` 已内嵌上下文，但 `map_err(|e| e.to_string())` 之后 `#[from]`/source 链消失，只能靠 Display 文本；
2. 位置上下文丢失：22 处 to_string 不附加 command 名/文件路径，前端报错定位困难；
3. 前端只能启发式恢复：ipc.ts `commandErrorPayload` 仅当异常对象恰好有 `{code,message}` 字段时恢复结构——返回 String 的 177 个 command 全部退化为裸字符串 toast。

**为什么存在/未被拦住**：Tauri 早期模板惯例即 `Result<T, String>`（String 实现 Serialize）；仓库已开始向 UserFacingError/CommandError 迁移但只覆盖 10/188，迁移无计划表，无 lint（如 `clippy::result_large_err` 不覆盖此）强制收敛。

**修复方向**：为 ProductError 实现 `Into<CommandError>`，新 command 一律 typed；存量 String 按接触面逐步替换（前端 payload 解析已就绪，向后兼容）。

### F-corr-08 IPC 线框长度截断（minor）

protocol.rs:104 `let len = payload.len() as u32;`——usize→u32 静默截断。读侧 :116 `if len > 16*1024*1024` 拒绝超限帧，故对端会以"frame too large"报错而非解析错位，攻击面与实际触发概率均极低（写入侧 payload 均为序列化消息，16MiB 内）。定级 minor（正确性完备性修补）。

### F-corr-09 fire-and-forget spawn 无 panic 监督（minor）

38 处生产 spawn 中 ~15 处丢弃 JoinHandle（清单见总表）。已验证 spawn 体内部 `Result` 错误路径均有 `tracing::warn/error`（如 commands.rs:9680 重试循环带完整日志；main.rs 事件转发循环处理 Lagged/Closed）。缺口：任务体 panic 时 JoinError 随句柄丢弃，**panic 无日志**（tokio 默认不打印）。gateway.rs:549-568 对工具执行 panic 的 CatchUnwind 隔离是仓库内已有的正确范式，推广到主 spawn 即可。
**为什么存在**：tokio::spawn 返回值按惯例忽略；无 `tokio_unhandled_ignores` 类 lint。

### F-corr-10 store 层阻塞 IO 在 async 上下文（minor）

change_service.rs 43 个 async fn、review.rs 20、verification.rs 13 内直接执行 rusqlite 查询与 `std::fs::read`（如 capture_baseline:342 `std::fs::read` + :356 `db.conn()`）。已验证无连接跨 await（F-corr 无并发违规），但同步 IO 占用 tokio worker：默认 worker = CPU 核数，一个慢查询即可延迟同 worker 上的 UI 事件转发。仓库已存在 spawn_blocking 用法（tauri_commands.rs:1114/1342 等 4 处），说明模式已知但未覆盖 store 热路径。属正确性/性能边界问题，定 minor（多线程 runtime 下不致死锁，仅延迟放大）。

### F-corr-11 前端正确性（无发现）

`as any` 0 处（全仓含 scripts）；非空断言 12 处全部为判别联合守卫后的窄化（如 `item.kind === "permission" ? item.permission!.id`，InboxScene.tsx:46）；`.catch` 85 处 / `try` 332 处 / async 338 处；轮询统一走 lib/poll.ts 的 try/catch + reportSyncFailure 通道（:40-46），启动刷新 void promise 均挂 catch（App.tsx:93-100），IPC 错误统一在 ipc.ts:172-186 包装。抽查 store/tasks.ts 与 lib/ 未发现未捕获 await 在用户路径裸奔。**无需修复**。

## 正面发现（供其他维度参考）

1. **std guard 跨 await = 0**：40 处疑似全部验证为 tokio 锁或测试区；plan_review.rs:712 显式注释文档化「rusqlite guard 词法块内先 drop 再 await 保 Send」——这是团队级纪律而非偶然。
2. **panic 隔离架构**：gateway.rs 对 in-process 工具执行 panic 的 CatchUnwind 隔离并转为 `ProductError`（:549-568），panic 不穿越工具边界。
3. **DB 池配置**：r2d2 max_size=8 + WAL + busy_timeout=5000 + 建池前 bootstrap 连接统一设 journal_mode（database.rs:42-46 注释解释了竞态原因）；无嵌套双连接。
4. **`todo!`/`unimplemented!` 生产区 0 处**；panic 宏生产区仅 16 处且 12 处为循环穷尽/枚举穷尽守卫。
5. 前端 0 `as any`（同规模 React+TS 项目罕见）。

# RV-06 性能维度证据（2026-08-29）

全部命令在 `D:/project/rust/r-code` 下执行（Git Bash + rtk 代理）。输出为原文摘录或计数。

## E1 `.clone()` 总计数（热文件）

```
$ rg -c '\.clone\(\)' crates/r-code-agent-worker/src/agent_loop.rs crates/r-code-agent-worker/src/llm_runtime.rs src-tauri/src/codex_interaction.rs src-tauri/src/commands.rs crates/r-code-gateway/src/gateway.rs crates/r-code-gateway/src/tools.rs src-tauri/src/mcp_manager.rs

src-tauri/src/mcp_manager.rs:47
crates/r-code-gateway/src/tools.rs:2
crates/r-code-gateway/src/gateway.rs:54
src-tauri/src/codex_interaction.rs:23
crates/r-code-agent-worker/src/agent_loop.rs:77
crates/r-code-agent-worker/src/llm_runtime.rs:288
src-tauri/src/commands.rs:835
```

## E2 format! 计数

```
$ rg -c "format!" crates/r-code-agent-worker/src/llm_runtime.rs src-tauri/src/commands.rs crates/r-code-gateway/src/gateway.rs

crates/r-code-gateway/src/gateway.rs:16
crates/r-code-agent-worker/src/llm_runtime.rs:174
src-tauri/src/commands.rs:416
```

## E3 每轮全历史克隆链（F-perf-02）

```
$ rg -n "messages.clone\(\)|request.messages|to_vec\(\)" crates/r-code-agent-worker/src/llm_runtime.rs | head -40
2632:        async fn history_snapshot(
2640:        Ok(Some(session.messages.clone()))
3803:    let mut out = messages.to_vec();
3984:    let mut out = messages.to_vec();
4510:                session.messages.clone(),
4511:                session.model_projection.clone(),
4533:            .unwrap_or_else(|| canonical_messages.clone());
4540:                    messages = canonical_messages.clone();
4552:                    session.messages = canonical_messages.clone();
4563:                    messages: canonical_messages.clone(),
4708:                    .to_vec();
4750:                        model_projection = Some(messages.clone());
5294:                    model_projection = Some(messages.clone());
5431:                    session.messages = canonical_messages.clone();
5023:        request_messages.extend(messages.iter().cloned());
5067:        let dispatch_ref_messages = request_messages.clone();

$ sed -n '828,838p' crates/r-code-agent-worker/src/agent_loop.rs
    request.messages = messages.clone();
    request.tools = tools.to_vec();
    let attempt_request = request.clone();

$ rg -n "provider.stream\(attempt_request.clone" crates/r-code-agent-worker/src/agent_loop.rs
881:        let connection = provider.stream(attempt_request.clone());
```

## E4 工具目录每轮重建 + 仅为长度的序列化（F-perf-06）

```
$ rg -n "let tools_json_len" crates/r-code-agent-worker/src/llm_runtime.rs
4657:        let tools_json_len = serde_json::to_string(&tools)
9669:            let tools_json_len = serde_json::to_string(&tools)

$ sed -n '4657,4659p' crates/r-code-agent-worker/src/llm_runtime.rs
        let tools_json_len = serde_json::to_string(&tools)
            .map(|json| json.len())
            .unwrap_or(0);

$ sed -n '827,845p' crates/r-code-gateway/src/gateway.rs   # tool_specs 每次重建
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        let mut specs = self.tools.read()...values()
            .filter_map(|stack| stack.last())
            .map(|entry| { ... ToolSpec { name: tool.name().to_string(), description: ... } })

$ sed -n '1328,1337p' src-tauri/src/mcp_manager.rs        # MCP 目录整份 clone
    fn tool_specs(&self) -> Vec<ToolSpec> {
        let mut specs = external_tool_specs();
        specs.extend(
            self.direct_catalog.read()...specs.clone(),
        );
```

## E5 IPC 每事件 emit + 双重序列化（F-perf-03）

```
$ rg -n 'serde_json::json!\(\{ "task_id": task_id, "event": event \}\)' src-tauri/src/main.rs
654:                let payload = serde_json::json!({ "task_id": task_id, "event": event });

$ rg -n "sink\(&task_id, event\)" src-tauri/src/commands.rs
8806:                    sink(&task_id, event);

$ rg -n "AGENT_EVENT_DRAIN_INTERVAL" src-tauri/src/commands.rs
8671:const AGENT_EVENT_DRAIN_INTERVAL: Duration = Duration::from_millis(40);
```

## E6 每事件 fs stat（F-perf-04）

```
$ rg -n "ensure_session_log\(session_store" src-tauri/src/commands.rs
5756:    if let Err(error) = ensure_session_log(session_store, sessions_dir, &event_storage_id).await {   # persist_runtime_event 入口，每事件
6298:    ensure_session_log(session_store, sessions_dir, &branch.storage_id).await?;

$ sed -n '5424,5431p' src-tauri/src/commands.rs
async fn ensure_session_log(...) -> Result<(), String> {
    if session_file_path(sessions_dir, storage_id).exists() {   // 每调用一次 syscall
        return Ok(());
    }
```

## E7 SessionStore append 全局锁 + 开文件（F-perf-05）

```
$ sed -n '37,48p' vendor/agent-contracts/crates/agent-store/src/session_store.rs
fn append_lock_for(path: &Path) -> Arc<SessionAppendLock> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<SessionAppendLock>>>> = OnceLock::new();
    let mut locks = LOCKS.get_or_init(...).lock()...;
    locks.retain(|_, lock| lock.strong_count() > 0);      # 每次 append 全表扫描
    ...

$ sed -n '77,103p' vendor/agent-contracts/crates/agent-store/src/session_store.rs
    pub async fn append(&self, session_id: &str, event: SessionEvent) -> Result<()> {
        self.append_batch(session_id, &[event]).await      # 单事件 → 单开单写
    }
    pub async fn append_batch(...) {
        ...
        let _guard = append_lock.lock().await;
        let mut file = tokio::fs::OpenOptions::new().append(true).create(true).open(&path).await?;
        file.write_all(&encoded).await?;
        file.flush().await?;
```

## E8 Codex 输出缓冲 O(n²)（F-perf-01 / F-perf-10）

```
$ sed -n '733,775p' src-tauri/src/codex_interaction.rs
    const HEAD_CHARS: usize = 65_536;
    const TAIL_CHARS: usize = 65_536;
    fn push(&mut self, delta: &str) {
        for ch in delta.chars() {
            if !self.head_full {
                if self.head.chars().count() >= Self::HEAD_CHARS { ... }   # 每字符 O(head.len())
                else { self.head.push(ch); continue; }
            }
            self.tail.push(ch);
            if self.tail.chars().count() > Self::TAIL_CHARS {              # 每字符 O(tail.len())
                let keep: String = self.tail.chars().skip(overflow).collect();  # 每字符 ~64KB 重建
                self.tail = keep;

$ rg -n "accumulate_tool_output" src-tauri/src/commands.rs src-tauri/src/codex_interaction.rs
src-tauri/src/codex_interaction.rs:801:    pub fn accumulate_tool_output(&mut self, item_id: &str, delta: &str) -> String {
src-tauri/src/commands.rs:23657:                let _ = projection.accumulate_tool_output(&item_id, &safe_delta);  # render() 全量结果被丢弃

$ sed -n '23615,23616p' src-tauri/src/commands.rs
23615:    let params = value.get("params").cloned().unwrap_or_default();    # 每帧深拷整棵 params
```

## E9 工具 input 多次序列化（F-perf-08）

```
$ rg -n "input.to_string\(\)" crates/r-code-gateway/src/gateway.rs
930:                ToolCall::new(run_id, task_id, tool_name, input.to_string(), risk_level);
941:        let mut audit = ToolCall::new(run_id, task_id, tool_name, input.to_string(), risk_level);
975:                &input.to_string(),          # execute_call 路径权限检查入参
1114:        let mut audit = ToolCall::new(run_id, task_id, tool_name, input.to_string(), risk_level);
1294:        let mut audit = ToolCall::new(run_id, task_id, tool_name, input.to_string(), risk_level);

$ sed -n '989,1002p' crates/r-code-agent-worker/src/agent_loop.rs   # ToolUseComplete
    assistant_blocks.push(ContentBlock::ToolUse { id: id.clone(), name: name.clone(), input: input.clone(), ... });
    emit(AgentEvent::ToolCall { name: name.clone(), input: input.clone(), call_id: id.clone(), ... });
$ sed -n '460,462p' crates/r-code-agent-worker/src/agent_loop.rs
    let execution = tool_host.call_with_id(&call_id, &tool_name, call.input.clone());  # 第 3 次深拷
```

## E10 前端轮询矩阵（F-perf-07）

```
$ rg -n "usePoll\(" src-tauri/frontend/src -g '*.tsx' -g '*.ts' --glob '!*.test.*' | head -20
src\components\plan\useTaskPlan.ts:62
src\components\shell\Rail.tsx:62
src\components\shell\MenuBar.tsx:52            # 15s 通知
src\components\scenes\HomeScene.tsx:201,226
src\components\scenes\SettingsScene.tsx:2907   # 1.5s 日志尾部
src\components\scenes\InboxScene.tsx:147       # 2s
src\components\scenes\DeckScene.tsx:65
src\components\scenes\RoomScene.tsx:354        # 2s refreshDetail
src\components\scenes\DashboardScene.tsx:59    # 2.5s
src\components\scenes\MemoryPanel.tsx:118      # 5s
src\components\scenes\ConversationsScene.tsx:46
src\components\scenes\ActivityScene.tsx:28
src\components\room\Canvas.tsx:1145            # 2s refreshGitStatus
src\components\room\Canvas.tsx:2678            # 1s 终端输出 pull
src\components\room\Canvas.tsx:2744            # 1.2s terminalList
src\components\room\Canvas.tsx:2981            # 2s verificationList
src\components\room\Canvas.tsx:3005            # 10s gitDeliveryStatus（多子进程）
src\components\scenes\ArchiveScene.tsx:35      # 5s

$ sed -n '354,357p' src-tauri/frontend/src/components/scenes/RoomScene.tsx
  usePoll(
    () => (currentTaskId ? refreshDetail(currentTaskId) : undefined),
    2000,
    currentTaskId != null,      # 空闲任务也全速轮询
  );

$ rg -n "Command::new\(\"git\"\)|fn status\(" crates/r-code-store/src/git_service.rs
96:        let mut command = Command::new("git");      # repo_root 等走 git 子进程
149:    pub fn status(&self) -> Result<Vec<GitFileStatus>, ProductError> {
611:        let mut cmd = Command::new("git");

$ sed -n '12428,12441p' src-tauri/src/commands.rs     # cmd_session_messages 全量读
    let content = match tokio::fs::read_to_string(&path).await { ... }
    Ok(session_messages_for_task(state, task_id, &content, ...).await)

$ sed -n '660,677p' src-tauri/frontend/src/components/room/Canvas.tsx  # auditStamp 变化即全量重取
  const auditStamp = detail ? `${detail.events.length}:${detail.changes.length}:${detail.verifications.length}` : "";
  useEffect(() => { ... sessionMessages(taskId).then((list) => ...); }, [taskId, auditStamp]);
```

## E11 流式 Markdown 重解析（F-perf-09）

```
$ sed -n '44,51p' src-tauri/frontend/src/components/room/Markdown.tsx
export const Markdown = memo(function Markdown({ text, streaming = false, ... }) {
  const nodes = useMemo(() => parseMarkdown(text), [text]);   # 流式中 text 每 100ms 变 ⇒ 全文重解析

$ rg -n "intervalMs = 100|const flush" src-tauri/frontend/src/components/room/model.ts
1153:export function createAgentEventCoalescer(apply, schedule, intervalMs = 100) { ... }
```

## E12 其余 minor 佐证

```
$ rg -n "sessions: Arc<Mutex<HashMap" crates/r-code-agent-worker/src/llm_runtime.rs
1921:    sessions: Arc<Mutex<HashMap<String, SessionState>>>,   # F-perf-11，4503-4520 持锁做 messages.clone

$ sed -n '649,672p' src-tauri/src/codex_interaction.rs        # F-perf-12 push_delta 线性扫 + to_string

$ sed -n '716,722p' src-tauri/src/main.rs                     # F-perf-13 mcp-status 全量克隆
                    let payload = mcp_statuses.borrow_and_update().values().cloned().collect::<Vec<_>>();

$ rg -n "EnvFilter::new" src-tauri/src/logging.rs             # F-perf-14
63:        .unwrap_or_else(|_| EnvFilter::new("info,tauri_plugin_updater=off"));
93:        .unwrap_or_else(|_| EnvFilter::new("debug,tauri_plugin_updater=off"));  # dev 默认 debug

$ rg -n "event_sink\(event.clone\(\)\)" src-tauri/src/commands.rs   # F-perf-15
20685:        event_sink(event.clone());
23987:        event_sink(event.clone());

$ sed -n '4100,4130p' crates/r-code-agent-worker/src/llm_runtime.rs # F-perf-16 fingerprint to_value→to_vec
```

## E13 已排除项（防误报）

```
$ rg -n "Regex::new" --type rust -g '!*test*' -g '!tests/**' crates src-tauri | rg -v "LazyLock|OnceLock|static"
# 仅 secret.rs LazyLock 内部命中 + control_service.rs:236（每次 wait 调用编译一次，非循环内）

$ rg -n "useAppStore\(\)|useTasksStore\(\)" src-tauri/frontend/src -g '*.tsx' --glob '!*.test.*'
# 零命中：前端全部 selector 订阅

$ rg -n "MAX_READ_BYTES" crates/r-code-gateway/src/tools.rs
34:const MAX_READ_BYTES: usize = 100_000;   # read_file 输出有界

$ sed -n '62,63p' src-tauri/src/logging.rs   # 生产 info 级，log_buffer 非每 token 热路径
```

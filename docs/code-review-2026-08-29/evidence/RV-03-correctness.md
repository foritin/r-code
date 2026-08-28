# RV-03 正确性维度 — 扫描证据（2026-08-29）

工作树现状：`feat/code-review-2026-08-29` 分支 + 未提交 WIP（llm_runtime.rs、commands.rs 等，见 `phase0-git-status.txt`）。未跑 cargo build/test（约束）；全部结论来自 rg/grep + 局部 Read。

## 0. 关键方法论：cfg(test) 跨度切分

**问题**：本仓库把测试大量内联在生产文件尾部的 `#[cfg(test)] mod tests`，甚至散布在文件中部（commands.rs 测试 mod 位于 19226/19281/21567/22125/22186/22549/26169 行，其前后均有生产代码）。按文件聚合计数会高估 40 倍。

**方法**：自研 awk 跨度检测器（字符串字面量/原始字符串 r#"..."#/行注释感知的括号深度跟踪），先枚举每个文件所有 `#[cfg(test)]` 项的 (起始行-结束行) 跨度，再统计跨度之外的匹配行。在 commands.rs 上与人工核对完全一致（25 处）。

辅助脚本（会话临时目录，不入库）：`cfgtest_span.awk`（跨度枚举）、`outside_spans2.awk`（跨度外匹配）、`prod_spans2.sh`（管道）。

注意事项：Windows rg 输出反斜杠路径，`rg -g '!*/tests/*'` glob 不可靠；统一用前斜杠转换后 `grep -v -E '[/]tests[/]|_tests[.]rs'`（本仓 grep 为 ugrep，`/pat/` 是文件名语法，必须写成 `[/]pat[/]`）。

## 1. unwrap/expect 面

```
$ rg -n '\.unwrap\(\)' --type rust | wc -l
5720
$ rg -n '\.expect\(' --type rust | wc -l
883
$ rg -n '\.unwrap\(\)|\.expect\(' --type rust | wc -l
6601

# 按路径粗分
$ rg -n '\.unwrap\(\)|\.expect\(' --type rust -g '*/tests/*' -g '*_tests.rs' -g '*_test.rs' | wc -l
954          # (a) 类：tests/ 目录与 *_tests.rs 文件
$ rg -n '\.unwrap\(\)|\.expect\(' --type rust -g '!*/tests/*' -g '!*_tests.rs' -g '!*_test.rs' | wc -l
5647         # 含内联 cfg(test) 模块（误报源）

# 跨度切分后（生产区，排除 tests 目录与 _tests.rs）：
$ wc -l /tmp/unwrap_prod_v3.txt
159          # (c)+(d) 类真实生产面

# 逐文件 top（跨度切分后）：
29  crates/r-code-agent-worker/src/llm_runtime.rs
25  src-tauri/src/commands.rs
13  crates/r-code-core/src/testing.rs        # 测试支撑模块（cfg(test) 消费）
11  crates/r-code-core/src/secret.rs          # Regex::new(静态字面量).unwrap()
 9  vendor/agent-contracts/crates/agent-llm/src/openai.rs
 9  crates/r-code-gateway/src/gateway.rs
 8  crates/r-code-agent-worker/src/delegation_tree.rs
 6  src-tauri/src/browser/installer.rs
 5  vendor/agent-contracts/crates/agent-llm/src/anthropic.rs
 5  src-tauri/src/native_notification.rs
 4  src-tauri/src/main.rs                     # (b) 类 bin 入口，弱接受
 3  src-tauri/src/plan_policy.rs / log_buffer.rs / r-code-terminal/manager.rs
...（其余每文件 ≤2）
```

commands.rs 核对（跨度：866-869, 1871-1879, 15834-15853, 19226-19279, 19281-19323, 21567-21583, 22125-22132, 22186-22240, 22549-22556, 26169-41299）：

```
337/1355/1368/8091/8524/8666/20058/25722/25904  agent_event_sink.lock().unwrap().clone() 系列（10 处，短作用域）
14821/14846/14872    ark_kind.expect("checked above") —— 上游 14805 行白名单 filter 保证
18942/18947          expect("... is a built-in Codex permission profile") —— 常量表自证
21312/21320/24819(+21380/21475)  slots.lock().expect("pending user input lock")
21742                expect("Codex R-Code delegation registry poisoned")
24058                expect("one-shot Codex App Server transport is live")
24161                expect("guarded by is_some") —— 24145 行 if approval.rcode_delegate.is_some() 真实守卫
24283/24292/24310    expect("thread/start params are an object") —— 24263 行 serde_json::json! 宏构造必为对象
```

## 2. panic 宏

```
$ rg -n 'panic!|unreachable!|todo!|unimplemented!' --type rust | wc -l
199
# 跨度切分后生产区（/tmp/panic_prod_v2.txt，16 处）：
llm_runtime.rs:8461       unreachable!("only Codex-preferring policies reach this branch")
mcp/web.rs:458            unreachable!("bounded retry loop always returns")
commands.rs:6176          unreachable!("已过滤为子代理终态")
commands.rs:6258          unreachable!("split_scoped_event 已解包所有作用域")
commands.rs:7823/7856     unreachable!("bounded steer ... retry loop always returns")
commands.rs:26035         _ => unreachable!()
agent-llm/lib.rs:147/159/171/182   dialect unreachable（见 F-corr-05）
gateway.rs:570            unreachable!("tool retry loop always returns on its final attempt")
memory_store.rs:1218/2188/2287/2521  Noop/Off => unreachable!()
# gateway.rs:1618 panic!("kaboom") 是 #[cfg(test)] PanicTool，跨度检测已排除
# todo!/unimplemented! 生产区：0
```

## 3. 错误传播策略

```
$ rg -c 'Result<[^>]*,\s*String>' --type rust -g '!*_tests*' | awk -F: '{s+=$2;n+=1} END{print n" files, "s}'
31 files, 580
$ rg -n 'anyhow' --type rust | wc -l          → 1（main.rs:1018 IPC server 入口）
$ rg -n 'thiserror' --type rust | wc -l       → 14（error 定义处）
$ rg -c 'ProductError' --type rust -g '!*_tests*' | awk -F: '{s+=$2} END{print s}'
1918
$ rg -n '\.map_err\(\|e\| e\.to_string\(\)\)' --type rust | wc -l
22   # close_gate.rs 2、mcp_server.rs 7、attachment_migration.rs 9、commands.rs 5、（测试内 4 已排除于非测试计数）
$ rg -c '#\[tauri::command\]|#\[command\]' src-tauri/src/*.rs | awk -F: '{s+=$2} END{print s}'
196

# command 返回错误类型（签名解析 awk，commands/tauri_commands/lifecycle/plan_entry/settings/mcp_settings）：
177  String
  7  r_code_core::UserFacingError
  3  CommandError
  1  误报（Result<usize,...> 参数行）
```

## 4. 并发

### 4a. std guard 跨 .await

```
# 自研 guard_await.awk：let g = x.lock().unwrap() 赋值后 30 行内出现 .await 且无 drop(g)
$ rg -l --null 'async fn|async move' --type rust -g '!*_tests*' | xargs -0 awk -f guard_await.awk
# 命中 40 处 → 逐一 Read 验证锁类型：
#   llm_runtime.rs sessions/children      → tokio::sync::Mutex（.lock().await）合法
#   gateway.rs permission.rs state        → tokio
#   mcp/runtime.rs mutation/gate/config   → tokio
#   codex_app_server.rs slots/state       → tokio
#   commands.rs _guard/cache/bridge       → tokio（subagent_config_mutations、CODEX_AUTH_PREFLIGHT_CACHE）
#   terminal/manager.rs、web_security.rs  → 测试区
# 结论：std::sync guard 跨 await = 0 处
```

plan_review.rs:712 有显式注释佐证纪律："Keep rusqlite guards in a lexical block. They are intentionally dropped before the first await so the command future remains Send"。

### 4b. lock().unwrap()/expect() 生产清单

```
$ PAT='[.](lock|read|write)[(][)][.](unwrap|expect)[(]' prod_spans2.sh < lock_files.txt
44 处生产区（完整清单见 findings F-corr-04 附件；raw 全仓 160 处）
```

### 4c. rusqlite/r2d2

```
database.rs: max_size(8), min_idle(1), WAL, busy_timeout=5000
conn_await.awk 扫描 17 处疑似 → Read 验证全部在 { } 词法块内先 drop 再 await（change_service.rs:328-389 有"释放连接后重新查询"注释）
嵌套双连接（同调用路径持有两个 PooledConnection）：0 处
```

### 4d. tokio::spawn

```
$ rg -n 'tokio::spawn|tauri::async_runtime::spawn' --type rust -g '!*_tests*' | wc -l
101      # 非测试路径 raw
# 跨度切分后生产区（/tmp/spawn_prod.txt）：
38        # commands.rs 8、main.rs 5、llm_runtime 4、codex_app_server 3、ipc/server 2、updater 2、
          # tauri_commands 2、verification 2、其余 10 文件各 1
绑定句柄（let x = spawn）：18；显式 fire-and-forget：约 15 处
# 抽查 commands.rs:8691/9680、main.rs:662/692/713：spawn 体内部均有 tracing::warn/error 错误路径
# gateway.rs 具备 in-process 工具 panic 的 CatchUnwind 隔离（"in-process tool panicked; containing as a tool error"）
```

## 5. 整数/切片

```
$ rg -n ' as (u8|u16|u32|u64|usize|i8|i16|i32|i64|isize|f64|f32)\b' --type rust -g '!*_tests*' | wc -l
272
$ rg -n '\.(len\(\)|chars\(\)\.count\(\)) as (u8|u16|u32|i32|i16|usize)' --type rust -g '!*_tests*'
10 处长度语义转换（IPC 线框长度 1 处、DTO 计数 4、内存字符计数 4、diff 限制 1）
$ rg -c 'try_into\(\)|try_from\(' --type rust -g '!*_tests*' | awk -F: '{s+=$2} END{print s}'
75        # 已存在安全转换用法

直接索引抽查（全部有守卫）：
  change_service.rs:948   group[0]/group.last().unwrap() —— groups 由 or_default().push 构建，非空保证
  tools.rs:1288/1295      window.last() —— 1270 行 needle_lines.is_empty() 早退 + windows(2)
  tools.rs:1046/1070      windows(2) 窗口必 2 元素；matches[0] 前置非空判断
  classifier.rs:492/621   shell_command_heads 284 行过滤空 token 列表；532 行 heads.is_empty() 早退
  review_git.rs:539       534 行 is_empty 早退
  terminal/manager.rs:617 bytes.len() >= 3 守卫
  subagent_providers.rs:696  hex.len()==64 + as_chunks::<2>
  openai.rs:1168 / anthropic.rs:809  while !remaining.is_empty() 循环不变量
vendor/agent-ipc/protocol.rs:104  payload.len() as u32（写侧截断，读侧 116 行有 16MiB cap）
```

## 6. 前端

```
$ rg -n ' as any' src/ scripts/      → 0 处
非空断言（grep '[A-Za-z0-9_\]]!(\.|[a-zA-Z\[(])' 排除 !==）：12 处
  model.ts:510 payload!.questions!、InboxScene 2、ActivityScene 2、GuideSheet 1、
  browser-mock-runtime 5、store/tasks.ts:426 detail!.task.id —— 均为判别联合窄化后断言
$ rg -c '\.catch\(' src/  → 85     $ rg -c 'try \{' src/ → 332     async 计数 338
App.tsx:93-100：void refreshX().then(clear).catch(reportSyncFailure) —— 启动刷新有 catch
lib/poll.ts:40-46：轮询循环统一 try/catch + reportSyncFailure —— usePoll 消费者免 catch
lib/ipc.ts:172-186：invoke 统一包装 toUserFacingIpcError/commandErrorPayload —— 结构化错误通道
```

## 7. 根因证据：lint 门禁

```
.github/workflows/ci.yml:134-135:
  - name: cargo clippy
    run: cargo clippy --workspace --all-targets -- -D warnings
# 仅默认 lint 集；unwrap_used/expect_used 等 restriction lint 未启用
# Cargo.toml 无 [workspace.lints] 段；无 clippy.toml
```

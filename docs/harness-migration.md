# 三层架构实施清单（R-Code -> agent-harness -> agent-contracts）

> 操作手册。本文是 `docs/archive/deepseek-harness.md`（尤其 §5 现状盘点、§7 分层方案、
> §8 建议清单）的落地执行版：按顺序给出每一步的具体命令、代码落点与验收标准。
> 分析与理由不在本文重复，遇到「为什么」请回看评估文档对应章节。
>
> 文中行号以 2026-08-17 调研为准，执行时先重新定位。每个阶段独立成一个（或一组）PR，
> 全绿后再进下一阶段；任何一步测试不过就停下修复，不带着红测试前进。

## 总览

```
阶段 0  前置修复（数据正确性，~1 天）
阶段 1  P0 可靠性三件套（~1 周，与阶段 2 可交错）
阶段 2  三接缝泛型化（~2 周）
阶段 3  harness crate 就位（仓内拆分，零行为变化，~1 周）
阶段 4  拆出为子模块（零行为变化，~1-2 天）
```

目标形态：

```
r-code（产品仓库）
  ├── vendor/agent-contracts   子模块 @ gitlink        （合同层，已有）
  ├── vendor/agent-harness     子模块 @ gitlink        （运行时层，新仓）
  └── crates/* + src-tauri     产品层
依赖方向：r-code -> agent-harness -> agent-contracts
         r-code -> agent-contracts（词汇直连，合法菱形边）
禁止：agent-harness -> 任何 r_code_*（CI grep 守卫）
```

---

## 阶段 0：前置修复

> 全部在 agent-contracts 子模块内完成，然后 bump 父仓 gitlink。
> 这是数据正确性问题，优先于一切架构动作。

### 0.1 修复 `session_path` 硬编码

**问题**（详见评估文档 §5.4）：`SessionStore::session_path` 忽略 `id` 参数，
所有会话写进同一个 `glm-5.3_common.jsonl`，与宿主 14 处 `{storage_id}.jsonl`
读取路径分裂。

```text
仓库：vendor/agent-contracts
文件：crates/agent-store/src/session_store.rs:47-49
```

改法：

```rust
fn session_path(&self, id: &str) -> PathBuf {
    self.base_dir.join(format!("{id}.jsonl"))
}
```

同步修复同文件测试里对 `glm-5.3_common.jsonl` 路径的断言
（`session_store.rs` 测试段有 gz 归档路径断言同样硬编码）。

**执行顺序**：

1. 在子模块内改代码 + 改测试，`cargo test -p agent-store` 全绿；
2. 检查开发机是否已有混写的历史文件
   （`ls <sessions_dir>/glm-5.3_common.jsonl`）；存在则写一次性归并脚本：
   按行读入、以每段 `Meta` 事件的 `id` 切分、追加到对应 `{id}.jsonl`
   （已存在则跳过重复行），归并后人工抽查再删除原文件；
3. 子模块 commit -> push；
4. 父仓 `cd vendor/agent-contracts && git fetch && git checkout <新 commit>`，
   然后 `git add vendor/agent-contracts` 更新 gitlink，随 PR 提交。

**验收**：父仓 `cargo test -p r-code-core --test contract_tests` 及
`src-tauri` 会话相关测试全绿；新建两个任务后确认 `sessions/` 下出现两个
独立 `{storage_id}.jsonl`。

### 0.2 清理三处边界渗漏（评估文档 §5.2）

都在子模块内，可与 0.1 同一批提交：

1. **事件名去产品前缀**：`crates/agent-store/src/session_store.rs:21-22`
   - `r_code_durable_user_message` -> `durable_user_message`
   - `r_code_durable_user_message_cancelled` -> `durable_user_message_cancelled`
   - **兼容性必须处理**：旧 JSONL 里已有 `r_code_` 前缀事件。加载侧加
     读取别名（serde 反序列化时把旧名映射到新变体），写入侧只用新名；
     宿主 `src-tauri/src/commands.rs:46` 的 `use agent_store::{...}` 常量名不变。
2. **引擎枚举中性化**：`crates/agent-config/src/lib.rs`
   - `MainAgentEngine::RCode` -> `MainAgentEngine::Native`（`:436`）
   - `QualityReviewer::RCode` -> `QualityReviewer::Native`（`:471`）
   - 默认值 `default_agent_engine = "r_code"` -> `"native"`（`:681`）
   - serde 层保留旧字符串值兼容（`#[serde(alias = "r_code")]`）；
     r-code 侧如有匹配这两个枚举的代码同步改名。
3. **死依赖摘除**：`crates/r-code-agent-worker/Cargo.toml` 删除
   `agent-store` 与 `agent-compaction` 两行（源码零引用，评估文档 §4.5）。
   注意：宿主 `src-tauri` 与 `r-code-core` 对 agent-store 的依赖是活的，不动。

**验收**：`cargo build --workspace` + 全量 `cargo test --workspace` 绿；
`grep -rn "r_code" vendor/agent-contracts/crates/*/src/` 零命中
（测试 fixture 里的兼容别名除外，标注 `// legacy name, read-only`）。

### 0.3 bump gitlink 的标准流程（后续所有子模块改动通用）

```bash
cd vendor/agent-contracts
git add -A && git commit -m "fix(store): ..." && git push origin main
cd ../..
git add vendor/agent-contracts
git commit -m "chore(vendor): bump agent-contracts"
```

CI 的 submodule-pin job（`.github/workflows/ci.yml:205`）会比对 gitlink
与 checkout HEAD，忘了 `git add vendor/agent-contracts` 会直接红。

---

## 阶段 1：P0 可靠性三件套

> 三项互相独立，可并行。落点原则：**新代码直接写在未来的 harness 模块位上**
> （阶段 3 只挪位置不改逻辑）。

### 1.1 工具 panic 隔离

**落点**：`crates/r-code-gateway/src/gateway.rs` 的 `execute_registered_tool`。

参考 `.reference/rust-deepseek-harness/src/tool.rs` 的 `catch_unwind` 模式：

```rust
use std::panic::{catch_unwind, AssertUnwindSafe};

// 包裹进程内工具 dispatch：
let outcome = catch_unwind(AssertUnwindSafe(|| {
    futures::executor::block_on(tool.call(args))  // 按现有执行模型调整
}))
.unwrap_or_else(|panic| {
    tracing::error!(tool = %name, ?panic, "tool panicked");
    ToolCallOutcome::error(format!("internal error: tool panicked"))
});
```

注意：只覆盖进程内工具；bash（`kill_on_drop`）与 MCP（abort 轮询）已有
自己的隔离，不要重复包。若 panic payload 非 String，打
`panic::panic_message` 或降级为固定文案，绝不让 panic 逃逸成会话死亡。

**验收**：单测注册一个 `panic!()` 的 mock 工具，断言拿到 `is_error` 结果
且循环继续下一轮。

### 1.2 工具注册可逆化

**落点**：同文件注册处。新增 `register_guarded`，保留旧 `insert`：

```rust
pub struct EffectGuard {
    on_drop: Option<Box<dyn FnOnce() + Send>>,
}
impl Drop for EffectGuard {
    fn drop(&mut self) { if let Some(f) = self.on_drop.take() { f(); } }
}

impl ToolGateway {
    /// 栈式注册：同名后注册覆盖先注册，guard drop 时弹出本次注册。
    pub fn register_guarded(&mut self, spec: ToolSpec, tool: Arc<dyn Tool>) -> EffectGuard {
        // 内部结构从 HashMap<String, Entry> 改为 HashMap<String, Vec<Entry>>
        // 查找取栈顶；guard 闭包里按注册 id retain 弹出。
    }
}
```

**验收**：单测覆盖「注册 A -> 注册 A'（同名）-> drop A' 的 guard ->
调用得到 A」的栈语义；现有 `insert` 行为回归不变。

### 1.3 request/header 快照 + 派发前重建自检（最高价值，工作量最大）

分两半做，本阶段先做完整闭环，不做则跳过整个 1.3（不要做一半）：

**合同半**（子模块 `agent-contract/src/session.rs`）：`SessionEvent` 增变体：

```rust
/// 模型请求信封快照。每次派发前追加；reason 区分 initial/resume/change。
RequestHeader {
    system_sha256: String,
    tools_sha256: String,
    messages_sha256: String,   // 派发消息列表的哈希，不存全文（体积考虑）
    reason: String,            // "initial" | "resume" | "change"
    /// 尾部注入清单（本地时钟/task_context/plan mode 等），校验时排除
    excluded_tails: Vec<String>,
},
```

**运行时半**（`crates/r-code-agent-worker/src/llm_runtime.rs`，复用 P2-H
的 `cache_shape.rs` 捕获点）：

1. 每轮派发前，对 `request.messages` + system + tools 算 sha256，
   append `SessionEvent::RequestHeader` 到 JSONL；
2. 重建自检：用 `SessionStore::load` 的投影重算哈希，与本次派发值比对；
   不一致 -> 报错并标注差异段（消息数 / system 变 / tools 变）；
3. 尾部注入集（本地时钟 / task_context / plan mode 消息）在
   `excluded_tails` 里登记后排除，不算不一致。

**先做一个决策再动手**：字节级还是语义级判等。建议第一版用
「serde_json 规范化序列化后字节级」，语义级（字段白名单）留到有误报再说。

**验收**：单测三个场景--正常轮追加且校验通过；人为篡改内存消息触发
不一致报错；尾部注入登记后不误报。手工跑一个长会话，`jq` 抽
RequestHeader 行确认 reason 序列合理（首个 initial，其余 change）。

---

## 阶段 2：三接缝泛型化（下沉的前提）

> 目标：让「通用件」与「产品件」在类型边界上分开。本阶段结束后，
> agent_loop / delegation_tree / run_guard / inbox 不再出现任何 r_code 类型。

### 2.1 接缝一：HarnessEvent 事件枚举

**落点**：`crates/r-code-agent-worker` 内新建 `src/harness_event.rs`（阶段 3
随包迁走）。定义产品无关的运行时事件：

```rust
pub enum HarnessEvent {
    RunStarted { run_id: String },
    PhaseChanged { phase: HarnessPhase },   // 替代 dto::AgentActivityPhase
    ToolCallStarted { name: String, call_id: String },
    ToolCallFinished { call_id: String, is_error: bool },
    Usage { usage: agent_contract::Usage },
    SteerAccepted { operation_id: String },
    RunFinished { outcome: HarnessOutcome },
    // …按 agent_loop.rs 实际产出的 dto::AgentEvent 变体逐一映射
}
```

改造点（评估文档 §7.4 已定位）：

- `agent_loop.rs:27` 输出类型 `dto::AgentEvent` -> `HarnessEvent`
  （内部 `StreamEvent`/`Usage` 等 agent-contract 类型原样透传）；
- `agent_loop.rs:927, 951, 988, 1018, 1172` 五处 `AgentActivityPhase` ->
  `HarnessPhase`；
- `delegation_tree.rs:4` 的 `dto::{AgentEventScope, SubagentState}` ->
  harness 侧 `Scope` / `ChildState` 通用枚举；
- 产品侧（llm_runtime / src-tauri 桥）加
  `impl From<HarnessEvent> for dto::AgentEvent`，映射集中在宿主桥一处。

**验收**：`grep -n "r_code_core" crates/r-code-agent-worker/src/agent_loop.rs
crates/r-code-agent-worker/src/delegation_tree.rs` 零命中；
全量测试绿（映射层有单测覆盖每个变体）。

### 2.2 接缝二：错误类型抽象

`agent_loop.rs:28` 的 `ProductError` -> 泛型 `E` 或 `agent_error::Error`
（推荐后者，少一个泛型参数）；产品侧 `From` 包装。涉及 agent_loop 的
函数签名与 mock 测试。**验收**同上（grep 零命中 + 测试绿）。

### 2.3 接缝三：ApprovalGate trait

**落点**：先放 `crates/r-code-gateway`（阶段 3 移入 harness）：

```rust
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    /// ask/approve/deny 一次性语义；缺席或无法回答 = deny。
    async fn check(&self, request: ToolApprovalRequest) -> ToolApprovalDecision;
}
```

`ToolGateway` 现有「Mutex 下原子提交 pending + 终止决策」逻辑（亮点，
不动）改为 `impl ApprovalGate`；工具管线（阶段 1 的 panic 隔离、未来的
pre/post execute）只面向 `ApprovalGate` 调用。

**验收**：现有权限测试全绿；新增一个用 mock ApprovalGate 的管线测试，
覆盖「gate 返回 deny 时工具不执行且结果带 is_error」。

---

## 阶段 3：harness crate 就位（仓内拆分，零行为变化）

> 纯搬家 + 改 import，不改任何逻辑。目标：本仓出现
> `crates/r-code-harness`，产品代码从它 import。

### 3.1 建 crate

```bash
cargo new crates/r-code-harness --lib
```

`Cargo.toml` 只允许依赖：`agent-contract`、`agent-error`、`agent-config`、
tokio/futures/serde/tracing/thiserror 等基础件。**禁止**出现任何 `r_code_*`
与 `agent-llm`（harness 只见 trait，provider 装配归产品组合根）。

`crates/r-code-harness/src/` 目标布局（来源见括号）：

```
lib.rs            重导出全部模块
inbox.rs          双队列收件箱（llm_runtime.rs:1642-1644,2023-2040,3247-3267 抽取）
loop_core.rs      agent_loop.rs 全文件迁入（接缝改造后已无产品类型）
run_guard.rs      run_guard.rs 迁入，trip_reason_to_dto 留产品侧
delegation.rs     delegation_tree.rs 迁入（通用枚举已替换）
checkpoint.rs     checkpoint.rs 零改动迁入
cache_shape.rs    cache_shape.rs 零改动迁入
tool_pipeline.rs  panic 隔离 + EffectGuard + ApprovalGate（阶段 1/2 成果）
request_check.rs  request/header 自检器（阶段 1.3 成果）
```

`Cargo.toml`（根 workspace）members 加一行 `"crates/r-code-harness"`。

### 3.2 逐模块搬迁（每模块一个 commit，独立可回滚）

搬迁顺序按依赖从零到有：`checkpoint` -> `cache_shape` -> `run_guard` ->
`delegation` -> `inbox` -> `loop_core` -> `tool_pipeline` -> `request_check`。
每步：`git mv`（或复制删除，保留 git 历史 prioritize `git mv`）->
修 import 路径 -> `cargo test --workspace` -> commit。

**收件箱抽取（唯一需要真正写代码的地方）**：从 `LlmAgentRuntime` 把
`steer_queue` / `accepting_steer` / drain 逻辑抽成独立
`Inbox { next_turn: VecDeque<..>, next_step: VecDeque<..> }`，
`claim()` 原子取件，保留「锁内取队列 + 判断完成」的现有原子性
（评估文档 P1-5 的全部要求）。

### 3.3 产品侧接线

- `r-code-agent-worker` 删掉已迁走的模块文件，`Cargo.toml` 加
  `r-code-harness = { workspace = true }`；
- 根 `Cargo.toml` `[workspace.dependencies]` 加
  `r-code-harness = { path = "crates/r-code-harness" }`；
- llm_runtime 改为组合 harness 组件（loop_core 以参数注入 provider /
  tool host / approval gate）。

**验收**（阶段 3 出口条件，全部满足才进阶段 4）：

```bash
# 1. harness 无产品依赖
grep -rn "r_code" crates/r-code-harness/           # 零命中
# 2. workspace 全绿
cargo test --workspace && cargo clippy --workspace -- -D warnings
# 3. 行为无变化
cargo test -p r-code-agent-worker                  # 既有测试全绿，未改断言
```

---

## 阶段 4：拆出为子模块（零行为变化）

> 把 `crates/r-code-harness` 整目录变成 `vendor/agent-harness` 子模块。
> 本阶段**不改任何代码**，只有仓库操作与接线。

### 4.1 建远端仓库

GitHub 上新建空仓 `foritin/agent-harness`（不要带 README/license 初始化，
避免首提交冲突）。

### 4.2 迁出

```bash
# 1. 以 crates/r-code-harness 为根初始化独立 git 历史
cd crates/r-code-harness
git init && git add -A
git commit -m "feat: extract agent-harness from r-code"
git branch -M main
git remote add origin https://github.com/foritin/agent-harness.git
git push -u origin main
cd ../..

# 2. 从本仓移除目录
git rm -r crates/r-code-harness
git commit -m "refactor: extract r-code-harness to standalone repo"

# 3. 以子模块挂回
git submodule add https://github.com/foritin/agent-harness.git vendor/agent-harness
```

### 4.3 harness 仓库自身配置

- `Cargo.toml`：对 contracts 声明 git 依赖（独立构建用）：

```toml
[dependencies]
agent-contract = { git = "https://github.com/foritin/agent-contracts.git", rev = "<当前pin的commit>" }
agent-error    = { git = "https://github.com/foritin/agent-contracts.git", rev = "<同上>" }
agent-config   = { git = "https://github.com/foritin/agent-contracts.git", rev = "<同上>" }
```

- 复制 agent-contracts 的 CI 模板（fmt / clippy / test / audit / deny）；
- 写 README：定位（运行时层，「怎么跑」）、九条硬规矩引用、
  「禁止依赖 r_code」边界声明。

### 4.4 r-code 侧接线（关键：单副本保证）

**workspace members**（根 `Cargo.toml:5-13` 附近）：

```toml
members = [
    # 现有 agent-contracts 9 行之后追加：
    "vendor/agent-harness",
    "crates/r-code-core",
    # …其余不变
]
```

**patch 段**（根 `Cargo.toml` 末尾新增，把 harness 声明的 git 源重定向到
本地 vendored 子模块，保证 r-code 构建永远编译自己 pin 的那一份 contracts）：

```toml
[patch."https://github.com/foritin/agent-contracts"]
agent-contract = { path = "vendor/agent-contracts/crates/agent-contract" }
agent-error    = { path = "vendor/agent-contracts/crates/agent-error" }
agent-config   = { path = "vendor/agent-contracts/crates/agent-config" }
agent-store    = { path = "vendor/agent-contracts/crates/agent-store" }
# …harness 依赖到的每一个都列全
```

**workspace.dependencies**：

```toml
agent-harness = { path = "vendor/agent-harness" }
```

产品 crate 的 `r-code-harness = { workspace = true }` 全局改名为
`agent-harness = { workspace = true }`（`cargo` 里 crate 名随新仓
`Cargo.toml` 的 `name = "agent-harness"`）。

### 4.5 CI 更新（`.github/workflows/ci.yml`）

1. **submodule-pin job 扩展**：仿照现有 agent-contracts 段
   （`ci.yml:205`）加一段 harness 比对：

```yaml
      - name: Verify agent-harness submodule pin
        run: |
          EXPECTED=$(git ls-tree HEAD vendor/agent-harness | awk '{print $3}')
          ACTUAL=$(git -C vendor/agent-harness rev-parse HEAD)
          [ "$EXPECTED" = "$ACTUAL" ] || { echo "harness pin mismatch"; exit 1; }
```

2. **边界守卫（新 job）**：

```yaml
  harness-boundary:
    name: Harness Boundary Check
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6  # 与现有 job 同款 pin 版本
        with:
          submodules: recursive
          persist-credentials: false
      - name: Forbid product deps inside harness
        run: |
          if grep -rn "r_code\|r-code" vendor/agent-harness/src/ \
              vendor/agent-harness/Cargo.toml; then
            echo "ERROR: agent-harness must not reference r-code"
            exit 1
          fi
```

3. 现有 fmt/clippy/test 各 job 的 checkout 已是
   `submodules: recursive`，harness 挂为子模块后自动纳入，无需改。

### 4.6 验收（最终出口条件）

```bash
cargo build --workspace                    # patch 生效，单副本编译
cargo test --workspace                     # 全绿
cargo tree -p agent-harness                # 确认无 r_code、无第二份 agent-contract
git submodule status                       # 两个子模块均无前缀（干净 pin）
```

push 后观察 CI：submodule-pin、harness-boundary、既有全部 job 绿。

---

## 后续（不属于本次拆分，按评估文档 P1/P2 节奏另行排期）

- 子代理主动回报 + settled 通知（P1-6）；
- 记忆两层注入 + 召回（P1-7，产品层）；
- JSONL 显式 seq（P1-8）；
- fork 边界 / replay 坏行标注 / 压缩事件化与双套压缩合并（P2）；
- 压缩状态机下沉时决策：P2-G 骨架进 harness，`agent-compaction` 策略
  作为其可插拔实现（评估文档 §5.3，勿长期维持两套）。

## 风险与回滚

| 阶段 | 主要风险 | 回滚方式 |
| --- | --- | --- |
| 0 | 旧 JSONL 兼容（事件名/文件名） | 兼容别名只读不写；git revert 子模块 commit |
| 1 | 1.3 误报阻塞正常会话 | 自检器先「只记录不阻断」跑一周，确认零误报再升级为阻断 |
| 2 | 映射层漏变体导致前端事件缺失 | From 实现用穷举 match（无通配），编译器强制全覆盖 |
| 3 | 搬迁引入行为漂移 | 每模块独立 commit；对比搬迁前后 `cargo test` 输出 |
| 4 | patch 段漏列 crate 导致双副本 | `cargo tree -d` 查重复 crate；submodule-pin + boundary job 兜底 |

## 参照

- 分析与决策依据：`docs/archive/deepseek-harness.md`（§5 现状、§7 分层、§8 清单）
- 现有子模块模式：`.gitmodules`、`.github/workflows/ci.yml:205`、
  `vendor/agent-contracts/contract-lock.json`
- 接缝与行号明细：评估文档 §7.3 切分表、§7.4 三接缝

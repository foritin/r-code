# 多模态附件、上下文预算与 DeepSeek Plan 锚定实施规格

状态：待实施  
适用范围：R-Code 内置 Agent、桌面端附件链路、DeepSeek 类 Provider  
默认发布策略：所有数据格式先双读；附件引用写入与 Plan 锚定开关均先默认关闭  
目标：修复图片 Base64 导致的伪超窗与 `1 → 2 → 4` 输出预算错误，并在现有 Plan 状态机上完成可开关、可恢复、可审计的 DeepSeek 两阶段锚定

本文是实现合同。文中的“必须”“不得”“仅允许”是验收条件，不是建议。

## 1. 最终结论与实施范围

### 1.1 `dsh-anchored-standard` 核心逻辑的现有实现结论

R-Code 已有核心主干，但尚不能认定为完整实现：

| 能力 | 当前状态 | 本文要求 |
| --- | --- | --- |
| Plan 首轮收窄真实工具目录 | 已实现 `plan_native_v1` bootstrap 5 项目录 | 复用，不另造目录系统 |
| 首次 durable outcome 后晋升 | 已实现 `bootstrap -> resident` CAS，resident 为 8 项只读目录 | 复用并保持 fail closed |
| Plan 期间禁止写入、Shell、子代理 | 已有 `ToolPolicy::Plan` 硬门 | 保持为独立安全边界 |
| Plan 期间减少自动注入 | 仅部分实现；当前仍可能带入主系统提示、MCP 文案、用户编排提示、peer mailbox、进度或 governor 尾部 | 改为统一 `ContextInjectionProfile` 闸门 |
| Plan 批准后恢复完整工具 | 当前通过 `stage_implementation_dispatch()` 把任务切回 `auto`，下一 run 隐式恢复 | 固化为可审计的不变量并增加恢复事件与测试 |
| 用户滑钮 | 现有 `suggest_complex_tasks` 只控制“是否建议 Plan”，不控制锚定本身 | 新增独立 `deepseek_plan_anchoring` 滑钮 |
| 重启、fork、compact 后不回退 | bootstrap/resident 已使用持久状态 | 扩展测试到完整执行恢复状态 |

因此实施方向是：扩展现有 `plan_native_v1`、`ResolvedPlanRuntimeProfile`、`PlanStore`、`prepare_runtime_session()` 和 `ToolPolicy::Plan`，不得并行创建第二套 Plan 生命周期。

### 1.2 本文必须交付的结果

1. 图片、PDF 等二进制附件只在 BlobStore 保存一次；会话、排队消息和模型投影只保存 `AttachmentRefV1`。
2. WebView 上传和 Provider 请求若协议要求 Base64，可以临时物化 Base64；临时值不得进入 JSONL、SQLite 业务载荷、压缩摘要或 token 字符估算。
3. 主模型被确认支持图片输入时，原图或等价的 Provider 文件引用必须直接发送给当前主模型；OCR 调用次数必须为 0。
4. 主多模态模型拒绝图片时，必须返回能力声明漂移错误；不得静默 OCR，也不得静默改用另一视觉模型。
5. 只有主模型被确认不支持图片，或能力为 unknown 且用户显式配置了图片理解引擎时，才允许 OCR 或独立视觉模型生成文本投影。
6. 上下文 token、图片 token、单轮输出 token 和 HTTP wire bytes 分开核算。
7. 发送前硬闸门无法获得最低可执行输出额度时，Provider 调用次数必须为 0；不得把额度强制改成 1。
8. 设置页允许用户限制“每轮最大输出”，Provider 的“服务端最大输出”只作为上界，不得再把输入框锁死在厂商上限。
9. DeepSeek 复杂任务在用户同意进入 Plan 后，Plan 阶段使用最小真实工具目录和最小自动注入；Plan 获批进入实施后恢复该任务正常配置下的全部可用工具、MCP、hosted tools、子代理和标准上下文。
10. 新增独立滑钮控制 DeepSeek Plan 锚定。开关关闭时必须与当前 baseline Plan 的请求形状保持一致。

### 1.3 非目标

- 不更换现有 Plan UI、PlanStore、计划批准和实施队列的业务语义。
- 不让 Plan 锚定绕过权限审批、PathGuard、ToolPolicy 或用户禁用的工具。“完整工具”指该任务按正常配置本来可用的完整目录。
- 不把 OCR 删除。OCR 继续作为文本主模型的显式图片理解引擎存在。
- 不要求 Provider 永远不返回 `MaxTokens`。本次修复保证额度计算正确、用户可限制单轮输出、错误分类正确，并消除由伪超窗产生的 `1 → 2 → 4` 重试。
- 不在本文加入厂商横向比较、评测成绩或外部项目安装步骤。

## 2. 不可变产品合同

### 2.1 多模态路由合同

后端必须在持有 task-local send lock 后解析一次 `ResolvedModelCapabilities`，并把结果冻结到本次 origin request：

```rust
enum CapabilityTruth {
    Confirmed,
    Unsupported,
    Unknown,
}

struct ResolvedModelCapabilities {
    provider_name: String,
    provider_kind: String,
    model_id: String,
    protocol: String,
    vision: CapabilityTruth,
    vision_profile: Option<String>,
    context_window_tokens: Option<u32>,
    provider_max_output_tokens: Option<u32>,
}
```

路由必须遵循以下真值表：

| `vision` | 图片处理路径 | 失败行为 |
| --- | --- | --- |
| `Confirmed` | `NativeMainVision`：原图或 Provider 文件引用直接给当前主模型 | 显示 `VISION_CAPABILITY_DRIFT`，OCR 与辅助视觉模型调用均为 0 |
| `Unsupported` | 按 `image_understanding.engine` 显式选择 OCR 或独立视觉模型 | 所选引擎失败即返回错误，不得自动换另一引擎 |
| `Unknown` | 仅按用户显式选择的图片理解引擎处理；未完成配置则阻断 | 显示“能力未知，请确认图片理解方式”，不得猜成多模态，也不得静默 OCR |

主模型 `vision == Confirmed` 时，即使设置页的图片理解引擎选择了 OCR，也必须优先 `NativeMainVision`。图片理解引擎只服务不能直接读图的主模型。

多模态预处理仅允许：

- 校验文件格式、魔数、尺寸和字节上限；
- 去除 EXIF 等非必要元数据；
- 按 Provider 明确上限等比缩放或分块；
- 生成请求专用派生图并保存其 hash；
- 把 Blob 临时物化为 Provider 需要的 Data URL、`file_id` 或 URL。

多模态预处理不得：

- 先 OCR 再用 OCR 文本替换原图；
- 在主模型失败后自动调用 OCR；
- 在主模型失败后自动改用设置中的独立视觉模型；
- 把图片 Base64 当作普通文本 token；
- 把请求专用 Base64 回写到会话或模型投影。

### 2.2 Base64 使用合同

Base64 只允许存在于以下两个临时边界：

1. WebView 把浏览器 `File` 交给 `cmd_attachment_stage` 的一次性 IPC 载荷；后端必须立即解码、校验、写 Blob，并在命令返回前丢弃字符串。
2. Provider 协议明确要求 Data URL/Base64 时，在最终 HTTP 请求构造阶段从 Blob 临时物化；请求完成或失败后立即释放，不得持久化。

下列位置出现二进制 Base64 即为验收失败：

- `SessionEvent::Message`、`HistorySnapshot`、`ModelProjection`；
- `queued_messages.attachments_json` 的新格式；
- request audit sidecar；
- 压缩 map/reduce 输入；
- `message_chars()` 或其替代函数的文本字符统计；
- 错误详情、tracing 字段、支持包和 UI 状态缓存。

### 2.3 请求预算合同

每次请求发送前必须产生一份不含敏感正文的 `RequestBudgetV1`：

```rust
struct RequestBudgetV1 {
    context_window_tokens: u32,
    text_tokens: u32,
    tool_schema_tokens: u32,
    image_tokens: u32,
    document_tokens: u32,
    estimated_input_tokens: u32,
    requested_output_tokens: u32,
    effective_output_tokens: u32,
    reserve_tokens: u32,
    materialized_wire_bytes: u64,
    attachment_count: u32,
}
```

必须满足：

```text
estimated_input_tokens
  = text_tokens + tool_schema_tokens + image_tokens + document_tokens

effective_output_tokens
  = min(
      requested_output_tokens,
      provider_max_output_tokens,
      context_window_tokens - ceil(estimated_input_tokens × 1.15) - reserve_tokens
    )
```

`provider_max_output_tokens` 只用于限制单轮输出，不得作为每轮固定的上下文预留。压缩和发送闸门必须预留本次 `requested_output_tokens`，而不是无条件预留 DeepSeek 的 393,216 上限。

最低可执行输出额度固定为：

| 请求类型 | 最低 `effective_output_tokens` |
| --- | ---: |
| 普通纯聊天 | 2,048 |
| Agent 工具回合 | 8,192 |
| Plan bootstrap/resident | 16,384 |
| 压缩或最终收尾专用请求 | 4,096 |

低于对应阈值时，必须返回 `OUTPUT_HEADROOM_BELOW_MINIMUM`，并保证 Provider mock 的请求计数为 0。

### 2.4 Plan 锚定合同

锚定只在以下条件全部成立时生效：

```text
task.agent_engine == r_code
AND resolved provider_kind == deepseek
AND planning.deepseek_plan_anchoring == true at Plan creation
AND internal emergency off == false
AND task has an attached workspace
```

任一条件不成立时使用 baseline Plan。显示名、URL 子串和其他 Provider 的错误类型不得触发 DeepSeek 锚定。

开关值、Provider route 和运行 profile 在 Plan 创建时冻结。运行中切换设置只影响之后新建的 Plan，不得改变当前 Plan 的目录和上下文。

## 3. 已确认故障链与修复判定

用于回归的原始故障样本固定为：

```text
provider_kind: deepseek
model: deepseek-v4-flash-vision-exp
context_window: 1,000,000
configured max_tokens: 393,216
thinking: enabled
reasoning_effort: max
image decoded bytes: 3,383,259
image dimensions: 1818 × 1026
base64 chars: 4,511,012
```

当前错误链是：

1. `src-tauri/src/commands.rs::user_message_with_attachments()` 把原图 Base64 放入 `ContentBlock::File.data`。
2. `agent-llm` 在 Provider 边界把它转为 Data URL；这一步作为 wire 格式可以保留。
3. `crates/r-code-agent-worker/src/llm_runtime.rs::message_chars()` 把 4,511,012 个 Base64 字符当普通文本。
4. 默认 `0.25 token/char` 把图片估算成约 1,127,753 token。
5. `clamp_request_max_tokens()` 使用 `headroom.max(1)`，把本应为预检失败的请求改成 1 token。
6. `agent_loop.rs` 对空输出执行两次翻倍，形成 `1 → 2 → 4`。
7. 自动压缩的 exact tail 保留最新图片，`truncate_message_chars()` 又完整 clone File/Image，强制整理无法消除该伪预算。
8. 硬闸门两次整理后即使仍超窗也退出循环并继续派发。

以下任一做法都不算完整修复：

- 只把 `0.25 token/char` 调小；
- 只把图片压缩成更小 JPEG；
- 只提高 `MAX_TOKEN_ESCALATIONS`；
- 只把 `max(1)` 改成另一个小常数；
- 遇到失败改走 OCR；
- 仅在 UI 隐藏错误而不改变请求派发条件。

## 4. 附件数据模型与存储

### 4.1 `AttachmentRefV1`

在 `vendor/agent-contracts/crates/agent-contract/src/message.rs` 增加显式引用块，不再复用含 `data` 的 `FileSource` 表示 R-Code 持久附件：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentRefV1 {
    pub version: u8,                 // 固定为 1
    pub attachment_id: String,       // UUID；不包含本机路径
    pub name: String,
    pub media_type: String,
    pub kind: AttachmentKind,        // image | text | pdf
    pub byte_len: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub purpose: AttachmentPurpose,  // native_input | text_input | display_only
}

pub enum ContentBlock {
    // 既有变体保持双读兼容
    Attachment { source: AttachmentRefV1 },
}
```

规则：

- `attachment_id` 是解析 Blob 的唯一公开键；调用方不得提交 `blob_hash` 或文件路径来绕过所有权检查。
- 读取时以数据库元数据为权威，消息内元数据只用于无 IO 展示和预算预检；不一致时返回 `ATTACHMENT_METADATA_MISMATCH`。
- `native_input` 只用于当前主模型直接读取的原图/PDF。
- `text_input` 用于普通文本附件，Provider 投影阶段读取 Blob 并展开 UTF-8 文本。
- `display_only` 用于文本主模型场景下保留原图预览；它不进入 Provider 请求。
- OCR 或独立视觉模型生成的描述是独立 `Text` 块，并带 `derived_from_attachment_id` 的审计事件；不得把原图引用改写成伪文本文件。

### 4.2 SQLite migration 034

在 `crates/r-code-store/src/migrations.rs` 增加 migration 034，并把 `LATEST_SCHEMA_VERSION` 更新为 34：

```sql
CREATE TABLE attachments (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    blob_hash TEXT NOT NULL,
    name TEXT NOT NULL,
    media_type TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('image', 'text', 'pdf')),
    byte_len INTEGER NOT NULL CHECK (byte_len > 0),
    width INTEGER,
    height INTEGER,
    state TEXT NOT NULL CHECK (state IN ('staged', 'committed')),
    lease_expires_at TEXT,
    created_at TEXT NOT NULL,
    committed_at TEXT,
    FOREIGN KEY(task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    FOREIGN KEY(blob_hash) REFERENCES blobs(hash) ON DELETE RESTRICT
);

CREATE INDEX idx_attachments_task ON attachments(task_id, created_at);
CREATE INDEX idx_attachments_blob ON attachments(blob_hash);
CREATE INDEX idx_attachments_staged_lease
    ON attachments(state, lease_expires_at);

CREATE TABLE session_attachment_migrations (
    storage_id TEXT PRIMARY KEY,
    source_sha256 TEXT NOT NULL,
    target_sha256 TEXT,
    state TEXT NOT NULL CHECK (state IN ('pending', 'committed', 'failed')),
    error TEXT,
    updated_at TEXT NOT NULL
);
```

不得给 `attachments.blob_hash` 加唯一约束。同一内容可以在多个消息中拥有不同的逻辑附件记录，但物理 Blob 必须因内容 hash 相同而只保存一份。

### 4.3 `AttachmentStore`

在 `crates/r-code-store` 新增 `attachment_store.rs` 并从 `lib.rs` 导出。必须提供：

```rust
stage(task_id, metadata, bytes) -> AttachmentRefV1
get_owned(task_id, attachment_id) -> AttachmentRecord
read_owned(task_id, attachment_id) -> Vec<u8>
commit_many(task_id, attachment_ids)
discard_staged(task_id, attachment_id)
reconcile_session_refs(storage_id, refs)
gc_expired_staged(now)
list_hashes_for_task(task_id)
```

`stage()` 的顺序固定为：

1. 校验 task 存在且未归档。
2. 校验名称、MIME、魔数、字节数和图片尺寸。
3. 对内容计算 BLAKE3。
4. 在 `blobs_dir` 同目录创建临时文件，完整写入并 `sync_all()`。
5. 以原子 rename 安装为 `{blobs_dir}/{hash}`；目标已存在时删除临时文件并复用目标。
6. SQLite `IMMEDIATE` 事务中插入/递增 `blobs.ref_count`，再插入 `attachments(state='staged')`。
7. 事务提交后返回引用。

若第 4～5 步成功但数据库事务失败，允许留下无 ledger 的物理文件，由现有 `prune_unreferenced_files()` 回收。不得出现数据库已提交但 Blob 尚未安装的顺序。

staged lease 默认 24 小时。删除草稿附件时立即 `discard_staged()`；WebView 崩溃或用户直接关闭窗口时由 GC 回收。GC 删除前必须扫描活动 JSONL 和 queued message 引用，防止“消息已落盘但 commit 标记未写”的崩溃窗口误删附件。

### 4.4 发送与排队的持久化顺序

直接发送：

1. Composer 先逐个调用 `cmd_attachment_stage`，React 状态只保留 `AttachmentRefV1` 和本地 Object URL。
2. `cmd_agent_send` 只接收 attachment id 列表。
3. 后端在 task-local lock 内用 `get_owned()` 重新验证所有权和元数据。
4. JSONL 追加只含 `ContentBlock::Attachment` 的用户消息。
5. JSONL append 成功后调用 `commit_many()`。
6. 若步骤 5 前崩溃，启动恢复通过 JSONL 引用把 staged 记录补为 committed。

排队发送：

1. `queued_messages.attachments_json` 改为版本化对象：`{"version":2,"attachments":[AttachmentRefV1...],"route":ImageDeliveryRouteV1}`。
2. enqueue 与 `commit_many()` 必须在同一个 SQLite 事务完成。
3. dispatcher 只恢复引用，不解码 Base64。
4. route snapshot 与当前任务 route 不一致时，把队列项标为 failed 并显示 `ATTACHMENT_ROUTE_DRIFT`；不得重新解释为 OCR。

前端变更位置：

- `src-tauri/frontend/src/components/Attachments.tsx`：`DraftAttachment.data` 改为 `attachmentRef`；`readAsDataURL()` 只存在于 staging 调用的局部变量。
- `src-tauri/frontend/src/components/room/Composer.tsx`：发送引用，乐观气泡继续使用 `URL.createObjectURL(file)`，不得拼接持久 Data URL。
- `src-tauri/frontend/src/lib/types.ts`、`ipc.ts`、`browser-mock-runtime.ts`：同步新 DTO 和命令。
- 删除草稿时调用 `cmd_attachment_discard`；发送成功后只 revoke Object URL，不删除已 committed 附件。

## 5. 模型能力与图片路由实施

### 5.1 单一能力解析入口

新增 `src-tauri/src/model_capabilities.rs`，作为后端唯一能力解析入口。以下调用方必须全部改用它：

- `main_model_handles_images_natively()`；
- Composer 的 capability DTO；
- `build_provider_config()` 创建 Provider runtime 时的能力字段；
- 图片理解分派；
- 请求预算估算；
- Provider materialization；
- request audit。

`provider_kind + model_id + selected protocol` 是能力键。显示名和错误消息中的厂商名不得参与判定。自定义 base URL 不得把目录中已确认的多模态模型静默降级为 OCR；若中转实际不支持，走 `VISION_CAPABILITY_DRIFT`。

必须修复当前不一致：`provider_catalog.rs` 已把 `deepseek-v4-flash-vision-exp` 标为 `vision=true`，但 `agent-llm/src/deepseek.rs::capabilities()` 当前恒为 `supports_vision=false`。修改后：

```text
deepseek-v4-flash-vision-exp -> supports_vision = true
deepseek-v4-flash            -> supports_vision = false
deepseek-v4-pro              -> supports_vision = false
```

同时验证所选协议适配器能序列化图片。模型支持但协议适配器不支持时，在发送前返回 `VISION_WIRE_UNSUPPORTED`，不得改走 OCR。

### 5.2 图片理解分派重构

把 `apply_image_understanding()` 重构成纯路由加三个明确执行器：

```rust
enum ImageDeliveryRouteV1 {
    NativeMainVision { route_revision: String, vision_profile: String },
    OcrForTextMain { engine: String },
    VisionHelperForTextMain { provider: String, model: String, route_revision: String },
}
```

执行规则：

- `NativeMainVision`：不调用 `mark_ocr_engine_images()`、`apply_native_ocr()` 或 `apply_vision_model_understanding()`；原图引用保留为 `native_input`。
- `OcrForTextMain`：原图引用改为 `display_only`，OCR 文本作为 `Text` 块进入模型。
- `VisionHelperForTextMain`：原图引用改为 `display_only`，辅助模型描述作为 `Text` 块进入主模型。
- 删除 `apply_vision_model_understanding()` 中“辅助视觉模型失败后自动 OCR”的分支。用户选择视觉模型引擎后，失败必须原样返回可操作错误。
- 删除或忽略前端可伪造的 `native_ocr` 决策位。路由只能由后端根据冻结能力与设置产生。

### 5.3 Provider 错误映射

当 `NativeMainVision` 请求收到以下类型错误时，统一映射为 `VISION_CAPABILITY_DRIFT`：

- unsupported image/input_image/content type；
- model does not support vision；
- selected endpoint/protocol rejects image block；
- Provider 返回的模型与请求冻结模型不一致。

错误必须包含安全字段：provider profile 名、`provider_kind`、模型、协议和 route revision。不得包含 API key、原图、Base64 或完整响应体。此错误路径 OCR 调用计数必须保持 0。

## 6. token、图片与 wire bytes 预算

### 6.1 替换字符总数估算

在 `llm_runtime.rs` 中：

1. 把 `message_chars()` 改名为 `message_text_chars()`。
2. `Attachment` 只统计名称等少量可见元数据字符，不统计 Blob、Base64 或 wire Data URL。
3. 新增 `estimate_request_budget()`，分别处理 text、tools、image、PDF。
4. `CompactionState::calibrate()` 遇到图片请求时，必须从 Provider usage 中减去已估算的视觉 token 后再校准文本比例；Provider usage 无法拆分且减法不可靠时跳过本轮校准。
5. `automatic_compaction_message_source()` 序列化 `AttachmentRefV1`，不得先物化附件。

### 6.2 DeepSeek 视觉预算 profile

在 `provider_catalog.rs` 给多模态模型增加 `VisionBudgetProfile`。首版 `deepseek_vision_exp_v1` 使用保守、确定性的 tile 上界：

```text
tile_width = 512
tile_height = 512
base_tokens = 1,024
tokens_per_tile = 2,048
safety_multiplier = 1.25
max_request_edge = 4,096
max_request_pixels = 16,777,216
```

计算公式：

```text
tiles = ceil(request_width / 512) × ceil(request_height / 512)
image_tokens = ceil((1,024 + tiles × 2,048) × 1.25)
```

1818×1026 的固定回归值为 4×3 个 tile，图片预算为 32,000 token。它不得接近由 Base64 产生的 1,127,753 token。

超过 request edge/pixel 上限时，保留原始 Blob，并生成仅供本次 Provider 请求使用的等比缩放派生 Blob。预算和 wire bytes 按派生图计算；UI 仍展示原图。派生图也按内容 hash 去重，并在请求结束后按引用策略回收。

目录中 `vision=true` 但缺失视觉预算 profile 时，返回 `VISION_BUDGET_PROFILE_MISSING`。不得回退到 Base64 字符估算或 OCR。

### 6.3 Provider 请求前物化

向 `LlmAgentRuntime` 注入异步 `AttachmentResolver`。主循环顺序固定为：

1. 以 canonical `AttachmentRefV1` 构造模型投影。
2. 计算 `RequestBudgetV1`。
3. 执行压缩或附件感知的视觉 checkpoint。
4. 再次计算预算并执行硬闸门。
5. 写入不含附件正文的 `RequestHeader`。
6. 把发送副本中的引用物化为 `Image`、`File`、Data URL 或 Provider `file_id`。
7. 校验实际 `materialized_wire_bytes` 未超过 profile 限制。
8. 调用 Provider。
9. 丢弃物化副本，只把本轮新产生的 assistant/tool 消息追加到 canonical history。

`agent-llm` 的各协议适配器必须在最终序列化前断言不存在未解析的 `Attachment`。断言失败返回类型化错误，不得把引用降级成占位文本后继续请求。

### 6.4 输出上限配置与硬闸门

保留 Provider 配置字段 `max_tokens` 作为“用户每轮输出上限”，但修改其默认和 UI 语义：

- `Preset.max_output_tokens`：服务端硬上限，只读显示；DeepSeek V4 为 393,216。
- 新增 `Preset.recommended_output_tokens`：未显式配置时的单轮默认；DeepSeek V4 首版为 65,536。
- `ProviderConfig.max_tokens=None`：采用 `recommended_output_tokens`，不得再自动采用服务端硬上限。
- 已有用户显式保存的 393,216 保持不变，不做静默迁移。
- 设置页“每轮最大输出”允许编辑，范围为 2,048 到 Provider 硬上限；显示“自动 65,536 / 服务上限 393,216”而不是锁死输入框。

把 `clamp_request_max_tokens()` 改为可失败函数：

```rust
fn resolve_request_max_tokens(...) -> Result<ResolvedOutputBudget, PreflightError>;
```

必须删除 `.max(1)`。硬闸门在最多两次整理后必须重新检查不变量；仍超限则返回 `CONTEXT_PREFLIGHT_FAILED`，不得跳出循环后继续发送。主 Agent 和原生子代理两条 run loop 必须同时修改。

### 6.5 `MaxTokens` 终态处理

删除当前对任意空 `MaxTokens` 回合盲目执行两次 `×2` 的策略。新规则：

- 若请求在派发前已因上下文 headroom 被钳制，收到 `MaxTokens` 后返回 `CONTEXT_CONSTRAINED_OUTPUT_EXHAUSTED`，不重放。
- 若请求使用用户/自动配置的正常上限且无任何正文或工具调用，返回 `OUTPUT_BUDGET_EXHAUSTED`，记录 attempted、configured、provider ceiling 和 reasoning effort。
- 已有正文或工具调用时保留已有“不得整轮重放”的规则，避免重复输出或重复执行。
- 不得自动把显式 `reasoning_effort=max` 改成别的值。用户可以在设置中降低每轮输出或推理强度；程序必须准确呈现原因。
- request audit 中 `effective_output_tokens < minimum` 的请求永远不应存在，因此 `1 → 2 → 4` 路径必须从测试和产品代码中消失。

## 7. 附件感知压缩与旧数据迁移

### 7.1 新会话压缩规则

canonical transcript 永远保存 `AttachmentRefV1`。模型投影按以下规则处理图片：

1. exact tail 内的 `native_input` 图片保留引用，并在 Provider 边界重新物化。
2. 图片即将移出 exact tail 时，使用当前同一个多模态主模型生成 `VisualCheckpointV1`；这是模型自身的视觉理解，不是 OCR。
3. checkpoint 请求必须携带原图引用和相邻用户文本，且使用同一冻结 provider/model route。
4. 只有模型以完整 stop reason 返回非空结果，且结果事件记录了全部 attachment id，才允许在模型投影中用 checkpoint 文本替代旧图片。
5. checkpoint 失败时保留旧投影；若因此无法满足窗口硬闸门，返回明确预检错误，不得 OCR。
6. canonical transcript 中的原图引用不被 checkpoint 改写，用户后续明确要求重新看原图时仍可重新物化。

文本主模型场景已经在首次发送时生成 OCR/辅助视觉模型文本，压缩只处理该文本。`display_only` 原图不进入压缩 Provider 请求。

`truncate_message_chars()` 必须显式处理 `Attachment`：保留完整引用或按 checkpoint 规则替换，绝不能 clone 含旧 Base64 的 File/Image 块。新格式下函数不得接触附件字节。

### 7.2 双读兼容

`agent-store::SessionStore::load()` 和 queued attachment reader 必须同时支持：

- v1：`FileSource.data` / `ImageSource.data` 或旧 `attachments_json` 数组中的 Base64；
- v2：`ContentBlock::Attachment` / `attachments_json.version == 2`。

兼容读取 v1 时先做大小、MIME 和 Base64 校验，再调用 `AttachmentStore::stage()`；成功后只向运行时返回引用投影。任何 legacy Base64 都不得继续传给 `message_text_chars()`。

### 7.3 JSONL 原子迁移

启动后的后台迁移器按单个 `storage_id` 串行处理，步骤固定为：

1. 计算源 JSONL SHA-256，在 `session_attachment_migrations` 写 `pending`。
2. 完整解析事件；遇到一处损坏即把该会话标为 failed，源文件保持不变。
3. 对每个二进制 Base64 解码、校验并 stage Blob；相同内容复用同一物理 Blob。
4. 把 Message、HistorySnapshot、ModelProjection 中的二进制块改为 `AttachmentRefV1`。
5. 在原目录写临时 JSONL，flush、`sync_all()`，再重新 load 并验证消息数、tool pairing、附件可读性和无 Base64。
6. 以同目录原子 rename 替换活动 JSONL。
7. `commit_many()`，记录 target SHA-256，把迁移状态改为 committed。
8. 只有步骤 7 成功后才删除迁移临时文件。不得长期保留含 Base64 的 `.bak`。

崩溃恢复：

- `pending` 且活动文件仍为 source hash：重新执行。
- `pending` 且活动文件为可验证 target：补 commit 并标 committed。
- 两者都不匹配：标 failed，禁止自动覆盖，向诊断页报告 storage id。

### 7.4 queued message 迁移

旧 `attachments_json` 在 claim 前懒迁移：

1. 在同一 SQLite `IMMEDIATE` 事务领取旧队列项。
2. 事务外 stage Blob，得到 refs。
3. 回到事务，以原 attachments JSON hash 作为 CAS，把 payload 改为 v2 refs 并 commit attachments。
4. CAS 失败时释放本次 staged 逻辑引用并重新读取。
5. 任一附件损坏则把队列项标 failed，保留可读错误；不得丢附件后发送纯文本。

### 7.5 删除与生命周期

修改 `LifecyclePurgeStore` 和任务删除路径：

- 删除任务前列出其 attachment blob hashes 和引用次数。
- SQLite 事务删除任务、attachments 和业务记录。
- 事务提交后按逻辑引用数调用 BlobStore decrement；仍被其他任务/消息引用的物理 Blob 保留。
- 磁盘删除失败走现有 prune 恢复，不得回滚已经成功的业务删除。
- 旧 `{app_data}/attachments/{task_id}` OCR 预览目录在迁移完成后由一次性清理器删除；迁移前不得删除。

## 8. DeepSeek 两阶段 Plan 锚定

### 8.1 配置合同

在 `agent-config::PlanningConfig` 和前端 `PlanningConfig` 增加：

```toml
[planning]
suggest_complex_tasks = true
deepseek_plan_anchoring = false
```

字段语义：

- `suggest_complex_tasks`：是否允许复杂任务先询问用户要不要进入 Plan；保持现有语义。
- `deepseek_plan_anchoring`：用户实际进入 DeepSeek Plan 后，是否启用最小 Plan 轨迹和完整执行恢复。
- 两者互不替代。用户可以关闭自动建议，但手动进入 Plan 时仍启用锚定；也可以保留建议但让 Plan 使用 baseline。
- `deepseek_plan_anchoring` 默认 false，旧配置缺字段时为 false。
- `R_CODE_PLANNING_EMERGENCY_OFF=1` 同时关闭建议和锚定，但不关闭 Plan 的只读安全硬门。

### 8.2 冻结 profile

把 `ResolvedPlanRuntimeProfile` 版本提升到 v2，至少增加：

```rust
struct ResolvedPlanRuntimeProfileV2 {
    enabled: bool,
    catalog_profile: PlanCatalogProfile,
    context_profile: PlanContextProfile,
    profile_version: u32,              // 2
    provider_name: String,
    provider_kind: String,
    model_id: String,
    protocol: String,
    provider_route_revision: String,
    anchoring_preference: bool,
}
```

所有 Plan 创建路径必须显式传入同一次设置与 route 快照：

- UI `plan_create`；
- `enter_plan_mode`；
- 接受 `PlanEntryOffer`；
- 手动从 task mode 切到 Plan。

`request_scope_decision` 继续使用 baseline，除非该入口后来明确变成完整 Plan。存储层不得自己读取全局设置。

### 8.3 派生状态机

不新增第二套数据库状态。锚定状态从现有 durable 字段派生：

```text
Off
  profile.enabled == false

PlanBootstrap
  task.mode == plan
  plans.catalog_phase == bootstrap

PlanResident
  task.mode == plan
  plans.catalog_phase == resident

ExecutionFull
  plan.state == executing
  implementation_dispatch_state == dispatched
  task.mode == auto
```

允许的单向转换：

```text
Off stays Off
PlanBootstrap --first durable assistant/tool outcome--> PlanResident
PlanResident --plan_publish + user approval + durable dispatch--> ExecutionFull
```

禁止：

- restart、resume、fork、branch reload、compact 或 clear context 让 Resident 回到 Bootstrap；
- ExecutionFull 再次启用 Plan bootstrap；
- 仅凭模型说“计划完成”进入 ExecutionFull；
- PlanStore CAS 或 queue staging 未持久化时发送下一次请求。

### 8.4 各阶段请求面

| 阶段 | client tools | hosted/MCP | 自动注入 | 写操作 | inference/output |
| --- | --- | --- | --- | --- | --- |
| PlanBootstrap | 精确 5 项：`glob`, `plan_publish`, `read_file`, `request_user_input`, `search_files` | 全部隐藏 | 仅原用户消息、用户主动 steer、固定 Plan system、权威 PlanContextCapsule、用户附件引用 | 硬拒绝 | 保留用户冻结 inference；使用正常请求输出上限，不施加廉价 governor |
| PlanResident | 精确 8 项：bootstrap + `git_status`, `list_files`, `load_skill` | 全部隐藏 | 同 Bootstrap | 硬拒绝 | 同 Bootstrap |
| ExecutionFull | 该任务正常配置允许的完整工具目录 | 恢复 enabled MCP、hosted tools | 恢复 memory、clock、task context、用户编排提示、delegation、progress/governor 等标准注入 | 按正常权限策略 | 恢复正常 inference、temperature、输出上限与 governor |

Plan 阶段的“最小环境”不得通过伪工具实现。API `tools` 必须只含上述真实工具 schema；Gateway 的 Plan hard gate继续独立拒绝隐藏调用。

### 8.5 统一上下文注入闸门

在 `llm_runtime.rs` 引入：

```rust
enum ContextInjectionProfile {
    Standard,
    PlanMinimalV1,
}

enum ContextSource {
    Memory,
    LocalClock,
    TaskContextCapsule,
    PlanPolicy,
    UserAgentPrompt,
    McpPolicy,
    PeerMailbox,
    PlanSuggestion,
    ToolProgressCheckpoint,
    DelegationHint,
    HostedWebFallback,
    SummaryRecovery,
    ReasoningGovernor,
}
```

所有 system 和 tail 构造必须先经过同一个 profile，不得在各来源处零散判断。`PlanMinimalV1` 只允许：

- 固定 Plan 安全/system 文本；
- 权威 `PlanContextCapsule`；
- 原始用户请求与用户主动 steer；
- 用户消息中的原始多模态附件引用；
- 为 `plan_publish`、`request_user_input` 必需的固定协议说明。

必须禁止：memory、local clock、普通 task context、用户配置的主/子 Agent 协作文案、MCP 管理文案、peer mailbox、Plan 建议尾部、工具进度 checkpoint、委派提示、hosted web fallback、cheap/full reasoning governor 尾部。

被禁止的 peer message 不得从 mailbox 消费后丢弃；保持 pending，进入 ExecutionFull 后再按正常规则读取。

`build_main_system_prompt()` 必须接收 `ContextInjectionProfile`。PlanMinimal 不得先构造 Standard system 再做字符串删除；必须从固定最小模板正向构造。

### 8.6 完整工具恢复不变量

复用 `PlanStore::stage_implementation_dispatch()` 当前同一事务中的两项写入：

- `tasks.mode = 'auto'`；
- `plans.implementation_dispatch_state = 'dispatched'` 并插入 deterministic queue row。

在事务成功后增加 `CatalogAnchorPhase::RestoredFull` 审计事件。下一 run 的 `prepare_runtime_session()` 必须：

1. 从数据库重新读取 task mode、plan state 和 implementation dispatch state。
2. 派生 `ExecutionFull`。
3. 调用 `update_plan_native_catalog(session, None)`。
4. 使用 Standard `ContextInjectionProfile`。
5. 构造未裁剪的正常 client tools 与 active hosted tools。
6. 恢复 delegation supervisor、MCP external tools 和标准推理 governor。
7. 在 Provider 请求前断言目录不再等于 5/8 项；若仍被收窄则 fail closed，错误为 `PLAN_FULL_CATALOG_NOT_RESTORED`。

“完整”仍受用户配置和权限控制：被用户禁用、未连接、无权限或当前平台不支持的工具不得因为恢复事件而出现。

### 8.7 route 漂移

每个 Plan 请求比较当前 task route 与 profile 中的 `provider_route_revision`：

- PlanBootstrap/Resident 漂移：停止并提示用户重新创建 Plan 或恢复原 route。
- Plan 批准到首次 ExecutionFull 请求之间漂移：实施队列保持 failed/retryable，不得用另一个 Provider 静默执行既有 Plan。
- ExecutionFull 首次请求成功后，后续普通任务模型切换遵循现有任务设置语义，但不得重新触发锚定。

## 9. 设置页滑钮

在 `SettingsScene.tsx` 的现有 Planning 卡片中保留“复杂任务先建议制定计划”，并新增第二个开关：

```text
DeepSeek Plan 锚定
规划时仅保留必要的只读工具和上下文；批准实施后恢复当前任务的全部可用能力。
```

交互合同：

- 只有至少存在一个 ready 的 `provider_kind=deepseek` 配置时显示该卡；不要求 DeepSeek 是默认 Provider。
- `customer_switch_enabled=false` 或 emergency off 时禁用，并显示明确原因。
- 保存键为 `planning.deepseek_plan_anchoring`。
- 保存成功后 reload，关闭应用再打开值必须一致。
- 活动 Plan 旁显示“本计划已冻结为 开/关；设置变更从下一个计划生效”。
- 切换开关不得修改 pending offer、活动 Plan 的 runtime profile 或正在运行的请求。
- 前端不得自行按 provider 名称猜 DeepSeek；使用 `cmd_planning_status` 返回的稳定状态。

更新位置：

- `vendor/agent-contracts/crates/agent-config/src/lib.rs`；
- `src-tauri/frontend/src/lib/types.ts`、`mock-data.ts`；
- `src-tauri/frontend/src/components/scenes/SettingsScene.tsx`；
- `src-tauri/src/plan_policy.rs`、`plan_entry_commands.rs`；
- `src-tauri/frontend/scripts/app-shell.test.mjs` 及设置搜索测试；
- GuideSheet 中现有 Plan 指引文案，明确两个开关的独立语义。

## 10. 分阶段实施顺序

以下顺序是合并顺序。后一阶段不得在前一阶段的完成条件未通过时启用写路径。

### 阶段 A：冻结回归夹具与审计字段

改动：

1. 给 request audit 扩展安全字段：provider name/kind、model、protocol、context window、text/image/document token、requested/effective output、wire bytes、attachment count、anchoring phase、context profile、tool names、hosted tool names。
2. 审计只写数值、id 和 hash，不写图片、文本附件正文、API key 或完整 Provider body。
3. 增加 1818×1026 / 3,383,259 bytes 图片 fixture 的元数据与确定性预算单测；二进制 fixture 可由测试生成，不把大 Base64 提交到仓库。
4. 给 Provider mock 增加请求计数、OCR 调用计数、helper vision 调用计数和捕获的最终 content block 类型。

完成条件：旧代码下至少有一条测试能稳定复现伪 1,127,753 token 或 `1 → 2 → 4`；新审计 schema 的序列化测试通过且不含 `data`/`base64` 字段。

### 阶段 B：附件引用与 BlobStore

改动：

1. 实现 migration 034、`AttachmentStore`、`AttachmentRefV1` 和双读 decoder。
2. Blob 写入改为临时文件 + fsync + 原子 rename。
3. 实现 staging、commit、discard、lease GC、任务删除 refcount。
4. SessionStore、UI DTO、timeline 和 preview reader 识别新引用。
5. 保持旧写路径关闭，只运行双读和存储单测。

完成条件：相同图片 stage 两次只有一个物理 Blob；逻辑引用数正确；任一逻辑引用删除不会误删仍在使用的 Blob；旧 JSONL 仍能读取。

### 阶段 C：WebView staging 与新写路径

改动：

1. 新增 `cmd_attachment_stage`、`cmd_attachment_discard`。
2. `AttachmentInput` 从发送命令移除 Base64，改传 attachment ids。
3. Composer 只在 staging 调用局部持有 Base64；草稿态使用 Object URL。
4. direct、queue、send-now 和收尾竞态回队列全部使用 refs。
5. 新写路径由内部 feature flag 控制；开启后新 JSONL/queue 不再出现 Base64。

完成条件：粘贴 8 MiB 以内图片后，React 可序列化状态、SQLite queued payload 和活动 JSONL 搜索不到该图片的 Base64 前 64 字符；UI 仍可预览。

### 阶段 D：能力解析与禁止多模态 OCR 兜底

改动：

1. 实现后端单一 `ResolvedModelCapabilities`。
2. 修复 DeepSeek vision model 的 Provider capability。
3. 重构 `apply_image_understanding()` 为三条显式 route。
4. 删除 helper vision 失败后的 OCR fallback。
5. 实现 `VISION_CAPABILITY_DRIFT` 和 `VISION_WIRE_UNSUPPORTED`。

完成条件：`vision=true` 请求中有 `input_image` 或等价 Provider 文件引用，OCR/helper 调用均为 0；Provider 拒图后仍为 0。

### 阶段 E：预算、物化和硬闸门

改动：

1. 实现 `RequestBudgetV1` 和 DeepSeek 视觉 profile。
2. 修改压缩预算只预留实际 requested output。
3. `resolve_request_max_tokens()` 返回 Result，删除 `.max(1)`。
4. 主循环和子代理循环整理后必须重新检查，不满足就零发送。
5. Provider 调用前物化 refs，调用后丢弃物化副本。
6. 移除 `1 → 2 → 4` 自动升档。
7. 设置页拆分 recommended output 与 provider maximum，允许编辑每轮上限。

完成条件：固定图片回归的 image token 为 32,000；配置 393,216 时有效输出不因图片 Base64 被压成个位数；预检失败时 mock 请求数为 0。

### 阶段 F：附件感知压缩与迁移

改动：

1. exact tail 保存 refs。
2. 实现同一多模态主模型的 `VisualCheckpointV1`。
3. 实现 JSONL 原子迁移和 queue lazy migration。
4. 实现崩溃恢复、诊断状态和旧预览目录清理。

完成条件：迁移后的活动 JSONL、HistorySnapshot、ModelProjection 和 queue payload 无二进制 Base64；迁移前后可见消息数、tool pairing 和附件预览一致。

### 阶段 G：DeepSeek Plan 锚定滑钮和统一上下文闸门

改动：

1. 增加配置字段和 UI 滑钮。
2. profile v2 冻结 preference 与 route。
3. `build_main_system_prompt()` 与全部 tail source 接入 `ContextInjectionProfile`。
4. 复用 5→8 目录和 PlanStore promotion。
5. 在 implementation dispatch 后发出 `RestoredFull`，下一 run 验证完整目录。
6. 对 resume/restart/fork/compact/clear context 增加状态不倒退测试。

完成条件：开关关闭与 baseline 请求 hash 一致；开启后 Plan 请求只包含允许来源；批准后的首个实施请求恢复全部当前可用能力。

### 阶段 H：全链路回归与发布开关

改动：

1. 运行本文第 12、13 节全部测试。
2. 先开启 AttachmentRef 新写，确认迁移完成，再允许移除 legacy write。
3. Plan 锚定保持默认 false，由用户滑钮开启。
4. 保留 attachment write emergency flag 与 `R_CODE_PLANNING_EMERGENCY_OFF`。
5. 同步更新 `docs/archive/implementation/settings-ux-and-image-understanding.md`、`docs/archive/implementation/plan-mode-dual-track-gate.md` 和 `docs/readme.md`：删除“多模态失败自动 OCR”、单一 Planning 开关和旧 Base64 持久化语义，避免实现完成后仍有相互矛盾的操作文档。

完成条件：所有自动验收通过，真实 DeepSeek vision smoke test 通过，支持包不含附件正文，回滚演练通过。

## 11. 错误、可观测性与用户提示

必须增加或扩展以下稳定错误码：

| 错误码 | 触发条件 | 是否发送 Provider 请求 |
| --- | --- | --- |
| `ATTACHMENT_NOT_FOUND` | ref 不存在 | 否 |
| `ATTACHMENT_OWNERSHIP_MISMATCH` | ref 不属于当前 task | 否 |
| `ATTACHMENT_METADATA_MISMATCH` | 消息元数据与 DB 不一致 | 否 |
| `ATTACHMENT_ROUTE_DRIFT` | queued/Plan 冻结 route 与当前 route 不一致 | 否 |
| `VISION_BUDGET_PROFILE_MISSING` | 已确认多模态但无预算 profile | 否 |
| `VISION_WIRE_UNSUPPORTED` | 协议适配器不能发送图片 | 否 |
| `VISION_CAPABILITY_DRIFT` | Provider 拒绝已确认的图片能力 | 是，恰好一次 |
| `CONTEXT_PREFLIGHT_FAILED` | 整理后输入+输出仍超窗 | 否 |
| `OUTPUT_HEADROOM_BELOW_MINIMUM` | 有效输出低于当前请求阈值 | 否 |
| `OUTPUT_BUDGET_EXHAUSTED` | Provider 真正用尽正常输出上限且无产物 | 是，不自动翻倍 |
| `PLAN_ANCHOR_ROUTE_DRIFT` | 活动 Plan route 漂移 | 否 |
| `PLAN_ANCHOR_PROMOTION_FAILED` | bootstrap→resident CAS 失败 | 下一请求不得发送 |
| `PLAN_FULL_CATALOG_NOT_RESTORED` | ExecutionFull 仍看到收窄目录 | 否 |

日志和 request audit 至少记录：

- origin request id、task/run/session/storage id；
- resolved provider name/kind/model/protocol；
- attachment ids 和内容 hash 的短前缀，不记录正文；
- 预算分项和预算来源；
- configured/recommended/provider ceiling/effective output；
- anchoring phase、context profile、tool count、hosted tool count；
- OCR/helper/native main 三类调用计数；
- migration state 与 source/target hash。

任何错误不得根据曾经出现过的 Ark、其他 provider 名称或显示标签误报当前 Provider。错误文案必须使用本次冻结 route。

## 12. 自动化测试矩阵

### 12.1 Rust 单元与集成测试

`agent-contract`：

- `AttachmentRefV1` roundtrip；
- legacy File/Image 双读；
- RequestHeader 新字段缺省兼容；
- 序列化结果不包含 attachment bytes。

`r-code-store`：

- stage/commit/discard/GC；
- 相同内容 Blob 去重；
- refcount 在跨任务引用和任务删除时正确；
- 原子写失败、DB 失败、崩溃窗口恢复；
- migration 034 新建/重放保护；
- JSONL source/target hash 状态机。

`agent-llm`：

- DeepSeek 三个模型的 `supports_vision` 真值；
- Chat、Responses 和支持图片的协议输出正确 image block；
- 未物化 Attachment 在 adapter 边界 fail closed；
- Provider 拒图映射不触发 OCR。

`r-code-agent-worker`：

- 1818×1026 图片预算精确为 32,000；
- Base64 长度变化不改变 image token 计算；
- `message_text_chars()` 不统计附件字节；
- `resolve_request_max_tokens()` 在 headroom 不足时返回 Err；
- 硬闸门整理两次后仍超限，mock 请求数为 0；
- 主循环和 child loop 都没有 `.max(1)` 行为；
- `MaxTokens` 不再产生 `1,2,4` 请求序列；
- exact tail 保留 ref，VisualCheckpoint 成功/失败符合合同；
- checkpoint 使用当前主多模态模型，OCR mock 调用为 0；
- Plan bootstrap 精确 5 项、resident 精确 8 项；
- PlanMinimal 的禁止注入全部缺席；
- promotion CAS 失败后无第二请求；
- ExecutionFull 恢复完整目录和标准注入。

`r-code-host`：

- direct/queue/send-now/收尾竞态队列只持久 refs；
- route snapshot 按 `provider_kind` 判定；
- vision 主模型直发；
- text 主模型按显式 OCR/helper 设置分派；
- helper 失败不 OCR fallback；
- setting 保存、默认值、legacy config；
- Plan 创建时冻结开关，活动 Plan 不受设置热更新影响。

建议命令：

```powershell
rtk cargo test -p agent-contract
rtk cargo test -p agent-llm
rtk cargo test -p r-code-store
rtk cargo test -p r-code-agent-worker
rtk cargo test -p r-code-host
rtk npm test --prefix src-tauri/frontend
rtk npm run build --prefix src-tauri/frontend
```

### 12.2 前端测试

必须覆盖：

- paste/select 后调用 staging，发送 IPC 不带 Base64；
- staging 失败显示错误且不留下可发送草稿；
- remove 调用 discard 并 revoke Object URL；
- optimistic bubble 与 reload 后预览一致；
- 两个 Planning 滑钮独立保存；
- 无 DeepSeek 配置时锚定卡隐藏；
- 活动 Plan 显示冻结语义；
- 开关切换只影响新 Plan；
- Provider 最大输出只读、每轮输出可编辑且范围校验正确。

### 12.3 静态泄漏检查

测试生成一个已知图片，其 Base64 前 64 字符记为 canary。完成 direct、queue、compact、restart 和 migration 后，对以下范围搜索 canary，结果必须为 0：

```text
sessions/**/*.jsonl
sessions/request-audit/**/*.jsonl
SQLite queued_messages / task events / plans
logs/**
support bundle/**
```

Blob 文件本身按二进制 hash 保存，不参与 Base64 canary 搜索。

## 13. 严格验收步骤

### 13.1 多模态真实链路

前置条件：

1. 新建隔离 AppData 与数据库。
2. 配置 `provider_kind=deepseek`、`deepseek-v4-flash-vision-exp`，选择已支持图片的协议。
3. 上下文窗口为 1,000,000；每轮最大输出显式设置 393,216；thinking enabled；reasoning effort max。
4. 开启 request audit。
5. 准备 1818×1026、decoded 3,383,259 bytes 的 PNG。

操作与预期：

1. 粘贴图片，发送“请描述这张图并指出关键界面元素”。
2. staging 后检查 JSONL：存在 `attachment_id`，不存在图片 Base64。
3. 检查 request audit：
   - `provider_kind == deepseek`；
   - `model == deepseek-v4-flash-vision-exp`；
   - `image_tokens == 32000`；
   - `estimated_input_tokens < 1_000_000 - 393_216 - reserve`；
   - `effective_output_tokens == 393216`，除非真实文本/tools 已占用足够 headroom，此时必须是合理的大于 Plan/Agent minimum 的数值；
   - `materialized_wire_bytes` 大于 decoded bytes但不进入 token 字段。
4. 捕获 Provider 请求：必须有 `input_image`、图片 content block 或等价 `file_id`。
5. 检查计数：native main vision = 1，OCR = 0，helper vision = 0。
6. 模型正常返回内容；不得出现“重试至 4”。

验收通过条件：上述六步全部满足。

### 13.2 多模态拒图

1. 使用声明 `vision=true` 但 mock Provider 返回 unsupported image 的 route。
2. 发送一张有效 PNG。
3. 预期只发送一次 Provider 请求。
4. UI 显示 `VISION_CAPABILITY_DRIFT` 和冻结 provider/model/protocol。
5. OCR 与 helper vision 调用数均为 0。
6. 用户消息和 Blob 保留，可在修复 route 后重试。

### 13.3 文本模型图片理解

OCR 路径：

1. 使用 `deepseek-v4-flash` 或其他 `vision=false` 模型。
2. 显式选择 OCR 引擎。
3. 发送 PNG；预期主模型请求只有 OCR 文本，原图 ref 为 UI display-only。
4. OCR 失败时直接报错，不调用 helper vision。

独立视觉模型路径：

1. 使用同一文本主模型，显式选择 helper provider/model。
2. 发送 PNG；helper 恰好调用一次，主模型接收描述文本。
3. helper 失败时直接报错，OCR 调用数为 0。

### 13.4 输出与上下文闸门

1. 构造输入使剩余 headroom 只有 4,000 token，并以 Agent 工具回合发送。
2. 预期 `OUTPUT_HEADROOM_BELOW_MINIMUM`，Provider 调用数 0。
3. 构造压缩两次后仍超窗的历史。
4. 预期 `CONTEXT_PREFLIGHT_FAILED`，Provider 调用数 0。
5. 构造 Provider 返回空 `MaxTokens`。
6. 捕获 max_tokens 序列；不得是 `[1,2,4]`，不得有自动翻倍重放。
7. 设置每轮最大输出为 65,536，重新打开设置后值保持；Provider 请求不得超过该值。
8. 设置为 393,216，Provider 请求不得超过服务端上限，也不得因图片 Base64 被降为个位数。

### 13.5 持久化、去重与迁移

1. 同一图片在同一任务连续发送两次，再在第二任务发送一次。
2. 物理 blobs 目录只有一个对应 hash 文件；attachments 有三个逻辑引用。
3. 删除一个任务，另一个任务仍能预览和重发图片。
4. 删除最后一个引用后，Blob ledger 归零并可被 prune 清理。
5. 准备旧 JSONL 和旧 queued Base64 payload，执行迁移。
6. 迁移后活动 JSONL、HistorySnapshot、ModelProjection、queue payload 都无 Base64。
7. 重启、加载历史、预览、继续对话均成功。
8. 在迁移 rename 前、rename 后/DB commit 前分别注入崩溃，恢复结果符合第 7.3 节且不丢消息。

### 13.6 DeepSeek Plan 锚定关闭

1. `deepseek_plan_anchoring=false`。
2. 对同一固定 Plan case 捕获 baseline 与关闭开关后的首轮请求。
3. system、messages、tools、hosted tools、inference、max_tokens 的规范化 hash 必须与 baseline 一致。
4. 不产生 CatalogAnchor narrowed/promoted/restored 事件。
5. 非 DeepSeek Provider 无论开关值为何都满足同样的不受影响条件。

### 13.7 DeepSeek Plan 锚定开启

1. `suggest_complex_tasks=true`、`deepseek_plan_anchoring=true`，任务绑定 DeepSeek 和 workspace。
2. 发送复杂任务，模型调用现有 `propose_plan_mode`；用户选择进入 Plan。
3. 检查 Plan profile v2：preference、provider kind、model、protocol、route revision 已冻结。
4. 首个 Plan 请求工具名必须精确等于 5 项 bootstrap，hosted/MCP/子代理均为空。
5. 请求中不得包含 memory、clock、普通 task context、用户 Agent 协作文案、MCP 文案、peer、delegation、progress 或 governor 尾部。
6. 原用户请求、PlanContextCapsule 和原始图片 ref 必须仍在。
7. 首个 durable outcome 后 PlanStore CAS 成功，下一请求工具名精确等于 8 项 resident。
8. `plan_publish` 后任务仍为 Plan、写操作仍拒绝。
9. 用户批准；同一事务把 plan 置 executing、dispatch 置 dispatched、task mode 置 auto，并入队实施消息。
10. 首个实施请求产生 `RestoredFull`，恢复当前配置下完整 client tools、MCP、hosted tools、子代理和 Standard 上下文。
11. inference、temperature 和每轮输出上限与 Plan 前冻结任务配置一致，不残留 Plan 专用 governor/cap。
12. restart、resume、fork、compact、clear context 各执行一次，阶段不倒退且 ExecutionFull 不重触发 bootstrap。

### 13.8 设置滑钮

1. 无 ready DeepSeek 配置：锚定卡不显示。
2. 添加 ready DeepSeek：卡出现，默认关闭。
3. 打开后关闭设置再重开：仍为打开。
4. 创建 Plan 后把全局开关关闭：活动 Plan 仍按已冻结的打开状态运行，新 Plan 使用关闭状态。
5. emergency off：开关禁用，已有 Plan 退回 baseline 目录但 Plan 写硬门仍有效。

## 14. 发布、回滚与数据安全

### 14.1 发布顺序

必须分两版或两个可独立回滚的发布阶段：

1. 兼容版：上线 AttachmentRef/legacy 双读、migration 034、预算审计和 Plan 新字段读取，但保持 legacy attachment write 与 Plan 锚定默认关闭。
2. 生效版：开启 AttachmentRef 新写和后台迁移；确认稳定后允许用户打开 Plan 锚定。

不得直接回滚到不认识 `AttachmentRefV1` 的旧二进制。生效版只能回滚到兼容版。

### 14.2 运行时急停

- AttachmentRef 新写使用内部 emergency flag；关闭后停止创建新 refs，但双读和已迁移数据读取必须继续。
- `R_CODE_PLANNING_EMERGENCY_OFF=1` 关闭 DeepSeek 建议和锚定，Plan 安全硬门保持。
- 急停不得删除 Blob、逆迁移 JSONL、改写活动 Plan profile 或清空用户设置。

### 14.3 回滚演练

1. 在生效版创建新 ref 会话、queued message 和已执行 Plan。
2. 切换到兼容版。
3. 验证会话加载、附件预览、queue dispatch 和 baseline Plan 均可用。
4. 再切回生效版，迁移状态不得重复增加 refcount或重复生成 Blob。
5. 校验所有活动 JSONL 仍无 Base64，所有 attachment id 可解析。

## 15. Definition of Done

以下项目必须全部完成，才能关闭本修复：

- [ ] 多模态主模型直读原图，OCR/helper 调用为 0。
- [ ] 多模态拒图返回能力漂移，不静默兜底。
- [ ] 文本主模型只按显式图片理解引擎处理，helper 失败不自动 OCR。
- [ ] 新会话、queue、HistorySnapshot、ModelProjection、request audit 无二进制 Base64。
- [ ] 同一内容物理 Blob 去重，引用生命周期和任务删除正确。
- [ ] 固定图片 image token 为 32,000，不再按 4,511,012 字符估算。
- [ ] 上下文、输出、图片、wire bytes 四类预算分离并可审计。
- [ ] `resolve_request_max_tokens()` 可失败，代码中无 headroom `.max(1)`。
- [ ] 硬闸门失败时 Provider 请求数为 0。
- [ ] 产品中不再出现由伪额度产生的 `1 → 2 → 4` 重试。
- [ ] 用户可编辑每轮最大输出，Provider 最大值只作为上限。
- [ ] legacy JSONL/queue 迁移可恢复、可重复执行且不重复 refcount。
- [ ] `deepseek_plan_anchoring` 滑钮默认关闭、可保存、只影响新 Plan。
- [ ] Plan bootstrap 5 项、resident 8 项目录保持精确且使用真实工具 schema。
- [ ] PlanMinimal 只保留允许的权威上下文，所有禁止注入均有负向测试。
- [ ] Plan hard gate 独立拒绝写入、Shell、MCP mutation 和子代理。
- [ ] Plan 批准后的首个实施请求恢复当前配置下完整工具和标准上下文。
- [ ] 非 DeepSeek Provider 和关闭开关时请求形状不变。
- [ ] restart、resume、fork、compact、clear context 后阶段不倒退。
- [ ] request audit 能准确显示实际 provider kind/model/protocol，不再误归因到其他 Provider。
- [ ] 相关设置、Plan 与文档索引不再描述被本方案废止的 OCR fallback、单开关或 Base64 持久化行为。
- [ ] 第 12 节自动测试与第 13 节严格验收全部通过。

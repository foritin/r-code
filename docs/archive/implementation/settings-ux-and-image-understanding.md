# 设置体验优化与图片理解引擎 · 实施方案

- 状态：已实施（2026-08-22；含 A/B/C/D/E 全部 P0-P2 条目与测试改写）
- 日期：2026-08-22
- 基线分支：`feat/request-audit-anchoring`（含 `706f131` DeepSeek 复杂任务 Plan 建议双轨 Phase 0）
- 关联文档：[plan-mode-dual-track-gate.md](./plan-mode-dual-track-gate.md)、[plan-mode.md](../../plan-mode.md)、[architecture.md](../../architecture.md)
- 行号为撰写当日快照，实施时以符号搜索为准。

本方案回答两个使用疑问（规划建议卡的"验证中"与"默认 provider"、子代理连通测试），并落地三项需求：

| 编号 | 主题 | 类型 |
| --- | --- | --- |
| A | 规划建议卡：移除证据门，滑钮打开即用 | 问题修复 + 行为变更 |
| B | 子代理面板：自动连通测试覆盖已保存槽位 | 问题修复 |
| C | Provider 模型能力标注（文本 / 多模态） | 新需求（需求 1 延伸） |
| D | 图片理解引擎配置（本机 OCR / 视觉模型，二选一，默认 OCR） | 新需求（需求 1） |
| E | 设置整体交互与 UI 优化、指引手册补全 | 新需求（需求 2） |

---

## 0. 疑问答疑（现状结论）

### 0.1 为什么"复杂任务先建议制定计划"显示"功能仍在验证中，暂不可启用"？

这不是 bug，是刻意 fail-closed 的**证据门**设计（`docs/archive/implementation/plan-mode-dual-track-gate.md` §3.3/§16）：

- 开关可用性由后端命令 `cmd_planning_status` 返回的 `PlanningStatusView` 决定，前端 `SettingsScene.tsx:1451-1462` 按级联提示渲染，`SettingsScene.tsx:1502` 以 `customer_switch_enabled` 控制禁用。
- `evidence_validated == release_state == Validated`（`src-tauri/src/plan_entry_commands.rs:444-445`）。而 `release_state` 恒为 `off`，因为构建期要嵌入的 DeepSeek 三臂评估证据文件 `eval/plan-eval/artifacts/manifest.json` **尚不存在**：`src-tauri/build.rs:49-52` 读不到时嵌入字面 `"null"`，`plan_policy.rs` 的 `load_validated_manifest()` 返回 `None`，兜底 `Off`。
- 解锁路径（原设计）：真实跑完 75 次能力 + 40 次路由评估（`eval/plan-eval/`），生成通过硬门（dual-track 净多解 ≥4、回退 ≤1、McNemar p≤0.10 等）的 manifest 并重新构建。开发联调可走内部实验开关 `R_CODE_PLANNING_EXPERIMENT=1`（`plan_policy.rs:269-279`）。

**产品决策（2026-08-22）：移除该证据门。** 客户滑钮 `planning.suggest_complex_tasks` 成为唯一开关，打开即生效，不再要求预注册评估、manifest 嵌入或实验环境变量；`R_CODE_PLANNING_EMERGENCY_OFF` 急停开关保留作为兜底。完整改造见 A3。

### 0.2 "只对 DeepSeek 生效"为什么和"默认 provider"绑在一起？

功能资格的真实判定链是**按任务运行时 route**：`plan_entry_commands.rs:110-133` 的 `route_context_for_task` 对任务取其绑定的 provider，未绑定才回退 `config.default_provider`；任务在创建时即绑定当时激活的服务（`commands.rs:4787-4793`）。DeepSeek 判定写死在 `plan_policy.rs:393` 的 `provider_kind == "deepseek"`（稳定目录身份，非显示名）。

问题出在**设置卡的状态计算**：`planning_status`（`plan_entry_commands.rs:412-448`）只按 `config.default_provider` 计算 `default_provider_is_deepseek` 与 `customer_switch_enabled`。于是默认服务不是 DeepSeek 时，即使配置了可用的 DeepSeek 服务、且 DeepSeek 任务在运行时本可享受建议，设置卡也显示"切换到 DeepSeek 后可配置"并禁用开关——把"按任务生效"误表达成了"必须设为默认"。

修复方向见 A1：状态判定改为"存在可用的 DeepSeek 服务"，运行时逻辑不动。

### 0.3 "用于新对话"就是"设为默认 provider"吗？

是，两者是同一个东西：按钮（`SettingsScene.tsx:1370-1374`）→ `settingsSelectProvider` → `cmd_settings_select_provider`（`commands.rs:13638-13654`）→ 写全局配置顶层字段 `default_provider`。"保存并用于新对话"（`SettingsScene.tsx:1379`）同理，只是保存草稿时顺带 `activate: true`（`commands.rs:13610-13615`）。列表行的"正在使用"徽标即 `name === config.default_provider`（`SettingsScene.tsx:1000,1015`）。

命名确实晦义（"默认"概念从未出现在 UI 上）。改名与交互优化见 E1。

### 0.4 子代理面板不是没有自动测试，而是覆盖不全

进入面板的自动探测**已存在**（`SubagentProvidersPanel.tsx:164-206`），但它只测**候选目录条目**——即每个 provider 配置里的**默认模型**（`subagent_providers.rs:948-956`）：

```ts
// SubagentProvidersPanel.tsx:170-173 —— 只看 catalog.entries
const requests = entries
  .filter((entry) => entry.ready && entry.selectable && entry.model
    && entry.health.state !== "connected")
```

而手动"全部测试"会合并**目录 + 已保存槽位**的 `(source, model)`（`SubagentProvidersPanel.tsx:339`）。健康回执按 `(source, model)` 精确键控（`subagent_providers.rs:758-769`），槽位一旦用了非默认模型，自动探测永远测不到它 → 槽位停在"未测试/失败"，保存被拒（`commands.rs:14534-14547` 要求全部 Connected），用户只能手动点。另有 60 秒模块级节流（`AUTO_PROBE_THROTTLE_MS`，108-109 行）与"自动探测失败静默"（190-192 行），进一步造成"没自动测"的观感。

修复见 B：自动探测合并已保存槽位，并把探测结果显示出来。

---

## A. 规划建议卡：移除证据门，滑钮打开即用

### A1 状态判定改为"存在可用的 DeepSeek 服务"

**后端** `src-tauri/src/plan_entry_commands.rs`：

1. `PlanningStatusView`（含前端镜像 `types.ts:450-459`）新增字段（证据相关字段的删除见 A3）：
   ```rust
   /// 任一已配置且就绪（provider_readiness_error 为 None）的服务 provider_kind == "deepseek"。
   pub deepseek_configured: bool,
   ```
   `default_provider_is_deepseek` 一并删除（不再有消费方）。
2. `planning_status`（412-448 行）改为遍历 `config.providers`，只要存在就绪的 DeepSeek 服务即 `deepseek_configured = true`；`customer_planning_surface`（395-410 行）简化为：
   ```rust
   let switch_enabled = deepseek_configured && !control.emergency_off;
   ```
   不再评估默认 provider 的 route，不再要求 Validated。
3. 运行时武装链 `resolve_suggestion_for_run`（137-201 行）结构不动——资格链在 A3 放宽后自然覆盖全部 DeepSeek route。

**前端** `SettingsScene.tsx:1452-1462` 级联调整为：

```
!status                        → 暂时无法读取功能状态，仍可手动使用 Plan 模式。
status.emergency_off           → 功能当前已暂停，仍可手动使用 Plan 模式。
!status.deepseek_configured    → 此功能只对使用 DeepSeek 的任务生效；尚未配置可用的 DeepSeek 服务。
其余                           → 开 = 复杂任务先询问；关 = 全部直接执行。
```

卡片描述文案同步改为："仅在 DeepSeek 识别到复杂任务时询问（每个任务最多一次）……只对使用 DeepSeek 服务的任务生效，无需把 DeepSeek 设为默认服务。"

### A2 指引手册更新

`components/settings/GuideSheet.tsx` 的 `plan-suggestion` 条目：

- 删除"首发只支持经过验证的 DeepSeek"表述，改为"配置可用的 DeepSeek 服务即可启用"；
- 补充"按任务实际使用的服务生效，无需把 DeepSeek 设为默认服务"；
- 保留既有行为说明：每任务最多一次、拒绝后本任务安静、可随时手动 Plan 模式。

### A3 移除证据门（核心改动）

产品决策：客户滑钮是唯一开关，打开即生效。不再要求 75+40 次预注册评估、不要求构建期嵌入 manifest、不要求实验环境变量。逐文件改动：

1. `src-tauri/src/plan_policy.rs`：
   - `resolve_release_control()`（296-365 行）重写：只读 `R_CODE_PLANNING_EMERGENCY_OFF`。急停时返回关闭态（basis 说明急停）；否则返回开放态。`PlanningReleaseControl` 结构体保留（`resolve_plan_runtime_profile`、审计快照等仍在消费，避免连锁改动），但 `allowed_models / allowed_protocols / allowed_endpoint_classes` 恒为空且资格判定不再读取。
   - `resolve_plan_entry_eligibility()`（388-464 行）：删除 model/protocol/endpoint 三段 allowlist 检查（417-458 行）。资格 = `provider_kind == "deepseek"` 且未急停——任意 DeepSeek 模型、任意线路（官方或中转）、任意协议均可。
   - 删除 `load_validated_manifest()` 与 manifest 硬门校验（约 200-267 行）、`EXPERIMENT_ALLOWED_*` 常量（283-286 行）、`R_CODE_PLANNING_EXPERIMENT` 读取（275-279 行）。
2. `src-tauri/build.rs`：删除 manifest 嵌入逻辑（49-52 行）与相关文件读取——`eval/plan-eval/artifacts/manifest.json` 不再参与构建。
3. `src-tauri/src/plan_entry_commands.rs`：`PlanningStatusView` 删除 `evidence_validated / evidence_version / eligibility_profile_version / basis`；`release_state` 收缩为 `"off" | "open"` 二值供诊断（急停 = off）。
4. 前端：`types.ts:450-459` 同步删字段；`browser-mock-runtime.ts:687-697` 的 mock 改为开放态。
5. 评估工具链降级为**可选质量工具**（代码保留、不再是启用前提）：`src-tauri/src/bin/plan_eval.rs` 与 `eval/plan-eval/` README 标注"用于事后质量回归，不阻塞功能启用"；`scripts/release.test.mjs` 与 src-tauri 测试中断言 manifest 嵌入 / fail-closed 的用例改写为断言"无 manifest 也可启用"。
6. `R_CODE_PLANNING_EMERGENCY_OFF` 急停开关保留：线上出现异常弹窗时的一键全局兜底，说明保留在维护者文档。

**连带生效（预期行为，非回归）**：开放态会同时解锁 Plan-only 双轨（Plan 原生目录 Bootstrap/Resident）在 DeepSeek route 上的生效——原 Phase 0 设计中建议与双轨同受证据门控制，门移除后两者一并按资格链生效。若日后需要独立控制双轨，另立配置项，不在本方案范围。

### 验收

- 任一 DeepSeek 服务就绪（不限模型、不限官方/中转线路）：滑钮可点；打开后复杂任务弹建议，关闭后不弹。全程无 manifest、无实验环境变量。
- 默认服务是其他厂商但存在就绪的 DeepSeek 服务：滑钮同样可用（运行时按任务 route 生效）。
- `R_CODE_PLANNING_EMERGENCY_OFF=1`：全局急停，滑钮禁用并显示"功能当前已暂停"。
- 手动 Plan 模式行为零变化（不依赖 release_state）。
- 测试改写：`plan_policy` / `plan_entry_commands` 中断言 fail-closed 的既有用例更新；新增三条——非白名单 DeepSeek 模型放行、急停仍判负、`planning_status` 不再返回证据字段。

---

## B. 子代理面板：自动测试覆盖已保存槽位

### B1 自动探测请求列表合并槽位

`SubagentProvidersPanel.tsx` 的 `autoProbe` 改为接收整个 `snapshot`，请求列表合并三处来源并按 `candidateKey(source, model)` 去重：

```ts
const requests = dedupeByCandidateKey([
  // 目录条目：就绪、可选、未连通（现状逻辑）
  ...entries.filter((e) => e.ready && e.selectable && e.model
      && e.health.state !== "connected")
    .map((e) => ({ source: e.source, model: e.model })),
  // 已保存槽位：有模型且当前槽位健康不是 connected（slot_health 按 (source, model) 键控）
  ...(snapshot.pool?.slots ?? [])
    .filter((s) => s.model.trim()
      && (slotHealthOf(s, snapshot)?.state ?? "untested") !== "connected")
    .map((s) => ({ source: s.source, model: s.model })),
]);
```

约束：

- 槽位对应 provider 未就绪（无密钥）时跳过（沿用目录条目 `ready` 判定，按 source 反查）。
- 保留 60 秒节流与 `auto-probe` busy 互斥；手动"测试连接 / 全部测试"不受节流影响（现状保留）。
- 批量探测成功后 `setSnapshot(response.snapshot)` 已会刷新 `slot_health`，无需额外同步。
- 后端 `subagent_provider_test_batch`（`commands.rs:14489-14518`）本身按 `(source, model)` 去重、单批 ≤64，无需改动。

### B2 自动探测结果可见

现状自动探测完全静默，是"没自动测"观感的主因。在面板头部 `<details>“连通性与保存规则”`（410-413 行）旁增加一条常驻状态行：

```
本次进入已自动测试 5 项：4 项连通，1 项失败（失败项可手动重测）。
```

- 数据来自本次自动探测的批量响应；节流窗口内重复进入显示"沿用 X 分钟内的连通结果"。
- 失败不弹错误条（保持现状不打断配置），只计入状态行。

### 验收

- 槽位使用非默认模型、receipt 过期（成功 TTL 30 分钟，`commands.rs:13700-13701`）后重进面板：无需手动点击，槽位恢复 connected 并可保存。
- 一分钟内反复进出面板不重复发探测请求（节流回归）。
- 手动"全部测试"、单条"测试连接"、CAS 保存行为不变。

---

## C. Provider 模型能力标注（文本 / 多模态）

现状：模型列表只是 `string[]`，图片能力靠前端模型名启发式（`components/room/model-capabilities.ts:217-273`），后端 `Capabilities.supports_vision`（`vendor/agent-contracts/crates/agent-contract/src/provider.rs:252`）只写不读。本节建立**目录元数据 → 前端统一出口**的能力标注。

### C1 预设目录结构升级

`src-tauri/src/provider_catalog.rs`：

```rust
#[derive(Debug, Clone, Copy, Serialize)]
pub struct PresetModel {
    pub id: &'static str,
    /// 是否接受图片输入。预设目录是人工核对的一手信息，视为权威。
    pub vision: bool,
}

pub struct Preset {
    // ...
    /// 候选模型（含能力标注）。默认模型 `model` 也应出现在此列表中。
    pub models: &'static [PresetModel],
}
```

- 机械更新全部 ~30 条预设（`PRESETS`，224 行起）：Anthropic/OpenAI/Gemini/GLM-4V/Qwen-VL 等标 `vision: true`；DeepSeek、代码模型等标 `vision: false`。
  - 实施核对（2026-08-22，官方文档复核）：DeepSeek 的 V4 正式模型（flash/pro）**均为纯文本**，
    官方唯一支持图片输入的是实验模型 `deepseek-v4-flash-vision-exp`（已入目录，`vision: true`）；
    同时补入 GLM-4.6V 系列（zhipu/zai）与 Qwen-VL 系列（bailian/dashscope）作为多模态候选。
- 一致性测试：`model` ∈ `models`；`vision_models ⊆ models`（改结构后即自查）；JSON 序列化快照测试。

### C2 同步模型与三态合并

`provider_models.rs::discover_models` 拉取的远端清单**没有** modality 信息，统一映射为 `vision: null`（未知）。前端能力判定采用三态优先级（`model-capabilities.ts` 新增统一出口）：

```ts
export type ImageCapability = "supported" | "unsupported" | "unknown";

// 1) 预设目录标注（provider + model 精确命中）→ 权威
// 2) Codex CLI 目录 supports_images（types.ts:1369-1374）→ 权威
// 3) 现有名称启发式 imageCapabilityFor → 兜底
// 4) 其余 → unknown
export function resolveImageCapability(model: string, opts?: { providerId?: string }): ImageCapability;
export function modalityLabel(cap: ImageCapability): "多模态" | "文本" | "能力未知";
```

现有调用点（`Attachments.tsx`、`ModelSwitcher` 等）改走 `resolveImageCapability`，启发式降级为内部兜底。

### C3 前端类型与 UI 呈现

- `types.ts` `ProviderPreset.models: string[]` → `{ id: string; vision: boolean | null }[]`；`lib/provider.ts` 聚合逻辑同步（远端模型 `vision: null`、localStorage 历史模型 `vision: null`，预设命中时回填权威值）。
- 徽标呈现（P1 范围）：
  - 设置页 provider 编辑表单的模型下拉：选项后缀 `[多模态]` / `[文本]`，未知不加标；
  - 图片理解配置的二级下拉（见 D；实施修订见 D3）列出该服务全部模型并逐项带徽标，未知能力的模型不加标但可选择。
- `SubagentProvidersPanel`、`ModelSwitcher` 的徽标列为 P2（数据就绪后顺手加）。

### 验收

- 目录快照测试通过；前端下拉徽标与预设标注一致。
- `resolveImageCapability` 单测：预设命中 > Codex 目录 > 启发式 > unknown 的优先级用例。
- 现有附件降级行为（unsupported→OCR、unknown→照发）在引入元数据后判定更准、无回归。

---

## D. 图片理解引擎配置（OCR / 视觉模型，二选一）

### D1 现状与设计目标

现状图片处理链（供实施对照）：

- 用户贴图/选图 → `Attachments.tsx`（base64，`types.ts:20-27` `AttachmentInput`）→ `cmd_agent_send` → `validate_attachments`（`commands.rs:6145`，native OCR 仅 png/jpeg，6199-6206）→ `apply_native_ocr`（`commands.rs:6342-6421`）把图片替换为 `[OCR · 原图片：xxx]\n文本` 文本附件，原图落盘仅作 UI 预览；
- 是否 OCR 由前端启发式决定（模型 unsupported 且平台有 OCR），模型 supported 时直发 base64，Linux 无 OCR 且不支持的图被**静默剔除**；
- 平台 OCR：Windows WinRT（`windows_ocr.rs`）、macOS Vision（`mac_ocr.rs`）、Linux 无。

设计目标：把"图片怎么被理解"从隐式降级变成**显式配置**：

| 引擎 | 行为 | 默认 |
| --- | --- | --- |
| 本机 OCR | png/jpeg 一律走系统 OCR 转文本注入上下文（即现有降级路径升级为主路径） | ✅ |
| 视觉模型 | 图片发给用户指定的多模态模型，由其生成结构化描述文本注入主对话上下文（"图片理解代理"） | |

### D2 配置模型

`vendor/agent-contracts/crates/agent-config/src/lib.rs`（注意这是子模块，先提交子模块再提交主仓库）：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ImageUnderstandingEngine {
    #[default]
    Ocr,
    Model,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImageUnderstandingConfig {
    pub engine: ImageUnderstandingEngine,
    /// engine == Model 时必填：config.providers 的 key。
    pub model_provider: Option<String>,
    /// engine == Model 时必填：该 provider 下的模型 id。
    pub model: Option<String>,
}

// Config 增加（serde default 保证旧配置可反序列化）：
#[serde(default)]
pub image_understanding: ImageUnderstandingConfig,
```

- `Config::validate()`（lib.rs 699-704 附近）追加：`engine == Model` 时 `model_provider` 必须存在于 `providers` 且 `model` 非空。
- `settings_set` 是通用点分路径写入（`commands.rs:21802-21895`），`image_understanding.*` 无需白名单改动；发送时（见 D4）再做权威校验并返回可读错误。
- 前端 `types.ts` 增加 `ImageUnderstandingConfig`，挂到 `AppConfig.image_understanding?`。

### D3 设置 UI

新增设置区块 `ImageUnderstandingSection`，放在**「模型服务」面板**（providers pane，`ProviderSection` 之后；该主题强依赖 provider 选择）：

```
图片理解
  引擎（radiogroup，同现有主题选择的 button group 模式）：
    (•) 本机 OCR（默认）   离线、免费；仅提取图片中的文字（png/jpeg）
    ( ) 视觉模型           由指定的多模态模型理解整张图片并生成描述
        服务：[下拉：仅列出「已配置 && 就绪 && 存在 vision 模型」的服务]
        模型：[二级下拉：该服务下 vision === true 的模型（C3 元数据）]
        hint：切换后对新发送的图片生效；原图仅本地留存预览。
  指引手册 →（新增 guide 条目，见 E2）
```

- 服务下拉数据（实施修订 2026-08-22）：列出**全部**已配置服务——能力标注下沉到模型级展示，
  服务级不做多模态过滤（否则纯文本预设服务会在下拉中整体消失，用户误以为配置丢失）；
  未就绪（缺密钥）的服务条目禁用并标注原因。
- 模型下拉列出该服务全部候选（预设 + 配置中的当前模型），逐项带能力徽标
  `[多模态]` / `[文本]`（未知不加标）；切换服务时自动预选第一个 `[多模态]` 模型，
  没有则预选第一个候选。选择 `[文本]` 模型的后果由降级链兜底（PNG/JPEG 自动降级
  本机 OCR，其余返回错误）。
- 保存走现有 `settingsSet("image_understanding.engine" | ".model_provider" | ".model")`，切换回 OCR 时可清空 model 字段。

### D4 后端执行链

`commands.rs` 在 `agent_send_with_mode_and_attachments`（6703 行）里把 `apply_native_ocr` 前置分派为新的 `apply_image_understanding`：

```rust
async fn apply_image_understanding(
    state: &CommandState, task_id: &str, attachments: &mut [ValidatedAttachment],
) -> Result<(), String>
```

- **engine = Ocr**：对全部 png/jpeg 图片执行系统 OCR（不再依赖"主模型不支持"这一条件——用户显式选了 OCR）；gif/webp/pdf 维持现有能力路径。平台无 OCR（Linux）时按现状报错文案，但补充"可切换到视觉模型引擎"。
- **engine = Model**：对每张图片（png/jpeg/gif/webp）调用配置的视觉模型一次：
  - 用 `agent_llm::create_provider` 构造（复用 `build_provider_config` 的协议分派），请求体 = 单条 user 消息（图片 base64 + 固定提示词："描述这张图片的内容、界面元素与文字，输出结构化中文描述"），`max_tokens` 钳制（建议 2048），超时 60s（新常量，对齐 `SUBAGENT_PROVIDER_PROBE_TIMEOUT` 的模式）；
  - 成功：附件替换为文本附件 `[视觉模型 {provider}/{model} · 原图片：{name}]\n{描述}`，原图走 `persist_attachment_preview` 落盘供 UI 回显（复用现有 OCR 原图回读链 `attach_image_previews`，10980-11025 行）；
  - 失败（超时/4xx/余额）：png/jpeg 且平台有 OCR → 自动降级 OCR 并在文本头标注"视觉模型失败，已降级本机 OCR"；否则返回明确错误（列明图片名与原因），不静默剔除。
- 沿用现有预算约束（4 张/回合等，`commands.rs:5923-5931`），视觉模型路径复用同一预算常量，避免一次贴 N 图打爆计费。

**前端配合**（`Attachments.tsx`）：

- `sendableAttachmentInputs`（423-437 行）按引擎改写标记：OCR 引擎下 png/jpeg 全部 `nativeOcr: true`；模型引擎下图片附件不再因"主模型 unsupported"被剔除（理解工作由视觉模型承担），chip 文案分别显示"本机 OCR → 文本" / "视觉模型 {model} → 文本"。
- `attachmentUsesNativeOcr`（79-88 行）判定输入增加引擎参数；`validate_attachments` 对 `native_ocr` 的 png/jpeg 白名单校验维持不变。

### D5 行为变化与兼容性（需在 CHANGELOG 标注）

- 默认 `engine = Ocr` 意味着：升级后**原本直发给 vision 主模型（Claude/GPT 等）的图片也会先走本机 OCR**。这是需求明确要求的默认值，但属于可见行为变化：
  - 影响面：依赖主模型直接读图（截图 UI 细节、图表非文字信息）的用户体验下降——OCR 只提取文字。
  - 缓解：设置项放在「模型服务」面板首屏可见区块 + 指引手册说明取舍；CHANGELOG 显著标注。
  - 预留扩展（P2，本方案不实施）：增加第三档 `auto`（主模型支持则直发，否则按引擎降级），若反馈强烈再评估。
- 视觉模型引擎是"描述注入"而非"原图直发"：主模型看到的是描述文本，细粒度识图能力受描述 prompt 上限约束；手册中说明。

### 验收

- 全新安装/旧配置升级：默认 OCR，不产生配置迁移错误（serde default）。
- OCR 引擎：png/jpeg 在 vision 主模型下也走 OCR；Linux 报错含切换指引。
- 模型引擎：图片经视觉模型转为描述文本，主模型上下文无 base64；视觉模型失败时降级链正确；`image_understanding.model_provider` 指向已删除服务时发送返回可读错误。
- 回归：无图片附件的发送、Codex 引擎路径（`prepare_codex_attachments` 不受影响）、附件预览回读。

---

## E. 设置整体交互与 UI 优化、指引手册

### E1 "默认服务"语义显性化（P0）

- 按钮改名：`用于新对话` → `设为默认服务`，title/hint："设为默认后，新对话将使用这项服务；已开始的对话不受影响"。
- `保存并用于新对话` → `保存并设为默认`。
- 列表行徽标 `正在使用` → `默认`（`SettingsScene.tsx:1015`），并给行内增加轻量"设为默认"快捷动作（hover 显示，替代必须先点进编辑表单才能切换的现状；仍受 `providerStatus[...].ready` 约束）。
- `selectProvider` 成功提示同步："已设为默认，新对话将使用这项服务。"

### E2 指引手册扩展（P0 随各主题交付）

`GuideSheet.tsx` 的 `GUIDE_ENTRIES` 目前只有 `plan-suggestion`。新增条目并挂"指引手册 →"按钮：

| GuideId | 挂载位置 | 内容要点 |
| --- | --- | --- |
| `providers` | 模型服务区块标题行 | 服务 vs 默认服务、协议/线路选择、密钥存储位置、同步模型、多模态标注含义 |
| `subagents-pool` | 子代理面板头部 | 候选来源、权重必须合计 100%、连通回执与 TTL、all-or-nothing 语义、自动测试说明（呼应 B2） |
| `image-understanding` | 图片理解区块 | 两引擎取舍、默认 OCR 的行为变化说明、失败降级链 |
| `plan-suggestion`（扩充） | 现有卡片 | 改为"配置 DeepSeek 即可用"、补充"无需设为默认服务"与急停开关说明（A2） |

菜单栏 `MenuBar.tsx` 同步增加"设置与模型服务指引"入口（现有 150 行模式）。

### E3 InfoTip 轻量提示组件（P2）

现状只有原生 `title=`（无障碍与触控差）与行内 `hint`。新增 `components/ui/InfoTip.tsx`：`?` 图标 + focus/hover 展开说明（复用 `AnchoredSurface`），优先用于：规划建议卡、权重输入、图片理解引擎、协议选择。替换 8 处关键 `title=`（SettingsScene 1116/1147/1158/1246/1268/1385/2771/2830 行）。

### E4 设置搜索（P2）

设置页 7 个面板已稳定，增加顶部搜索框：按区块标题 + 字段 label + 手册关键词过滤，命中跨面板时列出可点击深链（复用 `setSettingsPane` + `scrollIntoView` + `flash-target` 既有机制，`app.ts:531-538`、`SettingsScene.tsx:502-521`）。

### E5 其他小项（P2，按需）

- 通用设置保存反馈统一：`settingsSet` 成功后统一轻量 toast（部分面板已有 notice，去重）。
- 「诊断」面板的请求审计区块与规划建议卡的跨页跳转已有 `GuideAction` 机制，图片理解/子代理手册如需跳转沿用同一模式。
- Onboarding 第 3 步（Provider）补一句默认服务说明（`OnboardingCampaign.tsx:299-324`）。

---

## 实施排期与依赖

| 阶段 | 内容 | 依赖 | 预估 |
| --- | --- | --- | --- |
| P0-1 | B 子代理自动探测合并槽位 + 状态行 | 无 | 0.5 天 |
| P0-2 | A 证据门移除 + 判定/文案/手册（含测试改写） | 无 | 1–1.5 天 |
| P0-3 | E1 默认服务改名与快捷动作 | 无 | 0.5 天 |
| P1-1 | C 模型能力标注（目录结构 + 统一出口 + 下拉徽标） | 无 | 1.5–2 天 |
| P1-2 | D 图片理解引擎（schema → UI → 后端分派 → 降级链） | C | 3–5 天 |
| P2 | E2 收尾、E3 InfoTip、E4 搜索、E5 小项、D-auto 评估 | 各自 P1 | 按需 |

提交约定：`agent-config` 与 `agent-contract` 位于 `vendor/agent-contracts` 子模块，schema 改动（D2）先在子模块提交、升版本，再在主仓库跟进引用（现有 `git-commit` skill 流程即要求子模块先行）。

## 测试清单

- **cargo（src-tauri）**：
  - `settings_set` 往返 `image_understanding.*`（对齐 26938 行既有测试模式）；
  - `planning_status` 的 `deepseek_configured` 两分支；证据门移除回归（非白名单 DeepSeek 模型放行、急停判负、无 manifest 可启用、状态视图无证据字段）；
  - `apply_image_understanding` 分派单测（OCR 引擎全量 png/jpeg、模型引擎成功/失败/降级；provider 调用以现有探测测试的可注入 seam 为准）；
  - 目录快照与一致性测试（C1）。
- **前端（node test，对齐 `frontend/scripts/*.test.mjs` 模式）**：
  - `resolveImageCapability` 优先级矩阵；
  - `Attachments` 附件标记在两引擎下的输出；
  - `SubagentProvidersPanel` 自动探测请求合并与节流（纯函数抽出后单测）。
- **手工回归**：三平台贴图（Windows/macOS OCR、Linux 报错）、Codex 引擎带图、子代理保存全流程、规划建议卡三态文案。

## 风险与开放问题

1. **默认 OCR 的行为变化**（D5）是最主要的用户可感风险，靠文案 + 手册 + CHANGELOG 缓解；若灰度反馈差，P2 增加 `auto` 档。
2. 视觉模型描述质量依赖提示词，首版用固定提示词，不做可编辑（避免与 `agent_prompts` 体系耦合过多）；反馈需要时再纳入。
3. 目录结构变更（C1）波及 `cmd_provider_catalog` 消费方与前端聚合，需一次性改齐并靠快照测试锁住。
4. 子模块版本升级节奏需与主仓库发布对齐（见 releasing.md）。
5. 证据门移除后无预注册误弹率背书：兜底依赖运行时防打扰机制（每任务至多一次、branch 拒绝后安静）与 `R_CODE_PLANNING_EMERGENCY_OFF` 急停；建议保留的 `eval/plan-eval/` 工具做事后质量回归，用户反馈异常时先急停再修复。`plan-mode-dual-track-gate.md` 中与证据门相关的章节需同步标注"已废弃，见本方案 A3"。

## 附录：涉及文件索引

| 主题 | 文件 |
| --- | --- |
| 规划建议卡 | `src-tauri/frontend/src/components/scenes/SettingsScene.tsx`（1421-1510）、`src-tauri/src/plan_entry_commands.rs`、`src-tauri/src/plan_policy.rs`、`src-tauri/src/build.rs`、`src-tauri/frontend/src/components/settings/GuideSheet.tsx`、`src-tauri/src/bin/plan_eval.rs`、`eval/plan-eval/`、`scripts/release.test.mjs` |
| 默认服务 | `SettingsScene.tsx`（998-1021、1370-1379）、`src-tauri/frontend/src/lib/ipc.ts`（1112-1119）、`src-tauri/src/commands.rs`（13455-13687）、`src-tauri/frontend/src/lib/provider.ts` |
| 子代理 | `src-tauri/frontend/src/components/scenes/SubagentProvidersPanel.tsx`（106-206、335-362）、`src-tauri/src/commands.rs`（13695-13701、14130-14554）、`src-tauri/src/subagent_providers.rs` |
| 模型能力 | `src-tauri/src/provider_catalog.rs`（160-224 起）、`src-tauri/src/provider_models.rs`、`src-tauri/frontend/src/components/room/model-capabilities.ts`、`src-tauri/frontend/src/lib/provider.ts`、`vendor/agent-contracts/crates/agent-contract/src/provider.rs` |
| 图片理解 | `src-tauri/src/commands.rs`（5923-5931、6145-6223、6310-6449）、`src-tauri/src/windows_ocr.rs`、`src-tauri/src/mac_ocr.rs`、`src-tauri/frontend/src/components/Attachments.tsx`、`vendor/agent-contracts/crates/agent-config/src/lib.rs` |
| 设置框架 | `SettingsScene.tsx`（70-82、491-669）、`src-tauri/frontend/src/lib/types.ts`（1399-1415）、`src-tauri/src/commands.rs`（21802-21895）、`src-tauri/frontend/src/components/settings/GuideSheet.tsx`、`src-tauri/frontend/src/components/onboarding/OnboardingCampaign.tsx` |

---

## 实施状态修订（2026-08，docs/archive/implementation/multimodal-attachments-and-deepseek-plan-anchoring-implementation.md 生效后）

本文以下旧语义已被新实施**取代**，以新合同为准：

1. **视觉模型失败自动降级 OCR 已删除**（本文 §322 行为废止）：用户选择视觉模型引擎后，失败原样返回可操作错误，不再自动换引擎。需要 OCR 时由用户在设置中显式切换。
2. **附件不再以 Base64 持久化**：图片/文件在 staging（`cmd_attachment_stage`）即写入 BlobStore 并返回 `AttachmentRefV1` 引用；会话 JSONL、排队消息与模型投影只保存引用。Base64 仅存在于两个临时边界：WebView staging 的一次性 IPC 载荷与 Provider 请求构造期的物化副本。
3. **图片路由由后端按冻结能力产生**（`model_capabilities::resolve_image_delivery_route`），前端启发式与 `native_ocr` 决策位不再参与判定：主模型目录确认多模态（vision=Confirmed）时原图直发（OCR/helper 调用数为 0）；Unsupported 按显式引擎；Unknown 且未配置引擎时明确阻断。
4. **图片 token 按确定性 profile 核算**（`VisionBudgetProfile`；deepseek-v4-flash-vision-exp 的 1818×1026 回归值为 32,000），Base64 字符数不再参与任何 token 估算。
5. **每轮最大输出可编辑**：Provider 服务端上限只作为上界展示（"自动 65,536 / 服务上限 393,216"），未显式配置时后端采用目录 `recommended_output_tokens`，不再自动采用服务端硬上限，也不再锁死输入框。

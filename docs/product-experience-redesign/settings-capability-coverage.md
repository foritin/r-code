# Settings 功能保全与设计盘点矩阵

<!-- generated_from_settings_capability_baseline: f36468c0efdcbe125dbe3b7a6f946e25a14f878565eaea15b99e652ffc0e9458 -->

> 本文区分两件事：结构化 inventory 对当前 `dev` 能力的分类，以及逐项源码证据、精确行为合同和运行时验证。前者不能替代后三者。
>
> 当前结论：机器基线登记 **127 个 inventory item / CapabilityID**，其中 **111 项 `production_existing`**、**14 项 `new_requirement`**、**2 项 `planned_demo`**。D0 源码语义证明已完成：47/47 个 manifest 文件、52 个 manifest-level locator、111/111 个 production item-level source evidence 和 127/127 份 17 维合同均通过，`source_inventory_proof.status=passed`。这不等于产品已经重构或验收；真实 UI → IPC → Host → persistence 的 `verified_count` 仍为 **0/111**。

## 1. 权威边界

- 权威来源是当前生产 `SettingsScene`、Settings 子面板、类型、frontend IPC、Tauri/Host command、持久化和相关运行代码。HTML 原型、截图和 PRD 只能定义目标，不能证明生产能力存在。
- 2026-08-27 的只读审计固定了 47 个 SourceID、仓库相对路径、角色、严格 UTF-8 文本经 CRLF/CR → LF 规范化后的 SHA-256，以及至少一个非空 locator；未读取真实配置值、用户内容或密钥。文件级 locator 解析成功只证明“该文件与锚点存在”，不证明任一 CapabilityID 的完整行为。
- `production_existing` 每项均把唯一可解析的 item-level locator 绑定到 authority、positive、failure，适用时再绑定 disabled 与 atomicity；当前为 111/111。该证明固定“现有源码实际表达什么”，不替代 M5 运行验收。
- `new_requirement` 与 `planned_demo` 均有 `read_only_source_absence_review`、固定 SourceID 和明确的 `classification_basis`。它们参与 capability mapping、17 维合同、原型目标、正式目标和 trace 校验，但不能计入生产下界；当前合同完成度为 127/127。
- 当前 `verified_count=0` 是刻意保留的诚实状态。只有正式产品完成 UI → IPC → Host → persistence、失败保旧值、重启恢复和迁移往返验证后，才可增加生产验证计数。
- 机器基线：[settings-capability-baseline.json](./settings-capability-baseline.json)
- 二值门禁：[settings_capability_gate.py](./tools/settings_capability_gate.py)
- 生成报告：[settings-capability-gate.json](./settings-capability-gate.json)
- 唯一实施清单：[r-code-experience-redesign-prd.md](./r-code-experience-redesign-prd.md)

## 2. 生产下界与总设计盘点

| 审计组 | 生产下界 | 总 inventory | 生产处置 | 非生产增量 |
| --- | ---: | ---: | --- | --- |
| settings_shell | 3 | 4 | 1 preserve + 2 migrate | 1 new_requirement |
| providers | 15 | 18 | 15 preserve | 3 new_requirement |
| image_understanding | 9 | 9 | 9 preserve | 0 |
| agents_codex | 18 | 19 | 17 preserve + 1 migrate | 1 new_requirement |
| subagents | 10 | 11 | 10 preserve | 1 new_requirement |
| tools_mcp_rtk | 19 | 19 | 19 preserve | 0 |
| browser | 0 | 2 | — | 2 planned_demo |
| permissions | 0 | 1 | — | 1 new_requirement |
| security | 0 | 2 | — | 2 new_requirement |
| knowledge | 19 | 19 | 19 preserve | 0 |
| preferences | 12 | 15 | 7 preserve + 5 migrate | 3 new_requirement |
| lifecycle | 0 | 1 | — | 1 new_requirement |
| diagnostics | 6 | 7 | 6 preserve | 1 new_requirement |
| **合计** | **111** | **127** | **103 preserve + 8 migrate** | **14 new_requirement + 2 planned_demo** |

两套数字用途不同：

- **111 项 `production_existing` 源码语义基线**回答“重构必须保全什么”。它已完成 D0 逐项源码证据与合同证明，但尚未完成产品实现与运行回归。
- **127 项总 inventory**回答“本次设计需要追踪什么”。它包含生产保全、新需求和仅计划演示的能力。
- 当前 mapping、唯一原型锚点、稳定正式目标和 17 维合同均为 127/127；production item-level source evidence 为 111/111。group SourceID、标题、原型锚点或 trace 数量仍不能单独替代逐项证据。

## 3. 分类与处置规则

| Classification | 合法 disposition | 含义 |
| --- | --- | --- |
| production_existing | preserve / migrate / merge / explicitly_retired | 当前生产源码已经存在；必须保全或提供结构化兼容/退役授权 |
| new_requirement | add | 固定源码边界确认生产中不存在，本轮 PRD 要求新增 |
| planned_demo | demo | 原型可演示，但生产 Settings 和 Host 合同尚不存在；必须受 feature flag 与实施门禁约束 |

生产处置合同：

- `preserve`：视觉或组件可变，字段、动作、默认值、值域、权限、错误、副作用和持久化语义不变。
- `migrate`：必须固定旧 route/deep-link/config key/enum/IPC 映射，并证明迁移幂等、可降级、可回滚和 old→new→old→new 往返。
- `merge`：只有多个真实旧入口共享权威状态、且每个旧触发器和恢复路径仍可达时才成立；本基线当前为 0。
- `explicitly_retired`：必须有独立 RequirementRef 与用户批准；当前为 0。
- `add` / `demo`：不能引用兼容合同伪装成旧能力，也不能挤掉 111 项生产下界。

## 4. 源码事实收窄

本轮重新核对源码后，以下能力按真实行为收窄，避免原型反向改写生产事实：

- 摘要算法迁移时，对 `SettingsScene.tsx`、`lib/ipc.ts`、`lib/types.ts`、`main.rs`、`tauri_commands.rs` 的真实内容漂移另做了 `a55b34ce… → a1afe400…` 只读 delta audit（合计 `+327/-12`），没有把它们伪装成换行迁移。新增接线分别落入既有 `SET-PREF-002`、`SET-NOTIF-001`、`SET-UPD-001..005`；通知分类/测试仍为 `SET-NOTIF-002 new_requirement`，Browser 仍为 `SET-BROWSER-001/002 planned_demo`，关闭询问仍为 `SET-LIFE-001 new_requirement`。当前五个 normalized SHA 可接受并与文件精确匹配。
- `SET-PROV-015`：固定的 Settings/IPC/Host 来源没有通用 Provider 测试 command 或回执 UI；生产中只有子代理 Provider 的单项/批量 probe。因此它从错误的生产 `merge` 改为 `new_requirement/add`，目标为 `#provider-test-editor`。
- `SET-CODEX-003`：生产浏览器/设备码登录采用串行 2 秒轮询和 3 分钟 timeout；“取消等待”只清理前端 timer，不会终止已启动的 CLI 登录进程。真正取消底层进程单列为 `SET-CODEX-009` 新需求。
- `SET-SUB-009`：生产整池保存使用 CAS 与 Host 回执复验；revision 冲突后调用 `load(true)` 载入最新 Host snapshot，并会覆盖本地槽位草稿。保留 local/Host 双快照和 discard/reapply/merge 单列为 `SET-SUB-011` 新需求。
- `SET-TOOL-002`：保存 Bash 路径后，Host 立即调用 gateway `update_shell_override`，Windows 同时失效 shell cache；新值从下一次工具调用生效，`apply_mode=immediate`，不是 next run。
- `SET-TOOL-003`：`web_search` / `web_fetch` 是由 Host 工具注册和 Provider 能力派生的只读可用性状态；固定来源中没有独立持久开关，因此类型为 `read_only_status`。
- `SET-IMG-004`：切换到一个 ready 的视觉 Provider 时，`preferredImageModel` 优先选择目录明确标注 `vision=true` 的第一个模型；若没有明确多模态候选，则退到该 Provider 的第一个候选，而不是伪造多模态能力。
- `SET-IMG-008`：视觉模型理解失败不会自动降级 OCR。Host 汇总失败后不发送任何消息，返回可操作错误，并要求用户在 Settings 中显式切换 OCR；目录确认多模态的主模型仍通过 `main_model_handles_images_natively` 原图直发。

## 5. 全部非生产 inventory 的 absence 依据

下表完整列出 16 项非生产能力。SourceID 是基线固定的审计边界；“未发现”只对该固定边界负责，不把原型、截图或仓库其他实验代码当作生产 Settings 接线证据。

| CapabilityID | 分类 | 固定 SourceID | pinned-source absence 结论 |
| --- | --- | --- | --- |
| `SET-SHELL-004` | new_requirement | `SRC-SETTINGS-SCENE`, `SRC-APP-STORE`, `SRC-FRONTEND-IPC`, `SRC-HOST-COMMANDS`, `SRC-SETTINGS-STORE` | 只有 pane-local loading/error/dirty；没有共享 lifecycle reducer、last-good authority 或 discard/reapply/merge 冲突协议。 |
| `SET-PROV-015` | new_requirement | `SRC-SETTINGS-SCENE`, `SRC-FRONTEND-IPC`, `SRC-TAURI-COMMANDS`, `SRC-HOST-COMMANDS`, `SRC-SUBAGENT-PROVIDERS` | 没有通用 saved-provider 精确测试 command/receipt UI；仅有子代理 Provider probe。 |
| `SET-PROV-017` | new_requirement | `SRC-SETTINGS-SCENE`, `SRC-FRONTEND-IPC`, `SRC-TAURI-COMMANDS`, `SRC-HOST-COMMANDS`, `SRC-SETTINGS-STORE`, `SRC-SUBAGENT-PROVIDERS` | 自动 probe 只在打开子代理面板时触发；没有 Shell-ready Host scheduler、全局 opt-out、共享队列或通用 readiness command。 |
| `SET-PROV-018` | new_requirement | `SRC-SETTINGS-SCENE`, `SRC-PROVIDER-LIB`, `SRC-FRONTEND-TYPES`, `SRC-FRONTEND-IPC`, `SRC-HOST-COMMANDS`, `SRC-SUBAGENT-PROVIDERS` | 有 configured 状态和子代理回执，但没有供 Shell、Composer、selector、Settings 共用的全局 health projection。 |
| `SET-CODEX-009` | new_requirement | `SRC-CODEX-GATE`, `SRC-CODEX-LOGIN-WATCHER`, `SRC-FRONTEND-IPC`, `SRC-TAURI-COMMANDS`, `SRC-HOST-COMMANDS` | UI 明确只取消等待；IPC 没有终止底层登录进程的 command。 |
| `SET-SUB-011` | new_requirement | `SRC-SUBAGENT-PANEL`, `SRC-FRONTEND-IPC`, `SRC-TAURI-COMMANDS`, `SRC-HOST-COMMANDS`, `SRC-SUBAGENT-PROVIDERS` | 冲突处理会重新加载并替换本地 slots；没有 recovery buffer 或显式 discard/reapply/merge。 |
| `SET-BROWSER-001` | planned_demo | `SRC-SETTINGS-SCENE`, `SRC-EXECUTION-ENV-CARD`, `SRC-FRONTEND-TYPES`, `SRC-FRONTEND-IPC`, `SRC-TAURI-COMMANDS`, `SRC-TAURI-MAIN` | 固定 Settings 来源没有 Browser runtime manifest/install/verify/repair UI 或已注册 command；仓库其他 feature-flag contract 不能证明此卡片已接线。 |
| `SET-BROWSER-002` | planned_demo | `SRC-SETTINGS-SCENE`, `SRC-FRONTEND-TYPES`, `SRC-FRONTEND-IPC`, `SRC-TAURI-COMMANDS`, `SRC-HOST-COMMANDS` | 没有 task+origin+browse/interact grant registry、授权 UI 或 revoke command。 |
| `SET-PERM-001` | new_requirement | `SRC-SETTINGS-SCENE`, `SRC-FRONTEND-TYPES`, `SRC-FRONTEND-IPC`, `SRC-HOST-COMMANDS`, `SRC-CODEX-PERMISSIONS` | 有 Codex/global permission mode，但没有 same-root read 风险分类、grouped request DTO 或 Run-scoped grant registry。 |
| `SET-SEC-001` | new_requirement | `SRC-SETTINGS-SCENE`, `SRC-SETTINGS-STORE`, `SRC-MCP-SETTINGS`, `SRC-SUPPORT-BUNDLE`, `SRC-TAURI-MAIN` | 安全保障分散存在，但没有统一只读 security projection 或权威状态 adapter。 |
| `SET-SEC-002` | new_requirement | `SRC-SETTINGS-SCENE`, `SRC-SETTINGS-STORE`, `SRC-SUPPORT-BUNDLE`, `SRC-TAURI-MAIN` | 没有 local-cache preview DTO、限定 Provider receipt/临时诊断的 cleanup command 或 Settings action。 |
| `SET-PREF-003` | new_requirement | `SRC-SETTINGS-SCENE`, `SRC-APP-STORE` | 只有单一视图的内存 `deckDensity`；没有全应用 comfortable/compact 设置和持久 authority。 |
| `SET-PREF-004` | new_requirement | `SRC-SETTINGS-SCENE`, `SRC-APP-STORE`, `SRC-COMPANION-STORE` | 有 Companion motion 偏好和 CSS media 行为，但没有全应用持久化 reduced-motion 控件。 |
| `SET-NOTIF-002` | new_requirement | `SRC-SETTINGS-SCENE`, `SRC-NOTIFICATION-SETTINGS`, `SRC-NATIVE-NOTIFICATION`, `SRC-APP-STORE` | 只有真实 OS 权限状态/请求；没有审批、失败、Review Ready 分类偏好或用户触发的应用内测试。 |
| `SET-LIFE-001` | new_requirement | `SRC-SETTINGS-SCENE`, `SRC-APP-STORE`, `SRC-FRONTEND-IPC`, `SRC-TAURI-COMMANDS`, `SRC-TAURI-MAIN` | `main.rs` 在 Windows/macOS 直接隐藏、Linux 直接退出；没有 ask/hide/quit 偏好、CloseIntent IPC、单实例弹窗、remember 或 Settings reset。 |
| `SET-DIAG-007` | new_requirement | `SRC-SETTINGS-SCENE`, `SRC-FRONTEND-TYPES`, `SRC-FRONTEND-IPC`, `SRC-HOST-COMMANDS`, `SRC-SUPPORT-BUNDLE`, `SRC-LOG-BUFFER` | 有日志和支持包 preview/export，但没有 Provider/Browser/MCP 聚合 self-check command、DTO 或 Settings action。 |

## 6. `production_existing` 源码语义摘要

以下 111 项已通过 item-level source evidence 与 17 维合同门禁；它们仍未通过 M5 真实产品运行验证。

- **Settings 壳层（3）**：多关键词 AND 搜索、无结果、跨页定位、控件聚焦和旧 Codex 深链迁移。
- **Providers（15）**：草稿/dirty、默认项、删除、凭据、协议/线路、模型同步、联网能力、失败恢复；通用测试不在生产计数中。
- **图片理解（9）**：确认多模态的主模型原图直发、OCR/视觉显式二选一、悬空 Provider、能力三态；视觉模型失败整批不发送且不自动降级 OCR。
- **Agent / Codex（18）**：四态委派、质量复核、Plan 双开关、十项运行护栏、Codex setup/login/config/model/permission；底层登录取消不在生产计数中。
- **子代理（10）**：exact source+model receipt、部分失败、最多三槽/权重 100%、Prompt、revision CAS 保存；冲突保留双草稿不在生产计数中。
- **Tools / MCP / RTK（19）**：Shell 三态与立即 gateway 更新、RTK 来源/回滚、MCP 凭据、exact launch token、HTTPS、市场和两步删除；内置 Web 工具只读派生状态。
- **Knowledge（19）**：Scope、Memory 审批/任务/版本/清空、Prompt append/override、Skill 继承/同步。
- **Preferences（12）**：主题/语言、Companion revision/Host 回滚、通知真实权限状态、Updater 全失败恢复；密度、全局 reduced motion、通知分类属于新需求。
- **Diagnostics（6）**：新会话审计、实时日志/过滤/跟随/保留、支持包 preview→选择目录→导出；聚合 self-check 属于新需求。

## 7. 证据链与二值门禁

每个 inventory item 都必须完成以下链路：

    47 个 UTF-8 文件的换行规范化 SHA-256 + manifest-level locator
      → production item 的可解析 source_evidence（authority / positive / failure / disabled / atomicity）
        或固定边界的 absence review
      → classification + disposition
      → 17 维精确合同（含 source、positive、disabled、operation failure、apply、IPC、permission、visibility、side effect 等）
      → 唯一 CapabilityID
      → 唯一 prototype anchor
      → 稳定 planned product target
      → RequirementRef / TaskID / AssertionID / Profile

当前可证明状态（以 `settings-capability-gate.json` 为机器权威）：

| 指标 | 当前值 | 结论 |
| --- | ---: | --- |
| manifest 文件 / 已解析文件 | 47 / 47 | 文件级快照通过 |
| manifest-level locator | 52 | 全部可解析；不等于逐项能力证据 |
| inventory / capability mapping / prototype target / planned target | 127 / 127 / 127 / 127 | 结构映射完整 |
| production classification / mapped | 111 / 111 | 分类映射完整 |
| production item-level source evidence | 111 / 111 | D0 源码角色与 locator 完整 |
| 17 维合同完成度 | 127 / 127 | source/default/value/persistence/apply/IPC/permission/visibility/side effect/positive/failure/disabled 均物化 |
| `source_inventory_proof.status` | passed | D0 源码语义证明通过 |
| `verified_count` | 0 / 111 | 尚未执行真实产品链路 |

源码与合同缺口数组当前均为空，baseline、coverage、freeze 与 live Settings report 的摘要绑定已经同步，整体门禁为 `passed`。这只证明 D0 文档与源码语义链闭合，不会把 `verified_count` 从 0 提前标绿。

D0 通过条件包括 127/127 映射、111/111 production locator/role、127/127 份 17 维合同、空缺口数组和 `source_inventory_proof.status == passed`。实现阶段另要求 `verified_count == baseline_count`；截图或 HTML mock 不能提前标绿。

## 8. 运行方式

在仓库根目录运行：

    & 'D:\software\rtk\rtk.exe' proxy python.exe docs/product-experience-redesign/tools/settings_capability_gate.py --check
    & 'D:\software\rtk\rtk.exe' proxy python.exe docs/product-experience-redesign/tools/settings_capability_gate.py --update-report
    & 'D:\software\rtk\rtk.exe' proxy python.exe docs/product-experience-redesign/tools/capture_states.py
    & 'D:\software\rtk\rtk.exe' proxy python.exe docs/product-experience-redesign/tools/worklist_gate.py --check --update-freeze

`--check` 完全只读，供 CI 与审阅使用。`--update-report` 仅在有意刷新 Coverage 绑定和 JSON 报告时单独运行；即使门禁诚实失败，它仍会写入最新失败报告并以非零码退出。两个模式互斥，脚本不接受无参数调用。

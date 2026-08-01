# R-Code 首次启动导览：后端内容依据

这份表只记录第三版原型中可见、可验证的产品事实。营销数字、虚构进度、虚构服务状态均未进入界面。

| 原型内容 | 后端 / 当前前端依据 | 设计处理 |
| --- | --- | --- |
| 会话保存 `agent_engine` 与 `provider_name` | [`commands.rs`](../../../src-tauri/src/commands.rs) 中 `task_create_with_agent` 将二者写入 `Task`；[`types.ts`](../../../src-tauri/frontend/src/lib/types.ts) 定义 `TaskAgentEngine = "r_code" \| "codex"` | 第一屏画成一条持久化执行总线，不使用抽象“AI 能力”图标 |
| 已有会话不会被后续全局默认静默改写 | [`commands.rs`](../../../src-tauri/src/commands.rs) 的 Provider 解析明确只为无绑定的旧会话回退全局默认，并有 session-scoped 测试 | 第一屏直接显示 `GLOBAL DEFAULT ≠ EXISTING SESSION` |
| 运行中不能切换 Agent、Provider 或模型 | [`commands.rs`](../../../src-tauri/src/commands.rs) 的 `task_set_provider`、`task_set_agent_engine`、`task_set_model` 共享运行态约束 | 第一屏显示 `ACTIVE RUN GUARD`，不把它写成含糊提示 |
| R-Code 使用自定义 Provider，支持宿主路由与质量复核 | [`HomeScene.tsx`](../../../src-tauri/frontend/src/components/scenes/HomeScene.tsx) 的主 Agent 菜单文案 | 第二屏作为 R-Code 执行路径的原文说明 |
| Codex CLI 使用本机登录，需完成协作配置并附加工作区 | [`HomeScene.tsx`](../../../src-tauri/frontend/src/components/scenes/HomeScene.tsx)、[`AgentEngineSwitcher.tsx`](../../../src-tauri/frontend/src/components/room/AgentEngineSwitcher.tsx)、[`commands.rs`](../../../src-tauri/src/commands.rs) | 第二屏把 `integration_ready`、登录归属、工作区前置条件拆成三个真实 gate |
| R-Code 无工作区仍可纯聊天；本地工具不可用 | [`HomeScene.tsx`](../../../src-tauri/frontend/src/components/scenes/HomeScene.tsx) 与 [`commands.rs`](../../../src-tauri/src/commands.rs) 的 `attached_task_workspace_root` | 第一、四屏同时说明“可以聊天”和“本地工具不可用”，避免把两件事混为一谈 |
| DeepSeek 默认 `deepseek-v4-flash`、`openai_chat`、`https://api.deepseek.com`，上下文 1,000,000、输出 393,216 | [`provider_catalog.rs`](../../../src-tauri/src/provider_catalog.rs) 的 `deepseek` 预设及 [`commands.rs`](../../../src-tauri/src/commands.rs) 测试 | 第三屏按当前目录真实值展示，旧稿默认值已纠正 |
| OpenAI 默认 `gpt-5.6-sol`、Responses、`https://api.openai.com/v1`，上下文 1,050,000 | [`provider_catalog.rs`](../../../src-tauri/src/provider_catalog.rs) 的 `openai` 预设 | 第三屏切换 OpenAI 时同步更新地址、协议、模型和说明 |
| Anthropic 默认 `claude-sonnet-5`、Messages、`https://api.anthropic.com`，认证为 X-API-Key | [`provider_catalog.rs`](../../../src-tauri/src/provider_catalog.rs) 的 `anthropic` 预设 | 第三屏切换 Anthropic 时同步更新全部参数，不复用 Bearer 文案 |
| API key 先写 OS keychain，再写剥离密钥的 TOML | [`settings.rs`](../../../src-tauri/src/settings.rs) 的 `save_global`；[`commands.rs`](../../../src-tauri/src/commands.rs) 的 `settings_save_provider` | 第三屏把原子保存顺序画成 01→02→03 的写入流水线 |
| WebView 不接收密钥正文，只收到 `ready`、`source` 和实际协议 | [`commands.rs`](../../../src-tauri/src/commands.rs) 的 `settings_get` 会清空 `api_key` 并返回 `provider_status` | 第三屏明确显示 `source=keychain` 的状态语义 |
| 工作区必须是已打开、可规范化且存在的目录，路径继续受 `PathGuard` 限制 | [`commands.rs`](../../../src-tauri/src/commands.rs) 的 `workspace_root`、`workspace_open` 与 `resolve_workspace_path` | 第四屏用 root / outside-root 的边界图对应真实安全模型 |
| 权限模式按项目持久化；权限只影响 Agent 自动工具调用，不改变工作区边界 | [`ProjectAccessSelector.tsx`](../../../src-tauri/frontend/src/components/ProjectAccessSelector.tsx)、[`commands.rs`](../../../src-tauri/src/commands.rs) 的持久化测试 | 第四屏标题明确区分“硬边界”和“审批频率” |
| `请求批准`：R0 自动，R1–R3 询问，R4 拒绝；`替我审批`：R0/R1 自动，R2/R3 询问，R4 拒绝；`完全访问权限`：R0–R3 自动，R4 拒绝 | [`permission.rs`](../../../crates/r-code-gateway/src/permission.rs) 的 `requires_approval` 与 R4 前置拒绝 | 第四屏以 3×5 决策矩阵呈现，不用模糊的“安全 / 不安全”描述 |

## 原型边界

- `D:\Projects\my-app` 明确标为“示例路径”，不是读取用户机器得到的伪状态。
- “保存密钥”“创建会话”仅演示反馈与流程；HTML 原型不会写配置、凭据库或数据库。
- 导览不自动轮播；支持按钮、底部端口、方向键和鼠标水平拖动。
- 顶栏本身可拖动，用于表达最终桌面悬浮层应具备的移动能力。

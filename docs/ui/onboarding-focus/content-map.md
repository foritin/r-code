# Focus 版内容取舍

这一版不是减少后端准确性，而是把信息改成渐进披露：每页只让用户完成一个决定，其余事实留在设置页、帮助说明或操作反馈中。

| 页面 | 主任务 | 保留在主画面的后端事实 | 主动画面主动移除的内容 |
| --- | --- | --- | --- |
| 欢迎 | 理解设置范围 | Agent、Provider、工作区选择会随新会话保存 | 执行总线、字段名、运行态 guard 图 |
| 主 Agent | 选择执行路径 | R-Code 使用自定义 Provider且可纯聊天；Codex CLI 使用本机登录并要求工作区；运行中不可切换 | integration gate 的逐字段状态 |
| Provider | 完成模型连接 | DeepSeek 默认 `deepseek-v4-flash` / `openai_chat` / 官方地址；密钥只进入系统凭据库 | 上下文窗口、输出上限、完整模型候选、原子写入时序图 |
| 工作区与权限 | 决定本地范围和审批方式 | 本地路径始终在工作区内；权限按项目保存；三种权限的准确一句话；R4 始终拒绝 | R0–R4 的 3×5 矩阵 |
| 确认 | 创建会话 | Agent、Provider、模型、工作区和权限的会话摘要；全局默认不静默改写已有会话 | 技术字段名与后端对象结构 |

## 真实来源

- 主 Agent 与创建条件：[`HomeScene.tsx`](../../../src-tauri/frontend/src/components/scenes/HomeScene.tsx)、[`AgentEngineSwitcher.tsx`](../../../src-tauri/frontend/src/components/room/AgentEngineSwitcher.tsx)、[`commands.rs`](../../../src-tauri/src/commands.rs)
- Provider 默认值：[`provider_catalog.rs`](../../../src-tauri/src/provider_catalog.rs)
- 凭据处理：[`settings.rs`](../../../src-tauri/src/settings.rs)、[`commands.rs`](../../../src-tauri/src/commands.rs)
- 工作区边界：[`commands.rs`](../../../src-tauri/src/commands.rs) 中的 `workspace_root` / `PathGuard` 路径
- 权限文案与策略：[`ProjectAccessSelector.tsx`](../../../src-tauri/frontend/src/components/ProjectAccessSelector.tsx)、[`permission.rs`](../../../crates/r-code-gateway/src/permission.rs)

## 原型边界

- `D:\Projects\my-app` 明确标为示例，不是读取用户本机得到的状态。
- 访问密钥输入不会写入文件、localStorage、系统凭据库或网络；“保存”仅改变原型状态。
- “创建会话”只展示原型说明，不调用后端。
- 使用项目内的高清应用图标与当前产品截图，没有第三方素材或网络依赖。

# 文案依据

- R-Code：自定义 Provider；无工作区可聊天。
- Codex CLI：使用本机登录；需要已附加工作区。
- DeepSeek：`https://api.deepseek.com` / `deepseek-v4-flash`。
- 密钥：只写系统凭据库。
- 工作区：本地路径硬边界。
- 替我审批：中高风险询问；R4 始终拒绝。
- 会话：运行中不可切换主 Agent；已绑定配置不被全局默认静默改写。

来源：`HomeScene.tsx`、`AgentEngineSwitcher.tsx`、`provider_catalog.rs`、`settings.rs`、`commands.rs`、`ProjectAccessSelector.tsx`、`permission.rs`。

原型不会写配置、凭据或数据库；`D:\Projects\my-app` 是示例路径。

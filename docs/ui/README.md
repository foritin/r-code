# R-Code UI — Current

当前 UI 的唯一可执行评审入口是 [完整产品交互 Demo](./demo/index.html)。它直接由 `src-tauri/frontend/` 的正式 React 前端构建，页面、组件、样式和交互不再另写一套。

## 完整 Demo

Demo 覆盖应用外壳、新对话、项目仪表盘、对话列表、活动、任务房间、待处理、项目、项目文件和设置；任务房间内包含工作台启动器、运行与子代理、终端、文件、审核/变更、权限处理与审核窄轨。

浏览器内存后端让创建任务、发送消息、审批、审核、文件保存、终端命令、项目记忆和设置变更都能在页面内演示。数据仅存在当前页面，刷新即复位，不会触碰本机文件或桌面端配置。

常用确定性入口：

- `scene=home|dashboard|conversations|activity|room|inbox|projects|editor|settings`
- `task=queue|review|permission|api|complete`
- `tab=summary|changes|files|terminal|review`
- `settings=providers|preferences|diagnostics|codex`
- `theme=light|dark|system`

完整参数、构建与验证说明见 [demo/README.md](./demo/README.md)。

## 任务房间设计约束

- 主对话与右侧工作台属于同一个任务房间；工具互斥切换，任务上下文、文件草稿和终端选择不丢失。
- 子代理只展示公开生命周期、工具审计和可见结果，不展示私有推理。
- 文件、终端、变更和审核始终受当前任务附加的工作区范围约束。
- 颜色不是唯一状态提示，核心控件具备键盘与无障碍语义，窄屏保持可用。

完整工作台规范见 [SPEC.md](./SPEC.md)，能力边界见 [BACKEND-CONTRACT.md](./BACKEND-CONTRACT.md)，Codex 参照事实见 [PRODUCT-FACTS.md](./PRODUCT-FACTS.md)。

## 工作台视觉参考

`prototypes/workbench/` 下的 12 张图片是本轮外壳、比例和交互语言的参考。当前界面验收以可执行 Demo 与正式前端源码为准，参考图不会被构建脚本自动覆盖。

## 构建与验证

```powershell
cd src-tauri/frontend
npm run build:demo
cd ../../docs/ui/demo
node qa.cjs --smoke
node qa.cjs
```

验证矩阵覆盖完整产品场景、双主题、三档视口和关键跨页面流程，输出位于 `target/ui-demo/`。

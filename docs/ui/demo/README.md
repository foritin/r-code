# R-Code 完整产品交互 Demo

直接打开 `index.html` 即可体验。Demo 由正式 React 前端打包生成，并使用只在普通浏览器中启用的内存后端；桌面应用仍然只使用真实 Tauri IPC。

这不是单独维护的视觉仿制品。页面结构、组件、样式、状态管理和主要交互都来自 `src-tauri/frontend/`，因此正式前端变化时可以重新构建并同步到这里。

## 覆盖范围

- 应用外壳：全局搜索、通知中心、侧栏折叠、亮色/暗色/跟随系统、缩放与快捷键
- 新对话：首页输入、工作区与模型选择、斜杠命令、创建任务并进入任务房间
- 项目仪表盘：项目指标、需要处理、任务列表、最近完成与项目动态
- 对话与活动：筛选、任务状态、归档入口、跨项目运行和最近事件
- 任务房间：时间线、主运行与子代理、权限请求、消息控制，以及启动器、运行、终端、文件、审核四个互斥工作台工具
- 待处理：权限决定、审核、请求修改、接受与回滚
- 项目：最近工作区、权限模式与项目记忆
- 项目文件：文件树、快速打开、预览、编辑与保存
- 设置：模型服务、外观与无障碍、诊断与支持包、Codex 协作设置

任务、审批、文件编辑、终端命令和设置变更都在当前页面内存中生效；刷新页面会恢复确定性初始数据，不会访问或修改本机项目。

## 确定性入口

通用参数：

- `scene=home|dashboard|conversations|activity|room|inbox|projects|editor|settings`
- `theme=light|dark|system`
- `rail=expanded|collapsed`
- `reset=1`：清除 Demo 使用的外观与布局偏好

任务房间：

- `task=queue|review|permission|api|complete`
- `tab=summary|changes|files|terminal|review`
- `state=launcher|run|terminal|files|review|review-collapsed|hidden`

其他场景：

- `project=r-code|api-server`
- `file=src/main.rs`
- `settings=providers|preferences|diagnostics|codex`

示例：

```text
index.html?scene=dashboard&project=r-code&theme=dark
index.html?scene=room&task=review&tab=review&theme=light
index.html?scene=settings&settings=codex&theme=dark
```

`state` 是工作台验收入口；`tab` 则保留为正式前端内部视图的直接深链。

## 重新构建

在 `src-tauri/frontend/` 下执行：

```powershell
npm run build:demo
```

构建会先执行 TypeScript 检查，再把正式前端与浏览器内存后端打包为 `app.js` 和 `styles.css`。输出使用经典脚本格式，因此 `index.html` 可以从本地文件直接打开。

## 验证

在本目录执行：

```powershell
# 首次使用先在 src-tauri/frontend 执行 npm install
node qa.cjs --smoke
node qa.cjs
```

完整验证覆盖全产品场景、六种工作台状态、双主题、桌面/紧凑/窄屏视口，以及导航、搜索、新建对话、工具恢复、任务隔离、审核收起、专注模式和权限决定等关键交互。截图写入 `target/ui-demo/`；`prototypes/workbench/` 作为视觉参考，不由当前 Demo 覆盖。

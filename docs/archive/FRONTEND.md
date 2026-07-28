# 前端选型说明

## 选型决策：Vite + React + TypeScript

### 为什么不用静态 HTML

R-Code 是一个 session-first 的 AI coding 桌面应用，UI 复杂度高：

- **对话流**：流式消息输出、滚动定位、消息分组
- **Monaco 编辑器**：代码高亮、diff 视图、多标签
- **xterm 终端**：多实例、resize、主题联动
- **多面板布局**：Session Rail + Composer + Canvas + Terminal
- **实时状态**：任务进度、权限请求弹窗、Agent 事件流
- **主题系统**：浅/深色 + 多皮肤切换

纯静态 HTML 在处理这些需求时，DOM 操作和状态同步会迅速膨胀到不可维护。

### 为什么选 React + TypeScript

| 维度 | 选择 | 理由 |
| --- | --- | --- |
| **框架** | React 18 | 生态最大，Monaco/xterm 都有成熟 React 封装；团队熟悉度高 |
| **语言** | TypeScript | 类型安全，与 Rust 后端的类型合同对齐 |
| **构建工具** | Vite 5 | 快速 HMR，Tauri 官方推荐，零配置 |
| **状态管理** | Zustand | 轻量（无 Provider 嵌套），API 简洁，替代 Redux 的 90% 场景 |
| **IPC** | @tauri-apps/api | Tauri 2 官方 JS 绑定，类型安全的 invoke |

### 为什么不用 Vue/Svelte

- Vue：同样优秀，但 Monaco Editor 的 React 封装（@monaco-editor/react）更成熟
- Svelte：编译时框架，包体积更小，但生态和 Monaco/xterm 集成不如 React

### 为什么不用 Next.js/Remix

- Tauri 应用不需要 SSR/SSG，Vite SPA 模式足够
- Next.js 的路由和数据获取层在桌面应用中是多余的开销

## 项目结构

```
src-tauri/frontend/
├─ package.json           # 依赖与脚本
├─ vite.config.ts         # Vite 配置（port 1420, esbuild minify）
├─ tsconfig.json          # TypeScript 配置（strict mode）
├─ index.html             # Vite 入口
├─ src/
│  ├─ main.tsx            # React 挂载点
│  ├─ App.tsx             # 根组件（Sidebar + 视图切换）
│  ├─ styles.css          # GitHub Dark 主题（CSS 变量）
│  ├─ lib/
│  │  └─ ipc.ts           # Tauri IPC 封装（invoke 泛型）
│  ├─ store/
│  │  └─ app.ts           # Zustand 全局状态（视图/缩放/Diff 模式）
│  └─ components/
│     ├─ Sidebar.tsx      # 侧边栏导航
│     └─ views/
│        ├─ HomeView.tsx       # 任务启动器 + 近期任务
│        ├─ TaskRoomView.tsx   # 时间线 + 审批 + 变更 + 验证
│        ├─ EditorView.tsx     # 文件树 + 代码视图 + 终端
│        └─ SettingsView.tsx   # 设置 + 无障碍 + 更新
```

## 开发工作流

```powershell
# 首次：安装依赖
cd src-tauri/frontend
npm install

# 开发模式（Vite HMR + Tauri 窗口）
./dev.ps1
# 等价于：cargo tauri dev
# -> Vite dev server 启动在 http://localhost:1420
# -> Tauri 窗口加载该 URL，前端改动实时热重载

# 生产打包
cargo tauri build
# -> beforeBuildCommand 自动执行 npm run build
# -> Vite 构建到 frontend/dist/
# -> Tauri 将 dist/ 嵌入二进制
```

## 后续扩展方向

- **Monaco Editor**：`npm install @monaco-editor/react`，在 EditorView 中集成
- **xterm.js**：`npm install @xterm/xterm`，在终端面板中集成
- **主题系统**：用 CSS 变量 + Context Provider 实现多皮肤
- **路由**：如需 URL 路由，加 react-router（当前用 Zustand 状态切换视图，足够）

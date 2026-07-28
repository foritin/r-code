# R-Code UI Prototype V1

状态：B+C 母版、12 张核心页面与 4 张任务执行渐进披露状态图均已完成确定性重绘，当前目录可作为产品讨论和前端实现的视觉基线。

`00-master-project-dashboard.png` 是唯一视觉母版。01、02、04–12 已按该母版重绘并通过横向一致性检查；13–16 专门描述任务运行、命令与子代理的逐级展开关系。原 03 项目仪表盘草稿已删除，避免与 00 形成两个互相冲突的母版。

## 统一基准

- [项目仪表盘母版](./00-master-project-dashboard.png)
- [UI 规范与组件契约](./UI-SPEC.md)
- [整套页面职责与重绘规范](./PAGE-SUITE-SPEC.md)
- [固定演示数据](./prototype-fixtures.json)
- [可复现 HTML/CSS 原型源](./prototype-src/index.html?screen=dashboard)

所有页面必须复用相同的顶部全局栏、左侧导航、底部状态栏、Logo、“新对话”组件、图标体系、设计令牌和演示数据。

最终母版采用“工作台 + 按语义出现的上下文检查器”：稳定项目信息位于标题命令条；“项目动态”只在项目内出现，新对话、对话列表和应用设置没有右栏。审核摘要、权限详情、文件活动与事件证据收起后保留原名称，再展开恢复原内容和操作。

核心导航关系：

```text
全部项目 / 跨项目活动
├── 待处理中心
└── 项目仪表盘
    ├── 项目文件编辑器
    ├── 工作区与项目记忆
    └── 任务
        ├── 概览
        ├── 变更
        ├── 审核与验证
        ├── 任务文件
        ├── 集成终端
        └── 会话回放与分支
```

## 可交互 HTML Demo

直接打开 [交互 Demo](./prototype-src/index.html?screen=dashboard&project=r-code) 即可使用，无需启动服务器。Demo 在核心页面与任务执行状态原型之上补充了：

- `r-code` / `api-server` 独立项目仪表盘与项目内导航。
- 对话列表、新对话、任务概览与任务文件页。
- 无刷新路由、任务标签、语义化检查器开合、侧栏收起。
- 任务运行摘要、命令列表、单项技术详情与子代理检查器的三级渐进披露。
- `Ctrl/⌘ K` 搜索与命令面板。
- 权限、审核、保存、诊断、分支与复制操作的确认和反馈。
- 顶栏浅色 / 深色快速切换；偏好保存在浏览器本机。
- dark / light 两个品牌图标资产。

详细的设计判断见 [UI / 交互审计](./INTERACTION-AUDIT.md)。

交互 Demo 默认读取系统主题；添加 `mode=render` 会强制使用固定浅色并关闭交互增强，供确定性 PNG 导出使用。

## 最终页面集

全部图片为 `1586 × 992`。03 项目仪表盘直接由 00 母版承担，不重复存放同一页面。

| 编号 | 页面 | 图片 | 状态 |
| --- | --- | --- | --- |
| 00 / 03 | 项目独立仪表盘 | [查看](./00-master-project-dashboard.png) | 唯一母版；B 工作台 + C 收起检查器 |
| 01 | 新建对话 | [查看](./01-new-conversation.png) | 全局纯聊天起点；未附加项目 |
| 02 | 跨项目活动 | [查看](./02-global-activity.png) | 平面态势表与展开审核检查器 |
| 04 | 任务变更 | [查看](./04-task-room.png) | 统一任务标题栏、标签栏与差异工作台 |
| 05 | 待处理中心 | [查看](./05-inbox.png) | 权限、审核与其他待处理汇总 |
| 06 | 审核与验证 | [查看](./06-review-verification.png) | 四文件审核与三项验证 |
| 07 | 项目文件编辑器 | [查看](./07-files-editor.png) | 文件树、编辑器、文件活动三栏工作台 |
| 08 | 集成终端 | [查看](./08-terminal.png) | 任务终端与可读命令输出 |
| 09 | 会话回放与分支 | [查看](./09-replay-branching.png) | 章节、事件、证据与分支关系 |
| 10 | 工作区与项目记忆 | [查看](./10-workspace-memory.png) | 访问模式与项目级规则 |
| 11 | 模型服务与外部 CLI 设置 | [查看](./11-settings-providers.png) | 连续四栏设置工作台 |
| 12 | 关键状态规范板 | [查看](./12-critical-states.png) | 非产品路由；四类关键状态对照 |

任务执行状态原型不是四个新产品页面，而是同一任务概览的四个交互深度：

| 编号 | 状态 | 图片 | 说明 |
| --- | --- | --- | --- |
| 13 | 默认收起 | [查看](./13-task-run-collapsed.png) | 只显示当前动作、子代理摘要与呼吸状态 |
| 14 | 活动展开 | [查看](./14-task-run-expanded.png) | 显示整理后的人类可读步骤，支持 Hover 与单项点击 |
| 15 | 命令详情 | [查看](./15-task-command-detail.png) | 仅在点击命令后显示 Shell、输出、耗时与退出状态 |
| 16 | 子代理详情 | [查看](./16-subagent-inspector.png) | 点击子代理后打开右侧检查器，展示任务、活动与结果 |

## 可复现原型

最终 PNG 不是逐张生成图，而是由 `prototype-src/` 内同一套 HTML、CSS、Lucide 图标与设计令牌在 Chromium 中确定性导出。页面通过 `index.html?screen=<路由>` 切换，路由为：

`dashboard`、`new`、`conversations`、`activity`、`task`、`taskactivity`、`taskcommand`、`taskagent`、`changes`、`taskfiles`、`inbox`、`review`、`files`、`terminal`、`replay`、`memory`、`settings`、`states`。

在已安装 Node.js、Playwright 与 Sharp 的环境中可执行：

```powershell
node prototype-src/build-icons.cjs
node prototype-src/render-screens.cjs
node prototype-src/verify-renders.cjs
node prototype-src/verify-interactions.cjs
```

`render-screens.cjs` 默认写入 `prototype-src/rendered/`，不会直接覆盖本目录的正式 PNG；只有显式设置 `R_CODE_OUTPUT_DIR` 时才输出到其他位置。

交互验证截图写入 `prototype-src/qa/`，覆盖双主题、项目切换、命令面板、新对话、任务标签、权限、终端、审核确认和 `1440 × 900` 视口。

## 横向验收结论

- 顶栏、项目选择器、左栏、新对话按钮、项目树与底栏使用同一套组件。
- 全局页待处理数固定为 `5`；全局运行中为 `4`，项目运行中为 `3`。
- 任务页固定使用“概览、变更、文件、终端、审核、回放”标签顺序。
- 项目与任务上下文可收起为 `64px`窄轨；全局无选中对象的页面直接不显示右栏。
- 审核摘要、权限详情等对象收起时不丢失语义和内容，再展开可原样恢复。
- 普通信息改用表格、平面行和分隔线，不再使用卡片套卡片。
- 可见任务数据、差异统计与验证耗时已与 `prototype-fixtures.json` 对齐。
- 本套静态图是流程分镜，不是同一时刻的系统快照：00、02、04、05 展示命令授权前，08、09 展示授权后的验证结果；真实产品必须由同一运行时状态驱动。
- 16 张 PNG 均通过 `1586 × 992` 尺寸、字体加载、固定栏宽、检查器溢出和禁用视觉效果检查。
- 15 个产品页 / 交互状态的品牌区、全局搜索、“新对话”组件与底栏已做像素哈希比对，结果完全一致。

正式图片用于视觉与信息架构讨论，像素级实现以 `UI-SPEC.md` 的尺寸、组件契约和 `prototype-src/` 的共享样式为准。后续调整应先修改原型源并重新导出，不直接修图，以免再次产生跨页面漂移。

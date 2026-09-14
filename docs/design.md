# UI 设计契约 —— design/ R2 高保真稿落地（docs/design.md）

> 状态：设计阶段契约，代码可消费。前端工位照本契约实现，QA 照本契约验收。
> 设计稿权威来源：`design/renders/*.html`（DOM）+ `design/tools/proposal.css`（160 行）+ `design/tools/revision-2.css`（140 行）。
> **落地方式 = 增量合入：只写 `src/styles/opt-*.css` 增量层 + 对应场景组件 TSX，禁止重写、禁止修改任何既有 CSS 文件。**

---

## 1. 设计令牌

### 1.1 颜色映射（proposal.css 变量 → tokens.css 现有变量）

落地**以 tokens.css 现值为唯一权威**。proposal.css 里的微调色值（如 `--bg-app:#181a1b`、`--accent:#ef9d6e`）是设计稿的静态近似，**一律不得照抄**；双主题色值由 `:root[data-theme='obsidian' | 'studio-light']` 自动解析。

| 设计稿变量 | 落地变量 | 说明 |
|---|---|---|
| `--bg-app` / `--bg-panel` / `--bg-chip` / `--bg-hover` | 同名 | 直接复用 |
| `--bg-card` | `--surface-card` | 语义别名层（A4），消费面只允许用 surface 语义名 |
| `--fg` / `--fg-muted` / `--fg-faint` | 同名 | 直接复用 |
| `--border` / `--border-strong` | 同名 | 直接复用 |
| `--accent` / `--accent-fg` | 同名 | 直接复用 |
| `--signature-sidebar` | `--signature-sidebar` | 直接复用 |
| `--opt-soft`（柔和填充面） | `--surface-sunken` | 即 `--bg-inset` |
| `--opt-input`（输入框/终端/代码底色） | `--surface-sunken` | 同上，输入类底色统一沉底 |
| `--opt-success` / `--opt-warning` / `--opt-danger` | `--success` / `--warning` / `--danger` | 直接复用 |
| `--opt-tint` | `--tint-accent` | 10% 混合档 |
| `color-mix(success 10%, transparent)` 等手调混合 | `--tint-success` / `--tint-warning` / `--tint-danger` | 严禁自调百分比 |
| `--opt-shadow` | `--shadow-card` / `--shadow-popover` | 按层级取：卡片用 card，drawer/modal 用 popover |
| `#0b0d1040` 等遮罩字面色 | `--overlay-scrim` | 遮罩一律走 scrim 令牌 |

### 1.2 新增令牌（T01 落入 `src/styles/tokens.css`，全库唯一改动点）

| 令牌 | obsidian | studio-light | 用途 |
|---|---|---|---|
| `--agent-name` | `#75ccea` | `#126c8b` | 子代理名文字色（revision-2.css:2-3） |
| `--agent-shine` | `#ebfbff` | `#379abc` | 子代理名 sheen 动画高光（同上） |
| `--text-hero` | `32px`（:root 几何层） | 同左 | home 主标题（设计稿 34px，收敛到 32） |

几何令牌**改值**（同在 T01，全部有 renders 依据）：

| 令牌 | 旧值 | 新值 | 依据 |
|---|---|---|---|
| `--menubar-h` | 34px | **42px** | renders `--rc-topbar-h:42px`（proposal.css:18） |
| `--rail-w` | 252px | **264px** | proposal.css:18 `--rc-current-rail:264px` |
| `--rail-w-narrow` | 208px | **232px** | proposal.css:160 断点 ≤1050 → 232px |

> 改值影响全页面（含 keep 页）的共享 chrome，属设计稿定案的共享外壳变化，keep 页**内容**不受影响。

### 1.3 几何刻度映射（设计稿裸 px → tokens 刻度，就近取档 + 密度优先向下）

**字号**：10/11/12px→`--text-meta`；13px→`--text-sm`；14px→列表/表格/按钮行内用 `--text-sm`，页面说明段（`.opt-help`、page-head 的 p）用 `--text-base`；15px→`--text-base`；16/17px→`--text-lg`；18/19px→`--text-xl`；23/25/26px→`--text-2xl`；34px→`--text-hero`。

**字重**：450→`--weight-normal`；500–580→`--weight-medium`；600/620→`--weight-strong`。落地后代码里不允许出现 450–620 之间的裸字重。

**间距**：2-4→`--sp-1`/`--sp-2`；5-6→`--sp-3`；7-8→`--sp-4`；9-10→`--sp-5`；11-13→`--sp-6`；14-16→`--sp-7`；17-20→`--sp-8`；21-24→`--sp-9`；25-32→`--sp-10`；34-46（页边距类）→`--sp-10`。

**圆角**：5-6→`--radius-sm`；7-8→`--radius-md`；9-14→`--radius-lg`；20→`--signature-workspace-radius`；胶囊→`--radius-pill`。

**控件高**：27-30→`--control-h`；33-38→`--control-h-lg`；命中区 ≥`--hit-min`。

**图标视口**：设计稿 14/15/16/20/21/23px 一律收敛为 16px（行内）或 20px（工具格）；36px 图标容器→`--control-h-lg`。

### 1.4 动效

- 子代理名 sheen 动画（`opt-agent-name.is-running::after` + `@keyframes opt-agent-sheen`）按 revision-2.css:22-24 落地，**必须**带 `prefers-reduced-motion: reduce` 关停分支。
- 运行开关：沿用现有 `data-motion` 语义；proposal 全局 `animation:none!important` 是静态稿手段，**不落地**。

---

## 2. 核心组件状态矩阵（五态 × 双主题）

颜色一律写令牌名；双主题由 tokens 自动解析（下表一套定义同时覆盖 obsidian / studio-light）。焦点态统一配方：`outline: var(--ring-w) solid var(--ring-color); outline-offset: var(--ring-offset)`（先例 workbench-page.css）。

### 2.1 侧栏导航项（`.sidebar-nav-item` / `.sidebar-task` / `.sidebar-project-head`）

| 态 | 契约 |
|---|---|
| default | 背景 transparent；文字 `--fg-muted`；字号 `--text-sm`；min-height `--control-h-lg`；圆角 `--radius-md`；网格 `6px minmax(0,1fr)` 指示列 + 文本 |
| hover | 背景 `--bg-hover`；文字 `--fg` |
| active（当前任务） | 背景 `--tint-accent`；`box-shadow: inset 2px 0 var(--accent)`；文字 `--fg` |
| focus-visible | 统一焦点环配方 |
| disabled | 文字 `--fg-faint`；无背景响应 |

侧栏节标题（`.sidebar-section-head`）：`--text-meta`，letter-spacing `0.03em`，色 `--fg-faint`。侧栏时间戳（`sidebar-task time`）**隐藏**（proposal.css:30 定案）。brand 行：min-height 64px→`--sp-10`+`--sp-9` 组合，mark 27×27→28px（`--sp-7`×2 近似，取 28px 字面为图标内容尺寸例外）。

### 2.2 按钮（`.opt-button`，三变体）

| 态 | default 变体 | primary 变体 | danger 变体 |
|---|---|---|---|
| default | 背景 `--bg-panel`；边框 `--border`；文字 `--fg` | 背景/边框 `--accent`；文字 `--accent-fg` | 文字 `--danger`；背景/边框 transparent |
| hover | 背景 `--bg-hover` | 背景向 `--accent-2` 方向 `color-mix` 加深 12% | 背景 `--tint-danger` |
| active（按下） | 背景 `--bg-active` | 同上再加深一档 | 背景 `--tint-danger-hi` |
| focus-visible | 统一焦点环 | 同左 | 同左 |
| disabled | opacity .46；cursor not-allowed；三变体一致 | | |

尺寸：min-height 34px→`--control-h-lg`；padding 7px 13px→`--sp-3 var(--sp-5)` 组合取 `--sp-3`/`--sp-4`；圆角 `--radius-md`；小尺寸变体（面板头/表格行内）min-height 30px→`--control-h`、字号 `--text-meta`。

### 2.3 卡片（`.opt-card`）

default：背景 `--bg-panel`；边框 `--border`；圆角 `--radius-lg`；padding 22px→`--sp-9`。卡片本身无 hover/active 态（非可点击容器）；可点击卡片（tool-tile、theme-choice）hover 用 `--bg-hover` + 边框 `--border-strong`，selected 用边框 `--accent` + `box-shadow: 0 0 0 1px var(--accent)`。头部 h2 `--text-lg`/`--weight-strong`；描述 `--text-sm` + `--fg-muted`。

### 2.4 列表行（`.opt-row` / `.opt-inbox-line` / `.opt-project-line` / `.opt-tool-line`）

| 态 | 契约 |
|---|---|
| default | 底部分隔线 `color-mix(in srgb, var(--border) 55-65%, transparent)`（收敛为 60%）；背景 transparent；标题 `--text-sm` + `--weight-medium`；副文本 `--text-meta` + `--fg-muted` |
| hover | 背景 `--bg-hover` |
| active/selected | inbox-line：`background: linear-gradient(90deg, var(--tint-accent), transparent 96%)` + `box-shadow: inset 2px 0 var(--accent)`；其余列表行背景 `--tint-accent` |
| focus-visible | 统一焦点环 |
| disabled | 文字 `--fg-faint` |

### 2.5 输入框（`.opt-input` / `.opt-search-field` / `select`）

default：背景 `--surface-sunken`；边框 `--border`；圆角 `--radius-md`；min-height 37px→`--control-h-lg`+纵向 padding `--sp-3`；文字 `--fg`；placeholder `--fg-faint`。hover：边框 `--border-strong`。active/聚焦：边框 `--edge-accent-hi`。focus-visible：统一焦点环。disabled/readonly：背景 `--surface-sunken`；文字 `--fg-muted`；边框 `--border`。

### 2.6 标签页（`.opt-tabs` 两种变体）

- 胶囊变体：default 文字 `--fg-muted`；active 背景 `--tint-accent` + 文字 `--accent` + `--weight-strong`；hover（未激活）文字 `--fg`。
- line 变体（`.opt-tabs.line`）：default 文字 `--fg-muted`，底边线 `--border`；active 背景 none + `box-shadow: 0 2px var(--accent)` + 文字 `--accent`；inbox 工具栏细线变体 active 用 `box-shadow: 0 13px 0 -11px var(--accent)` + 文字 `--fg`。

---

## 3. 布局骨架约定

- **全局骨架**：`.app-shell` = menubar（高 `--menubar-h`=42px，`data-tauri-drag-region` 不变）+ `.app-sidebar`（宽 `--rail-w`=264px；≤1049px → `--rail-w-narrow`=232px；collapsed 48px 行为不变）+ `.main`（背景 `--bg-app`；左上圆角 `--signature-workspace-radius`=20px；去掉 `.main::before/::after` 装饰层，见 proposal.css:33-34）。
- **页面容器**：`.opt-page` padding 38px 46px→`--sp-10`；`.opt-page-head` = eyebrow（`--text-meta`+`--fg-faint`）+ h1（`--text-2xl`）+ 说明 p（`--text-base`+`--fg-muted`，max-width 68ch→`--measure` 近似，取 `68ch` 为排版例外值）。
- **设置页骨架**：`.opt-settings` = 左导航 188px + 主区 minmax(0,1fr)；主区 padding `--sp-9`/`--sp-10`；内容 max-width 1040px；sticky footer 特例（settings-prompts）按页实现。
- **会话页骨架**：`.scene-room` = `minmax(0,1fr)` + `--opt-panel-width`（默认 520px）双栏；`.room-splitter` 隐藏（proposal.css:113 定案）；canvas 面板背景 `--surface-sunken` + 左边线 `--border`。
- **响应式断点（收敛定案，与现有 workbench-page.css 家族对齐）**：
  - `≤1279px`：页边距收窄至 `--sp-9`；设置导航 166px；dashboard 侧栏 196px；inbox 详情 330px；project-line 列宽收窄（revision-2.css:137）。
  - `≤1049px`：rail 232px；设置导航 154px；dashboard/inbox 侧栏隐藏或双列转单列；设置搜索框隐藏；双列卡片 `.opt-two` 转单列。
  - wbpage 自有密度断点（≤1024/≤880/≤600）不变，仅约束 WorkbenchPage 内部。
- **keep 页（P13/P14/P25）**：共享 chrome（menubar/rail/main 圆角/令牌改值）随全局生效；三页**内容区**的组件与样式零改动。

---

## 4. 设计禁忌（本项目明确不采用）

1. **禁止字面主题色**：新增 CSS 中零 hex 色值；唯一例外 `--accent` 作文字色时的 color-mix 白/黑调深浅锚点（先例 workbench-page.css `--_accent-text`）与新增令牌 `--agent-name/--agent-shine` 的定义行本身。
2. **禁止照抄设计稿近似色值**（`#181a1b`/`#ef9d6e` 等）与自调 color-mix 百分比（tint/edge 只用 10/18/40/62 四档）。
3. **禁止裸 px 手感值**：几何只走 §1.3 映射后的刻度；例外清单 = 图标视口（16/20）、68ch 行长、2px 选中内嵌条、1px 线宽。
4. **禁止修改既有 CSS 文件**（tokens.css 的 §1.2 增量除外，仅 T01）：所有增量进本任务独占的 `opt-*.css`。
5. **禁止把 proposal.css 的压缩单行写法带入生产**：落地代码展开 + 头部注释规范，参照 `workbench-page.css` 头部（作用域/令牌依据/断点/骨架四段）。
6. **禁止改 keep 页**：ActivityScene、ArchiveScene、NativeNotificationSettings 及其专属样式（`styles/scenes/misc.css`、`styles/scenes/room.css`、`styles/memory.css` 等）对全部任务只读。
7. **禁止 emoji 当图标**、禁紫色渐变、禁编造数据（沿用团队默认禁忌）。
8. **禁带 `opt-layout-stamp`**：那是设计稿的布局校验浮标，不落地。
9. **禁用 `body[data-proposal="…"]` 作用域机制**：该机制是设计稿的按页开关；落地时把按页特例并入对应页面区块选择器。

---

## 5. opt-* 类名清单（消费面契约，T01 在 `opt.css`/各 `opt-*.css` 中定义为唯一定义点）

按家族分组（变体类随主类）：

- **骨架**：`opt-page` `opt-page-head` `opt-eyebrow` `opt-actions` `opt-footer` `opt-settings` `opt-settings-nav` `opt-settings-main` `opt-settings-topline` `opt-content`
- **基础件**：`opt-icon` `opt-button`(.primary/.quiet/.danger) `opt-pill`(.accent/.success/.warning/.danger) `opt-card`(+`opt-card-head`) `opt-section-head` `opt-row`(+`opt-grow`) `opt-table`(+`opt-last`) `opt-summary` `opt-two` `opt-notice`(.warning) `opt-empty` `opt-field` `opt-input` `opt-toolbar` `opt-search-field` `opt-tabs`(.line) `opt-kv` `opt-scope` `opt-muted` `opt-faint` `opt-mono` `opt-help`
- **工作区页**：`opt-home` `opt-suggestions` `opt-suggestion` `opt-home-foot` `opt-dashboard` `opt-dashboard-aside` `opt-feed-item` `opt-decision-row` `opt-projects-page` `opt-projects-list` `opt-list-caption` `opt-project-line` `opt-project-title` `opt-project-meta` `opt-current-label` `opt-open-section` `opt-projects-footer`
- **会话与工具面板**：`opt-subagent-lines`(.in-panel) `opt-subagent-line` `opt-agent-kind` `opt-agent-separator` `opt-agent-name`(.is-running/.is-complete) `opt-agent-stop`（T04 落地追认）`opt-agent-description` `opt-subagent-tail` `opt-tool-intro` `opt-tool-menu` `opt-tool-line` `opt-tool-copy` `opt-tool-footnote` `opt-panel-head` `opt-panel-body` `opt-panel-footer` `opt-live` `opt-tool-list` `opt-tool-tile` `opt-mini-stats` `opt-terminal`(+tabs/path) `opt-file-layout` `opt-file-tree` `opt-file-blank` `opt-file-tab` `opt-editor`(+`opt-editor-code`) `opt-line-no` `opt-code-key` `opt-code-value` `opt-code-foot` `opt-check-steps` `opt-review-files` `opt-review-file` `opt-diff`(+`opt-diff-add`/`opt-diff-del`) `opt-agents-list` `opt-inline-state` `opt-runtime-row` `opt-runtime-copy` `opt-runtime-steps` `opt-runtime-detail`
- **设置页**：`opt-setting-line` `opt-setting-control` `opt-agent-choice` `opt-routing-section` `opt-mcp-section-head` `opt-mcp-table` `opt-mcp-actions` `opt-mcp-note` `opt-service-title` `opt-environment-summary` `opt-environment-grid` `opt-skill-layout` `opt-skill-list` `opt-skill-detail` `opt-theme-grid` `opt-theme-choice` `opt-theme-window`(.dark/.system) `opt-validation-line` `opt-file-list-caption` `opt-inbox-file` `opt-inbox-detail-note`
- **叠层**：`opt-drawer`(-backdrop) `opt-confirm`(-backdrop) `opt-search-backdrop` `opt-search-modal` `opt-search-scope` `opt-search-input` `opt-search-results` `opt-search-result` `opt-search-footer` `opt-search-clear`（T06 落地追认）

> **落地追认定案（W1 收口后）**：① `opt-agent-stop` 为正式契约类名（T04 落地，opt-room.css 已定义）；② `canvas-body` **不挂** `opt-panel-body` —— 工作台主体容器保留原生类名，`opt-panel-*` 仅用于抽屉/浮层面板表面；③ TSX 消费方与 CSS 定义点分属不同任务时，合法性依据「类名唯一定义点」契约，定义点侧对已验收类只增不改。

> TSX 工位只允许消费上表类名；需要新类名（现有 DOM 无对应类）时，前缀沿用页面既有命名体系（如 `.wbpage-`、`.sidebar-`），并回传给主理人记录。

---

## 6. 待明确事项（Anything UNCLEAR）

1. **假设：设计稿静态内容 ≠ 运行时数据**。本次落地只做 DOM 结构与样式对齐；renders 中的演示文案/假数据不接 store，不改 `store/*` 数据流。若主理人要求接 demo 数据，需追加数据域任务。
2. **假设：`WorkbenchPage.tsx`（wbpage- 前缀）保留为独立实现参考**，不接入场景路由；其响应式/令牌先例作为落地规范来源。是否最终保留该文件由主理人在收口时定。
3. `.main::before/::after` 装饰层在现有 `signature.css`（1286 行）中的具体规则未逐行核对；落地时若发现与 20px 圆角冲突，按 proposal.css:33-34 的定案（content:none）写进 `opt-shell.css` 增量，不改 signature.css。
4. 设计稿断点（1280/1050）与现有 workbench-page 断点（1279/1024）存在 1px 家族差；契约取 §3 定案值（1279/1049），QA 按此验收。
5. 建议衔接 design-engine 专家团的场景：无 —— 本项目有已 approved 的高保真稿 + 增量 CSS，视觉稿已在 design/ 内，不需要额外视觉设计。

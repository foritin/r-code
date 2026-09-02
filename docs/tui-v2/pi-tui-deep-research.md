# pi TUI 深度调研与 R-Code TUI v2 落地对照

> 调研日期：2026-09-02（同日增补 Claude Code UI 调研与原型 v3 评审）
> 调研对象：badlogic/pi-mono（`packages/tui`、`packages/coding-agent`、`packages/ai`）；Anthropic Claude Code（官方 terminal-config / interactive-mode / statusline 文档 + 社区取证）
> 对照对象：本仓库 `crates/r-code-tui` + `r_code_host::commands`（CommandState）
> 前置文档：`docs/support/archive/pi-alignment/`（v1 PRD，已归档）

## 0. 结论速览

1. **协议层已就绪，表现层是空白。** R-Code 的 `InferenceOptions`（`thinking: enabled/disabled/adaptive` + `reasoning_effort: none/minimal/low/medium/high/xhigh/max` + `verbosity`）与 pi 的 `ThinkingLevel` 枚举逐值一致，说明 v1 对齐工作把"管道"铺完了；但 TUI 把 thinking 硬编码成 `disabled`（`crates/r-code-tui/src/main.rs:123`），模型/思考级别在界面上零暴露。v2 的主要工作量在 UI 层，不在协议层。
2. **pi 的核心竞争力是 inline（regular）模式**：对话历史自然流入终端 scrollback、行级差分重绘、同步输出包裹（CSI ?2026）防闪烁。R-Code 当前是 ratatui alt-screen 单模式，历史被锁在备用屏里，退出即消失。这是体验上最大的结构性差异。
3. **pi 的交互三角**：`Shift+Tab` 循环思考级别（按模型记忆）、`Ctrl+P` 循环模型 / `Ctrl+L` 打开模型选择器、footer 常驻显示 `(provider) model • thinking` + token/上下文统计。这三样是"配置模型、选择思考"的 pi 答案，R-Code 一样都没有。
4. **pi 绝不 mock**：无凭据时启动即进入 first-time-setup / login-dialog（OAuth 或 API key），真实 provider 不可用就直接报错引导。R-Code v1 TUI 因未调 `enable_real_agent_mode()` 跑在隐式 Mock 演示场景上——v2 红线：**禁止 mock 模式**，无配置必须显式引导。
5. **Windows 输入双击 bug 有 pi 解法**：pi 在框架层过滤 key-release 事件（Kitty 协议显式过滤 + Windows crossterm 侧 `KeyEventKind`），R-Code v1 的 `app.rs:81` 没过滤导致每键双写。

---

## 1. pi-mono 分层架构

```
packages/
├── ai/            统一 LLM 层（provider 适配、ThinkingLevel 抽象、streamSimple）
├── agent/         agent 循环（pi-agent-core：AgentSession、steering、compaction）
├── tui/           终端 UI 框架（与 agent 无关的通用库）
├── coding-agent/  harness + 交互层（interactive/print/json-event/rpc 四模式）
├── protocol/      会话/事件协议
├── client/        远程会话客户端
├── server/        RPC server
└── session-backends/sqlite-node   会话持久化
```

关键解耦：`pi-tui` 不知道 agent 存在（纯组件框架）；`coding-agent` 把 AgentSession 事件流翻译成组件树。R-Code 的对应关系：`r-code-tui` ≈ pi-tui + coding-agent/interactive 的合体，事件源 `AgentEvent` + `CommandState` 编排层已经承担了 pi 的 agent/session 职责——**分层是同构的，v2 不需要动宿主层**。

## 2. pi-tui 框架核心（packages/tui）

### 2.1 Component 协议

```ts
interface Component {
  render(width: number): string[];   // 按宽度渲染成行数组（非全屏 buffer）
  handleInput?(data: string): void;  // 原始输入字节，焦点时接收
  invalidate(): void;                // 主题变化/强制重绘时清缓存
  wantsKeyRelease?: boolean;         // Kitty release 事件，默认过滤
}
interface Focusable { focused: boolean }  // 聚焦时在光标位输出 CURSOR_MARKER
```

- 渲染单位是**行数组**而非 cell buffer——这是 inline 模式的前提。
- `CURSOR_MARKER = "\x1b_pi:c\x07"`（APC 零宽序列）：聚焦组件在光标位置埋标记，TUI 提取后把硬件光标移过去 → **IME 候选窗跟随假光标**。R-Code v1 的 `ime.rs` 只做了坐标计算，这条链路思路相同但 pi 落得更完整（含 `showHardwareCursor` 设置项）。

### 2.2 双模式渲染

| | TuiMainScreen（regular，默认） | TuiAltScreen（fullscreen） |
|---|---|---|
| 屏幕 | 主屏 + scrollback | 备用屏 + ScrollView |
| 历史 | 自然滚入 scrollback，退出后保留 | 锁在 alt screen，`fullscreenExitOutput: transcript/resume-hint` 决定退出时是否回放摘要 |
| 重绘 | 行级差分（firstChanged..lastChanged）+ append-only 尾部推进 | 视口内重绘 |
| 触发全量的条件 | 宽度变化 / 高度变化（Termux 除外）/ clearOnShrink / 差分起点滚出视口 | — |

差分渲染细节（`tui-main-screen.ts`）：
- 所有输出包在 `ESC[?2026h … ESC[?2026l`（synchronized output）里，重绘不闪烁；
- 输入事件走 `requestImmediateRender()` 绕过 16ms 节流定时器（注释明确：Windows 下 `setTimeout(0)` 可能吃满一个 tick）；
- 单行变化（spinner）只重写该行；
- 超宽行直接 crash 并写 `pi-crash.log`（组件契约：必须自己截断）。

### 2.3 Overlay 栈

`showOverlay(component, options)`：anchor（9 锚点）+ 百分比/绝对定位 + margin + `visible(w,h)` 回调 + `nonCapturing`；focusOrder 决定层叠与焦点归还链（preFocus 记忆）。模型选择器、thinking 选择器、session picker、login 对话框全是 overlay。

### 2.4 Editor（v1 R-Code InputBuffer 的完整版）

多行编辑器，能力清单：grapheme/CJK 安全折行（`Intl.Segmenter`）、undo/redo（undo-stack）、Emacs kill-ring（`Ctrl-K` 删到行尾 / `Ctrl-Y` yank / yank-pop）、词导航（word-navigation）、粘贴标记（paste markers，大粘贴折叠成 `[pasted N lines]`）、外部编辑器（`Ctrl+G` 起 `$VISUAL/$EDITOR`）、自动补全下拉（`@` 文件、`/` 斜杠命令，`autocompleteMaxVisible` 可调）。键位全部走 KeybindingsManager（见 §3.3）。

### 2.5 主题与终端探测

OSC 11 查询终端背景色 + DSR `?996n` 亮暗偏好 + `?2031h` 变更通知；主题是文件（内置 + `~/.pi/agent/themes/` + settings 注入），footer/组件统一从 `theme.fg("dim"|"error"|"warning", …)` 取色。

## 3. coding-agent 交互层

### 3.1 配置体系（用户点名重点①：配置模型）

路径与作用域：

```
~/.pi/agent/
├── settings.json      全局设置（锁文件保护，写回只写 modified 字段）
├── auth.json          凭据（provider → apiKey / OAuth token）
├── models.json        自定义模型目录（覆盖内置 catalog）
├── keybindings.json   用户键位覆盖
├── themes/  tools/  prompts/  skills/  sessions/
└── pi-debug.log
<project>/.pi/settings.json   项目作用域（须 project trust，deep merge 覆盖全局）
```

settings.json 中与模型/思考直接相关的字段：

```jsonc
{
  "defaultProvider": "anthropic",
  "defaultModel": "claude-opus-4-5",
  "defaultThinkingLevel": "medium",            // 全局默认思考级别
  "modelThinkingLevels": {                      // 按模型记忆（"provider/modelId" → level）
    "anthropic/claude-opus-4-5": "high"
  },
  "thinkingBudgets": { "minimal": 1024, "low": 4096, "medium": 8192, "high": 16384 },
  "enabledModels": ["anthropic/*", "openai/gpt-5*"],   // Ctrl+P 循环池（模式匹配）
  "hideThinkingBlock": false,
  "tuiMode": "regular",                         // regular | fullscreen
  "steeringMode": "one-at-a-time",              // 运行中插话的派发策略
  "followUpMode": "one-at-a-time",
  "compaction": { "enabled": true, "reserveTokens": 16384, "keepRecentTokens": 20000 },
  "externalEditor": "code --wait",
  "shellPath": "…"
}
```

**thinking 级别解析链**（SDK 文档确认）：`session history（thinking_level_change 事件）→ per-model override（modelThinkingLevels）→ 全局 defaultThinkingLevel → 'medium' → clamp 到模型能力`。会话中途切换会写入会话树（`thinking_level_change` entry），fork/回溯时级别随分支走。

### 3.2 ThinkingLevel 语义（packages/ai 统一抽象）

`'off' | 'minimal' | 'low' | 'medium' | 'high' | 'xhigh' | 'max'`——每模型 `thinkingLevelMap` 把级别翻译成 provider 原生参数（Anthropic budget_tokens、OpenAI reasoning_effort、DeepSeek…）。流事件统一为 `thinking_start/thinking_delta/thinking_end`。

**R-Code 已对齐**：`vendor/agent-contracts/.../provider.rs:46` `InferenceOptions { thinking, reasoning_effort, verbosity }`，`reasoning_effort` 枚举注释就是 `none/minimal/low/medium/high/xhigh/max`。差的只是两件事：
- TUI 上的循环/选择/持久化（对应 `modelThinkingLevels` 的 per-task 记忆，R-Code 已有 `task_set_inference` 挂在任务上——位置不同但语义可映射）；
- provider 适配器是否全量实现了级别映射（这属于宿主/worker 层，v1 已做过 Pi 对齐，此处不重查）。

### 3.3 键位体系（用户点名重点②：选择思考）

声明式 keybinding id，用户可在 `keybindings.json` 覆盖；Windows/WSL 有专门变体（`useWindowsKeybindings`：win32 或 WSL 检测）：

| 动作 | 默认键（非 Windows） | Windows/WSL | 说明 |
|---|---|---|---|
| app.thinking.cycle | `Shift+Tab` | 同 | 循环思考级别 off→…→max |
| app.model.cycleForward | `Ctrl+P` | 同 | 下一模型（enabledModels 池） |
| app.model.cycleBackward | `Shift+Ctrl+P` | `Alt+P` | 上一模型 |
| app.model.select | `Ctrl+L` | 同 | 模型选择器 overlay（fuzzy 搜索 + provider 分组 + 启停管理） |
| app.thinking.toggle | `Ctrl+T` | 同 | 显隐思考块 |
| app.tools.expand | `Ctrl+O` | 同 | 展开/折叠工具输出 |
| app.interrupt | `Esc` | 同 | 取消/中止 |
| app.clear | `Ctrl+C` | 同 | 清空编辑器（不退出） |
| app.exit | `Ctrl+D` | 同 | 空编辑器时退出 |
| app.editor.external | `Ctrl+G` | 同 | 外部编辑器 |
| app.message.followUp | `Alt+Enter` | `Ctrl+Q` | 排队 follow-up |
| app.clipboard.pasteImage | `Ctrl+V` | `Alt+V` | 贴图 |
| （双击）Esc | — | — | 会话树（doubleEscapeAction: fork/tree/none） |

### 3.4 Footer（状态栏）

三行结构（`footer.ts`）：
1. `~/dev/r-code (main) • 会话名` —— pwd（~ 缩写）+ git 分支 + 命名会话；
2. 左：`↑12.3k ↓4.5k R89k W2k CH97.8% $0.412 42.1%/200k (auto)` —— 输入/输出/缓存读/缓存写 token、缓存命中率、成本、上下文占用（>70% warning、>90% error 变色，`(auto)` 表示自动 compaction）；右：`(anthropic) claude-opus-4-5 • high`（多 provider 且放得下时才加括号前缀；模型不支持 reasoning 时省略 thinking 段）；
3. 扩展状态行（有扩展时）。

统计数据全部来自会话 entries 的 usage 累加（含 compaction/branch-summary 的 usage）——**不是 runtime 私有状态**，resume 后依然准确。

### 3.5 交互组件清单（modes/interactive/components）

`model-selector`（fuzzy + provider 分组 + enableAll/clearAll/toggleProvider/排序）、`scoped-models-selector`（管理 Ctrl+P 循环池）、`thinking-selector`、`session-selector`（resume/搜索/重命名/删除/排序/path 显隐）、`tree-selector`（会话树导航：fold/unfold/打标签）、`settings-selector`、`theme-selector`、`trust-selector`（项目信任）、`login-dialog`/`oauth-selector`（凭据）、`first-time-setup`（首启引导）、`keybinding-hints`、`tool-execution`（工具卡：折叠摘要/展开输出）、`assistant-message`（markdown + 思考块折叠 + mermaid streaming）、`diff`、`user-message`。

扩展系统可替换 header/footer/editor、在编辑器上下插 widget、注册 autocomplete provider、`setStatus` 进 footer。

### 3.6 启动与鉴权（禁 mock 的 pi 范式）

`cli.ts`：参数解析 → first-time setup 检测（无凭据 → 引导登录：OAuth/API key/跳过）→ project trust → session 装配（provider 从 auth.json + models.json catalog 解析；解析失败**直接报错退出**）→ 进入 interactive/print/json-event/rpc 模式。auth 子命令（`pi auth check/print`）与 `auth-guidance.ts` 负责把"为什么失败"翻译成可操作的指引。

## 4. R-Code TUI v1 现状差距清单

| # | 维度 | pi | r-code v1 | 差距定级 |
|---|---|---|---|---|
| 1 | runtime 接线 | 无条件真实 provider | 交互模式未调 `enable_real_agent_mode()`，Mock 演示场景隐式回放（`commands.rs:14228`） | **P0，红线** |
| 2 | 渲染模式 | regular inline（默认）+ fullscreen | 仅 ratatui alt-screen | P1（结构性） |
| 3 | 模型切换 | Ctrl+P 循环 + Ctrl+L 选择器 + enabledModels | 无（task 建死默认 provider） | **P0** |
| 4 | 思考控制 | Shift+Tab 循环 + per-model 记忆 + budgets | `thinking: Some("disabled")` 硬编码 | **P0** |
| 5 | footer 状态 | token/上下文/成本/模型/思考 常驻 | 无 footer | P1 |
| 6 | 键盘事件 | key-release 过滤（Kitty/Win） | 不过滤 `KeyEventKind` → 每键双写 | **P0（bug）** |
| 7 | 编辑器 | 多行/undo/kill-ring/词导航/补全/外编 | 单行 InputBuffer | P1 |
| 8 | 错误呈现 | 引导式（auth-guidance/first-time-setup） | `eprintln!`（alt-screen 下不可见） | **P0** |
| 9 | 会话管理 | resume/树/fork/重命名 | 无（每次新 task） | P2 |
| 10 | 上下文经营 | compaction + 上下文百分比 | 无可视化（宿主层有 retained_tail 等机制） | P2 |
| 11 | 键位/主题 | keybindings.json + 主题文件 + OSC 探测 | 硬编码 | P2 |
| 12 | 脚本模式 | print / json-event / rpc | print/json 已有雏形（但同受 Mock 门影响） | P1（接线修复后即活） |

已对齐、无需重做的：`InferenceOptions` 枚举、`AgentEvent` 事件源、CommandState 编排（队列/steer/abort/审计/会话 JSONL）、共享 data-dir 与桌面互通、IME 假光标坐标思路。

## 5. v2 落地建议（供 PRD 拆解，本轮不动代码）

**红线（用户拍板）**：
- R1 禁止 mock 模式：TUI 装配即 `enable_real_agent_mode()`；无 provider 配置 → 首屏引导（指向桌面设置页或配置文件路径），不降级、不回放演示场景；`push_demo_scenario` 的隐式调用点（`commands.rs:14228`）改为仅显式 `--mock`（评估线路）时可达，或彻底移除交互路径。
- R2 provider 不可用必须显式报错 + 可操作指引（对齐 pi auth-guidance）。
- R3（2026-09-02 拍板）产品定位为 **r-code cli**：r-code-tui 即 cli 本体（`--mode tui|print|json` 已具备 cli 的三种形态），可考虑 bin 更名/别名 `r-code`；交互形态 **默认 inline 滚动式（pi regular，claude code 同款语义）**，**不做独立 fullscreen 模式**——"全屏"语义由 ctrl+t transcript 浮层覆盖（2026-09-02 二次拍板，取代本条早先的 F10 切换方案）。

**里程碑草案**：
- **M1 真实化**：enable_real_agent_mode + agent_send 错误进 transcript（System 行）+ KeyEventKind 过滤 + 无配置首屏引导。完成后 TUI 第一次"说真话"。
- **M2 模型/思考三角**：Ctrl+P 循环（读 config.providers 健康集）+ Ctrl+L 模型选择器 overlay + Shift+Tab 思考级别循环（写 `task_set_inference`，per-task 记忆对齐 pi 的 modelThinkingLevels）+ footer 右侧 `(provider) model • thinking`。
- **M3 footer 完整化**：token/上下文统计（宿主 usage 投影已有数据源）+ 变色阈值 + `(auto)` compaction 标记。
- **M4 编辑器与键位**：多行 + undo + 词导航 + 粘贴标记 + Ctrl+G 外编 + 键位表（Windows 变体）+ Ctrl+T/Ctrl+O 折叠。
- **M5 inline 模式（已定案为默认且唯一形态）**：行差分渲染 + 同步输出包裹（CSI ?2026），历史进终端 scrollback、编辑器/footer 贴底；不做独立 fullscreen 模式，大段历史/输出查看由 ctrl+t transcript 浮层承担。这是渲染层的最大单项，需评估 ratatui InlineViewport 与自研行差分两条路线。
- **M6 会话管理**：resume 列表（共享 data-dir 的 tasks/sessions）、重命名、树视图。

**技术取舍提示**：
- M5 已定案默认 inline 且不做独立 fullscreen。pi 的行差分 + scrollback 方案在 Rust 侧无现成等价物（ratatui InlineViewport 接近但语义不同：它仍是 viewport 内重绘）。建议 M1-M4 先在现有 alt-screen 上完成（用户价值最快兑现），M5 单独立项做渲染层 PoC 对比（ratatui InlineViewport vs 自研行差分）。
- per-model thinking 记忆：R-Code 的自然落点是 task.inference（已有持久化），全局默认 + per-provider 预设可挂到 config.json（宿主 SettingsService），不必新建文件。

## 6. pi 调研小结

（§1–§5 为 pi-mono 调研与 v2 路线；Claude Code 调研与原型评审见 §7–§8。）

## 7. Claude Code UI 调研（2026-09-02 增补）

### 7.1 视觉语言（terminal-config 官方文档 + 社区取证）

- **输入框**：`╭─╮ ╰─╯` 圆角字符边框，边框色随模式切换（主题令牌 `promptBorder`/`bashBorder`/`planMode`/`autoAccept`）；`ultrathink` 关键词彩虹渲染（shimmer 渐变令牌）。
- **transcript 符号系统**：`⏺ Tool(args)` 动作行（粗体工具名）+ `⎿ 结果摘要 · 耗时` 缩进结果行；`✳ Thinking…` 思考行；`✻` 品牌星标。用户消息有**整行背景色**（`userMessageBackground` 令牌）；`!` 命令与 `#` 记忆条目各有背景色（`bashMessageBackgroundColor`/`memoryBackgroundColor`）；消息标签 `You`/`Claude` 有专属令牌（`briefLabelYou`/`briefLabelClaude`）。
- **粘贴折叠**：超 800 字符或 3 行折叠为 `[Pasted text #1 +120 lines]`，原文缓存 `~/.claude/paste-cache/`。
- **子代理**：8 种命名色区分（`<color>_FOR_SUBAGENTS_ONLY`）。
- **spinner**：品牌主色（`claude` 令牌）+ shimmer；底部 footer hints 行（"esc to interrupt"、"? for shortcuts"），配置自定义 status line 后大部分 hints 隐藏。
- **主题**：`~/.claude/themes/*.json`；颜色支持 `#rgb`/`ansi256(n)`/`ansi:<name>`；auto 明暗检测；daltonized 色盲预设；目录热重载。

### 7.2 交互（interactive-mode 官方文档）

- **Shift+Tab 循环的是权限模式**：Manual → acceptEdits → plan → bypassPermissions → auto（不是思考级别）；Windows 非 VT 输入用 `Alt+M` 替代。
- 思考/模型切换在 `Alt+T` / `Alt+P`（也有 `Ctrl+P` 历史翻页与模型混用语境）。
- **Esc 语义分层**：单击=中断/关对话框/权限拒绝；双击=有字→存草稿（↑ 召回）/ 空→rewind 检查点菜单。
- **运行中 Enter = 排队 follow-up**（含 `!` 命令与多数斜杠命令）；Esc 中断后立即发送排队项。
- 空输入 `?` = 快捷键面板；`/` = 命令菜单（全屏渲染下支持鼠标）；`@` = 文件补全（跨会话消息）；`!` = shell 直通（输出进上下文，`Ctrl+B` 后台化）。
- `Ctrl+O` transcript 查看器（时间戳/展开折叠/`{`/`}` 跳提示）；`Ctrl+R` 历史反搜；`Ctrl+G` 外部编辑器；`Ctrl+S` 暂存输入；`Ctrl+C` 一次清空/两次退出。
- 多行输入五种方式（`\`+Enter / Option+Enter / Shift+Enter / Ctrl+J / 粘贴）；vim 模式与 readline flavor 可选。
- **渲染模式**：默认 inline 经典渲染，`/tui fullscreen` 切全屏（`CLAUDE_CODE_NO_FLICKER` 等 env 辅助）；屏幕阅读器模式始终纯滚动文本。

### 7.3 对 v2 的直接采用

| 采纳项 | 来源 | 落点 |
|---|---|---|
| `⏺`/`⎿`/`✳` 符号系统 | claude code transcript | 原型 v3 已采用；替代工具卡边框 |
| 用户消息整行背景带 | `userMessageBackground` | 原型 v3 已采用 |
| 输入框 `╭─╮` 边框 + 模式变色 | `promptBorder` 系令牌 | 原型 v3：ask(dim)/plan(紫)/auto(橙)/bash(绿) |
| 粘贴折叠占位符 | paste-cache 机制 | 原型 v3 已演示 |
| Shift+Tab = 模式循环 | 权限模式循环 | 映射为 r-code TaskMode（Ask→Plan→Auto），思考循环让位 Alt+T |
| `?` 面板 / `/` 菜单 / `!` 直通 | interactive-mode | 原型 v3 已实现（菜单为输入框上方插入式列表） |
| 运行中 Enter 排队 | follow-up 队列 | 对齐宿主已有 queue 语义 |
| hints 行替代 toast | footer hints | toast 的终端形态 = hints 行短暂替换 |

## 8. 原型 v2 → v3 评审记录（按 huashu-design critique-guide）

**结论**：v2 完成了形态定案（inline + fullscreen），但媒介语言失真——按"任务成功度→媒介适配"评审，主要缺陷在媒介适配层。

**优先修复（已在 v3 完成）**：
1. 高 · CSS 圆角卡片/胶囊按钮/浮动 toast/渐变——终端不存在这些 → 全部替换为字符网格语言（box-drawing 框、ANSI 前景/背景色、符号前缀、hints 行）。
2. 高 · 工具卡带边框是网页思维 → `⏺`/`⎿` 符号系统（claude code 同款，信息密度更高）。
3. 中 · Shift+Tab 绑定思考循环与惯例冲突 → 改绑模式循环（r-code TaskMode 语义完全对应），思考挪 Alt+T。
4. 中 · 斜体思考行（SGR 3 终端支持不可靠）→ dim 色。
5. 中 · toast 浮层 → hints 行短暂替换。

**v3 修真细节**：字符框右边界按东亚宽度补格（汉字=2 列），全部框行 vw 验证一致；输入框边框按实测字符宽通栏；braille spinner（`⠋⠙⠹…`）+ 动作文案轮换 + reduce-motion 停帧。

**验证记录**：交互回归（模式循环/思考/模型/斜杠菜单过滤+选择器/帮助面板/发送流式/bash 边框/草稿/粘贴占位）全绿；欢迎框 7 行 vw=58 一致；输入框顶/底边 104 字符对称。

## 9. 交付物索引

- 本报告：`docs/tui-v2/pi-tui-deep-research.md`
- 交互原型 **v4**（codex 风，`docs/tui-v2/tui-v4-prototype.html`，见 §10/§11）；2026-09-02 拍板为唯一保留原型，v3（claude code 风）留档已删除
- **TUI v2 / R-Code CLI PRD（AI 实施清单）**：`docs/tui-v2/r-code-cli-prd.md`（2026-09-02 依据本报告 §4/§5 + §7–§11 定案产出；fullscreen 待拍板点已按"不做独立 fullscreen、ctrl+t 浮层覆盖"收口）
- v1 PRD 归档：`docs/support/archive/pi-alignment/`

## 10. Codex CLI 深度调研（2026-09-01，openai/codex 源码 + insta 快照取证）

**动机**：v3（claude code 风）评审后用户判定"还是太粗糙"，要求直接仿 codex cli。本轮调研对象是 `codex-rs/tui`（Rust+ratatui），取证方式 = 读源码（zread + curl raw）+ 读 insta `.snap` 渲染快照（即逐字符 ground truth），本地参照 codex-cli 0.149.1 / 仓库 main f40e084。快照文件给出的是**精确到每个字符与空格**的真实渲染输出，可信度高于任何二手描述。

### 10.1 会话头框（history_cell/session.rs + session_header 快照）

- 圆角 dim 边框 `╭─╮│╰─╯`，**内宽 ≤ 56**（`SESSION_HEADER_MAX_INNER_WIDTH`），行首 `>_ ` dim + 名称 **bold** + `(vX)` dim。
- `model:` / `directory:` 标签 dim，`padEnd(标签宽)+1 空格`，目录转 `~` 相对路径；行尾 cyan `/model` + dim ` to change`。
- `permissions: YOLO mode` 一行 magenta bold，**仅当** approval=never 且 sandbox 无限制时出现（快照 `session_header_yolo` 实证）。

### 10.2 消息单元（history_cell/messages.rs + 快照）

- **用户消息**：`› ` bold-dim 前缀，续行缩进 2 格；**整段背景带**（`user_message_style`：暗色主题上按白色 12% 混合），**前后空行也带背景**（快照 `user_cell` 实证）。
- **助手消息**：首行 `• ` dim 圆点 + markdown 正文，**无 "codex" 名签**；`code` 用 cyan、bold 保留。
- **推理（reasoning）**：dim+italic，`• ` 圆点；运行中的推理 bold 标题会**提升到状态头**显示。

### 10.3 状态指示器（status_indicator_widget.rs + motion.rs）

- 形态：`• Working (12s • esc to interrupt)`，可附 ` · 内联消息`；明细行前缀 `  └ `（DETAILS_PREFIX）dim，最多 3 行。
- 活动标记是**字符 shimmer**（truecolor 渐变扫过）或闪烁 `•`/`◦` 600ms；"Working" 词本身 shimmer。耗时格式 `0s` / `1m 00s` / `1h 00m 00s`（`fmt_elapsed_compact`），约 80-90ms tick。
- **与 v3 假设的差异**：codex 不用 braille spinner 做状态头（braille 仅出现在个别小件），状态头 = shimmer 圆点。

### 10.4 执行/编辑/计划单元（exec_cell/render.rs + patches.rs + 快照）

- 执行单元：圆点 `•` 成功绿 bold / 失败红 bold / 运行中动画；标题 bold `Running`/`Ran`/`You ran`（用户 `!` 直跑）；命令做 bash 语法高亮；折行续行 `  │ ` dim（最多 2 行）；输出首行 `  └ `、其余 `    `，**输出文本整体 dim**；截断行 `… +N lines (ctrl + t to view transcript)`；无输出显示 `(no output)` dim。
- transcript 模式里：`$ ` magenta + 命令，`✓` 绿 bold / `✗ (exit code)` 红 bold + ` • 耗时` dim；多条合并 `• Ran N commands · ctrl + t to view transcript`；探索合并 `• Exploring/Explored`，动词（Read/List/Search/Run）cyan。
- 编辑单元：`• Edited N files (+9 -9)` bold；每文件 `  └ path (+a -d)`，改名 `old → new`；diff 行号槽 dim + `+` 绿 / `-` 红，hunk 间 `⋮`。
- 计划单元：`• Updated Plan` bold + `  └ 注释`，条目 `✔` 绿（文字 dim）/ `□` 待定，缩进 4。

### 10.5 审批浮层（approval_overlay.rs）

- **内联在底部面板**，不占全屏；菜单面 = 背景带（`render_menu_surface`，左右各内缩 2 列、上下各 1 空行）。
- 标题 bold：`Would you like to run the following command?` / `...make the following edits?` / `...grant these permissions?`；可选 `Reason:` italic 行；`$ ` 命令。
- 选项从 1 编号，选中行 `› ` + **cyan bold**（`accent_style`，**不是反色**），键名后缀 dim。执行审批三选：`Yes, proceed (y)` / `` Yes, and don't ask again for commands that start with `<前缀>` (p) `` / `No, and tell Codex what to do differently (esc)`；补丁变体有 `(a)` 会话级放行。底行 `  Press enter to confirm or esc to cancel`。
- 权限模型：AskForApproval `untrusted/on-failure/on-request/never` × sandbox `read-only/workspace-write/danger-full-access`，组合成预设；`/permissions` 弹层标题 `Update Model Permissions`，当前项标 ` (current)`。

### 10.6 Composer 与 Footer（chat_composer.rs + bottom_pane/footer.rs）

- **Composer 没有边框框**——这是与直觉（v3 做了边框）最大的一次纠偏：整个输入区是**背景带**，`›` bold 在 0 列（禁用时 dim；bash 模式 `!` light-red bold；effort=max 时金黄，ultra 用 `»` + 点火动画）；正文从第 2 列开始；占位 `Ask Codex to do anything` dim；大段粘贴折叠为 `[Pasted Content N chars]`（>1000 字符，`#2`/`#3` 编号）；图片 `[Image #N]`。
- Footer（缩进 2，dim）：空闲 `? for shortcuts` + 右对齐 `{N}% context left`（或 `{x.xK} used`）；有草稿且运行中 `tab to queue message`；plan 模式 `Plan mode (shift+tab to cycle)` magenta；`ctrl + c again to quit`；`esc again to edit previous message`；`reverse-i-search: `；窄宽有坍缩规则。
- `?` 快捷键总览：两列 dim 列表 + 末尾 `customize shortcuts with /keymap`（cyan）。

### 10.7 斜杠弹层与命令体系（command_popup.rs + slash_command.rs + popup_consts.rs）

- 弹层在 composer **上方**，最多 8 行（MAX_POPUP_ROWS），`/名` + dim 描述，选中 cyan bold，Tab 补全，无匹配显示 dim italic `no matches`。
- 命令清单（主线）：/model /ide /permissions /keymap /vim /experimental /approve /memories /skills /import /hooks /review /rename /new /archive /delete /resume /fork /app /init /compact /plan /goal /agents /side(/btw) /copy /export /raw /diff /mention /status /cd /pwd /usage /debug-config /title /statusline /theme /pets /mcp /apps /plugins /logout /quit /feedback /ps /stop /clear /personality /subagents …
- **r-code cli 裁剪建议**：一期只取 /model /permissions /status /new /resume /compact /diff /usage /mcp /theme /quit /clear + `!` bash + `@` 文件提及；其余按 r-code 已有能力逐步映射。

### 10.8 键位（keymap.rs + keymap/bindings.rs）

- 全局：ctrl+t transcript、ctrl+g 外部编辑器、ctrl+o 复制、ctrl+l 清屏、alt+r raw；对话：Esc 打断、alt+, / shift+↓ 降推理、alt+. / shift+↑ 升推理；composer：Enter 提交、Tab 入队、`?` 总览、ctrl+r/s 历史搜索；固定：ctrl+c（打断/二次退出确认）、ctrl+d 退出；列表 ↑↓/ctrl+p/n/enter/esc；审批 y/a/p/d/esc/n/c；/vim 开 vim 模式。
- shift+Tab 循环模式（`MODE_CYCLE_HINT`="shift+tab to cycle"）——与 v3 决策一致，保留。

### 10.9 /status 卡、resume 选择器、transcript 浮层（快照实证）

- /status：圆角框内边距 2，标题 `  >_ OpenAI Codex (vX)`；标签 `padEnd(18)` 对齐，**唯独 `Directory:` 行不对齐**（codex 真实 quirk，原型如实保留）；`Token usage: 1.9K total (1K input + 900 output)`；`Context window: 100% left (2.25K used / 272K)`；限速条 `[██████░░░░░░] 28% left (resets 03:14)`。
- resume：光标用更重的 `❯`（区别于列表 `›`），双行行目，列 `Created at Updated at Branch CWD Conversation`，`↑ more/↓ more`，底行 `enter to resume     esc to start new     ctrl + c to quit     tab to toggle sort`。
- transcript（ctrl+t）：顶行 `/ T R A N S C R I P T / / / /…` dim，执行条目 `$ cmd` + ✓/✗，底部 `↑/↓ to scroll   pgup/pgdn to page   home/end…` 与 `q to quit   esc to edit prev`。
- 排队显示：`• Queued follow-up inputs` bold + 每条 `  ↳ <消息>` dim。

### 10.10 色彩语义（style.rs）

cyan=强调/选中/链接/代码/提示；绿=成功；红=失败/删除；magenta=模式（Plan/YOLO/side/fast）；dim=一切辅助信息；bold=标题；italic=推理/原因/空态；亮色终端主题下 accent=RGB(0,95,135)。**codex 无品牌橙色**——r-code 橙在一期让位于语义色，只在 `›` 提示符的 max-effort 态用金黄（codex 本身如此）。

### 10.11 与 pi/claude 的三方对照（选型定案）

| 维度 | pi tui | claude code | **codex cli（v4 选型）** |
|---|---|---|---|
| 会话头 | 无框文本 | ASCII 欢迎 | **圆角字符框 ≤56 内宽** |
| 用户消息 | 前缀 | `>` 前缀 | **`›` + 整段背景带** |
| 助手消息 | `●` | `⏺` | **`•` + markdown，无名签** |
| 状态头 | spinner 文本 | braille `✳`+动词 | **shimmer `• Working (Ns • esc to interrupt)`** |
| Composer | 边框框 | `────` 分隔线 | **无边框背景带** |
| 审批 | 弹层 | 编号选项 | **内联带面 + `›`+cyan bold 选中** |
| 模式循环 | — | shift+Tab | **shift+Tab（相同）** |
| 验证资产 | 无 | 无 | **insta 快照=逐字符 ground truth** |

codex 胜出的决定性因素：快照文件提供了可机验的渲染基准，且其视觉体系（带面/shimmer/内联审批）在三家中最克制、最"终端原生"。

## 11. v4 原型（仿 codex）评审与验证记录（2026-09-01）

**结论**：v4 按 §10 的源码+快照级 ground truth 整体重写，形态定案从"claude code 风"切换到"codex 风"；v3 留档备查。

**网络调研代理回传滞后于首版 v4**，交叉核对后应用 8 项修正：① composer 改无边框背景带（原实现有边框）；② 执行输出整体 dim；③ 审批改背景带面 + 精确前缀选项措辞 `(p)`；④ 排队格式 `• Queued follow-up inputs` + `  ↳`；⑤ transcript 顶行 `/ T R A N S C R I P T / /…` + 底部 hints + q 关闭；⑥ resume 用 `❯` 光标；⑦ `no matches` dim italic；⑧ max effort 金黄提示符。

**自测抓到的 4 个缺陷与修复**：
1. 高 · 头框右边界错位（边框行 vw=53、内容行 52）：`headerCell()` 纯文本镜像串空格数与 HTML span 不一致 → 修镜像串，复验 6 行 vw=52、像素各 411.3px。
2. 高 · 审批键全死：`openApproval` 里 `ta.disabled=true` 阻断 keydown → 审批键挪到 document 级监听，textarea 只 `blur()`。
3. 高 · Enter 双触发：打开审批的 Enter 冒泡到 document 层立即 `closeApproval(0)` → document 审批分支加 `if(e.defaultPrevented)return;`，全链路复验绿。
4. 中 · effort 弹层未预选当前档（默认落 minimal）→ openModels/openEffort/openPermissions 全部预选当前值（`› high (current)`）。

**验证记录**（IAB 内全绿；本机截图通道不可用——IAB "activity capture failed for guest"、computer-use 不支持此 Windows 主机截屏——以 canvas `measureText` 像素级测量替代）：
- 头框 6 行像素宽各 411.3px（逐像素对齐）；用户消息带与 composer 带均 `rgba(255,255,255,0.08)`、带宽 940px 通栏。
- 两轮连续 demo + 排队消息：reasoning×2、Ran×4、Edited×2、流式回复×2（含 `KeyEventKind::Press` 引用）、队列排空、footer `? for shortcuts`/`85% context left`、状态头结束后隐藏。
- `/mo` 过滤 → 模型弹层 → 选 deepseek-reasoner → effort 弹层预选 `› high (current)`；alt+. → `reasoning effort: xhigh`。
- 审批全链路：带面背景、bold 标题、`›` 光标、↓ 移动、`p` 按前缀放行并出注记、`esc` 拒绝并出错误单元、compwrap 隐藏。
- `?` 总览开合；ctrl+t transcript（`T R A N S C R I P T` 顶行）开合（q/esc）。
- **已知边界**：IAB 字体把 CJK 渲成 ~1.77× 拉丁宽（非 2×），字符框内若放中文会微偏——真实终端 CJK 严格 2 列，且当前框内纯 ASCII，无影响；终端实现按 vw=2 即可。

**采用 vs 适配决策**：r-code MODES 四档映射 codex 权限预设（Ask before edits / Plan mode / Workspace Write (auto) / Full Access (YOLO)）；占位文案用 `Ask R-Code to do anything`；codex 无 pi 式 F10 全屏切换——全屏能力由 transcript 浮层（ctrl+t）承担，**2026-09-02 拍板：默认 inline 不变，不做独立 fullscreen 模式**，"全屏"语义由 transcript/编辑器浮层覆盖。

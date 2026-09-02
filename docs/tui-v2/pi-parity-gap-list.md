# R-Code CLI ↔ pi tui 对照缺口清单（2026-09-03）

> 取证：badlogic/pi-mono（现 earendil-works/pi）main 分支源码调研（子代理 52 次工具调用，
> 关键文件：`packages/coding-agent/src/modes/interactive/interactive-mode.ts`、
> `core/slash-commands.ts`、`core/settings-manager.ts`、`core/auth-storage.ts`、
> `packages/tui/src/*`）。本清单用于用户拍板下一轮范围，不是已实施记录。

## 0. 本轮已修复（2026-09-03，用户实测三症状）

| 症状 | 根因 | 修复 | 回归测试 |
|---|---|---|---|
| 随便输入把上面的内容顶掉 | `InlineRenderer::frame` 帧间光标锚点 off-by-one：`cursor_to_live(0,·)` 把光标停在输入行，frame 仍按"块尾下一行"上移 `live_height` → 每帧上漂 1 行逐行吃历史 | 上移量改 `live_height-1`（锚点=输入行） | `inline_history::typing_does_not_rewrite_history_above`（PTY+R_CODE_TUI_RECORD 原始字节+迷你 VT 模型） |
| 上滑看不到历史 | 同一上漂 bug：历史被覆写、ED 清屏；scrollback 里从未真正沉淀 | 同上 + 证明 commit 行逐字落 scrollback | `startup_does_not_bulk_scroll`、`history_survives_in_scrollback` |
| /model 没有可配置流程 | 无配置时选择器空集 = 死端提示 | 新增 `/setup` 两步向导（选预设[过滤]→输 API key[掩码]→写 config+平台凭据+设默认）；`/model` 空集自动转进向导；引导文案加第三途径 | `setup_flow` 单测×4（隔离凭据后端）+ `setup_flow_reachable_and_cancellable` PTY e2e |

诊断基建（本轮沉淀）：`R_CODE_TUI_RECORD=<file>` 字节级输出记录（ConPTY 会重合成输出流，
流层面不可归因）；PTY 读线程+channel 模式（阻塞 read 会绕过 deadline）。

## 1. 已对齐 pi 的部分（无需动作）

斜杠命令核心集（/model /thinking /new /resume /rename /compact /clear /help /quit）；
多行编辑器（undo/词导航/grapheme）；粘贴折叠；队列 follow-up（alt+enter 语义我们用 tab）；
`!` 直通与 `@` 文件提及；外部编辑器 ctrl+g；transcript 浮层；footer 用量统计；
掩码 key 输入（pi 的 Input 组件反而**不支持 mask**，我们比 pi 强）；/setup 对应 pi 的
/login API-key 分支（pi 另有 OAuth，见缺口）。

## 2. 缺口清单（按用户价值排序）

### P0 — 日常可用性（✅ 2026-09-03 M7 已全部落地）

| # | 缺口 | 落地方式 |
|---|---|---|
| G1 ✅ | 模型选择器直开热键 | ctrl+l 直开选择器；空集自动转 /setup 向导（斜杠菜单/浮层/transcript 的 ↑↓ 拦截同步适配） |
| G2 ✅ | /model 双语义 | 选择器内 **Enter=本次会话**（task_set_provider/model）、**Ctrl+S=设为全局默认**（写 config default_provider + 该服务默认模型，save_global）；浮层首行提示两键语义 |
| G3 ✅ | 历史导航边界 | ↑/↓ 改为**先做垂直光标移动**（保持 char 列、目标行短钳行尾；InputBuffer::move_up/down + 边界判定），仅在首行/末行边界翻历史；ctrl+p/n 保持无条件翻（shell 惯例） |
| G4 ✅ | auth 检查 CLI | `r-code-tui auth check [--data-dir]`：逐 provider 打印认证状态，默认服务已认证 exit 0 否则 exit 1（口径与 /model 选择器同源 build_snapshot） |
| G5 ✅ | /compact [prompt] | 接线宿主已有 `task_compact_context(state, task, focus)`——focus 即自定义压缩指令；结果行进 transcript（N→M 条消息/低于阈值/错误）；`compaction_supported()` 翻 true |

### P1 — 高价值功能面

| # | 缺口 | pi 证据 | 工作量估 |
|---|---|---|---|
| G6 | **图片支持**：剪贴板粘贴图片 + `@file` 图片附件 + kitty/iterm2 内联渲染 | `terminal-image.ts`、`cli/file-processor.ts` | 大 |
| G7 | **/export /import /copy /share**（会话 HTML/JSONL 导出） | slash-commands.ts | 中（JSONL 已有，序列化+写盘） |
| G8 | **会话树 /tree + /fork /clone**（分支跳转、标签） | interactive-mode.ts | 大 |
| G9 | **/session 统计卡**（文件/ID/消息数/token/成本） | slash-commands.ts | 小（/status 部分覆盖，补会话维度） |
| G10 | **OAuth 登录流**（pi /login 账号订阅路线；设备码/浏览器回调） | `oauth-selector.ts`、`login-dialog.ts` | 大（依赖 provider 侧 OAuth 支持） |
| G11 | **环境变量 auth 解析**（pi：auth.json → 环境变量回退；我们 config `$ENV:VAR` 已支持引用，但 /setup 不提供 env-var-only 模式） | `auth/helpers.ts` `envApiKeyAuth()` | 小 |

### P2 — 外围/生态（远期，先记录不做）

主题系统、键位自定义文件 + /reload、TypeScript 扩展体系、skills/prompt 模板命令、
/debug 彩蛋、自更新、MCP（pi 也没有——**我们宿主有 McpManager，TUI 侧反而是潜在优势项**）。

## 3. 建议节奏

- ~~**M7**：P0 全部（G1-G5）~~ → **已完成（2026-09-03）**：ctrl+l / ctrl+s 双语义 / ↑↓ 边界导航 / auth check / /compact [prompt]，全量测试+clippy+门禁 78/78 绿。
- **M8（候选）**：G7 + G9 + G11（导出/统计/env-auth，中件）。
- **专项**：G6 图片、G8 会话树、G10 OAuth 各自单独立项（大件，先出 PoC）。

## 4. 与 pi 的架构差异备忘（不是缺口，是选型差异）

- pi 会话存储明文 JSONL + 0600；我们 JSONL 走 SessionStore + SQLite 索引（GUI 互通是硬需求）。
- pi 无 MCP/无 subagents 内建（subagents 是示例扩展）；我们宿主两者都有——TUI 侧的暴露是增量机会。
- pi 渲染为自研 canvas 行差分；我们 commit/live 双区（本轮修复后语义等价：历史 append-only 进 scrollback）。

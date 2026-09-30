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

### P1 — 高价值功能面（✅ 2026-09-02 M8 已落地 G7/G9/G11；✅ 2026-09-03 专项收口 G6/G8/G10）

| # | 缺口 | pi 证据 | 工作量估 |
|---|---|---|---|
| G6 ✅ | **图片支持**（2026-09-03 专项）：① **Ctrl+V 读系统剪贴板图片**（Windows 走 CF_PNG/CF_DIBV5/CF_DIB——DIB 手动解析 24/32bpp + 全零 alpha 按不透明；macOS 走 `osascript` PNGf hex；Linux 走 wl-paste/xclip；bracketed paste 只能传文本，图片字节必须走 OS 剪贴板）；② **`@file` 图片提及**（白名单扩展名 + 中英文标点分隔符容错，发送时读文件入附件）；③ **transcript 半块 ANSI 预览**（pi terminal-image 形态：`▀` 上前景/下背景 truecolor、48×16 行内等比缩放永不放大、透明合成黑底——字符网格原生，零 kitty/sixel 终端依赖）。发送走宿主既有附件管线 `agent_send_with_mode_and_attachments`（魔数校验/主模型无 vision 时 OCR 转换/排队持久化），单图 8MiB 上限与宿主一致；agent-contracts 契约层零改动 | `terminal-image.ts`、`cli/file-processor.ts` | 已落地 |
| G7 ✅ | **/export /copy**（M8）：`/export [路径]` 按扩展名导出 `.md`（默认）/`.html`（单文件自包含）/`.jsonl`（TranscriptRow 原生序列化）；`/copy` 复制最后一条回复（OSC 52 终端剪贴板，零依赖、SSH 生效、64KiB 上限）；附带修复 ShellRow serde tag 与 TranscriptRow 撞名（`kind`→`shell_kind`）导致 JSONL 无法反序列化的缺陷。pi 的 /import /share 未做（导入走既有 resume 链路即可，share 无服务端载体） | `slash-commands.ts` | 已落地 |
| G8 ✅ | **会话树 /tree + /fork /clone**（2026-09-03 专项）：**/fork** = 消息级分叉（选择器列活跃分支 user 消息 → 文本回填编辑器可改写 → `agent_resend` 前缀复制 + 新分支激活 + 重发，原分支保留）；**/tree** = 分支树导航（宿主新增 `session_branch_list` + `task_switch_branch` 薄命令；树形缩进渲染、❯ 光标、活跃标记、分叉锚点注记；切换后从 JSONL 重放前缀）；**/clone** = 克隆当前会话为新任务（宿主 `task_clone`：活跃分支整文件复制 + Meta 改指 + 模型绑定随行，源会话不动）。附带修复两个存量缺陷：**/resume //new 此前只提示不切换**（task 句柄改 `Arc<Mutex<String>>` 动态读取 + `adopt_task` 真正装回：切换句柄 → JSONL 重建 transcript → footer 投影刷新）；**浮层闪关**（Windows Press+Release 双事件下 Release→`KeyAction::Ignore` 落进浮层 `_ => close` 兜底，浮层被打开键的 Release 瞬间关闭——PTY 取证靠 R_CODE_TUI_RECORD 应用侧字节流才定位，ConPTY 同步更新合并让闪关内容在流里不可见） | `interactive-mode.ts` | 已落地 |
| G9 ✅ | **/session 统计卡**（M8）：与 /status 同款圆角框——id（截断）/标题/模型/创建时间/消息计数（user·assistant·tool，System/Shell 不计）/runs/token/成本/JSONL 会话文件路径（最近 run 的 external_session_id 解析；未发送过显示"未落盘"） | `slash-commands.ts` | 已落地 |
| G10 ✅(收窄) | **OAuth 登录流 → /login Codex 委托登录**（2026-09-03 拍板收窄）：调研结论——30 个 provider 预设无一提供第三方 TUI 可用的 OAuth/device-code 端点（Anthropic OAuth 需官方客户端白名单有 ToS 风险；OpenRouter/DeepSeek/国内厂全部 API key；pi 的 oauth-selector 证据也仅组件名级别），**通用 OAuth 做了就是 dead code（踩禁 mock 红线）**。落地形态：`/login` 接线宿主**已存在的真实 OAuth 通道**——Codex CLI 委托登录（`codex_start_login` 浏览器 / `codex_start_device_login` 设备码，新开系统终端窗口完成 OAuth 交互，不读登录输出不碰 auth.json）+ 后台 5s 轮询登录完成自动确认 + 刷新状态；其余厂商浮层里诚实引导 `/setup`（Tab 可切环境变量鉴权），不出现假 OAuth 选项 | `oauth-selector.ts`、`login-dialog.ts` | 已落地（收窄） |
| G11 ✅ | **环境变量 auth 模式**（M8）：/setup key 步 **Tab 切换环境变量鉴权**——空密钥落盘（不触碰平台凭据后端），加载链由宿主既有 `settings::apply_env` 回填（厂商别名 `ANTHROPIC_API_KEY`/`OPENAI_API_KEY`/`DEEPSEEK_API_KEY` + profile 作用域 `R_CODE_PROVIDER_<ID>_API_KEY`）；渲染展示变量清单与当前设置态；pi `envApiKeyAuth()` 同款语义 | `auth/helpers.ts` `envApiKeyAuth()` | 已落地 |

### P2 — 外围/生态（远期，先记录不做）

主题系统、键位自定义文件 + /reload、TypeScript 扩展体系、skills/prompt 模板命令、
/debug 彩蛋、自更新、MCP（pi 也没有——**我们宿主有 McpManager，TUI 侧反而是潜在优势项**）。

## 3. 建议节奏

- ~~**M7**：P0 全部（G1-G5）~~ → **已完成（2026-09-03）**：ctrl+l / ctrl+s 双语义 / ↑↓ 边界导航 / auth check / /compact [prompt]，全量测试+clippy+门禁 78/78 绿。
- ~~**M8**：G7 + G9 + G11~~ → **已完成（2026-09-02）**：/export（md/html/jsonl）/copy（OSC 52）/session 统计卡/setup 环境变量鉴权模式；测试 104 单测 + 11 PTY e2e 全绿，clippy -D warnings 零告警。
- ~~**专项**：G6 图片、G8 会话树、G10 OAuth~~ → **已完成（2026-09-03）**：G6（剪贴板 Ctrl+V + @file 图片 + 半块预览 + 宿主附件管线复用）、G8（/tree /fork /clone + /resume //new 真切换修复 + 浮层闪关修复 + 事件按当前任务过滤 + 视图代际清屏重排）、G10（收窄为 Codex 委托 /login，通用 OAuth 判不可行并记录理由）。TUI 134 单测 + PTY e2e 全绿，宿主 746 单测全绿，clippy -D warnings 零告警。

## 4. 与 pi 的架构差异备忘（不是缺口，是选型差异）

- pi 会话存储明文 JSONL + 0600；我们 JSONL 走 SessionStore + SQLite 索引（GUI 互通是硬需求）。
- pi 无 MCP/无 subagents 内建（subagents 是示例扩展）；我们宿主两者都有——TUI 侧的暴露是增量机会。
- pi 渲染为自研 canvas 行差分；我们 commit/live 双区（本轮修复后语义等价：历史 append-only 进 scrollback）。

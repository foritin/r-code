# 联网工具与 MCP

本文说明 R-Code 当前的联网与 Model Context Protocol（MCP）能力、配置方式、安全边界和故障恢复。用户入口位于“知识与指令 → 联网与 MCP”。

## 能力分层

R-Code 把普通联网和外部 MCP 分成两层：

| 能力 | 默认策略 | 用途 |
| --- | --- | --- |
| 原生 `web_search` / `web_fetch` | 优先使用 | 一般检索、打开公开网页、核对即时信息 |
| 内置 `r-code-research` MCP | 默认开启，可关闭 | 用户明确要求深度、完整、多来源调研时进行多查询汇总 |
| 用户或 Registry MCP | 默认不自动启用 | 专业数据源、认证服务或原生工具无法完成的能力 |

已启用服务完成 `tools/list` 后，其工具会按 `mcp__<服务>__<工具>` 直接加入模型工具目录，并沿用 MCP 返回的真实说明和输入 schema。`mcp_call` 继续保留，作为旧会话、异常工具名和目录暂不可用时的兼容兜底。

这一优先级属于不可被可编辑 Prompt 覆盖的运行时策略。主 Agent 可以搜索官方 Registry，并为一个精确条目生成“安装”或“启用”确认卡，但准备动作本身不会写配置、启用服务或启动进程。真正操作仍由用户点击确认；Plan、只读模式和子代理不能生成这类全局配置动作。推荐卡仍可打开准确的 MCP 条目或市场搜索，并保留返回原对话的导航入口。

原生搜索当前使用无需 API key 的 Jina 搜索入口。搜索和抓取均返回来源 URL 与获取时间；深度调研不会把“模型记忆”当作实时事实来源。

## 使用管理页

### 内置服务

`r-code-research` 随应用配置创建，默认开启，但启动仍是惰性的：桌面外壳启动时不会建立外部连接；第一次真实 Agent 运行会自动发现已启用服务的工具，测试连接或实际调用也会创建会话。关闭后，下一次 Agent 调用会得到普通的“服务已关闭”结果，不会中断整段对话。

### 添加自定义 MCP

支持两种传输：

- `stdio`：填写一个可执行文件、独立参数和环境变量名；
- `streamable HTTP`：填写 HTTPS 地址和请求头名称。

敏感值不要写进命令参数、URL 或普通配置。环境变量和请求头只保存“凭据引用”；macOS 将实际值写入当前用户 profile 的本地加密文件且不访问 Keychain，Windows/Linux 写入系统凭据库。读取接口只返回“已配置/未配置”。

自定义服务保存后默认关闭。首次启用前，R-Code 会完整展示待执行的可执行文件、每一个参数以及环境变量名，确认令牌在短时间后失效且只能使用一次。启动形态发生变化后，旧确认自动失效，服务也不会沿用旧的启动授权。

启用成功后 R-Code 会自动请求 `tools/list` 并更新模型工具目录，不需要再手动点击“测试连接”。应用重启后的第一次 Agent 运行也会自动重建内存目录；多个服务并行发现，单个服务的目录握手最多等待 15 秒。服务离线只会隐藏该服务的直连工具，不会阻断原生工具或其他 MCP。自动发现失败后会短暂退避，避免每条消息重复等待同一个离线服务；“测试连接”和重新启用始终可以立即重试。

### MCP 市场

市场读取官方 MCP Registry 的 `v0.1` API，不需要 API key。Registry 目前是预览服务，条目没有经过 R-Code 或 Registry 的安全审核，因此 UI 会持续显示风险提示、来源、版本、仓库和精确启动方案。

市场流程分两次授权，设置页和 Agent 确认卡共用同一套后端校验：

1. “添加”只在用户确认精确方案后写入本机配置，不启动进程，安装结果保持关闭；
2. 首次“启用”再次确认当前精确方案，然后才允许惰性启动。

R-Code 不会在浏览市场、搜索、添加配置或应用启动时运行 Registry 条目。Registry 查询结果在应用数据目录缓存一小时；网络失败时可显示最近缓存，并明确标记为过期数据。

Agent 不能把任意命令、URL 或完整 Registry 对象当作安装方案提交。`mcp_prepare_install` 会在后端重新查询 Registry，并按服务名与版本精确匹配，再由后端构造启动方案和五分钟内有效的一次性确认令牌。Registry 的标题、描述和仓库字段始终按不可信数据处理，不能覆盖系统策略或变成 Agent 指令。

## 权限与审计

Agent 始终看到固定的 MCP 控制工具；已启用且发现成功的服务还会发布有真实 schema 的直连工具：

- `mcp_discover` 只查询本机已配置目录，不访问 Registry，并会自动补齐尚未加载的已启用服务目录；
- `mcp__<服务>__<工具>` 直接调用对应 MCP 工具；名称过长或包含 Provider 不接受的字符时，R-Code 会生成带稳定摘要的安全名称并在进程内保留准确路由；
- `mcp_registry_search` 只搜索官方预览 Registry，并返回有界、脱敏后的候选摘要；
- `mcp_prepare_install` 与 `mcp_prepare_enable` 只生成确认动作，不执行安装或启用；二者只对主 Agent 模式可见，Plan、严格只读和所有子代理均被运行时边界拒绝；
- 直连工具和 `mcp_call` 在每次调用前都会重新检查开关状态；
- 所有第三方 MCP 调用一律按 R2 处理；服务返回的 `annotations.readOnlyHint` 只用于展示，不能降低授权要求；
- Plan 与严格只读策略不会向模型暴露直连工具或 generic `mcp_call`，即使构造旧工具调用也会在执行边界拒绝；原生 `web_search` / `web_fetch` 仍按各自受信 Host 的 R1 策略处理；
- 实际调用进入与内置工具相同的权限判断和审计记录；
- 未知服务、未知工具、关闭状态和连接失败均采用失败关闭，不会隐式换用另一个服务。

MCP 返回的工具说明、输入 schema 文本和调用结果都按不可信外部数据处理，不能覆盖系统策略、权限边界或用户请求。模型工具名限制为各 Provider 都能接受的 ASCII 字符和 64 字节以内；原始服务与工具名不会因为缩短而丢失路由精度。

Agent 不具备读取或填写 MCP 凭据的工具。确认动作只包含服务标识、来源、精确可执行文件/参数或 HTTPS 地址，以及环境变量名/请求头名；实际值只由用户在凭据编辑器中填写并保存到平台凭据后端（macOS 本地加密文件，Windows/Linux 系统凭据库）。

原生网页访问只允许公开 HTTP(S) 地址，并执行 DNS/IP 检查、逐跳重定向复查、响应类型和大小限制。跨来源重定向会移除认证、Cookie 与订阅类请求头，避免凭据被带到其他站点。

## R-Code 作为 MCP Server

R-Code 也能反向通过 stdio MCP 暴露给 Codex 或其他外部模型。独立入口为 `r-code-host mcp-server --data-dir <应用数据目录>`；设置中的 Codex 协作安装流程会使用 `codex mcp add r-code -- ...` 注册当前签名的独立 host，不依赖桌面窗口持续运行。

该服务公开五个模型可直接调用的工具：

- `r_code_delegate`：创建默认只读、可显式申请完整访问的 R-Code 子任务；
- `r_code_delegate_readonly`：只读兼容入口；
- `r_code_task_status`：读取本 MCP 进程创建的任务状态；
- `r_code_wait_for_result`：有界等待结果；
- `r_code_cancel_task`：取消仍在运行的任务。

注册状态可用 `codex mcp list` 和 `codex mcp get r-code` 检查。Codex 通常在进程启动时加载 MCP 配置，因此首次安装或重新注册后应重启 Codex；已经打开的任务不会凭空获得新工具。R-Code MCP 进程只接受自己创建的任务 ID，完整访问还必须携带明确授权并继续服从工作区权限策略。

## 本地数据与隐私

非敏感配置和 Registry 缓存位于操作系统的 R-Code 应用数据目录：

- `config/mcp-servers.toml`：服务 ID、开关、传输、参数、凭据引用和启动确认指纹；
- `config/mcp-registry-cache.json`：Registry 搜索缓存；
- macOS `config/credentials/` 本地加密文件，或 Windows/Linux 系统凭据库：环境变量和 HTTP 请求头的实际敏感值。

模型直连工具目录只保存在当前进程内，由已启用服务自动重建；不会把第三方 schema 写进项目，也不会把凭据混入目录缓存。

这些文件不会写入项目目录。支持包只包含服务 ID、传输类型、开关、运行状态和归类后的错误，不包含命令、参数、URL、请求头名、环境变量名、凭据引用或凭据值。支持包仍可能包含其他诊断路径，分享前应先预览。

联网工具会把查询或 URL 发给对应搜索/网页服务；远程 MCP 会把工具参数发给其服务运营方；本地 stdio MCP 是本机独立进程，能够做什么取决于该程序本身。安装或启用前应核对发布者、代码仓库、数据处理条款和所请求的凭据。

## Windows 与 macOS

### Windows

R-Code 使用参数化进程 API，不拼接 shell 命令。`.bat`、`.cmd`、`.ps1`、`cmd.exe` 和 PowerShell 不能作为自定义 MCP 启动器，因为它们可能把参数重新解释为命令。

官方 Registry 中常见的 npm 方案使用 `npx`。在 Windows 上，R-Code 会在生成确认方案时解析本机 Node.js 安装，并转换为准确的：

```text
node.exe <绝对路径>/node_modules/npm/bin/npx-cli.js <Registry 参数...>
```

确认窗口展示的是转换后的实际启动形态。若找不到 `node.exe` 或 `npx-cli.js`，先修复 Node.js/npm 安装；R-Code 不会退回不安全的 `.cmd` 启动。

### macOS

从 Finder 启动的 GUI 应用不一定继承交互式 shell 的完整 `PATH`。如果 `uvx`、`npx` 或自定义服务在终端可用、但 R-Code 报“无法启动进程”，请在配置中使用原生可执行文件的绝对路径，常见 Homebrew 前缀为 `/opt/homebrew/bin`（Apple Silicon）或 `/usr/local/bin`（Intel）。参数仍应逐项填写，不要把整条命令放入一个字段。

## 故障恢复

| 状态或错误 | 处理方式 |
| --- | --- |
| 已关闭 | 在 MCP 管理页手动开启；当前对话无需重建 |
| 待确认 | 核对完整启动方案并确认；方案改变后需重新确认 |
| 缺少凭据 | 打开“凭据”，填写缺失字段；保存后输入框会立即清空 |
| 无法启动进程 | 检查原生可执行文件和 GUI `PATH`；Windows 不要使用脚本 shim |
| 初始化/协议失败 | 先“测试连接”，核对 MCP 版本、传输类型和服务日志 |
| 服务已启用但模型没有直连工具 | 开始下一轮真实 Agent 运行或点击“测试连接”刷新；若是刚注册给 Codex，则重启 Codex 进程 |
| Registry 离线 | 使用带“缓存”标记的结果，或恢复网络后刷新 |
| 远程 HTTP 失败 | 确认使用 HTTPS、地址无内嵌账号密码且凭据仍有效 |

关闭、编辑或删除服务会先结束已有会话。应用退出时 supervisor 会执行有界关闭；关闭失败会归类为状态错误，但不会把凭据写入日志或支持包。

## 维护者入口

- 协议、配置和客户端：`crates/r-code-mcp/`
- Host 管理器与持久化：`src-tauri/src/mcp_manager.rs`、`mcp_settings.rs`
- Agent 固定控制工具与动态 MCP schema：`crates/r-code-mcp/src/host.rs`、`src-tauri/src/mcp_manager.rs`
- Agent 系统策略与 Plan/子代理边界：`crates/r-code-agent-worker/src/llm_runtime.rs`
- 管理 UI：`src-tauri/frontend/src/components/scenes/McpPanel.tsx`
- 离线端到端合同：`src-tauri/tests/r9_e2e_tests.rs`
- 前端交互回归：`src-tauri/frontend/scripts/mcp-management.test.mjs`

新增传输或市场来源时，必须继续满足：无自动安装/启用、精确方案确认、凭据只存平台凭据后端、每次调用重检开关、统一权限审计、支持包严格白名单。

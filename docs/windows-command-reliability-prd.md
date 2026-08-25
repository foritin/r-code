# Windows 命令执行可靠性与 Codex 执行质量 PRD / AI 实施清单

> 文档状态：`frozen`（只表示执行合同已完整，不表示产品功能已经实现）
> 执行合同：`prd-to-ai-worklist` v1.1.0
> 取证基线：2026-08-25 本机会话存储（`%APPDATA%/com.r-code.app[r-code]/r-code`，dev 库 673 条 Codex 命令 + 143 条原生 bash 调用）
> 行业参照：Claude Code（Git Bash 硬依赖）、openai/codex@master（`shell_detect.rs`/`shell_snapshot.rs`/`powershell.rs`）、gemini-cli #18022/#3126/#15493（PowerShell 路线反面样本）
> 固化清单：[`windows-command-reliability-freeze.yaml`](./windows-command-reliability-freeze.yaml)
> 唯一完成状态：本文 §8 主 Checklist；任务卡、任务包与证据不得维护第二套 Checkbox

## 执行导航

- 首次执行：§0 → §2 → §4 → §7 → §8 → §9 的首个 ready 任务。
- 中断恢复：`artifacts/ai-tasks/current.yaml` → §10 对应任务卡 → `artifacts/ai-tasks/evidence/windows-reliability/`。
- 判断完成：§7 统一 Harness → §9 断言 → `artifacts/ai-tasks/verification/windows-reliability/`。
- 产品终态与非目标：§1。
- 不可变决策：§2。
- 机器合同（解析链/环境合成/诊断/金集/设置键）：§4。
- 验收与里程碑：§7。

## 0. AI 执行入口

<!-- AI_WORKLIST_VOLATILE_START -->

- 当前进度：`0 / 12` 项完成。
- 下一执行项：`M0-01`（建立统一验证 Harness 与金集语料）。
- 当前任务包：`artifacts/ai-tasks/current.yaml`（该文件当前属于已完成的 codex-rich-interaction 清单；本项目首次启动时按 §10 模板重建，不保留其内容）。
- 注意：工作区存在未跟踪文件 `NUL`（Windows 设备名残留），视为用户资产，任何任务不得尝试 add/提交/删除它。

<!-- AI_WORKLIST_VOLATILE_END -->

### 0.1 首次启动

1. 只读检查 Git revision、完整 worktree、Rust/Node 运行时、本机 Git Bash 探测结果和现有测试基线；已有未提交改动一律视为用户资产。
2. 读取本节、§2、§4、§7、§8 和首个 ready 任务卡，不需要每轮重读全文。
3. 从编号最小且依赖已通过的未完成 MUST 任务开始；建立 `current.yaml` 后直接进入实现，不在里程碑边界等待人工确认。
4. 每个可验证子步更新任务包；断言和累计门禁均通过、证据真实存在后，才能勾选 §8 中唯一 Checkbox。

### 0.2 续跑

1. 读取 `current.yaml`、对应任务卡和已归档证据。
2. 核对 `changed_paths` 与真实 worktree；对已完成断言运行最小 smoke。
3. 从首个未完成 step 或 assertion 继续，不重复创建 Harness、fixture、语料或第二套验收。
4. 若任务包与代码不一致，以代码、测试和可访问证据为准修正任务包，不能凭状态文件宣称完成。

### 0.3 授权与中断边界

- 允许：仓库内可逆的源码、测试、文档、fixture、验证脚本修改；注册表**只读**访问；测试仅可写入并清理 HKCU 下的临时测试子键；金集在临时目录工作区内真实执行 bounded 命令。
- 不允许：提交、推送、发布；写入用户业务注册表键；修改用户全局 Git Bash/codex 安装；删除未跟踪用户文件（含 `NUL`）；放宽 §2 安全红线。
- 只有扩大权限/范围、需要不可获得的真实凭据（如真实 Codex 账号的 ≥92% 链路复测）、执行不可逆生产动作，或两条同优先级要求会改变产品语义且无法由事实消解时，才请求用户。
- Windows 与 macOS 是同一功能合同；平台差异只能留在 shell 解析、注册表访问和系统集成 adapter 中。
- 实施纪律：预期 >1 分钟的命令（全量 cargo test、金集 slow 档）前台运行并显式落日志文件，不使用后台模式。

<!-- AI_WORKLIST_NORMATIVE_START -->

## 1. 背景、目标、终态与非目标

### 1.1 已确认问题

**主观痛点**："Windows 下命令执行成功率一直比较低，尤其是 Codex CLI 作为子代理时。"

**取证数据（2026-08-25，本机 dev 库）**：

| 链路 | 样本 | 成功率 | 失败构成 |
|---|---|---|---|
| 原生 `bash` 工具 | 143 | **96.5%**（138 ok） | 5 条 error 全部是命令真实失败（测试/构建非零退出），**零方言类失败** |
| Codex 链路（App Server `commandExecution` 投影） | 673 | **87.1%**（586 ok） | 87 error：75 真实测试/构建失败、5 条 PowerShell `ParserError`/调用层方言失败、4 条 `blocked by policy`、3 条超时 |

关键交叉证据：

- **157 / 673（23.3%）条 Codex 命令文本含 bash 式引号拼接**（形如 `'$p='"'D:\path'"`）——模型（含 codex 侧模型）在为 PowerShell 书写命令时使用 bash 引号拼接习惯，这正是 R-Code 主代理用 `.ps1 + -File` 已经免疫、而 codex 内部 `pwsh -Command` 路线无法免疫的引号重解析陷阱。
- 4 条 `blocked by policy` 拒掉的恰是引号拼接畸形命令（codex 策略解析器无法理解 → 拒绝）。
- 仅 2 条 `not recognized`，且均为**相对路径 `.exe` 直接调用**（PowerShell 需要 `&` 调用操作符）——属方言问题，**PATH 瘦身假设被排除**。
- 本机已装 pwsh 7（`C:\Program Files\PowerShell\7\pwsh.exe`）——"PowerShell 5.1 `&&` 陷阱"假设在本机不成立（仍需为无 pwsh 7 的机器设防）。

**归因结论**（按权重）：

1. **方言错配**：bash 语料 ≫ PowerShell 语料的模型，在 PowerShell 解释器上书写命令；R-Code 主代理靠"文件操作分流到进程内工具 + `.ps1` 暂存"免疫，codex 子进程内部无此免疫，R-Code 转发层也无处插手（codex 的 `shell_detect.rs` 在 Windows 自主选择 `pwsh → powershell → cmd`，无公开配置键可覆盖）。
2. **Codex 子代理固定降智**：`commands.rs:20098` 固定 `model_reasoning_effort="medium"`，命令书写质量对推理档位敏感。
3. **委派默认只读 + never 审批的显性度不足**：写类命令被拒时用户与模型都难以察觉是权限档位问题。

### 1.2 规范性需求

- **R-SHELL-01（MUST）**：Windows 上 `bash` 工具以 **Git Bash 为第一方言**。解析链固定为：设置覆盖（`execution.bash_shell_path`）→ 已知安装位置（`Program Files\Git\bin\bash.exe`、x86、`%LOCALAPPDATA%\Programs\Git\bin\bash.exe`、scoop）→ 由 PATH 上 `git.exe` 反推 `<git根>\bin\bash.exe` → PATH 上 `bash.exe`（**显式排除 `C:\Windows\System32\bash.exe`（WSL 启动器）**）→ 回落现有 PowerShell 链。
- **R-SHELL-02（MUST）**：PowerShell 回落路径保留全部现有加固（`.ps1` 暂存 + `-File`、UTF-8 前缀、`$LASTEXITCODE` 透传），并改为 pwsh 7 优先于 powershell.exe 5.1。
- **R-SHELL-03（MUST）**：Git Bash 执行环境治理：`bash -c` 直接 argv 传命令（不经临时脚本）、不加载 login profile、设置 `MSYS_NO_PATHCONV=1`、`LANG=C.UTF-8`；`UNIX_ONLY_HINTS` 前置拦截仅在 PowerShell/cmd 回落档生效；工具描述按档位声明当前方言与语法约束。
- **R-ENV-01（MUST）**：Windows 子进程环境继承完整方案：从注册表（HKLM 系统 + HKCU 用户，`REG_EXPAND_SZ` 展开）合成**实时 PATH**，替代进程继承的陈旧 PATH；进程 PATH 中不在注册表集合内的条目按原序追加；应用于 `bash` 工具子进程与 Codex 子进程（exec 与 app-server 两条拉起路径），与 RTK 前缀单次拼装。
- **R-DX-01（MUST）**：失败诊断引擎：`bash` 工具与 Codex `commandExecution` 错误输出经模式分类后追加有界"诊断提示"（`ParserError` + bash 残留语法 → 等价写法；相对路径 `.exe` 报错 → `&` 调用符指引；`blocked by policy` → 权限档位说明与 `full_access` 途径；`not recognized`/`command not found` → 安装/PATH 建议）。提示只追加、不改写原始输出，长度有界（≤400 字符），仅匹配错误签名不回显正文。
- **R-CDX-01（MUST）**：Codex 委派提示模板在 Windows 下注入**命令书写规约**（有界 ≤300 字符）：单一简单命令优先、双引号、禁止 bash 式引号拼接与 `'…'"$var"'"…'` 插值、相对路径可执行文件用 `&` 调用、路径分隔符统一；Unix 不注入。
- **R-CDX-02（MUST）**：移除 Codex 子代理固定 `model_reasoning_effort="medium"` 覆盖，改为继承（不传覆盖，使用 codex 自身默认/用户 config）；`web_search="disabled"` 保持；新增可选宿主设置 `codex.subagent_reasoning_effort` 供显式收紧。
- **R-CDX-03（MUST）**：委派访问档位显性化：`delegate_task` 工具描述明示 `full_access` 参数语义；Codex 子代理被 policy 拒绝 ≥2 次时，R-Code 在子代理事件流中插入一条系统性提示（非模型生成，System 通道）说明当前只读档位。
- **R-MET-01（MUST）**：金集度量体系：≥40 条 golden corpus 命令（八类：dialect-chain/env-prefix/quoting/encoding/path/pipe/exit-code/policy），Windows 与 macOS 双平台可重放，输出机器可读报告（成功率、方言类失败数、诊断提示命中数）；改造前基线与改造后对照均入库；CI Windows job 接入 fast 档作为回归门禁。
- **R-MET-02（SHOULD）**：诊断命中计数随现有 `request_audit` 式旁路暴露，供运营观察方言失败趋势。
- **R-SEC-01（MUST）**：分类器（`classifier.rs`）在 Windows 切换 bash 方言后，风险分级规则只收紧不放宽：地板 R2、`sudo`/`rm -rf`/`curl|sh`/管道位置定级与 `powershell -Command` 包壳识别必须有专项测试，且分级不劣于 Unix 现状。
- **R-OPS-01（SHOULD）**：宿主设置提供执行环境卡（bash 路径覆盖、当前探测结果、未检出 Git Bash 的回落警示）；`docs/architecture.md` 命令执行节与 `docs/operations.md` Windows 排障条目与实现一致。

### 1.3 Definition of Done

`implementation_verified` 仅在以下全部成立时达成：

1. Windows 上 `bash` 工具在有 Git Bash 的机器上经 Git Bash 执行（金集验证），无 Git Bash 的机器回落 PowerShell 且行为与现状等价或更好。
2. WSL `bash.exe` 在任何解析层级都不会被选中（负向单测覆盖）。
3. 注册表合成 PATH 生效：GUI 启动 + 安装新工具（不重启 R-Code）后新命令可在 `bash` 工具与 Codex 子进程中找到（端到端用例）。
4. 诊断提示在四类错误样本上输出正确且不泄漏敏感信息；金集 `fail-with-hint` 类断言提示存在。
5. Codex 委派提示含命令书写规约；固定 reasoning 覆盖已移除；`full_access` 拒绝提示链路可用。
6. 金集基线报告入库：改造前后对比（基线数字见 §4.4），方言类失败率显著下降（目标：主链路保持 ≥96%，方言类失败占比 <2%）。
7. 分类器专项测试通过且不劣于现状（R-SEC-01）。
8. 前端构建、Rust 全量测试、Windows CI（含金集 fast 档）通过；macOS CI 不回归。

`production_release_ready` 还需要：真实 Codex 账号下 ≥92% 链路复测（取证重放评估脚本就绪属 implementation；真实数字属外部放行）、无 Git Bash 机器的实机回落验证。这些外部条件不得阻止离线 fixture/金集把实现推进到 `implementation_verified`。

### 1.4 非目标

- **不做**模型按命令选双方言（维持单方言 bash + 回落；取证表明错配源于方言并存，双轨扩大暴露面）。
- **不做**ConPTY 持久会话、IPython 内核、WSL 路线。
- **不改** codex 内部实现（无公开 shell 覆盖键；只做边界治理：提示规约、环境、降智移除）。
- **不放松**任何安全红线：风险分级地板 R2、审批矩阵、只读子代理默认不变。
- **不动**用户终端（PTY 注入）链路与 `vendor/agent-contracts` 子模块。

## 2. 已冻结决策

1. **方言统一为 bash**：工具名 `bash`、模型书写习惯、跨平台一致性三者在 Git Bash 路线上汇合；PowerShell 永远只是回落，不是并列选项。
2. **进程内工具优先的哲学不变**：文件读/搜/编继续引导到 `read_file`/`search`/`glob`/`edit`，shell 只承担项目命令；本次改造不扩大 shell 的职责面。
3. **环境合成是基础设施不是补丁**：注册表实时 PATH 是 Windows 侧对齐 macOS `fix_path_env` 的正式等价物，落在 `r-code-core` 共享层（`#[cfg(windows)]`），`bash` 工具与 Codex 子进程同源受益。
4. **度量先于结论**：金集是验收的一部分，"成功率"必须以可重放数字而非体感结算；M0-02 基线必须在 M1-01 合入前完成。
5. **不动 `vendor/agent-contracts` 子模块**：`execution.bash_shell_path` 与 `codex.subagent_reasoning_effort` 落在 `src-tauri/src/settings.rs` 宿主设置服务，经 `ToolExecutionContext` / Codex 拉起参数下传，不进 `agent-config` crate。
6. **PATH 只拼装一次**：`rtk.rs#prepend_managed_bin` 与 `win_env` 合成结果先拼装为最终 PATH 再一次性 `command.env()`，消除互相覆盖。
7. **安全边界不变**：分类器规则只收紧；只读子代理默认、审批矩阵、`SubagentAccessMode` 红线不动。
8. **验证脚本语言**：统一 Harness 用薄 Node orchestrator（复用 `scripts/verify-codex-interaction.mjs` 的 registry/runner 模式），编排既有 cargo/npm 测试；不为本项目引入新测试框架。

## 3. 仓库事实表

| 事实 | 位置 | 状态 |
|---|---|---|
| 现行 Windows 解释器策略（PowerShell + `.ps1` 暂存 + 引号重解析规避论证） | `crates/r-code-gateway/src/tools_command.rs:1-30, 211-258` | 待改造 |
| `UNIX_ONLY_HINTS` 前置拦截表 + `executable_on_path` | `crates/r-code-gateway/src/tools_command.rs:77-178` | 待门控 |
| Unix shell 选择 `/bin/sh -c` | `crates/r-code-gateway/src/tools_command.rs:261-269` | 不动 |
| 输出后处理（`clip_stream`/`StreamDrain`）与进程树收割（`taskkill /T`） | `crates/r-code-gateway/src/tools_command.rs:52-61, 271-329, 565-592` | 保留，诊断在其后追加 |
| 工具描述平台分支 | `crates/r-code-gateway/src/tools_command.rs:342,355` | 待改写 |
| Codex exec 拉起参数（`--sandbox`/approval/固定 reasoning/rtk/PATH） | `src-tauri/src/commands.rs:20084-20137` | 待治理 |
| App Server 拉起（无 PATH 处理） | `src-tauri/src/codex_app_server.rs:771-821` | 待应用 win_env |
| RTK PATH 整体覆盖 | `src-tauri/src/rtk.rs:537-547` | 待重构拼装 |
| 命令事件投影（"Codex 命令"卡） | `src-tauri/src/codex_interaction.rs:100-133, 1874-1890` | 诊断挂载点 |
| 宿主设置服务（无执行环境键） | `src-tauri/src/settings.rs` | 新增键 |
| macOS 登录 PATH 导入 | `src-tauri/src/main.rs:294-298` | Windows 等价物由 M2-01 提供 |
| 分类器命令定级 | `crates/r-code-gateway/src/classifier.rs` | 专项复核 |
| 现有统一验收脚本模式 | `scripts/verify-codex-interaction.mjs`、`scripts/verify-ai-worklist.mjs` | 复用模式 |
| CI Windows job | `.github/workflows/ci.yml` | 追加金集门禁 |
| Codex 自身 shell 解析（Windows 默认 pwsh，无覆盖键） | openai/codex `codex-rs/shell-command/src/shell_detect.rs` | 外部事实 |
| Codex `-Command` argv 直传（引号重解析暴露） | openai/codex `codex-rs/core/src/shell.rs#derive_exec_args` | 外部事实 |

## 4. 机器合同

### 4.1 Windows shell 解析链合同

`resolve_windows_shell()` 返回带方言标记的计划（`Bash{path}` / `PowerShell` / `Cmd`），五级顺序固定：

1. `execution.bash_shell_path` 设置值（存在即用，校验文件存在，失败报错不静默回落）；
2. 已知位置：`%ProgramFiles%\Git\bin\bash.exe`、`%ProgramFiles(x86)%\Git\bin\bash.exe`、`%LOCALAPPDATA%\Programs\Git\bin\bash.exe`、`%USERPROFILE%\scoop\apps\git\current\bin\bash.exe`；
3. PATH 上 `git.exe` 反推 `<git根>\bin\bash.exe` 与 `<git根>\usr\bin\bash.exe`；
4. PATH 上 `bash.exe`（大小写不敏感匹配，**跳过 `C:\Windows\System32\bash.exe` 及其它解析为 WSL 启动器的命中**）；
5. 回落：`pwsh.exe` → `powershell.exe`（保留 `.ps1` 暂存 + `-File`、UTF-8 前缀、`$LASTEXITCODE` 透传）→ `cmd.exe /D /C`。

bash 档执行：`<bash> -c <command>` 单 argv 直传，`cwd` 为绑定工作区，不加载 login profile。

### 4.2 win_env 合成算法合同

`r-code-core/src/win_env.rs`（`#[cfg(windows)]`）暴露 `synthesized_path() -> OsString` 与 `invalidate()`：

- 输入：HKLM `SYSTEM\CurrentControlSet\Control\Session Manager\Environment` 的 `Path` + HKCU `Environment` 的 `Path`（`REG_EXPAND_SZ` 按 `ExpandEnvironmentStringsW` 语义展开）。
- 输出顺序：HKLM 条目 → HKCU 条目 → 进程 PATH 中不在前两者的条目（大小写不敏感去重，保持进程内相对顺序）。
- 缓存：进程内 TTL 5 分钟；注册表读取失败时 fallthrough 到进程 PATH 并记录日志，不得 panic。
- 应用点：`tools_command.rs` bash/回落两档 spawn、`commands.rs` codex exec 拉起、`codex_app_server.rs` app-server 拉起；与 RTK managed bin 前缀拼装为最终 PATH 后一次性 `command.env("PATH", …)`。

### 4.3 诊断提示模式表

| 错误签名（大小写不敏感子串/正则） | 追加提示要点 |
|---|---|
| `ParserError` 且命令含 `&&` | PowerShell 5.1 不支持 `&&`；分号或 `if ($?)` 等价写法 |
| `is not recognized`/`command not found` 且命令头为相对路径 `.exe` | PowerShell 需 `&` 调用操作符（bash 档为 `./` 前缀说明） |
| `rejected: blocked by policy` | 当前委派为只读档位说明 + `full_access` 请求途径 |
| `is not recognized as an internal or external command` | 安装/PATH 建议（注明已启用注册表实时 PATH） |
| `was unexpected at this time` | cmd 链语法说明 |

提示由 `append_diagnosis(output, exit_code, dialect)` 生成：只追加不改写、≤400 字符、仅匹配签名不回显命令正文之外的内容；`bash` 工具输出与 codex `commandExecution` 错误投影共用同源实现。诊断命中计数写入旁路计数器（`request_audit` 式，只计数不含正文）。

### 4.4 金集与报告 schema

语料 `crates/r-code-gateway/tests/command_corpus/corpus.jsonl`，每行：

```json
{"id":"chain-and-env","cmd":"cd crates && cargo check -q","platform":"windows","tier":"fast","category":"dialect-chain","expect":"ok"}
```

- `platform ∈ windows|macos|both`；`tier ∈ fast(≤10s)|slow`；`category` 八类（§1.2 R-MET-01）；`expect ∈ ok|fail|fail-with-hint`。
- 数量下限：dialect-chain 6、env-prefix 4、quoting 6、encoding 4、path 6、pipe 4、exit-code 4、policy 4，合计 ≥40；含中文输出与空格路径样本。
- 报告 `artifacts/metrics/command-corpus/report-<git-sha>-<platform>.json`：

```json
{"git_sha":"…","platform":"windows","dialect":"git-bash","total":44,
 "ok":42,"fail":2,"dialect_failures":0,"hint_hits":3,"commands":[…]}
```

基线（M0-02 产出入库后回填本节）：会话取证锚点——原生 bash 96.5%（143 样本）、Codex 链路 87.1%（673 样本，拼接率 23.3%）；金集首跑数字待生成。

### 4.5 设置键与配置边界

- `execution.bash_shell_path: Option<String>`（绝对路径，空串表示强制回落）；`codex.subagent_reasoning_effort: Option<String>`（枚举受限子集，透传 codex `-c`）。
- 两键均落 `src-tauri/src/settings.rs` SettingsService，不进 agent-contracts；gateway 经 `ToolExecutionContext.shell_override` 收到前者。
- Codex 委派提示规约为独立常量（≤300 字符），`cfg(windows)` 注入，可整体置空回退。

## 5. 质量、性能与安全门禁

- Harness 反作弊：required assertion/metric 缺失视为失败；禁止删测试、降阈值、缩 corpus、改 fixture 真值或用 mock 冒充真实执行。
- 金集执行沙箱：每条命令绑定临时工作区，单条超时 60s，CI fast 档总预算 ≤5min；不触碰用户目录。
- 注册表访问只读（测试键除外且必须清理）；报告不记录密钥、用户路径以外的环境细节。
- 分类器专项（R-SEC-01）是安全回归门禁：bash 档分级不得低于 Unix 现状对应命令的分级。
- 性能预期：shell 解析为进程内探测（缓存命中后 O(1)）；win_env TTL 缓存避免每命令读注册表；金集 slow 档不进 CI。

## 6. 需求追踪表

| 需求 | 任务 | 断言 |
|---|---|---|
| R-SHELL-01 | M1-01 | `M1-01.A1`、`M1-01.A2` |
| R-SHELL-02 | M1-01 | `M1-01.A3` |
| R-SHELL-03 | M1-02 | `M1-02.A1`、`M1-02.A2`、`M1-02.A3` |
| R-ENV-01 | M2-01 | `M2-01.A1`、`M2-01.A2`、`M2-01.A3`、`M2-01.A4` |
| R-DX-01 | M2-02 | `M2-02.A1`、`M2-02.A2`、`M2-02.A3` |
| R-MET-02 | M2-02 | `M2-02.A4` |
| R-CDX-01 | M3-01 | `M3-01.A2` |
| R-CDX-02 | M3-01 | `M3-01.A1`、`M3-01.A3` |
| R-CDX-03 | M3-02 | `M3-02.A1`、`M3-02.A2`、`M3-02.A3` |
| R-MET-01 | M0-01、M0-02、M4-02 | `M0-01.A2`、`M0-02.A1`、`M0-02.A2`、`M4-02.A1`、`M4-02.A2` |
| R-SEC-01 | M4-01 | `M4-01.A1`、`M4-01.A2`、`M4-01.A3` |
| R-OPS-01 | M4-03 | `M4-03.A1`、`M4-03.A2`、`M4-03.A3` |

<!-- AI_WORKLIST_NORMATIVE_END -->

<!-- AI_WORKLIST_CONTRACT_START -->

## 7. Verification Harness 与里程碑

### 7.1 唯一产品验收入口

M0-01 建立并由后续任务扩展：

```powershell
node scripts/verify-windows-reliability.mjs --task <TASK_ID> --profile implementation
node scripts/verify-windows-reliability.mjs --through <MILESTONE_ID> --profile implementation
node scripts/verify-windows-reliability.mjs --through M4 --profile production
```

Harness 必须：

- 非交互运行；0 仅代表全部 required assertions 通过。
- 维护 assertion registry，支持 task、through、implementation/production profile。
- 编排 Rust unit/integration（gateway/core/tauri）、金集 corpus runner、前端组件测试、CI 脚本检查与重放评估脚本。
- 输出 `artifacts/ai-tasks/verification/windows-reliability/<profile>/<task-or-milestone>.json` 和证据索引。
- 报告 revision/worktree digest、平台、方言档、失败断言；不记录 secret 与用户环境细节。
- required fixture/metric 缺失视为失败。

M0-01 自身在 Harness 尚未存在时，先用任务卡列出的直接测试命令验收；随后必须用新 Harness 自验证一次。

### 7.2 里程碑

| 里程碑 | 能力出口 | 累计门禁 |
|---|---|---|
| M0 度量与验收地基 | Harness、金集语料、双平台基线报告 | `--through M0 --profile implementation` |
| M1 shell 统一 | 五级解析链、Git Bash 执行环境、描述统一 | `--through M1 --profile implementation` |
| M2 环境与诊断 | 注册表实时 PATH、诊断提示引擎 | `--through M2 --profile implementation` |
| M3 Codex 治理 | 降智移除、书写规约、档位显性化 | `--through M3 --profile implementation` |
| M4 收口与门禁 | 分类器专项、对照报告、CI 门禁、设置与文档 | `--through M4 --profile implementation` |

## 8. 主 Checklist（唯一状态源）

- [ ] **M0-01** 建立统一验证 Harness、金集语料与证据入口。证据：待生成
- [ ] **M0-02** 产出 Windows/macOS 金集基线报告并回填 PRD。证据：待生成
- [ ] **M1-01** 实现 Windows 五级 shell 解析链（Git Bash 优先，排除 WSL）。证据：待生成
- [ ] **M1-02** Git Bash 执行环境治理与工具描述统一。证据：待生成
- [ ] **M2-01** 注册表实时 PATH 合成并应用于 bash 与 Codex 子进程。证据：待生成
- [ ] **M2-02** 命令失败诊断提示引擎与命中计数。证据：待生成
- [ ] **M3-01** Codex 子代理降智移除与 Windows 命令书写规约。证据：待生成
- [ ] **M3-02** 委派档位显性化与 policy 拒绝系统性提示。证据：待生成
- [ ] **M4-01** 分类器 bash 方言风险分级专项。证据：待生成
- [ ] **M4-02** 对照报告与 CI 金集门禁合入。证据：待生成
- [ ] **M4-03** 执行环境设置卡与维护文档更新。证据：待生成

## 9. 详细任务卡

### M0-01 建立统一验证 Harness、金集语料与证据入口

- 结果：后续每项能力都有同一个非交互验收入口，金集语料冻结为可重放资产。
- 需求引用：R-MET-01、§4.4、§7。
- 依赖：无。
- 前置事实：`scripts/verify-codex-interaction.mjs` 提供可复制的 registry/runner 模式；`execute_bash` 可在测试中绑定临时工作区；`scripts/verify-ai-worklist.mjs` 已存在。
- 固定约束：语料 ≥40 条、八类下限与 §4.4 schema 完全一致；corpus 命令不得写用户目录；required 缺失必须失败。
- 决策空间：runner 用 Rust integration test 或薄 Node 编排均可，默认复用 codex-interaction 的 Node orchestrator + cargo test 组合；语料具体命令自选但须覆盖类别定义。
- 产物：`scripts/verify-windows-reliability.mjs` + assertion registry、`crates/r-code-gateway/tests/command_corpus/`（corpus.jsonl + runner）、`artifacts/ai-tasks/{evidence,verification}/windows-reliability/` 目录约定。
- 实施步骤：
  1. 只读盘点既有 Rust/Node 测试命令与 CI Windows job 结构。
  2. 编写 corpus.jsonl（八类、双平台标记、fast/slow 分层）与 schema 校验。
  3. 实现 `--task/--through/--profile`、JSON 报告、required 缺失失败；注册全部任务断言。
  4. runner 在临时工作区逐条真实执行（fast 档），产出 §4.4 报告骨架。
  5. runner/registry 自测。
- 验收断言：
  - `M0-01.A1`（contract）：未知 task、缺失 required assertion、失败子命令均返回非 0，报告列出准确失败 ID。
  - `M0-01.A2`（contract）：corpus.jsonl 通过 schema 校验（枚举合法、八类数量达下限、无重复 id）。
  - `M0-01.A3`（integration）：runner 真实执行至少一条 both/fast 命令并产出含 `dialect` 字段的报告。
- 验证：先跑 runner/registry 单测，再 `node scripts/verify-windows-reliability.mjs --task M0-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/windows-reliability/M0-01.yaml` 与对应 verification JSON。
- 失败处理：保存失败报告；修复 runner/registry，不得把失败断言改成 optional。

### M0-02 产出双平台金集基线报告并回填 PRD

- 结果：改造前的可对照数字入库，M1 起的每一步都有基线可回归。
- 需求引用：R-MET-01、§4.4、决策 4。
- 依赖：M0-01。
- 前置事实：当前实现为 PowerShell 链（§3），报告 `dialect` 字段应如实记录。
- 固定约束：基线必须在 M1-01 任何代码合入前生成并提交；不得事后补造。
- 决策空间：Windows 全量 + macOS both/fast 即可；slow 档基线可选，默认跑。
- 产物：`artifacts/metrics/command-corpus/report-<sha>-windows.json`、`report-<sha>-darwin.json`、PRD §4.4 回填。
- 实施步骤：
  1. 确认 worktree 干净且 M1 未动代码。
  2. Windows 跑全量语料，macOS 跑 both 档。
  3. 数字回填 §4.4，记录报告路径。
  4. 归档证据。
- 验收断言：
  - `M0-02.A1`（regression）：Windows 报告存在、schema 完整（git_sha/platform/dialect/total/ok/fail/dialect_failures）且 total ≥ 40。
  - `M0-02.A2`（regression）：macOS 报告存在且 both 档全部执行。
  - `M0-02.A3`（contract）：PRD §4.4 基线小节含两条报告路径与四个数字字段。
- 验证：`node scripts/verify-windows-reliability.mjs --task M0-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/windows-reliability/M0-02.yaml` 与报告文件。
- 失败处理：基线失败即修复语料/runner 后重跑；禁止在 M1 改动后补跑冒充基线。

### M1-01 实现 Windows 五级 shell 解析链

- 结果：Windows 上 `bash` 工具优先经 Git Bash 执行，无 Git Bash 时回落 PowerShell 链。
- 需求引用：R-SHELL-01、R-SHELL-02、§4.1。
- 依赖：M0-02。
- 前置事实：`plan_shell` 现为 pwsh→powershell→cmd（§3）；`UNIX_ONLY_HINTS` 与 `executable_on_path` 已存在。
- 固定约束：解析五级顺序与 §4.1 逐字一致；任何一级命中 WSL `bash.exe` 视为未命中并继续；PowerShell 回落保留全部现有加固。
- 决策空间：`ShellPlan` 扩展方式、探测结果缓存策略自定；设置下传可经 `ToolExecutionContext` 新字段或等价通道。
- 产物：`resolve_windows_shell()`、`ShellPlan` 方言标记、`execution.bash_shell_path` 下传、单测与 fixture。
- 实施步骤：
  1. 只读核对 `ToolExecutionContext` 构造点与 settings 读取路径。
  2. 实现五级解析与 WSL 排除。
  3. bash 档 `Inline` 直传 argv；回落档维持 `Script` 变体。
  4. 单测：五级各一例 + WSL 负向 + 设置覆盖失败报错。
  5. 金集 dialect-chain/env-prefix/quoting 类复跑。
- 验收断言：
  - `M1-01.A1`（unit）：五级解析链单测全绿，含设置覆盖、已知位置、git.exe 反推、PATH 探测。
  - `M1-01.A2`（security-negative）：PATH 首位为 `C:\Windows\System32\bash.exe` 时不会被选中，解析继续到下一级。
  - `M1-01.A3`（integration）：Git Bash 档金集 dialect-chain/env-prefix/quoting 全绿；fixture 模拟无 Git Bash 时回落 PowerShell 且结果不劣于基线。
- 验证：`cargo test -p r-code-gateway` + `node scripts/verify-windows-reliability.mjs --task M1-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/windows-reliability/M1-01.yaml` 与 verification JSON。
- 失败处理：解析链失败先修探测逻辑；金集失败按 §10 失败处理，不得放宽 expect。

### M1-02 Git Bash 执行环境治理与描述统一

- 结果：bash 档有确定的 MSYS 环境与诚实文案，回落档保留拦截。
- 需求引用：R-SHELL-03、§4.1。
- 依赖：M1-01。
- 前置事实：工具描述平台分支位于 `tools_command.rs:342,355`；`UNIX_ONLY_HINTS` 现为无条件拦截。
- 固定约束：`MSYS_NO_PATHCONV=1`、`LANG=C.UTF-8` 必设；hint 仅回落档生效。
- 决策空间：env 注入位置、描述文案措辞自定（语义须满足断言）。
- 产物：env 治理、hint 门控、描述改写、单测。
- 实施步骤：
  1. bash 档注入两个环境变量并加单测。
  2. hint 门控改造（PowerShell/cmd 档拦截、bash 档放行）+ 用例。
  3. 描述改写并加字符串断言。
  4. 金集 encoding/path/pipe 复跑。
- 验收断言：
  - `M1-02.A1`（unit）：bash 档子进程 env 含 `MSYS_NO_PATHCONV=1` 与 `LANG=C.UTF-8`。
  - `M1-02.A2`（unit）：`grep` 在 PowerShell 档被 hint 拦截、在 bash 档放行。
  - `M1-02.A3`（contract）：Windows bash 档描述含 "Git Bash"、回落档描述含 "PowerShell" 语义字符串。
  - `M1-02.A4`（regression）：金集 encoding/path/pipe 类通过，中文无 `�`。
- 验证：`cargo test -p r-code-gateway` + harness `--task M1-02`。
- 证据：`artifacts/ai-tasks/evidence/windows-reliability/M1-02.yaml`。
- 失败处理：MSYS 转换边界问题优先调 env 与语料类别定义，不改 expect。

### M2-01 注册表实时 PATH 合成并应用

- 结果：bash 工具与 Codex 子进程使用注册表实时 PATH，GUI 陈旧环境不再丢工具。
- 需求引用：R-ENV-01、§4.2、决策 3/6。
- 依赖：M0-02。
- 前置事实：`rtk.rs#prepend_managed_bin` 读进程 PATH 整体覆盖（§3）；macOS `fix_path_env` 为先例。
- 固定约束：算法与 §4.2 一致；只读注册表；读失败 fallthrough；TTL 5 分钟。
- 决策空间：注册表 API crate（默认 `windows-registry`）、缓存结构自定。
- 产物：`r-code-core/src/win_env.rs`、三处应用点改造、rtk 拼装重构、单测与端到端用例。
- 实施步骤：
  1. 实现 `synthesized_path()`/`invalidate()` 与 fixture 键单测。
  2. 三处拉起应用；rtk 改为前缀拼装后单次 `env()`。
  3. 端到端：临时目录放可执行文件并加入进程 PATH 差集，bash 工具能找到。
  4. macOS 编译回归确认 cfg 隔离。
- 验收断言：
  - `M2-01.A1`（unit）：合成顺序 HKLM→HKCU→进程差集、`REG_EXPAND_SZ` 展开、大小写不敏感去重。
  - `M2-01.A2`（integration）：bash 与两条 Codex 拉起路径的子进程 PATH 为合成值，RTK 前缀在最前且无覆盖丢失。
  - `M2-01.A3`（failure-path）：注册表读取失败时 fallthrough 进程 PATH 且有日志。
  - `M2-01.A4`（regression）：macOS 构建/测试零影响。
- 验证：`cargo test -p r-code-core -p r-code-gateway` + tauri 相关测试 + harness `--task M2-01`。
- 证据：`artifacts/ai-tasks/evidence/windows-reliability/M2-01.yaml`。
- 失败处理：注册表 API 兼容性问题换受约束实现（如 `winreg`），不降级为不合成。

### M2-02 命令失败诊断提示引擎与命中计数

- 结果：方言类失败一眼可修，policy 拒绝可归因到档位。
- 需求引用：R-DX-01、R-MET-02、§4.3。
- 依赖：M1-02。
- 前置事实：取证库有四类真实脱敏样本（§1.1）；codex 投影点见 §3。
- 固定约束：提示 ≤400 字符、只追加、不回显命令正文之外内容；计数只记类别不记正文。
- 决策空间：模式表实现（子串/正则）、计数器暴露形式（默认复用 request_audit 旁路文件）自定。
- 产物：`append_diagnosis()`、codex 投影挂载、计数器、单测。
- 实施步骤：
  1. 模式表实现 + 取证样本单测（ParserError/相对路径/not recognized/blocked by policy）。
  2. `tools_command.rs` 输出后处理挂载。
  3. `codex_interaction.rs` commandExecution 错误投影挂载。
  4. 计数器与暴露命令。
  5. 金集 fail-with-hint 类复跑。
- 验收断言：
  - `M2-02.A1`（unit）：四类样本各产出正确提示要点。
  - `M2-02.A2`（boundary）：正常输出零污染、提示长度 ≤400 字符。
  - `M2-02.A3`（integration）：codex commandExecution 错误投影含同源提示。
  - `M2-02.A4`（contract）：诊断命中计数可读取且只含类别与次数。
- 验证：`cargo test -p r-code-gateway` + tauri 投影测试 + harness `--task M2-02`。
- 证据：`artifacts/ai-tasks/evidence/windows-reliability/M2-02.yaml`。
- 失败处理：误报先收窄模式；不得为通过而删除某类提示。

### M3-01 Codex 子代理降智移除与命令书写规约

- 结果：codex 子代理命令质量恢复到继承档位，Windows 下有书写规约约束。
- 需求引用：R-CDX-01、R-CDX-02、§4.5。
- 依赖：M0-02。
- 前置事实：固定覆盖位于 `commands.rs:20096-20099`；委派提示组装点见 §3。
- 固定约束：`web_search="disabled"` 保留；规约常量 ≤300 字符、`cfg(windows)` 注入。
- 决策空间：规约具体措辞自定（语义须含五要素：单命令优先/双引号/禁拼接/相对可执行 `&`/路径分隔统一）；`codex.subagent_reasoning_effort` 枚举子集按 codex config 合法值定。
- 产物：覆盖移除、设置键、规约常量与注入、单测。
- 实施步骤：
  1. 删除固定 medium；接设置键（存在才传）。
  2. 规约常量 + 两处模板注入（exec 与 app-server turn）。
  3. 单测：设置缺失不传、设置存在传、Unix 不注入、长度上限。
  4. web_search 保持断言。
- 验收断言：
  - `M3-01.A1`（unit）：无设置时 codex exec 参数不含 reasoning 覆盖；有设置时按值传递。
  - `M3-01.A2`（unit）：Windows 委派提示含规约常量（五要素语义），Unix 不含；常量 ≤300 字符。
  - `M3-01.A3`（regression）：`web_search="disabled"` 与既有 codex 拉起参数不回归。
- 验证：`cargo test`（tauri）+ harness `--task M3-01`。
- 证据：`artifacts/ai-tasks/evidence/windows-reliability/M3-01.yaml`。
- 失败处理：模板注入点分歧（exec/app-server 组装差异）以最小改动对齐，不改权限参数。

### M3-02 委派档位显性化与 policy 拒绝提示

- 结果：用户与模型都能把"被拒"归因到只读档位而非玄学失败。
- 需求引用：R-CDX-03。
- 依赖：M3-01。
- 前置事实：`delegate_task` 描述与 System 事件通道已存在（§3）。
- 固定约束：提示为系统性 System 事件，非模型生成；阈值 ≥2 次才触发。
- 决策空间：计数的会话作用域（默认按子代理 run）自定。
- 产物：描述补全、拒绝计数与 System 提示、集成用例。
- 实施步骤：
  1. `delegate_task` 描述补 `full_access` 语义并加断言。
  2. policy 拒绝计数（识别复用 §4.3 签名）。
  3. 阈值触发 System 提示。
  4. mock codex 输出集成用例。
- 验收断言：
  - `M3-02.A1`（contract）：`delegate_task` 描述含 `full_access` 参数语义字符串。
  - `M3-02.A2`（integration）：mock 连续 2 次 `blocked by policy` 后事件流出现只读档位提示。
  - `M3-02.A3`（integration）：该提示来自 System 通道且 1 次拒绝不触发。
- 验证：harness `--task M3-02`。
- 证据：`artifacts/ai-tasks/evidence/windows-reliability/M3-02.yaml`。
- 失败处理：作用域歧义按最小可观察实现并记录决定。

### M4-01 分类器 bash 方言风险分级专项

- 结果：方言切换不引入安全回归。
- 需求引用：R-SEC-01、决策 7。
- 依赖：M1-02。
- 前置事实：`classifier.rs` 命令定级规则为 Unix 风格（§3）。
- 固定约束：只收紧不放宽；地板 R2 不变。
- 决策空间：新增规则的组织方式自定。
- 产物：专项测试组与必要规则补强。
- 实施步骤：
  1. 盘点现有规则在 bash 方言输入下的覆盖。
  2. 专项测试：`sudo`、`rm -rf`、`curl|sh`、管道位置定级、`powershell -Command` 包壳。
  3. 与 Unix 现状对比断言。
- 验收断言：
  - `M4-01.A1`（security-negative）：专项清单全部按预期定级，无漏判为 R0/R1。
  - `M4-01.A2`（regression）：同一命令集分级不低于 Unix 现状基线。
  - `M4-01.A3`（unit）：`powershell -Command` 包壳命令按内层命令定级。
- 验证：`cargo test -p r-code-gateway` + harness `--task M4-01`。
- 证据：`artifacts/ai-tasks/evidence/windows-reliability/M4-01.yaml`。
- 失败处理：漏判补规则；不得调低预期等级。

### M4-02 对照报告与 CI 金集门禁合入

- 结果：改造效果有数字结论，回归有门禁。
- 需求引用：R-MET-01、§1.3 DoD-6/8。
- 依赖：M1-02、M2-01、M2-02、M3-02、M4-01。
- 前置事实：基线报告在 M0-02；CI Windows job 现状见 `.github/workflows/ci.yml`。
- 固定约束：主链路 ≥96%、方言类失败占比 <2% 为 required；Codex 链路 ≥92% 属 production 外部放行（implementation 只要求评估脚本可跑）。
- 决策空间：CI 接入的具体 job/step 组织自定。
- 产物：改造后报告、对照表、CI 门禁、重放评估脚本。
- 实施步骤：
  1. 全量金集（fast+slow）产出改造后报告。
  2. 与基线并表回填 PRD §4.4。
  3. CI Windows job 追加 fast 档门禁。
  4. Codex 链路重放评估脚本（离线 fixture 演练，真实账号跑法写入外部放行说明）。
- 验收断言：
  - `M4-02.A1`（performance）：改造后报告满足主链路 ≥96% 且方言类失败占比 <2%。
  - `M4-02.A2`（ci-contract）：CI Windows job 含金集 fast 档步骤且失败会阻断。
  - `M4-02.A3`（contract）：重放评估脚本可离线运行并输出结构化结果。
- 验证：harness `--through M4 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/windows-reliability/M4-02.yaml` 与 verification JSON。
- 失败处理：未达阈值定位到对应任务修复后复跑；不得改基线或缩语料。

### M4-03 执行环境设置卡与维护文档更新

- 结果：用户可覆盖 bash 路径并看见当前方言档，文档与实现一致。
- 需求引用：R-OPS-01、§4.5。
- 依赖：M2-01。
- 前置事实：设置页结构与 SettingsService 见 §3；`docs/architecture.md`、`docs/operations.md` 为维护文档。
- 固定约束：设置读写必须经 SettingsService；未检出 Git Bash 必须警示。
- 决策空间：设置卡 UI 实现复用现有组件风格自定。
- 产物：设置卡组件与 IPC、文档更新、单测与组件测试。
- 实施步骤：
  1. SettingsService 增两键 + 单测。
  2. 设置卡 UI（路径输入、探测结果、警示态）+ 组件测试。
  3. `docs/architecture.md` 命令执行节重写、`docs/operations.md` Windows 排障条目。
  4. 文档一致性检查。
- 验收断言：
  - `M4-03.A1`（unit）：两键读写经 SettingsService 且空串语义正确（强制回落）。
  - `M4-03.A2`（component）：未检出 Git Bash 时警示可见；检出时展示路径。
  - `M4-03.A3`（docs-contract）：两份文档含方言策略与设置键说明（可 grep 断言）。
- 验证：前端测试 + harness `--task M4-03`。
- 证据：`artifacts/ai-tasks/evidence/windows-reliability/M4-03.yaml`。
- 失败处理：文档先行修正再验收，不留"实现后再补文档"。

## 10. 连续执行、恢复与证据协议

### 10.1 固定循环

选择编号最小且依赖已通过的 ready MUST → 建立/恢复 `current.yaml` → 实现一个可验证子步 → 更新任务包 → 跑任务断言 →（通过）跑累计门禁 → 归档证据 → 勾选 §8 唯一 Checkbox → 立即进入下一项。里程碑、汇报、测试通过都不是等待人工确认的节点。

### 10.2 证据规则

- 路径：`artifacts/ai-tasks/evidence/windows-reliability/<TASK_ID>.yaml`（任务包归档）与 `artifacts/ai-tasks/verification/windows-reliability/<profile>/<task-or-milestone>.json`（Harness 报告）。
- 金集报告：`artifacts/metrics/command-corpus/`，带 git-sha 与平台。
- 勾选前置：required 断言全过、Harness 当前任务 0 退出、累计回归满足、证据真实存在、无删测试/降阈值绕过。
- `current.yaml` 仅为单项恢复状态，重建时覆盖旧项目内容，不构成第二进度源。

### 10.3 自主决策与失败处理

- 决策阶梯：查文档/仓库 → 复用既有模式（优先复用 codex-interaction 的 harness/registry 与 settings 模式）→ 仓库内可逆选择按 安全>正确>简单>一致>可测试>性能 决断并记录 → 缺外部能力先 fixture/fake。
- 验证失败：保存失败报告 → 定位根因 → 聚焦修复 → 复跑；同方案无进展换受约束实现。
- 允许中断：仅 §0.3 列举条件；金集跑挂、lint 失败、平台差异等均不是中断理由。

## 11. 风险、兼容与外部放行

### 11.1 风险与回滚

| 风险 | 缓解 | 回滚 |
|---|---|---|
| 用户机器无 Git Bash | 五级解析回落 PowerShell，金集回落档对照验证 | 设置 `execution.bash_shell_path=""` 强制回落 |
| MSYS 参数转换边界（`cmd //c` 等） | `MSYS_NO_PATHCONV=1` + path 类金集 | 去掉该 env 即回旧行为 |
| 注册表读失败/权限异常 | fallthrough 进程 PATH + 日志 | M2-01 独立提交 revert |
| 提示规约拉长 codex 提示 | ≤300 字符 + M3-01 断言 | 规约常量置空 |
| 分类器换方言漏判 | M4-01 专项 + 只收紧原则 | 规则独立成组可回退 |
| rtk PATH 拼装重构回归 | M2-01.A2 集成断言 | rtk 改动独立 commit revert |

### 11.2 提交切片（建议，非门禁）

1. `test(corpus): 金集 harness 与 Windows 基线报告`（M0）
2. `feat(gateway): Windows bash 工具切换 Git Bash 解析链与环境治理`（M1）
3. `feat(core): 注册表实时 PATH 合成并应用于 bash 与 codex 子进程`（M2-01，rtk 重构独立 commit）
4. `feat(gateway): 命令失败诊断提示引擎`（M2-02）
5. `feat(codex): 子代理降智移除与命令书写规约`（M3）
6. `test(classifier): bash 方言风险分级专项`（M4-01）
7. `test(corpus): 对照报告与 CI 门禁` + `feat(ui/docs): 执行环境设置卡与文档`（M4-02/M4-03）

### 11.3 外部放行（production profile）

- 真实 Codex 账号下 ≥92% 链路复测（M4-02 重放脚本就绪即 implementation 完成）。
- 无 Git Bash 实机回落验证。
- CI Windows/macOS 全绿。
- 以上不阻塞 `implementation_verified`。

<!-- AI_WORKLIST_CONTRACT_END -->

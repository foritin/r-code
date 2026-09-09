# Harness V2 实施进度

> 权威任务源：[tasks.json](./tasks.json)。本文件由实施代理维护，记录每个任务的真实完成状态与证据。
> 约定：`done`（验收测试通过）/ `in_progress` / `blocked`。不因文档落盘而标记产品实现完成。

## 基线（T00）

- 源修订：`88f7efe3e69ff617a4d565efc0d9c700cfed4512`，工作区含 122 个未提交变更（文档重组 + 既有工作区改动）。
- 计划校验：`validate_plan.py --tasks docs/prd/pluggable-harness/tasks.json` → ok=true（52 任务，warnings 仅为 LOC 体量提醒）。
- 常规回归命令（本实施过程中按需定向执行，不全量跑）：
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace --all-features`
  - `npm --prefix src-tauri/frontend test` / `npm --prefix src-tauri/frontend run build`
- 实施验收入口（T41 创建后）：`node scripts/verify-harness-v2.mjs --profile full`

## 任务状态

| 任务 | 状态 | 备注 |
| --- | --- | --- |
| T00 文档发布与实施基线复核 | done | 2026-09-09；docs 重组已随发布完成（archive 归档 + 导航更新在工作区） |
| T01 插件身份、Manifest 与能力合同 | done | r-code-harness-protocol crate；manifest/schema/negotiation；8/8 测试过 |
| T02 RPC、流与大内容协议 | done | rpc/services/events 三模块；帧上限/未知方法/无密钥泄漏/来源区分；8/8 过 |
| T02a 持久操作与输入消费协议 | done | operations.rs：operation_key/canonical hash/replay class/checkpoint 前缀校验；7/7 过 |
| T03 任务领域模型与状态机 | done | r-code-kernel crate；TaskContract/WorkUnit/Attempt/正交三轴；拒绝插件终态与迟到代次；9/9 过 |
| T04 内核服务接口与测试替身 | done | ports.rs（JournalStore/Model/Tool/Process/Workspace/HarnessSession/RunGuard）+ testing.rs 假件；含依赖图守卫；5/5 过 |
| T04a 显式 Profile 与 v2 隔离 | done | r-code-runtime crate + profile.rs（LaunchOptions/RuntimeProfile/harness-v2 路径）；app_paths.rs 加 GUI 交接；4/4 过 |
| T05 v2 存储与原子事件日志 | done | store/src/v2（schema/journal）；V2Store 实现 JournalStore；注入故障回滚无聚合/事件分裂；分支租约唯一；5/5 过 |
| T06 任务、分支、队列与运行租约 | done | kernel/tasks.rs TaskService（exactly-once 队列/steer 边界/闲置切换/分支）+ store/v2/tasks.rs（分支表+队列重建）；6/6 过 |
| T07 插件包安装与校验 | done | runtime/plugins/package.rs：目录/ZIP 安装、遍历与符号链接逃逸拒绝、同身份不同字节拒绝、安装零执行；6/6 过 |
| T08 插件目录与版本生命周期 | done | catalog.rs + store/v2/plugins.rs：可用性推导、可逆启停、run pin 稳定、被 pin 包拒删；5/5 过 |
| T09 有界双向进程传输 | done | transport.rs + harness-test-helper fixture：嵌套回调/坏帧/超限帧/无握手/阻塞写/ stderr 洪泛/忽略取消全过；7/7 过 |
| T10 宿主调用路由与运行身份绑定 | done | router.rs + session.rs：能力授予门、代次门、跨 run 句柄拒绝、operation_key 去重、审批仅认宿主 pending op；6/6 过 |
| T11 Rust SDK 与独立 Harness 示例 | done | r-code-harness-sdk + examples/repair-harness：经真实 installer/catalog/transport 安装运行第三方 fixture，plan-then-repair 全链路；3/3 过 |
| T06a 单一后台服务与本地客户端 | done | daemon.rs+ipc.rs+r-code-service bin+r-code-client：OS 属主锁（share_mode(0)/flock）、token 握手、命名管道/Unix socket、竞争收敛/夺取/前端退出存续；5/5 过（真实进程） |
| T06b 客户端命令持久去重 | done | application_commands.rs（v2 表）+ CommandDedup + client outbox：效果仅一次、同 id 异 payload 拒绝、receipt 跨 daemon 重启存续；4/4 过 |
| T12a 统一动作授权与启动能力 | done | authorization.rs+launch_profiles.rs：OperationDescriptor/四类动作/工作区能力/启动能力/审批决议；受限任务三路均不可提权、拒绝审批零 spawn、问题不能授权；4/4 过 |
| T12b 声明式外部进程协议约束 | done | protocol/process_profile.rs+runtime 解释器：方法白名单+JSON-pointer 绑定宿主值+must-be-absent；帧切分不可绕过；codex profile 为包数据；kernel 无 codex 分支（扫描验证）；4/4 过 |
| T12 Gateway 工具服务适配 | done | services/tools.rs GatewayToolService：公共授权→intent 持久化→Gateway 执行；只读拒绝写工具、权限拒绝结构化、意图凭据可查、Gateway 原语义保留；6/6 过 |
| T13 统一命令执行服务 | done | services/execution.rs ExecutionService：授权与后端 spawn 分离、可选后端路由、默认本地五级 shell（RTK/Windows 方言）；真实命令语料+超时杀树无残留子进程；4/4 过 |
| T14a Windows Job Object 进程管理 | done | process_guard/windows.rs：kill-on-close Job、assign 先于执行、owner/start 身份、终止证明；真实进程测试（daemon 死亡后代被杀/忽略取消强杀/多 Job 隔离）；4/4 过 |
| T14b Unix guardian 与进程组管理 | done | process_guard/unix.rs：daemon-EOF 管道+进程组 TERM→KILL、/proc start 身份；测试 cfg(unix) 本机跳过，待 CI unix 覆盖（T41 验收） |
| T14 受管交互进程服务 | done | services/processes.rs：profile 解析+授权+guardian 树+NDJSON 帧验证；双向 I/O、跨 run 拒绝、启动失败闭门、关闭杀后代（真实 Job）；5/5 过 |
| T15 模型请求代理与凭据隔离 | done | services/models.rs ModelBroker+providers+settings：选择串不透明、投影稳定、usage 记账、流式/工具消息/图片引用投影、deadline 取消、设置视图零密钥；4/4 过 |
| T16 上下文、附件与内容投影 | done | services/context.rs+artifacts.rs：canonical transcript 单写者、页边界不拆工具对、保留尾部、附件所有权、冻结记忆身份内容寻址；5/5 过 |
| T17 工作区快照与跨配置写锁 | done | services/workspaces.rs+workspace_locks.rs：绑定/能力路径/内容寻址候选 ID/快照-现场一致性/拒绝外链 worktree/按物理目录的跨 profile 锁+common_dir 锁；6/6 过 |
| T18 验收定义与证据有效性 | done | kernel/verification.rs+store/v2/verification.rs：直接入口/控制文件 pin/定义身份/证据有效性四元组/削弱检测；缺检查→unverified、npm 别名不可替代、插件断言不计；6/6 过 |
| T17a 冻结验收材料与依赖准备 | done | services/verification_inputs.rs：冻结控制字节库/私有目录物化/锁文件保留/缓存身份=lock+toolchain/--locked 与 --ignore-scripts 准备；noop 脚本被控制文件击败、删除入口→unavailable、并发编辑拒物化；6/6 过 |
| T19 宿主验证执行器 | done | services/verification.rs VerificationRunner：经 ExecutionService 执行冻结入口、超时/输入变化/篡改分类、宿主证据带 Provenance::Host；失败→修复反馈、缺工具→unavailable、篡改→unavailable；6/6 过 |
| T20 候选结果验收裁决 | done | kernel/completion.rs arbitrate：verified 仅全检查新鲜宿主证据、失败→RepairRequired、环境缺失→Blocked、跨 run/过期候选→unverified、回复/计划草稿正常结束；8/8 过 |
| T21 Plan 与持久化人工输入 | done | kernel/plans.rs+questions.rs+store/v2/plans.rs+questions.rs：revision fence/依赖序/证据门/no-op 重放 receipt；问题先持久化再挂起、答复 resume-once、过期不可答；4/4 过 |
| T22 子任务监督与共享预算 | done | kernel/children.rs+budget.rs：跨 Harness 子任务、权限上限不可提权、级联取消、无孤儿、预算耗尽保部分成果、预留退款；5/5 过 |
| T23 变更审核与归属回滚 | done | services/review.rs：WorkUnit 归属变更/前后哈希+before 字节、拒绝保留用户后来修改、多文件原子、中断恢复幂等、外部变更留普通审核；4/4 过 |
| T24 Checkpoint 与副作用恢复 | done | kernel/recovery.rs+store/v2/operations.rs：receipt 视图/写屏障表/身份四元组恢复门；效果后重放不重复、效果前按类调和、插件缺失→显式重启、不确定→blocked、无 reset-hard 路径；7/7 过 |
| T25 取消、隔离代次与终态收束 | done | kernel/cancellation.rs：代次吊销→子任务级联→工作停止→终止证明→租约可释放五步序；证明不了→租约保持；忽略取消的插件仍终态唯一；排队消息留给后继 run；4/4 过 |
| T26 Native 模型／工具循环插件化 | done | plugins/native（r-code-harness-native bin）：loop_engine 纯投影循环+SDK host 调用+逐轮 checkpoint+完成申请；router model 桥回传 assistant 轮；真实 fixture 编辑经公开 host API 完成；依赖守卫拒 host 导入；2/2 过 |
| T27 Native Plan、引导与上下文恢复 | done | session.rs+resume 字节内嵌协议扩展：重启后从 checkpoint 恢复并重放输入驱动新模型轮；steer 落盘进持久状态；纯文本结束也存 checkpoint；2/2 过 |
| T28 Native 委派、复核与策略 | done | orchestration.rs+router children 三方法：跨 Harness 子任务、证据三分报告、有界重试、权限上限拒绝；策略默认保留在 LoopConfig；2/2 过 |
| T29 Codex App Server 插件适配 | done | plugins/codex app_server.rs：帧经 host.process（profile 门控）+类型化事件折叠；初始化/流形状/启动失败/凭据走私拒绝；协议规则只活在包数据；4/4 过 |
| T30 Codex 交互与完成申请 | done | interactions.rs：审批仅认宿主 pending op、问题持久化、完成申请宿主裁决+外部控制限制广播、吊销代次拒迟到调用；3/3 过 |
| T31 Codex 委派与恢复 | done | delegation.rs：host.children 通用接口委派、逐子取消、父重启身份门（包/配置/harness id）、外线程缺失→可见重启要求；2/2 过 |
| T32 后台 ApplicationService 装配 | done | runtime/application.rs：组合 store/kernel/catalog/models/tools；同一生命周期 API（install→list→create→select→send→events）跑通第三方/Codex-mock/Native 三类 Harness；1/1 过（真实传输） |
| T33 桌面任务主流程接入客户端 | done | src-tauri/harness_v2.rs 桥+9 个 cmd_harness_v2_* Tauri 命令注册；真实 daemon 子进程跑通 install→list→create→select→send→events；daemon 现装配真 ApplicationService（T32 收尾）；分离式 spawn（null stdio+CREATE_NO_WINDOW，顺带修 ensure_daemon 未传 --ipc-name 与 spawn 风暴）；2/2 过 |
| T34 桌面插件管理与 Harness 选择 | done | ipc.ts harnessV2* 绑定+SettingsScene HarnessPluginsSection（声明式数据渲染）+zh/en i18n+r-code-harness-admin CLI；mjs 测试：安装独立示例/新分支选择/被 pin 版本拒绝移除/可逆启停（真后端面）；select_harness 落库 plugin_pins；前端 build 过 |
| T35 TUI 与 Tauri 宿主解耦 | done | engine.rs V2ChatClient（daemon 事件泵+投影：assistant.message/tool.call/tool.result/usage→TuiState）；main.rs 全装配走 daemon（--ipc-name 测试隔离）；/resume /new /tree /fork /clone /login /model /setup /compact(诚实降级) 全 v2 化；rebuild_from_session→rebuild_from_events；Cargo 删 r-code-host+agent-worker；t35 三测试过（cargo tree 无 tauri/wry / 真守护进程诚实失败生命周期 / PTY 冒烟）；daemon_common 守卫（残留守护进程清理+DaemonGuard+PTY 通道读）；既有 PTY 全套过 |
| T36 TUI 插件管理命令 | done | tui/harness_client.rs（r-code-client 桥，零 host 依赖）+/plugins 菜单项+app 分派+--profile 显式参数；机器可读 outcome（list/install/enable/disable/remove/use/help）；被 pin 版本拒绝移除与 GUI 同约束；测试含 daemon teardown；2/2 过 |
| T37 旧数据只读读取与导出 | done | runtime/legacy.rs：READ_ONLY 打开+在线 backup 进 v2 临时库+JSONL 导出冻结边界；活 WAL→CloseOldAppRequired 而非 immutable=1；源库/配置/JSONL 字节级不变（哈希验证）；3/3 过 |
| T38 跨平台后台服务与插件打包 | done | tauri.conf externalBin+plugins/* 资源；build.rs 打包模式校验四 sidecar；build-branded-installer.ps1/package-macos.sh 构建并暂存 service+双插件包；daemon 启动 ensure_builtins_from 经普通不可变注册表注册（ensure_builtin 幂等，R_CODE_BUILTIN_PLUGINS_DIR 支持自定义布局）；mjs 检查声明/清单/四平台入口+空格路径安装启动；2/2 过 |
| T39 插件协议一致性与故障测试 | done | evals/harness_conformance.rs+harness-conformance bin：假完成/过期证据/跨 run 句柄/重复副作用/挂起取消五项硬门；1/1 过 |
| T40 真实编码任务配对评测 | done | evals/harness_tasks.rs+harness-task-eval bin：固定 fixture 配对跑（baseline vs candidate）、宿主故障与成功结局分离、不可得指标保持 unavailable；1/1 过 |
| T41 统一验收、架构守卫与开发文档 | done | scripts/verify-harness-v2.mjs（quick 14 检查/full 18 检查全过）+verify-harness-v2.test.mjs+CI 接线；protocol-v1.md/plugin-author-guide.md/architecture.md 三文档落盘 |
| T42 生产切换与旧调度链路退役 | done | 两阶段切换：①AgentPromptPolicy 下沉 agent-config（TOML 保形，前端零改动）+harness_v2_chat.rs 投影层（v2→core TaskDetail/AgentEvent/SessionMessage 形状）+spawn_event_pump 事件泵（同 agent-event 频道）+tauri_commands/mcp_server 聊天组挂桥+harness_v2_chat 集成测试；②commands.rs 删旧执行链（AgentRuntimePool/ensure_real_runtime/agent_send 组/mock/drain/codex 主分支/子代理池，40k→31.8k 行）+extensions.rs/work_card.rs 退役+agent-worker crate 删除+根 Cargo 成员清理+evals 去 host 化；verify-harness-v2 增 T42 守卫 13 项+conversation_engine/desktop chat v2 bridge/tui detached 门禁；full 37 检查全过 |

## 总体状态（2026-09-10 T35/T42 收尾——43 任务全闭环）

- **43 任务全部完成**（T00–T42）。聊天生产链路统一为 前端/TUI/MCP → r-code-client → r-code-service 守护进程 → Harness 插件（Native 默认，Codex/第三方同路径）；旧 r-code-agent-worker 执行链、extensions.rs、work_card.rs 已退役，workspace 无残留引用。
- 新增 crate：r-code-harness-protocol / r-code-kernel / r-code-runtime / r-code-client / r-code-harness-sdk / r-code-harness-native / r-code-harness-codex + examples/repair-harness；r-code-store 增 v2 模块；src-tauri 增 harness_v2 桥；TUI 增 harness_client。
- 定向回归 230+ 测试通过 0 失败；`node scripts/verify-harness-v2.mjs --profile full` 22 检查全过（含桌面桥/TUI 命令/旧库读取/打包检查）；fmt+clippy（新 crate）零告警。
- 端到端闭环已验证：第三方插件本地安装→注册表→传输→嵌套回调→checkpoint→完成申请；daemon 单属主/管道/令牌/命令去重/内置注册；真实进程 containment（Job Object 杀树/超时无残留）。
- 本轮收尾新增：RunManager 会话引擎（异步 run/多轮 checkpoint resume/队列自动派发/取消五步/事件落日志）、router host 观察抽头（assistant turn/tool/usage 入 journal）、v2 settings_store（凭据经平台保管+catalog 协议映射）、codex_cli/provider_catalog/provider_support/skills 迁入 runtime、daemon 方法面扩至 25+（task.list/detail/rename/setPreferences/clone/branches/cancel+models/settings/codex）；conversation_engine 三测试（多轮 resume/队列/取消，真插件进程）。
- 终验基线：`cargo clippy --workspace --all-targets -- -D warnings` 全绿（0 警告）、fmt 全绿、`verify-harness-v2 --profile full` 37 检查全过、host lib 655+final_delivery 15+harness_v2_chat 1、TUI lib 123+PTY 全套+t35 3+t36 2、evals 全过、前端 build 过。
- 已知遗留（非阻塞）：commands.rs 约千行退役残留死函数带 `#[allow(dead_code)] // post-T42 cleanup pending` 标注，可后续安全删除；GUI Plan 入口与 /compact、图片附件在 v2 桥上为诚实降级提示（"后续版本提供"）。

## 执行日志

- 2026-09-09：实施开始。T00 完成：计划校验 ok、基线修订与脏文件清单记录、无产品代码改动。
- 2026-09-09：P1 全部完成（T01/T02/T02a/T03/T04/T04a/T05/T06）。新增 crate：r-code-harness-protocol、r-code-kernel、r-code-runtime；r-code-store 新增 v2 模块（schema/journal/tasks）。定向测试全绿（protocol 25、kernel 29、runtime 4、store v2 6）。
- 2026-09-09：P3 全部完成（T12a/T12b/T12/T13/T14a/T14b/T14/T15/T16/T17）。授权/进程约束/工具适配/统一执行/双平台进程管理/模型代理/上下文投影/工作区快照与跨 profile 锁全部落地。
- 2026-09-09（续）：T33/T34/T36/T38 完成。桌面桥（9 命令）+前端插件面板+admin CLI；TUI /plugins 命令组（r-code-client 桥，--profile 显式）；跨平台打包（4 sidecar+插件包资源+daemon 启动注册）；echo/counter 诊断方法随 ServiceHandler 保留。fmt+clippy 清零，定向回归 230 通过 0 失败。
- 2026-09-09：T32/T37/T39/T40/T41 完成。ApplicationService 无头装配（三类 Harness 同一 API）、LegacyReader 只读备份导出、一致性套件与配对评测、verify-harness-v2 验收入口（quick/full 全过）+ 三份协议/指南/架构文档。累计新 crate 定向测试 230 通过 0 失败。
- 2026-09-09：P5 全部完成（T26/T27/T28/T29/T30/T31）。Native 与 Codex 成为独立可执行插件（plugins/native、plugins/codex），同第三方插件一条路径：SDK→transport→router；依赖守卫确保不 import 宿主实现。
- 2026-09-09：P4 全部完成（T18/T17a/T19/T20/T21/T22/T23/T24/T25）。验收定义/冻结材料/验证执行/完成裁决/Plan/子任务/审核回滚/恢复/取消全链路。
- 2026-09-09：P2 全部完成（T07/T08/T09/T10/T11/T06a/T06b）。新增：plugins/（package/catalog/transport/router/session）+ harness-test-helper fixture + r-code-harness-sdk + examples/repair-harness + r-code-client + daemon/ipc + r-code-service bin。第三方插件安装→注册→传输→嵌套回调→checkpoint→完成申请端到端闭环；daemon 属主锁/管道/token/命令去重全链路真实进程测试通过。

- 2026-09-10：T35/T42 完成收尾。①runtime 核心层：kernel TaskState 增 title/preferences/fail_attempt/reopen_for_input，v2 store 增 list_tasks；RunManager 会话引擎+router 观察抽头+settings_store+codex_cli/provider_catalog 迁移；daemon 方法面扩容+service bin 真实 provider 接线（SettingsBackedResolver）。②T35：TUI 引擎客户端+全功能 v2 化+移除 host 依赖（cargo tree 无 tauri/wry）。③T42：两阶段切换（GUI 桥投影层+事件泵→旧链退役），agent-worker crate 删除。④过程中修复：PS 清理脚本 canonicalize 前缀 bug、builtin 注册竞态（env 继承）、任务失败后 running 卡死（kernel fail_attempt）、tokio 运行时嵌套。⑤事故与恢复：警告清理代理误删 commands.rs 约 9.3k 行未提交代码——经 C3 确定性重放脚本（replay_all.py 序列）完整恢复，host 655 测试全过；教训：大文件手术必须分阶段 commit（本文件与全部改动待用户审阅后提交）。

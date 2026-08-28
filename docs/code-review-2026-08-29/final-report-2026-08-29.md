# 最终交付报告 — 全仓 Code Review + 根因级修复（2026-08-29）

**分支**：`feat/code-review-2026-08-29`（自 main 49f9193；全部提交仅落此分支，未推送）
**范围**：阶段 A 九维度全仓 review（92 findings）→ 阶段 B 修复执行（19/21 任务完成）

---

## 一、发现统计（阶段 A）

| 维度 | blocking | major | minor | 合计 |
| --- | --- | --- | --- | --- |
| RV-01 基线 | 0 | 1 | 2 | 3 |
| RV-02 架构 | 0 | 4 | 5 | 9 |
| RV-03 正确性 | 0 | 1 | 10 | 11 |
| RV-04 安全 | 0 | 0 | 9 | 9 |
| RV-05 健壮性 | 2 | 4 | 4 | 10 |
| RV-06 性能 | 2 | 8 | 6 | 16 |
| RV-07 可维护性 | 0 | 5 | 6 | 11 |
| RV-08 测试/CI | 0 | 4 | 6 | 10 |
| RV-09 可观测/配置/文档 | 0 | 5 | 7 | 12 |
| **合计** | **4** | **32** | **55** | **91+** |

全部 findings 带 file:line 与证据，见 `findings/01..09-*.md`；扫描命令与计数见 `evidence/RV-*.md`。

**4 个 blocking**：
- F-robust-01/02/03：Windows 下 codex app-server / mcp-server / 一次性 CLI 经 cmd.exe wrapper 启动，shutdown/超时只单杀 wrapper，node 后代整棵成孤儿（持文件锁/端口跨次累积）。
- F-perf-01：Codex 工具输出缓冲逐字符 `chars().count()`，输出超 64K 字符即 O(n²)（1MB 输出 ≈ 10¹¹ 次操作 + ~60GB 累计分配）。
- F-perf-02：每轮 provider 请求对全会话历史 5~7 次深拷贝（每轮成本 O(会话大小)）。

**安全维度结论**：无 blocking/major——PathGuard/凭据 keyring/SQL 参数化/SSRF 防护/BatBadBut 防护等工程质量显著高于同类产品。

## 二、修复统计（阶段 B）

19/21 任务完成，17 个功能提交 + 3 个文档/保全提交：

| 任务 | 提交 | 内容 |
| --- | --- | --- |
| FX-00 | 6097a7a | 用户 WIP 保全提交（36 文件，先于一切修复，零改动入库） |
| 基线修复 | d84d7f4 | ProductFeatureFlags 测试初始化器补字段（用户 WIP 编译遗漏） |
| FX-18 | 9868f51 | CI 门禁：7/7 meta 测试入 CI、自测合成 fixture（12/12）、金集 honest-ignored（#[ignore]+corpus-run --ignored，fast 41/41 验证）、flaky 对齐 threads=1+ubuntu 腿、symlink 特权跳过、readme/AGENTS 漂移修正 |
| style | 2f2dfe3 | cargo fmt 对齐 42 处用户 WIP 格式漂移 |
| FX-01 | d1a2320 | **blocking**：树杀原语收敛 r-code-core::process::kill_tree；codex_app_server/codex_mcp shutdown + 一次性 CLI 超时路径全部接线；taskkill 全仓唯一实现；含"已退出进程安全"与"超时即树杀"钉子测试 |
| FX-02 | 140f7d5 | **blocking**：输出缓冲增量字符计数 + 一次 drain 截断；去逐 delta 全量 render；2MB 流式 0.03s 钉子（旧实现分钟级）；语义测试保持 |
| FX-03 | 9006092 | **blocking**：repair 先序消除双重克隆、dispatch_ref 改 move（附件路径零额外拷贝）、冻结请求 move 化（单 attempt 少一次全量拷贝）；F-robust-05 子代理浓缩 complete() 120s 超时+降级 |
| FX-09 | 2d282a5 | 死代码 -437 行（codex 旧簇 7 函数 + 23 零引用 pub fn + 2 传导死 helper）；测试专用 helper cfg(test) 门控；全仓 0 warning |
| FX-04 | 41e3e05 | drain loop panic 终态护栏（Drop guard + try_lock，run 收敛 Aborted/Interrupted）；llm_runtime 13 处锁中毒降级（recover_poisoned_guard 唯一实现 + 测试）；9 处 host 锁降级；7 个生产 fire-and-forget spawn 监督日志 |
| FX-05 | e5179ea | IPC 出口 delta 批内合并（DeltaCoalescer 4 语义测试）+ 借用 Envelope 直发；ensure_session_log 记忆化；codex params 借用×5；emit 单消费者免 clone×2 |
| FX-06 | 0431522 | json_byte_len 零分配计数（与 to_string 逐字节等价，钉子测试）；权限摘要复用审计串（每次工具调用少一次全量 JSON 序列化） |
| FX-07 | a97e56c | provider 助手簇抽到 provider_support 叶子模块，**断开 memory_runtime→commands 文件环**；commands 文档失真声明修正 |
| FX-10 | 598544c | provider 名称身份判定唯一源 r-code-core::provider_identity（governor 内联清单与 is_deepseek_native_provider 改委托） |
| FX-12 | 7abba1d | 配置原子写统一 fs_util::atomic_write（settings/feature_flags/mcp_settings）；版本字段受 vendor 约束记遗留 |
| FX-19 | add69cf | MCP 失败驱逐半死会话；应用退出收束终端 PTY（kill_all 并入 2s 退出预算） |
| FX-20 | e9b2ff5 | 工作区 provider 网络面覆盖 fail-closed（base_url/api_key/protocol/provider_kind 剥除+留痕）；NSIS 进度文件限 $TEMP 前缀；CSP 副本对齐权威源并注明 |
| FX-13 | 864be33 | 单日日志 64MB 字节上限（跨日重置、内存缓冲不受影响）；启动水合反向分块 tail 读（10000 行只读尾部）；控制台格式器与落盘同源脱敏 |
| FX-14 | 21f02aa | provider 调用指标（requests/failures/retries/aborted/延迟聚合，AgentLoopOutcome 增 stream_recoveries）+ cmd_provider_metrics 暴露 |
| FX-17 | 3ff8375 | 前端格式化收敛：formatDurationMs/formatDateTimeMedium/formatDateTimeCompact 唯一实现，三处本地副本改委托（tsc 0 错误） |

## 三、测试与验证（最终回归）

- **cargo 全仓**：`evidence/final-cargo-test.log` —— **2322 通过 / 0 失败 / EXIT=0**（76 个测试二进制；基线为 1661 通过 / 1 环境性失败，该失败已由 FX-18 修复）。
- **过程中分项**：host lib 707/707、agent-worker 302/302、core 174+、store 519+、gateway 226/226、mcp 17/17、meta 测试 82/82、金集 fast 41/41。
- **全仓 0 warning**（cargo check --workspace --all-targets；CI clippy -D warnings 可过）。
- **前端**：tsc --noEmit 0 错误；build 验证 `evidence/final-frontend-build.log`（EXIT=0）；受影响测试文件（memory-ui 5/5、runs-panel-v2 4/4、codex-message-stream 3/3）绿。**全量 npm test 未在最终态重跑**（基线 249/304，53 个既有失败属用户 WIP 重构中源文件 + 本地 Playwright 超时，见 KNOWN-FAILURES.md；修复任务验收均以"不新增失败"为口径逐文件验证）。
- **未跑**：三平台 CI（本地仅 Windows；linux-cfg lint 有既有记忆盲区）、真机 macOS。

## 四、遗留清单（如实声明）

### 4.1 本次未执行的修复任务（大体量，设计已在 fix-plan 冻结）

| 任务 | 覆盖 findings | 未执行原因 | 就绪度 |
| --- | --- | --- | --- |
| FX-08 host 裸 SQL 收敛到 store | F-arch-02 (major) | ~20 处生产 SQL 逐点迁移为 repository 方法 + 语义钉住，体量 ≈ 数小时级专项；commands.rs 为 41k 行上帝模块，迁移即拆分 campaign 首期 | 设计+位点清单在 fix-plan/findings |
| FX-11 IPC 结构化错误契约 | F-corr-07 (major) | 177 个 command 签名迁移 + 前端错误规范化联动，同上体量 | 同上 |
| FX-15 Room 轮询降频 | F-perf-07 (major) | 行为变更面大（轮询语义测试敏感），且 RoomScene/Canvas 为用户 WIP 重构中文件，风险叠加 | 设计在 fix-plan |
| FX-16 Markdown 增量解析 | F-perf-09 (major) | 算法改造 + 渲染一致性钉住难度高，独立专项 | 设计在 fix-plan |

### 4.2 受外部约束的遗留（需 foritin/agent-contracts 子模块推送，超出本分支授权）

F-robust-04（anthropic SSE watchdog）、F-robust-09（退避抖动）、F-corr-05（dialect 双表）、F-corr-08（IPC u32 截断）、F-perf-05（SessionStore 逐事件落盘）、F-perf-02.vendor 部分（CompletionRequest 借用/Arc 接口）、FX-12 的 config 版本字段（agent-config::Config）。

### 4.3 campaign 级遗留（按设计延后）

F-arch-01/04/05（commands.rs/llm_runtime 拆分——FX-07 已交付首片）、F-maint-05（39 个超长函数）、F-obs-12（i18n 3557 条硬编码，基线锁防增量）、F-test-02（unwrap lint 门禁需先清 159 处生产 unwrap；锁中毒类已由 FX-04 消除）、F-corr-10（store 43 个 async fn 同步 IO spawn_blocking 化）、FX-06 目录 Arc 管线（Vec 返回的 trait 链 + 逐轮 policy 过滤依赖）、FX-07 的 plan_entry_commands↔commands CommandState 环（随拆分 campaign 解）。

### 4.4 minor 未修清单

F-sec-02/04/05/06、F-perf-11/12/13/14/15(部分)、F-maint-06/08/09/11、F-maint-10（.gitignore 补漏已在 FX-18 完成；根目录 *.log 建议人工清理，未代删）、F-test-05/07/08/09、F-obs-03（锁串行化部分）/09。

## 五、Revert 指引

- 分支整体未合并、未推送：`git checkout main` 即回到原状；工作树无游离改动（全部已提交在本分支）。
- 单任务回退：每个 FX-NN 一个提交（`git log --oneline` 见第二节对照表），`git revert <sha>` 即可；FX-00 wip 保全提交建议最后动（其后提交的 diff 干净性依赖它）。
- 用户 WIP：FX-00（6097a7a）原样保全了任务开始前的全部未提交工作；style 提交（2f2dfe3）只含空白格式化。

## 六、验证声明（如实）

- **已验证**：上表 19 个任务各自的验收命令与测试（见各任务 evidence/与 fix-plan）；最终全仓 cargo 回归与前端 build（日志在 evidence/）。
- **未验证**：CI 三平台矩阵（本地仅 Windows）；全量前端 e2e 最终态重跑（既有 53 失败未归因本次改动，逐文件对比未新增）；vendor 子模块相关修复（未执行）。

# 项目上下文引擎 · 验收前置（附录 A 落盘）

> 依据：[PRD §11 附录 A](../prd.md) 与 [§7 里程碑](../prd.md)。本目录是独立交付物，定义 M1a（及后续 M2/M3）验收用的 fixture、标准任务集、基线脚本与判定口径。PRD 为唯一需求权威；本目录只做可执行的验收落地，不新增需求。

## 布局

```
docs/prd/project-context/acceptance/
├─ README.md              # 本文件：流程、判定口径、运行前提
├─ fixtures.md            # A/B 组 fixture 定义与固定 commit 锚点
├─ tasks.json             # 标准任务集：每组 12 条（4 定位 + 4 修改 + 4 规划）+ 0–2 rubric
├─ scores.template.json   # 人工评分模板（复制为 scores-<group>.json 后填写）
├─ scripts/
│  ├─ prepare-fixtures.mjs  # 克隆/固定/校验 A、B 组 fixture（落 sandbox/，不入库）
│  ├─ run-baseline.mjs      # 单臂跑一组任务：逐任务驱动 daemon、收 token/耗时/事件
│  └─ score-summary.mjs     # 汇总两臂指标 + 人工评分，输出 M1a 判定
└─ artifacts/             # 基线运行产物（跑完后提交作证据）
   ├─ baseline-<group>-<arm>.json
   ├─ patches/<group>-<arm>/<taskId>.patch   # 修改类任务的 workspace diff 取证
   └─ verdict-m1a.json
```

大型产物不进 Git：fixture 克隆、daemon data-dir、事件转储都在 `sandbox/project-context-acceptance/`（已在 `.gitignore` 的 `/sandbox/` 下）。

## 运行前提

1. **Rust 工作区可构建**（`vendor/agent-contracts` 子模块已初始化）。
2. **一个有效 Provider 凭据**（唯一硬前置）。一次性环境装配脚本
   `sandbox/project-context-acceptance/scripts/bootstrap-baseline-env.mjs` 会：
   - 在隔离 profile（`data-baseline` + 管道 `m1a-baseline`，**不碰真实 profile**）拉起 daemon；
   - 安装 native builtin harness（staged manifest + 二进制）；
   - 配置 Provider 走 `envVar` 凭据：经 `sandbox/cred-bridge`（一次性小工具，**不落盘不打印**）
     从 GUI 凭据命名空间（service `r-code`，account `provider:<name>`）桥接为 daemon 环境变量；
   - 设默认并验证 `models.available` 的 `has_credential`。
   本机现状（2026-10-01 实测）：GUI 存的 deepseek key（35 字符、尾号 569d）已被服务端吊销（401）；
   openrouter/anthropic 条目是 10/12 字符掩码占位。**因此本机当前跑不了真实基线**——
   换一台有有效凭据的机器，或在本机凭据库补一个有效 key 后，从 bootstrap 步骤开始即可。
3. **注入开关臂控**：驱动二进制通过 RPC `context.settings.update { workspacePath, injectionEnabled }` 切换注入（M1a-06 起已实现）。
   两臂共用同一个 bootstrap daemon（臂差 = 每 workspace 的注入开关），runner 默认已指向该 profile。

## 流程（附录 A.3 的机械落地）

```bash
# 0a) 一次性环境装配（隔离 daemon + 插件 + Provider；可重复运行，复用已起 daemon）
node sandbox/project-context-acceptance/scripts/bootstrap-baseline-env.mjs

# 0b) 准备 fixture（A 组=本仓库快照克隆；B 组=jq 固定 tag）
node docs/prd/project-context/acceptance/scripts/prepare-fixtures.mjs

# 1) 采集 off 臂（新功能关闭）
node docs/prd/project-context/acceptance/scripts/run-baseline.mjs --group a --arm off
node docs/prd/project-context/acceptance/scripts/run-baseline.mjs --group b --arm off

# 2) 人工评分 off 臂（对每条任务给 0/1/2，依据 events 转储 + patch 取证）
cp docs/prd/project-context/acceptance/scores.template.json sandbox/project-context-acceptance/scores/scores-a-off.json
#    ...填写后同理 b-off

# 3) FR-1 落地后采集 on 臂，评分同理
node docs/prd/project-context/acceptance/scripts/run-baseline.mjs --group a --arm on
node docs/prd/project-context/acceptance/scripts/run-baseline.mjs --group b --arm on

# 4) 汇总判定
node docs/prd/project-context/acceptance/scripts/score-summary.mjs
```

## 判定口径（M1a）

| 指标 | 来源 | 门限 |
| --- | --- | --- |
| token 消耗 | 事件流 `model.usage` 四桶求和（`usage.totalTokens`） | A 组 on 臂总 token ≤ off 臂的 90%（下降 ≥10%） |
| 完成度均分 | 人工评分 0/1/2 取均分（两组臂同一批评分口径） | A 组 on 臂均分 ≥ off 臂（不降） |
| B 组 | 同上两项 | 对照组，只报告不设门（B 组无指令文件，注入近似空操作） |

- 非稳定结算（超时/中断）的任务 token 照计、完成度记 0 分（与 PRD"完成度"语义一致：未完成就是未完成）。
- 单臂单遍运行（PRD 附录 A.3 明确"跑一遍"）；如需复核可整臂重跑并保留两份产物，判定取后跑的一份并在 verdict 中注明。
- 记忆注入有账、只读子代理可回传：不以任务集度量，由 M1a 各 FR 的任务级验收（cargo 测试 + 代码断言）覆盖。

## 基线策略（两臂恒同，保证只测上下文特性）

- 目标即首条用户消息，单 run 结束为终态（`run.completed/failed/cancelled`）。
- 待决审批一律自动批准（`approvals.decide`，审计身份=驱动连接 client id），两臂相同策略；批准数计入产物。
- 每条任务前把 fixture 恢复纯净（`git checkout -- .` + `git clean -xfd`，含子模块），修改类任务结束后导出 diff 取证。
- 驱动二进制：`cargo run --quiet -p r-code-evals --bin context-baseline -- …`（CLI 契约见其文件头注释）。

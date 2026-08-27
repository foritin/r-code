# Plan 双轨三臂评估（M0-11 / M0-12）

> **定位变更（2026-08-22）**：预注册证据门已移除（见
> `docs/support/archive/implementation/settings-ux-and-image-understanding.md` A3）——客户滑钮是唯一开关，
> 评估结果**不再阻塞功能启用**。本目录降级为**可选的事后质量回归工具**：
> 用户反馈建议异常时先 `R_CODE_PLANNING_EMERGENCY_OFF=1` 急停，再用本协议
> 复测质量后再恢复。

对应设计文档：`docs/support/archive/implementation/plan-mode-dual-track-gate.md` §16（能力实验、路由实验、
Provider 来源与原始证据、预注册发布门）。本目录是**冻结协议**：任何改动阈值、
case 或 probe 都必须以新证据版本重跑完整评估，不能挑 case 补跑覆盖原结论。

## 目录

- `schema/preregistration.json` —— 预注册：三臂/数量/隔离要求与全部发布门阈值。
  真实运行前冻结（hash 进入 manifest）。
- `schema/corpus.schema.json` —— case 元数据 schema。
- `corpus/<case>/` —— 25 个自包含 case（5 类 × 5）：
  - `case.json` 任务指令 + 期望复杂度信号；
  - `fixture/` 只读起始工作区（hash 冻结）；
  - `oracle.patch` 参考修复（git 程序化生成，可直接 `git apply` 到工作区）；
  - `verify.mjs` 确定性验收（`node verify.mjs <工作区>`：fixture 必红、oracle 后必绿）。
- `corpus-lock.json` —— 全部 case/probe 文件的 sha256 锁。
- `routing/probes.json` —— 40 个只读路由 probe（20 simple + 20 complex），与能力
  实验完全分离，不进入 75 次成功率统计。
- `scripts/generate-corpus.mjs` —— 只在创建/修订 corpus 时运行；产物入库后冻结。
- `scripts/validate-corpus.mjs` —— 数量/分层/红绿/锁校验（`--freeze` 生成新锁）。
- `scripts/score.mjs` —— 只消费 raw results + preregistration 生成 manifest；
  任一预注册门失败即非零退出，不产出部分结论。
- `scripts/verify-manifest.mjs` —— 独立重算 manifest 每个数字（claim verification）。

## 运行协议

正式运行必须在**提交且 tracked worktree / 子模块干净**的版本上执行，并显式冻结
同一批 capability + routing 共用的证据身份、随机种子和价格。评测器不会猜测未来
DeepSeek 型号的价格，也不会把 `unknown` commit 或 0 token 当成证据：

```bash
DEEPSEEK_API_KEY=... \
PLAN_EVAL_EVIDENCE_VERSION=deepseek-plan-v1-2026-08-21 \
PLAN_EVAL_SEED=deepseek-plan-v1-seed-01 \
PLAN_EVAL_MODEL=deepseek-v4-flash \
PLAN_EVAL_PROTOCOL=openai_chat \
PLAN_EVAL_INPUT_USD_PER_MILLION=... \
PLAN_EVAL_CACHE_READ_USD_PER_MILLION=... \
PLAN_EVAL_OUTPUT_USD_PER_MILLION=...
```

三个费率必须来自本次运行冻结并留档的官方价格表：普通/缓存未命中输入使用
`INPUT`，缓存命中输入使用 `CACHE_READ`，输出使用 `OUTPUT`。不得为通过成本记录
而编造或沿用未经确认的价格。

```bash
# 1) 冻结校验（每次评估前）
node scripts/validate-corpus.mjs

# 2) 能力三臂（非 dry-run 需要 DEEPSEEK_API_KEY；fail closed 只认原生 DeepSeek）
cargo run -p r-code-host --bin plan_eval -- --corpus corpus --out artifacts

# 3) 路由 probe（自动建议开关在路由环境强制开启）
cargo run -p r-code-host --bin plan_eval -- routing --probes routing/probes.json --out artifacts

# 4) 计分 → manifest（全部预注册门通过才写出 artifacts/manifest.json）
node scripts/score.mjs

# 5) 独立重算（发布前必须通过）
node scripts/verify-manifest.mjs

# 管道冒烟（不产生证据；dry-run 记录被 score.mjs 拒绝）
cargo run -p r-code-host --bin plan_eval -- --dry-run --case <case> --arm <arm> --out <scratch-artifacts>
```

> 历史说明：manifest 曾由 `src-tauri/build.rs` 嵌入二进制并在运行时重验；证据门
> 移除后该链路已删除，manifest 只作为本目录内的质量回归产物保留。

## 隔离与失败规则（预注册）

- 每个 `(case_id, arm)` 使用独立 workspace / SQLite / session 目录 / runtime；
  `environment_fingerprint` 必须三臂互异，否则 score 拒绝（arm 污染）。
- capability 按 `SHA256(seed, case, arm)`、routing 按 `SHA256(seed, probe)` 做可复现
  随机排序；正式运行禁止 `--case` / `--arm`，防止挑选 case 覆盖完整证据。
- Plan 两臂在批准前比较 fixture / workspace 的确定性树摘要；任何漂移都记为
  `unapproved_side_effects`，且 harness 不再批准该 Plan。
- 每条记录必须能对账 origin request / operation / run ID、逐 run usage、正数
  rounds/tokens、重试、成本、真实 git commit、config/profile/fixture/diff/hash。
- `diagnostics.request_audit` 在隔离环境强制开启。完整 session sidecar 只存在于操作
  系统临时目录；发布树仅保存 RequestHeader 哈希字段和工具名等脱敏摘要，以及其
  artifact URI + SHA-256。API key 只从进程环境读取，评测器不把它写入 config、
  scratch、session 或 artifacts；scorer/独立 verifier 发现 artifacts 中任何
  `secrets.json` 都会拒绝。
- 记录缺失 / 重复 / 共享状态 / 非法重试 / raw artifact 不可获取 / Provider 来源
  不匹配，全部 fail closed。
- dry-run 记录显式标记 `dry_run: true`，只做管道冒烟，不进入任何统计。
- `raw-manifest.json` 只有在 75 + 40 条记录、两个 raw 文件 digest 与同一冻结身份
  全部齐备时才是 `complete`；`score.mjs` 开始时删除旧 `manifest.json`，失败不会
  留下陈旧绿灯。`verify-manifest.mjs` 不复用 scorer，实现独立重算 token median、
  wall p95、McNemar、路由率、成本和所有 raw/artifact/hash 声明。

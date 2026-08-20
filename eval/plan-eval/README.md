# Plan 双轨三臂评估（M0-11 / M0-12）

对应设计文档：`docs/plan-mode-dual-track-gate.md` §16（能力实验、路由实验、
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

```bash
# 1) 冻结校验（每次评估前）
node scripts/validate-corpus.mjs

# 2) 能力三臂（非 dry-run 需要 DEEPSEEK_API_KEY；fail closed 只认原生 DeepSeek）
cargo run -p r-code-host --bin plan_eval -- --corpus corpus --out artifacts

# 3) 路由 probe（自动建议开关在路由环境强制开启）
cargo run -p r-code-host --bin plan_eval -- routing

# 4) 计分 → manifest（全部预注册门通过才写出 artifacts/manifest.json）
node scripts/score.mjs

# 5) 独立重算（发布前必须通过）
node scripts/verify-manifest.mjs

# 管道冒烟（不产生证据；dry-run 记录被 score.mjs 拒绝）
cargo run -p r-code-host --bin plan_eval -- --dry-run ...
```

manifest 通过后由 `src-tauri/build.rs` 嵌入二进制（复制到本目录
`artifacts/manifest.json`），`plan_policy::load_validated_manifest` 在运行时再次
独立重验——构建产物绝不能携带半份证据。

## 隔离与失败规则（预注册）

- 每个 `(case_id, arm)` 使用独立 workspace / SQLite / session 目录 / runtime；
  `environment_fingerprint` 必须三臂互异，否则 score 拒绝（arm 污染）。
- 记录缺失 / 重复 / 共享状态 / 非法重试 / raw artifact 不可获取 / Provider 来源
  不匹配，全部 fail closed。
- dry-run 记录显式标记 `dry_run: true`，只做管道冒烟，不进入任何统计。

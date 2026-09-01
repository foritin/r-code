# RV-09 证据 — 日志与可观测性 / 配置管理 / 文档与代码漂移

日期：2026-08-29。环境：Windows/Git Bash，仓库 D:\project\rust\r-code，HEAD 49f9193 + 未提交 WIP。所有命令为只读。

## E-01 工作树基线

```
$ git log -3 --format="%h %ci %s"
49f9193 2026-08-28 17:41:46 +0800 feat(*): product-experience worklist 42/42 闭环 + 默认中文/updater 静音/窗口记忆
82c8c5c 2026-08-27 13:19:28 +0800 fix(docs): make redesign gates checkout-stable
a1afe40 2026-08-27 13:02:15 +0800 feat(*): complete product experience redesign and platform contracts

$ git status --short | wc -l        # → 48（含 WIP：llm_runtime、gen/schemas、i18n baseline、redesign docs 等）
```

## E-02（F-obs-04）无指标 crate

```
$ grep -rn "metrics\|prometheus\|histogram\|counter" --include="*.toml" Cargo.toml src-tauri/Cargo.toml crates | grep -v test
（无输出）

$ grep -n "tracing\|metrics" Cargo.toml
53:tracing = "0.1"
54:tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
55:tracing-appender = "0.2"
```

窄口径计数器证据：

```
$ grep -n "cmd_request_audit_counters" src-tauri/src/tauri_commands.rs
1760:pub async fn cmd_request_audit_counters(
# 其 doc 注释：A4：请求信封审计自检计数（headers_appended, mismatches）。
# Real runtime 不在场时返回 None；soak 期间经 devtools 消费，不进设置 UI。

$ grep -n "request_audit" crates/r-code-gateway/src/diagnosis.rs
180:/// 读取旁路命中计数（request_audit 式：只含类别与次数）。
```

## E-03（F-obs-01/02/03）日志滚动/保留/锁

- logging.rs:49 `tracing_appender::rolling::daily(&log_dir, crate::log_buffer::LOG_FILE_PREFIX);`（无 max_size 参数；tracing-appender 0.2 daily 不支持大小上限）
- log_buffer.rs:28 `pub const LOG_RETENTION_DAYS: i64 = 7;`
- log_buffer.rs:84-96（on_event 持全局锁）：
```rust
let entry = LogEntry { timestamp: chrono::Utc::now().to_rfc3339(), ... message: redact_text(&visitor.finish()) };
let mut buf = buffer().lock().unwrap();
if buf.len() >= CAPACITY { buf.pop_front(); }
buf.push_back(entry.clone());
```
- log_buffer.rs:123-141 `hydrate_from_persistence`：`read_persisted_entries(log_dir)?`（全量逐行 `serde_json::from_str`）→ retain 7 天 → `if entries.len() > CAPACITY { drain }`。
- logging.rs:62-63（release 可覆盖级别）：
```rust
let env_filter = EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| EnvFilter::new("info,tauri_plugin_updater=off"));
```

## E-04（1b 密度）info! 分布

```
$ grep -rc "info!" crates/*/src/*.rs src-tauri/src/*.rs | grep -v ":0" | sort -t: -k2 -rn | head
src-tauri/src/main.rs:13
crates/r-code-agent-worker/src/llm_runtime.rs:12
src-tauri/src/commands.rs:7
src-tauri/src/rtk.rs:3
crates/r-code-mcp/src/web.rs:2
...
```
llm_runtime.rs 全部 12 处 info! 行号与内容（均为低频事件，带 session_id/task_id）：3845（视觉检查点）、4671/4687/4743（压缩暂停/hint/fold）、5110（P2-H 前缀缓存形状变化）、5346（plan 目录晋升）、5922（run 收尾投影计数）、8226/8269（子代理派生计划确认）、9685/9699/9745（child 侧压缩三档）。未发现每消息粒度的 info!。

前端双写检查：

```
$ grep -rn "console\.\(log\|info\|debug\)" src-tauri/frontend/src --include="*.ts" --include="*.tsx" | wc -l
1
```

provider 失败日志：`llm_runtime.rs:4253 fn log_provider_request_failure(...)`，调用点 5743、10113；记录消息形状统计（structured items、hosted web items、function_call_pairs）与服务端错误类别。

## E-05（F-obs-05/06/08）配置写路径

```
$ sed -n "763,772p" src-tauri/src/settings.rs
    fn write_global(&self, config: &Config) -> Result<(), ProductError> {
        let path = self.config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let toml_str = toml::to_string(config)
            .map_err(|e| ProductError::ConfigError(format!("serialize config: {e}")))?;
        std::fs::write(&path, toml_str)?;      # ← 非原子
        Ok(())
    }
```

对照（同仓原子写范式）：

```
$ grep -n "NamedTempFile\|persist(" src-tauri/src/mcp_settings.rs
444:    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
448:        .persist(path)
$ sed -n "464p" src-tauri/src/settings.rs
464:        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;   # agent-prompts.toml
# support_bundle.rs:128-130 tempfile 写 + persist（同目录 rename）
```

```
$ sed -n "91,98p" src-tauri/src/feature_flags.rs
    pub fn save(&self, flags: ProductFeatureFlags) -> Result<(), ProductError> {
        std::fs::create_dir_all(&self.config_dir)?;
        let content = toml::to_string_pretty(&flags)...;
        std::fs::write(self.path(), content)?;     # ← 非原子
        Ok(())
    }
```

迁移现状：`settings.rs:715 migrate_legacy_provider_secrets`、`:738 migrate_legacy_provider_kinds`（按空值/None 触发的一次性迁移）；`grep -n "schema_version\|SETTINGS_VERSION" src-tauri/src/settings.rs` 无输出。

配置损坏的下游影响（load_global_unvalidated 消费点）：

```
$ grep -n "load_global_unvalidated" src-tauri/src/commands.rs | head
2057/3583/5085/6334/6950/7388/7535/15012/15086/15114（任务启动、模型发现、设置读写等）
```

## E-06（F-obs-07）CSP 双源比对

```
$ node -e "
const fs=require('fs');
const conf=JSON.parse(fs.readFileSync('src-tauri/tauri.conf.json','utf8'));
const rust=fs.readFileSync('src-tauri/src/security_config.rs','utf8');
const m=rust.match(/csp: \"([^\"]+)\"/);
console.log('conf :', conf.app.security.csp);
console.log('rust :', m && m[1]);
console.log('equal:', conf.app.security.csp === (m && m[1));"
conf : default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self' ipc: http://ipc.localhost
rust : default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ipc: http://ipc.localhost
equal: false
```

SecurityConfig 消费面（production() 无调用方）：

```
$ grep -rn "SecurityConfig" --include="*.rs" src-tauri/src/ | grep -v security_config.rs
src-tauri/src/lib.rs:81:pub use security_config::{should_block_navigation, should_block_window_open, SecurityConfig};
# main.rs 仅使用 should_block_navigation（main.rs:327）
```

## E-07（2c）catalog/capabilities 单入口

```
$ grep -n "provider_catalog" src-tauri/src/model_capabilities.rs
18:use crate::provider_catalog::{self, Preset};
95:  None => protocol_label(provider_catalog::resolve_protocol(
126: let preset = provider_catalog::preset_for(
154: resolved.vision_budget = provider_catalog::vision_budget_for(preset, &resolved.model_id);

$ git log -1 --format="%ci %h" -- src-tauri/src/provider_catalog.rs
2026-08-23 23:00:24 +0800 b879307
$ grep -c "base_url\|endpoint" src-tauri/src/provider_catalog.rs
124
```

## E-08（3d/3e）文档与版本

README 引用完整性（全部存在）：

```
$ for f in docs/readme.md docs/architecture.md docs/support/guides/plan-mode.en.md \
  docs/support/guides/mcp.md docs/support/guides/memory.md docs/support/operations/operations.en.md \
  docs/support/operations/releasing.md SUPPORT.md CODE_OF_CONDUCT.md SECURITY.md PRIVACY.md LICENSE \
  scripts/release.mjs scripts/build-branded-installer.ps1 scripts/manual/package-macos.sh; do ...
→ 全部 OK（无 MISSING）
# presign 变体 cwd:"../scripts" → scripts/presign-macos-bin.sh 存在
```

版本同步：

```
$ grep -n '^version' Cargo.toml          → 26:version = "1.0.0"
$ grep -n '"version"' src-tauri/tauri.conf.json → "version": "1.0.0"
$ head -8 CHANGELOG.md → ## [Unreleased]（空） / ## [1.0.0] - 2026-08-23
```

docs/ 一级目录最后实质更新：

```
docs/product-experience-redesign/ → 2026-08-28 17:41:46 +0800
docs/support/                     → 2026-08-27 13:19:28 +0800
docs/code-review-2026-08-29/      → 未提交（本次 review）
```

CI 门禁与 CONTRIBUTING 一致性：

```
$ grep -n "cargo deny\|cargo-deny" .github/workflows/ci.yml
226:  deny:
237:      - name: Install cargo-deny
240:          tool: cargo-deny@0.20.2
241:      - name: cargo deny check
$ grep -n "supply-chain" .github/workflows/release.yml → 187（--strict 生成，210 行）
$ grep -n "branches:" -A1 .github/workflows/ci.yml → push/pull_request 均为 [main, dev]
```

提交前缀（AGENTS/CONTRIBUTING 约定 vs 实际）：git log -15 显示 `feat(*)/fix(docs)/fix(test)/docs(prd)/test(corpus)/ci(test)/merge:` 等，与前缀约定一致。

## E-09（F-obs-10）readme.md 进度漂移

```
$ grep -n "1/42\|当前进度" docs/readme.md
26:| [产品体验重构 PRD / AI 实施清单](./product-experience-redesign/r-code-experience-redesign-prd.md) | `frozen`，当前进度 `1/42`；本次只完成原型和实施合同，产品代码尚未按清单实施 |

$ node -e "console.log(JSON.stringify(require('./docs/product-experience-redesign/worklist-gate.json')).slice(0,400))"
{"schema_version":"ai-worklist-gate.v1","mode":"update_freeze","worklist_id":"product-experience-gap-closure",
 "document":"docs/product-experience-redesign/r-code-experience-redesign-prd.md","passed":true,
 "counts":{"requirements":64,"checklist_tasks":42,"task_cards":42,"assertions":176}, ...}
```

## E-10（F-obs-11）AGENTS.md 工具规约

```
$ cat -n AGENTS.md | sed -n "8,10p;19,20p;26p"
8:- 按**文件名**找文件 → `glob`，必填 `pattern`（如 `**/*.rs`、`**/AGENTS.md`）
9:- 按**内容**搜文件 → `search_files`，必填 `path` + `pattern`
10:- 搜**网页** → `search`，必填 `queries`
19:- 读文件用 `read_file`，不用 cat/type/find/ls。
20:- 修改文件用 `edit`（精确替换），不要用 `apply_patch` 整文件重写。
26:- 变更前先 `git_status` 了解当前状态；提交遵循现有 `CHANGELOG.md` 与 `CONTRIBUTING.md` 约定
```

## E-11（4 / F-obs-12）i18n

基线机制（i18n-hardcoded.test.mjs:146-153，deepEqual 锁定）：

```js
test("JSX user copy cannot grow outside the reviewed i18n migration baseline", () => {
  const expected = JSON.parse(fs.readFileSync(baselinePath, "utf8"));
  assert.deepEqual(observed, expected,
    "Hardcoded JSX copy changed. New copy must use i18n keys; ...");
});
```

统计：

```
$ node -e "const b=require('./src-tauri/frontend/scripts/i18n-hardcoded-baseline.json'); \
const files=Object.keys(b).length; const total=Object.values(b).reduce((s,v)=>s+v.count,0); \
console.log('files:',files,'total hardcoded copy:',total)"
files: 73 total hardcoded copy: 3557

$ node -e "...sort by count desc, top8..."
509 src/components/scenes/SettingsScene.tsx
422 src/components/room/Canvas.tsx
170 src/components/plan/PlanPanel.tsx
132 src/components/room/Composer.tsx
125 src/components/scenes/McpPanel.tsx
125 src/components/scenes/MemoryPanel.tsx
116 src/components/scenes/KnowledgeSettingsPane.tsx
112 src/components/onboarding/OnboardingCampaign.tsx

$ wc -l src-tauri/frontend/src/i18n/locales/*.json
  201 src-tauri/frontend/src/i18n/locales/en-US.json
  201 src-tauri/frontend/src/i18n/locales/zh-CN.json
```

默认中文落点（i18n/index.ts）：

```
34-38: /** 2026-08-28 产品决定：默认语言始终为中文（zh-CN）。… */
export function localeFromLanguages(_languages) { return "zh-CN"; }
41-60: resolveInitialLocale — explicit 存档跟随；无 source 标记的旧"跟随系统"存档一次性重置 zh-CN；无存档 zh-CN
89:   fallbackLng: "zh-CN",
```

## E-12 tauri conf 变体（无漂移结论依据）

```
$ ls src-tauri/tauri.conf*.json
tauri.conf.json  tauri.dev.conf.json  tauri.local-package.conf.json
tauri.macos.conf.json  tauri.presign-macos.conf.json

# dev：productName "R-Code Dev" + updater endpoints dev-latest.json（与 README/dev 隔离叙述一致）
# macos：identifier com.rcode.desktop + runner ../scripts/cargo-tauri-macos-runner.sh（存在）
# presign-macos：beforeBundleCommand "bash presign-macos-bin.sh" cwd "../scripts" → scripts/presign-macos-bin.sh（存在）
# local-package：bundle.createUpdaterArtifacts=false
# gen/schemas/desktop-schema.json、windows-schema.json 为构建生成物（git status modified，WIP 正常）
```

## E-13 支持包与脱敏（正面确认依据）

- 白名单 DTO：support_bundle.rs:67-76 `McpServerSupportSummary { id, transport_kind, enabled, state, error_class }`（doc 注释：字段只能描述状态，不能承载启动配置或秘密）。
- 只读统计：:195-204 `Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)`，DB 缺失返回 0 且不创建文件。
- 原子导出：:128-130 NamedTempFile + persist（同目录 rename）。
- 三重脱敏：log_buffer.rs:89（落盘前 redact_text）、:196-199（磁盘读回再脱敏，注释"Older files may predate a newly added redaction rule"）、support_bundle.rs:169（导出再脱敏）。
- 收集口径：MAX_LOG_LINES=200，仅 WARN/ERROR（:90、:159-174）。

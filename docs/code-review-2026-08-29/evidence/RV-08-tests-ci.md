# RV-08 测试与 CI 覆盖 — Evidence（2026-08-29）

环境：Windows / Git Bash，仓库 D:\project\rust\r-code，HEAD 49f9193。除注明「实跑」的两条 node --test 外全部为静态扫描（rg/grep），未修改任何文件。

## E1. critical 路径源文件与内联测试计数

命令：
```
$ grep -c "cfg(test)" / grep -c "#\[test\]" （逐文件）
src-tauri/src/codex_permissions.rs   cfg(test)=1  test=5
src-tauri/src/close_gate.rs          cfg(test)=2  test=6
src-tauri/src/shutdown_coordinator.rs cfg(test)=1 test=4
src-tauri/src/security_config.rs     cfg(test)=1  test=15
src-tauri/src/windows_ocr.rs         cfg(test)=1  test=3
src-tauri/src/mac_ocr.rs             cfg(test)=1  test=3
src-tauri/src/subagent_providers.rs  cfg(test)=1  test=13
src-tauri/src/search.rs              cfg(test)=1  test=5
crates/r-code-store/src/migrations.rs cfg(test)=1 test=18
crates/r-code-core/src/security.rs（PathGuard） test=32, 1535 行
```

集成测试目录对 critical 模块名的引用（codex_permissions/subagent_providers/close_gate/shutdown_coordinator/windows_ocr/mac_ocr）：
```
$ grep -rln "codex_permissions\|subagent_providers\|close_gate\|shutdown_coordinator\|windows_ocr\|mac_ocr" src-tauri/tests/ crates/*/tests/
（无输出 —— 6 模块无集成测试，全部依赖内联 #[cfg(test)]）
```

## E2. 权限默认拒绝断言（codex_permissions.rs:356-394）

```
fn absent_configuration_preserves_the_historical_read_only_default() {
    assert_eq!(
        CodexDelegationPermissions::from_config(None, None, None),
        CodexDelegationPermissions::read_only()   // 默认拒绝有断言
    );
}
fn unknown_config_never_becomes_an_unvalidated_cli_argument() {
    ...assert_eq!(profile.sandbox().as_str(), "read-only");  // 未知值回落
}
```

## E3. 迁移回滚故障注入断言（migrations.rs test mod）

```
fn assert_fault_rolls_back_and_retries(fault_point: MigrationFaultPoint) {
    assert!(run_migrations_with_specs(&conn, &migrations, Some(&fault)).is_err());
    assert_eq!(rolled_back, (0, 0, 0));      // 回滚后零残留
    ...
    assert_eq!(committed, (1, 1, 1));        // 重试提交
}
fn migration_faults_rollback_ddl_data_and_version_then_retry() { 逐故障点参数化 }
```

## E4. 降级链最后一跳/循环防护断言（llm_runtime_tests.rs）

```
:3124  assert_eq!(requests.len(), 2, "fallback must be attempted exactly once");
:3143  async fn deepseek_hosted_web_fallback_never_loops_after_the_local_retry_fails()
:8303  fn deepseek_hosted_web_fallback_accepts_only_tool_contract_rejections_once()
       （should_fallback_from_deepseek_hosted_web 5 case 纯函数断言）
```

## E5. #[ignore] 与 skip 全仓为零

```
$ grep -rn '#\[ignore' src-tauri/src src-tauri/tests crates --include="*.rs"（含 '= 变体统计）
0
$ grep -rn '\.skip(\|skip: true' src-tauri/frontend（node:test skip）
（无测试文件命中）
```

## E6. flaky-tests.yml 与 ci.yml 的执行模式漂移

```
ci.yml:200          run: cargo test --workspace --all-features -- --test-threads=1
flaky-tests.yml:94  -- cargo test --workspace --all-features        （无 --test-threads=1）
flaky-tests.yml:75  matrix.os: [macos-latest, windows-latest]        （rust 腿无 ubuntu）
flaky-tests.yml:7   cron: "17 3 * * 1"                               （每周一次）
```

## E7. meta 测试 CI 覆盖（实跑 + workflow 引用扫描）

ci.yml 引用的全部 scripts：
```
$ grep -on "scripts/[a-z0-9./-]*" .github/workflows/ci.yml | sort -u
scripts/flaky-test-report.test.mjs
scripts/icon-assets.test.mjs
scripts/release-quality-gate.test.mjs
scripts/release.test.mjs
scripts/verify-codex-interaction.test.mjs
scripts/check-installer-frontend.mjs
scripts/release.mjs
scripts/windows-reliability/corpus-run.mjs
```
仓库共有 7 个 meta 测试（另含 verify-product-experience.test.mjs、verify-windows-reliability.test.mjs），后两者零 workflow 引用（release.yml 亦无）。

实跑（2026-08-29，唯一执行的两条测试命令）：
```
$ node --test scripts/verify-product-experience.test.mjs
ℹ tests 12
ℹ pass 11
ℹ fail 1
  ✖ AssertionError: fixture 需要 M9-04 存在未接线断言
    at scripts/verify-product-experience.test.mjs:105:10
（M9-04 已在 scripts/product-experience/wiring.mjs:221-222 接线 m9-04-checks.mjs，
 fixture 前提消失 → 测试腐烂，因不在 CI 无人发现）

$ node --test scripts/verify-windows-reliability.test.mjs
ℹ tests 11  pass 11  fail 0  exit=0
```

## E8. clippy lint 配置缺失

```
$ find . -maxdepth 2 -name "clippy.toml" -o -name ".clippy.toml"（排除 target）
（无命中）
$ grep -n "\[lints\|lints\." Cargo.toml src-tauri/Cargo.toml crates/*/Cargo.toml
（无命中）
$ ci.yml:135  run: cargo clippy --workspace --all-targets -- -D warnings   （仅默认 lint 集）
$ grep -c "\.unwrap()" src-tauri/src/commands.rs
1221
$ Cargo.toml:142  panic = "abort"
```

## E9. 金集 slow 档无门禁

```
$ grep '"slow"' crates/r-code-gateway/tests/command_corpus/corpus.jsonl
{"id":"slow-sleep-ok","cmd":"sleep 2 && echo slow-ok",...,"tier":"slow",...}
{"id":"slow-exit-1","cmd":"sleep 1; exit 1",...}
{"id":"slow-chain","cmd":"sleep 1 && echo step-a && sleep 1 && echo step-b","platform":"windows",...}
$ grep -rn "tier slow\|--tier all" .github/ scripts --include=*.yml --include=*.mjs
（除 corpus-run.mjs 自身参数定义外零引用；ci.yml:206 仅 --tier fast）
$ command_corpus_runner.rs:233-243  未设 CORPUS_RUN → 打印提示并 return（记 passed 非 ignored）
```

## E10. sleep 密度（全部测试代码）

```
$ grep -rc "sleep(" <各测试文件> 非 0 者：
crates/r-code-agent-worker/src/llm_runtime_tests.rs  27（5-300ms，最大 :6438=300ms）
crates/r-code-terminal/src/manager.rs                 8（内联）
src-tauri/tests/final_delivery.rs                     1（:438=250ms）
src-tauri/tests/health_check.rs                       2（:26,:45=50ms）
crates/r-code-terminal/tests/control_integration.rs   1（:57=40ms）
>1s 固定 sleep：0
```

## E11. 平台门控测试

```
$ src-tauri/src/lib.rs:20  #[cfg(unix)] pub mod control_door;     → 34 测仅 unix
$ src-tauri/src/lib.rs:28  #[cfg(target_os = "macos")] mod mac_ocr; → 3 测仅 macOS
$ src-tauri/src/lib.rs:59  #[cfg(target_os = "windows")] mod windows_ocr; → 3 测仅 Windows
  （windows_ocr.rs:8-13 含真实 Windows OCR 引擎对本地 fixture 的识别断言）
$ ci.yml:148 matrix.os = [ubuntu, macos, windows]，三腿均跑 cargo test --all-features
$ ci.yml:196 Windows 前端腿仅 app-shell/companion-window-ui/companion 三文件
  （companion.test.mjs 为纯 fs.readFileSync 静态断言，与宿主 OS 无关）
$ 正确范式：crates/r-code-store/src/verification.rs:537-565
  successful_command()/failing_command()/sleeping_command() 按 cfg 提供平台自适应命令夹具
```

## E12. 前端测试分层

```
$ find src-tauri/frontend -name "*.test.mjs"（排除 node_modules）= 61 文件
动态（chromium.launch/newPage 命中）= 39；纯静态 = 22
app-shell.test.mjs:        95 test / 584 assert / launch 系 97 处，headless: true（:103）
companion-window-ui.test.mjs: 107 assert；companion.test.mjs: 165 assert（全静态）
package.json: "test": "node scripts/run-tests.mjs"
run-tests.mjs:33: ["--test", "--test-concurrency=1", ...]（串行，支持位置参数选文件）
静态断言示例 model-switcher.test.mjs:19: assert.match(switcher, /\.filter\(\(choice\) => choice\.ready\)/)
```

## E13. 测试质量抽样（一次性脚本，跑完已删除）

```
脚本：解析 #[test]/#[tokio::test] 后续函数体，统计无 assert/expect 的测试函数
llm_runtime_tests.rs: total=160 assertless=0   （847 assert 行 → 5.3/测）
codex_interaction_tests.rs: total=25  assertless=0 （197 → 7.9/测；冻结 fixture
  fixtures/codex-interaction/protocol-0.145.0.json include_str wire 校验）
plan_store.rs: total=44 assertless=0            （244 → 5.5/测）
migrations.rs: total=18 assertless=1 → migration_is_idempotent（连续 unwrap 链=隐式断言，非空转）
subagent_providers.rs / close_gate.rs / codex_permissions.rs: assertless=0
```

## E14. CI 历史状态（仓库根遗留日志，2026-08-23 抓取）

```
ci-frontend-meta.log（ubuntu Frontend job，npm test）：
  # tests 202 / # pass 198 / # fail 2 / # skipped 2 / exit code 1
  not ok 74 - saved providers auto-sync models on open; manual sync stays in the new-provider flow
  not ok 183 - session strip exposes mutually exclusive step/change popovers and preserves the room when opening a file
  （另前置 meta 步 47/47 通过）
ci-ubuntu-test.log（ubuntu Test job，cargo test）：
  21 个 "test result:" 中 1 个 FAILED：
  tests::dry_run_collects_profile_without_starting_a_provider_run_or_leaking_secrets
    panicked at src-tauri/src/bin/plan_eval.rs:1920:
    called `Result::unwrap()` on an `Err` value: "dual-track arm froze an unexpected runtime profile"
```
（注：为 6 天前抓取的遗留日志，仅证明该时点双红；当前工作树状态未重跑全量验证。）

## E15. Rust 测试总量

```
$ 全仓 #[test]+#[tokio::test] 计数 = 2095
  src-tauri/src 内联（不含 *_tests.rs）= 728；src-tauri/tests 集成 = 94
  llm_runtime_tests.rs 单文件 = 160（1607/2804/9967 行三大测试文件见 E13）
$ r-code-terminal 内联合计 = 127（manager 31 / replay_parser 43 / control_service 17 /
  block 17 / shell_integration 11 / cli_detector 8）
```

/**
 * R20.A2 — 中继安全守卫（static）：部署物料存在且结构完整、四安全守卫
 * 在 R16/R18/R19 测试面有归属、默认无中继配置、密文不可见断言存在。
 */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..");

test("R20.A2: 部署物料齐备（Dockerfile 非 root + systemd 样例 + 手册）", () => {
  const dockerfile = join(root, "crates", "r-code-relay", "Dockerfile");
  assert.ok(existsSync(dockerfile), "Dockerfile 存在");
  const text = readFileSync(dockerfile, "utf8");
  assert.match(text, /USER 65534/, "非 root 运行");
  assert.match(text, /R_CODE_RELAY_CODES/, "注册码经环境注入");
  assert.match(text, /debian:bookworm-slim/, "最小运行镜像");

  const guide = readFileSync(
    join(root, "docs", "support", "guides", "relay-deployment.md"),
    "utf8",
  );
  for (const section of [
    "## 1. 最小规格与运行",
    "## 2. 升级 / 日志 / 备份",
    "## 3. 安全验收清单",
    "## 4. 与真实 VPS 的边界",
    "NoNewPrivileges",
    "systemd",
  ]) {
    assert.ok(guide.includes(section), `手册缺少「${section}」`);
  }
  assert.equal(/\bTBD\b|TODO/.test(guide), false, "手册无悬空标记");
});

test("R20.A2: 四安全守卫在测试面有归属", () => {
  const r16 = readFileSync(
    join(root, "crates", "r-code-relay", "tests", "r16_relay_integration.rs"),
    "utf8",
  );
  assert.match(r16, /r16_a2_audit_is_content_free/, "密文不可见守卫");
  assert.match(r16, /owner_code_used/, "注册码一次性守卫");
  const r19 = readFileSync(
    join(root, "crates", "r-code-runtime", "tests", "r19_relay_path.rs"),
    "utf8",
  );
  assert.match(r19, /r19_a2_revocation_enforced/, "吊销纵深守卫");
  const r18 = readFileSync(
    join(root, "crates", "r-code-runtime", "tests", "r18_relay_config.rs"),
    "utf8",
  );
  assert.match(r18, /no config → no outbound dials/, "默认无中继配置守卫");
});

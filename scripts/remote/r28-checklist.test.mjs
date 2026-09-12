/**
 * R28.A1/A2 — 真机外网清单完整性（static）：外部放行清单覆盖全部 5 个
 * 必经环节（组网/蜂窝/审批/吊销/高敏），与 worklist 的外部放行声明一致。
 * 真机执行本身=外部放行，不进 CI。
 */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..");
const path = join(
  root,
  "docs",
  "prd",
  "remote-control",
  "production-release-checklist.md",
);

test("R28: 真机外网验收清单存在且覆盖全部必经环节", () => {
  assert.ok(existsSync(path), "production-release-checklist.md 存在");
  const text = readFileSync(path, "utf8");
  for (const section of [
    "真机组网与远程监听",
    "真实蜂窝往返",
    "真机审批",
    "吊销",
    "高敏终桌面确认",
  ]) {
    assert.ok(text.includes(section), `缺少环节「${section}」`);
  }
  for (const item of [
    "after_seq 续传不丢",
    "证书指纹逐段核对",
    "revoked",
    "需要在电脑上确认",
  ]) {
    assert.ok(text.includes(item), `缺少取证项「${item}」`);
  }
});

test("R28: worklist 明确声明外部放行边界", () => {
  const worklist = readFileSync(
    join(root, "docs", "prd", "remote-control", "worklist.md"),
    "utf8",
  );
  const lines = worklist.split("\n");
  const r28 = lines.find((line) => line.includes("**R28**"));
  assert.ok(r28, "R28 行存在");
  assert.match(r28, /真机外网验收清单/, "R28 指向真机清单");
  assert.ok(
    /外部放行|production_release_ready/.test(r28),
    "R28 标注外部放行",
  );
});

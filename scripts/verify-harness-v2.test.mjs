import { test } from "node:test";
import { execFileSync } from "node:child_process";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

function verify(profile) {
  return execFileSync("node", [join(root, "scripts", "verify-harness-v2.mjs"), "--profile", profile], {
    encoding: "utf8",
    timeout: 15 * 60 * 1000,
  });
}

test("quick profile passes architecture guards and conformance", () => {
  const output = verify("quick");
  if (!output.includes("all ")) {
    throw new Error(`quick profile did not pass:\n${output}`);
  }
  for (const guardName of [
    "protocol crate exists",
    "kernel has no tauri/sqlite/gateway",
    "native plugin is host-free",
    "protocol doc exists",
  ]) {
    if (!output.includes(guardName)) {
      throw new Error(`missing guard ${guardName}`);
    }
  }
});

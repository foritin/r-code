import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoDir = path.resolve(frontendDir, "..", "..");
const toolCard = fs.readFileSync(
  path.join(frontendDir, "src/components/room/ToolCard.tsx"),
  "utf8",
);
const timeline = fs.readFileSync(
  path.join(frontendDir, "src/components/room/TimelineActivity.tsx"),
  "utf8",
);
const host = fs.readFileSync(path.join(repoDir, "crates/r-code-mcp/src/host.rs"), "utf8");
const runtime = fs.readFileSync(
  path.join(repoDir, "crates/r-code-agent-worker/src/llm_runtime.rs"),
  "utf8",
);

test("Agent exposes fixed MCP search and confirmation-preparation tools", () => {
  for (const name of ["mcp_registry_search", "mcp_prepare_install", "mcp_prepare_enable"]) {
    assert.match(host, new RegExp(`name: "${name}"\\.to_string\\(\\)`));
  }
  assert.match(runtime, /`mcp_registry_search` searches the official preview Registry/);
  assert.match(runtime, /They never install, write configuration, enable a service/);
  assert.doesNotMatch(runtime, /You cannot install or enable an MCP service/);
});

test("conversation confirmation cards are bound to exact MCP tools and host-issued tokens", () => {
  assert.match(toolCard, /toolName !== "mcp_prepare_install" && toolName !== "mcp_prepare_enable"/);
  assert.match(toolCard, /value\.status !== "confirmation_required"/);
  assert.match(toolCard, /mcpMarketInstall\(action\.request, action\.preview\.token\)/);
  assert.match(toolCard, /mcpToggle\(action\.serverId, true, action\.preview\?\.token \?\? null\)/);
  assert.match(toolCard, /preview\.server_id !== request\.server_id/);
  assert.match(toolCard, /启动方案已审批/);
});

test("MCP confirmations open automatically and replace noisy raw payloads", () => {
  assert.match(toolCard, /if \(hasMcpConfirmation\) setOpen\(true\)/);
  assert.match(toolCard, /!mcpConfirmation && output/);
  assert.match(toolCard, /transport\.args\.map\(\(arg, index\)/);
  assert.match(timeline, /item\.tools\.some\(\(tool\)[^]*hasMcpConfirmationPayload/);
  assert.match(timeline, /if \(hasMcpConfirmation\) setOpen\(true\)/);
});

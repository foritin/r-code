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
const commands = fs.readFileSync(path.join(repoDir, "src-tauri/src/commands.rs"), "utf8");
const manager = fs.readFileSync(path.join(repoDir, "src-tauri/src/mcp_manager.rs"), "utf8");
const workflowSkills = fs.readFileSync(path.join(repoDir, "src-tauri/src/workflow_skills.rs"), "utf8");
const mcpPanel = fs.readFileSync(
  path.join(frontendDir, "src/components/scenes/McpPanel.tsx"),
  "utf8",
);

test("Agent exposes fixed MCP search and confirmation-preparation tools", () => {
  for (const name of ["mcp_registry_search", "mcp_prepare_install", "mcp_prepare_enable"]) {
    assert.match(host, new RegExp(`name: "${name}"\\.to_string\\(\\)`));
  }
  assert.match(runtime, /`mcp_registry_search` searches the official preview Registry/);
  assert.match(runtime, /They never install, write configuration, enable a service/);
  assert.doesNotMatch(runtime, /You cannot install or enable an MCP service/);
  assert.match(manager, /fn name\(&self\) -> &str \{\s*"mcp_create_draft"/);
  assert.match(manager, /fn name\(&self\) -> &str \{\s*"mcp_save_draft"/);
  assert.match(commands, /CreateMcpDraftTool::new\(mcp_manager\.clone\(\)\)/);
  assert.match(commands, /SaveMcpDraftTool::new\(mcp_manager\.clone\(\)\)/);
  assert.match(runtime, /"mcp_create_draft"/);
  assert.match(runtime, /`mcp_save_draft`/);
  assert.match(workflowSkills, /"mcp-creator"/);
  assert.match(workflowSkills, /只在 transport 中声明环境变量名/);
  assert.match(workflowSkills, /点“配置”输入变量值/);
  assert.match(workflowSkills, /不得启动、测试、注册或启用服务/);
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
  assert.match(toolCard, /if \(hasMcpConfirmation \|\| hasMcpSettingsAction\) setOpen\(true\)/);
  assert.match(toolCard, /!mcpConfirmation && !mcpSettingsAction && output/);
  assert.match(toolCard, /transport\.args\.map\(\(arg, index\)/);
  assert.match(timeline, /item\.tools\.some\(\(tool\)[^]*hasMcpConfirmationPayload[^]*hasMcpSettingsActionPayload/);
  assert.match(timeline, /if \(hasMcpAction\) setOpen\(true\)/);
});

test("generated MCP drafts only deep-link to manual settings review", () => {
  assert.match(toolCard, /toolName !== "mcp_create_draft" && toolName !== "mcp_save_draft"/);
  assert.match(toolCard, /value\.action !== "open_mcp_settings"/);
  assert.match(toolCard, /草稿已创建，尚未启用/);
  assert.match(toolCard, /openMcpSettings\(null, action\.serverId\)/);
});

test("MCP HTTP editor explains the exact loopback cleartext boundary", () => {
  assert.match(mcpPanel, /<option value="streamable_http">HTTP \/ HTTPS<\/option>/);
  assert.match(mcpPanel, />服务地址<input/);
  assert.match(mcpPanel, /远程服务必须使用 HTTPS/);
  for (const host of ["localhost", "127.0.0.1", "\[::1\]"]) {
    assert.match(mcpPanel, new RegExp(host));
  }
  assert.match(toolCard, /transport\.url\.startsWith\("http:\/\/"\) \? "本机 HTTP" : "远程 HTTPS"/);
});

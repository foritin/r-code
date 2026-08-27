#!/usr/bin/env node
// 从 `codex app-server generate-json-schema --out <dir>` 的产物中提取
// docs/support/contracts/codex-rich-interaction-prd.md 涉及的最小协议 fixture。
//
// fixture 是离线重放的唯一事实源：Rust host 测试与 Node 检查脚本共用，
// 不依赖真实 Codex 登录或账号。升级 Codex CLI 时重跑本脚本并复用
// check-protocol-fixture.mjs 做 schema drift 门禁。
//
// 用法：
//   codex app-server generate-json-schema --out <schema-dir>
//   node scripts/codex-interaction/extract-protocol-fixture.mjs \
//     --schema-dir <schema-dir> --cli-version $(codex --version | awk '{print $2}') \
//     --out fixtures/codex-interaction/protocol-<version>.json

import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import process from "node:process";

// PRD §4/§5 涉及的 server notification 白名单。新增能力任务时在下游
// fixture 补帧，再考虑扩这个表；删除条目等于放弃该事件合同，需过 freeze。
const NOTIFICATION_WHITELIST = [
  "error",
  "warning",
  "thread/started",
  "thread/status/changed",
  "thread/tokenUsage/updated",
  "thread/compacted",
  "turn/started",
  "turn/completed",
  "turn/plan/updated",
  "turn/diff/updated",
  "item/started",
  "item/completed",
  "item/agentMessage/delta",
  "item/reasoning/summaryTextDelta",
  "item/reasoning/summaryPartAdded",
  "item/reasoning/textDelta",
  "item/commandExecution/outputDelta",
  "item/fileChange/outputDelta",
  "item/fileChange/patchUpdated",
  "serverRequest/resolved",
];

// PRD §4.3 的反向请求合同；response 来自 *Response.json 顶层 schema。
const SERVER_REQUEST_WHITELIST = ["item/tool/requestUserInput"];

function parseArgs(argv) {
  const args = { _: [] };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    const next = argv[i + 1];
    if (flag === "--schema-dir") {
      args.schemaDir = next;
      i += 1;
    } else if (flag === "--cli-version") {
      args.cliVersion = next;
      i += 1;
    } else if (flag === "--out") {
      args.out = next;
      i += 1;
    } else if (flag === "--captured-at") {
      args.capturedAt = next;
      i += 1;
    } else {
      args._.push(flag);
    }
  }
  return args;
}

function sha256(content) {
  return createHash("sha256").update(content).digest("hex");
}

function methodName(entry) {
  const method = entry.properties?.method;
  return method?.const ?? method?.enum?.[0] ?? null;
}

// 递归内联 $ref，使每条提取结果自包含；definitions 完全来自生成物，
// 不做任何改写。循环引用在 App Server schema 中不存在（schemars 展开
// 前已是 DAG）；若未来出现环，这里会抛错并阻止提取。
function inlineRefs(node, definitions, seen = new Set()) {
  if (Array.isArray(node)) {
    return node.map((item) => inlineRefs(item, definitions, seen));
  }
  if (node === null || typeof node !== "object") {
    return node;
  }
  if (Object.prototype.hasOwnProperty.call(node, "$ref")) {
    const name = node.$ref.replace(/^#\/definitions\//, "");
    if (seen.has(name)) {
      throw new Error(`circular $ref detected: ${name}`);
    }
    const target = definitions[name];
    if (!target) {
      throw new Error(`unresolved $ref: ${node.$ref}`);
    }
    return inlineRefs(target, definitions, new Set([...seen, name]));
  }
  const result = {};
  for (const [key, value] of Object.entries(node)) {
    result[key] = inlineRefs(value, definitions, seen);
  }
  return result;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (!args.schemaDir || !args.cliVersion || !args.out || args._.length > 0) {
    console.error(
      "usage: extract-protocol-fixture.mjs --schema-dir <dir> --cli-version <semver> --out <file> [--captured-at <iso8601>]",
    );
    process.exitCode = 2;
    return;
  }
  if (!/^\d+\.\d+\.\d+$/.test(args.cliVersion)) {
    console.error(`--cli-version must be plain semver, got: ${args.cliVersion}`);
    process.exitCode = 2;
    return;
  }

  const schemaDir = resolve(args.schemaDir);
  const readJson = async (name) => {
    const content = await readFile(resolve(schemaDir, name), "utf8");
    return { content, parsed: JSON.parse(content) };
  };

  const [serverNotification, serverRequest, userInputParams, userInputResponse] = await Promise.all(
    [
      readJson("ServerNotification.json"),
      readJson("ServerRequest.json"),
      readJson("ToolRequestUserInputParams.json"),
      readJson("ToolRequestUserInputResponse.json"),
    ],
  );

  const notifications = {};
  const missing = [];
  for (const entry of serverNotification.parsed.oneOf ?? []) {
    const method = methodName(entry);
    if (method && NOTIFICATION_WHITELIST.includes(method)) {
      notifications[method] = {
        method,
        title: entry.title ?? null,
        params_schema: inlineRefs(entry.properties?.params ?? { type: "null" }, serverNotification.parsed.definitions ?? {}),
      };
    }
  }
  for (const method of NOTIFICATION_WHITELIST) {
    if (!notifications[method]) {
      missing.push(method);
    }
  }

  const serverRequests = {};
  for (const entry of serverRequest.parsed.oneOf ?? []) {
    const method = methodName(entry);
    if (method && SERVER_REQUEST_WHITELIST.includes(method)) {
      serverRequests[method] = {
        method,
        title: entry.title ?? null,
        params_schema: inlineRefs(entry.properties?.params ?? { type: "null" }, serverRequest.parsed.definitions ?? {}),
      };
    }
  }
  for (const method of SERVER_REQUEST_WHITELIST) {
    if (!serverRequests[method]) {
      missing.push(method);
    }
  }
  if (missing.length > 0) {
    console.error(`whitelisted methods missing from generated schema: ${missing.join(", ")}`);
    process.exitCode = 1;
    return;
  }

  serverRequests["item/tool/requestUserInput"].response_schema = inlineRefs(
    userInputResponse.parsed,
    userInputResponse.parsed.definitions ?? {},
  );

  // 样例帧由 PRD §4.3 与上述 schema 手工锚定；check-protocol-fixture.mjs
  // 会对每帧跑 mini JSON-Schema 校验，schema 变化时此处会红灯。
  const sampleFrames = {
    request_user_input_request: {
      $schema_ref: "server_requests.item/tool/requestUserInput.params_schema",
      frame: {
        jsonrpc: "2.0",
        id: 41,
        method: "item/tool/requestUserInput",
        params: {
          threadId: "thr_demo",
          turnId: "turn_demo",
          itemId: "item_demo",
          autoResolutionMs: null,
          questions: [
            {
              id: "scope",
              header: "范围",
              question: "本次处理哪一部分？",
              isOther: true,
              isSecret: false,
              options: [{ label: "当前模块", description: "限制变更范围" }],
            },
          ],
        },
      },
    },
    request_user_input_success_response: {
      $schema_ref: "server_requests.item/tool/requestUserInput.response_schema",
      frame: {
        jsonrpc: "2.0",
        id: 41,
        result: {
          answers: {
            scope: { answers: ["当前模块"] },
          },
        },
      },
    },
    agent_message_delta: {
      $schema_ref: "notifications.item/agentMessage/delta.params_schema",
      frame: {
        jsonrpc: "2.0",
        method: "item/agentMessage/delta",
        params: {
          threadId: "thr_demo",
          turnId: "turn_demo",
          itemId: "item_msg_demo",
          delta: "正在检索配置入口…",
        },
      },
    },
    warning: {
      $schema_ref: "notifications.warning.params_schema",
      frame: {
        jsonrpc: "2.0",
        method: "warning",
        params: { message: "demo warning", threadId: "thr_demo" },
      },
    },
  };

  const fixture = {
    schema_version: "codex-interaction-protocol-fixture.v1",
    meta: {
      source: "codex app-server generate-json-schema",
      cli_version: args.cliVersion,
      captured_at: args.capturedAt ?? new Date().toISOString(),
      extraction: {
        notifications: NOTIFICATION_WHITELIST.slice().sort(),
        server_requests: SERVER_REQUEST_WHITELIST.slice().sort(),
        note: "schemas are inlined verbatim from the generated output; sample frames are hand-authored against them",
      },
      source_digests: {
        "ServerNotification.json": sha256(serverNotification.content),
        "ServerRequest.json": sha256(serverRequest.content),
        "ToolRequestUserInputParams.json": sha256(userInputParams.content),
        "ToolRequestUserInputResponse.json": sha256(userInputResponse.content),
      },
    },
    server_requests: serverRequests,
    notifications: notifications,
    sample_frames: sampleFrames,
  };

  const outPath = resolve(args.out);
  await mkdir(dirname(outPath), { recursive: true });
  await writeFile(outPath, `${JSON.stringify(fixture, null, 2)}\n`, "utf8");
  console.log(
    `wrote ${outPath}: ${Object.keys(notifications).length} notifications, ` +
      `${Object.keys(serverRequests).length} server requests, ` +
      `${Object.keys(sampleFrames).length} sample frames (codex-cli ${args.cliVersion})`,
  );
}

await main();

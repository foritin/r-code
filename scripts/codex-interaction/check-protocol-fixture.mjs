#!/usr/bin/env node
// M0-01.A2 断言命令：离线校验 fixtures/codex-interaction/protocol-<ver>.json
// 与 Codex 0.145.0 requestUserInput 合同一致，且来源版本可机器读取。
//
// 校验分四层（全部离线，不需要安装/登录 Codex CLI）：
//   1. meta 完整性：cli_version 与文件名版本一致、捕获时间、来源摘要。
//   2. requestUserInput 必需字段/响应映射逐项等于生成 schema 的冻结快照。
//   3. 每个样例帧通过 mini JSON-Schema 校验（帧与 schema 不再同步时红灯）。
//   4. 凭据扫描：fixture 全文不得出现 token/api key 形态的字符串。

import { readFile } from "node:fs/promises";
import process from "node:process";
import { pathToFileURL } from "node:url";
import { validateAgainstSchema } from "./schema-mini-validate.mjs";

const REQUEST_USER_INPUT = "item/tool/requestUserInput";
const CREDENTIAL_PATTERNS = [
  /sk-[A-Za-z0-9_-]{8,}/,
  /api[_-]?key\s*[:=]/i,
  /bearer\s+[A-Za-z0-9._-]{8,}/i,
  /(password|passwd|secret)\s*[:=]\s*['"]?[^\s'"]{4,}/i,
];

export function checkProtocolFixture(fixture, fixtureName = "protocol-0.145.0.json") {
  const issues = [];
  const push = (message) => issues.push(message);

  const meta = fixture?.meta ?? {};
  const cliVersion = meta.cli_version;
  if (typeof cliVersion !== "string" || !/^\d+\.\d+\.\d+$/.test(cliVersion)) {
    push(`meta.cli_version must be plain semver, got: ${JSON.stringify(cliVersion)}`);
  } else {
    const nameMatch = fixtureName.match(/^protocol-(\d+\.\d+\.\d+)\.json$/);
    if (!nameMatch) {
      push(`fixture filename must be protocol-<semver>.json, got: ${fixtureName}`);
    } else if (nameMatch[1] !== cliVersion) {
      push(`fixture filename version ${nameMatch[1]} != meta.cli_version ${cliVersion}`);
    }
  }
  if (typeof meta.source !== "string" || meta.source.length === 0) {
    push("meta.source missing");
  }
  if (typeof meta.captured_at !== "string" || Number.isNaN(Date.parse(meta.captured_at))) {
    push(`meta.captured_at must be ISO-8601, got: ${JSON.stringify(meta.captured_at)}`);
  }
  for (const [file, digest] of Object.entries(meta.source_digests ?? {})) {
    if (!/^[0-9a-f]{64}$/.test(digest)) {
      push(`meta.source_digests[${file}] must be sha256 hex, got: ${digest}`);
    }
  }

  const request = fixture?.server_requests?.[REQUEST_USER_INPUT];
  if (!request) {
    push(`server_requests["${REQUEST_USER_INPUT}"] missing`);
  } else {
    const methodEnum = request.params_schema && request.method;
    if (methodEnum !== REQUEST_USER_INPUT) {
      push(`server_requests method must be ${REQUEST_USER_INPUT}, got: ${JSON.stringify(request.method)}`);
    }
    const required = request.params_schema?.required ?? null;
    const expectedRequired = ["itemId", "questions", "threadId", "turnId"].sort().join(",");
    if ((required ?? []).slice().sort().join(",") !== expectedRequired) {
      push(`params required fields [${required}] != 0.145.0 contract [${expectedRequired}]`);
    }
    const question = request.params_schema?.properties?.questions?.items;
    const questionRequired = question?.required ?? [];
    if (questionRequired.slice().sort().join(",") !== "header,id,question") {
      push(`question required fields [${questionRequired}] != [header,id,question]`);
    }
    for (const flag of ["isOther", "isSecret"]) {
      const prop = question?.properties?.[flag];
      if (prop?.type !== "boolean" || prop.default !== false) {
        push(`question.${flag} must be boolean default false, got: ${JSON.stringify(prop)}`);
      }
    }
    const optionRequired = question?.properties?.options?.items?.required ?? [];
    if (optionRequired.slice().sort().join(",") !== "description,label") {
      push(`option required fields [${optionRequired}] != [description,label]`);
    }
    if (!Array.isArray(question?.properties?.options?.type)) {
      // 0.145.0 里 options 是 ["array","null"] 的宽松联合，收紧即 drift。
      const optionTypes = JSON.stringify(question?.properties?.options?.type);
      if (optionTypes !== JSON.stringify(["array", "null"])) {
        push(`question.options.type must be ["array","null"], got: ${optionTypes}`);
      }
    }
    const autoResolution = request.params_schema?.properties?.autoResolutionMs;
    if (JSON.stringify(autoResolution?.type) !== JSON.stringify(["integer", "null"])) {
      push(`params.autoResolutionMs.type must be ["integer","null"], got: ${JSON.stringify(autoResolution?.type)}`);
    }

    const responseRequired = request.response_schema?.required ?? [];
    if (responseRequired.join(",") !== "answers") {
      push(`response required [${responseRequired}] != [answers]`);
    }
    const answerSchema = request.response_schema?.properties?.answers?.additionalProperties;
    const answerRequired = answerSchema?.required ?? [];
    if (answerRequired.join(",") !== "answers") {
      push(`response answer required [${answerRequired}] != [answers]`);
    }
    if (answerSchema?.properties?.answers?.items?.type !== "string") {
      push("response answer answers[] items must be string");
    }
  }

  for (const [name, sample] of Object.entries(fixture?.sample_frames ?? {})) {
    const ref = sample?.$schema_ref;
    if (typeof ref !== "string" || ref.length === 0) {
      push(`sample_frames.${name}.$schema_ref missing`);
      continue;
    }
    const node = ref
      .split(".")
      .reduce((acc, segment) => (acc && typeof acc === "object" ? acc[segment] : undefined), fixture);
    if (!node) {
      push(`sample_frames.${name}.$schema_ref ${ref} unresolved`);
      continue;
    }
    const instance = ref.endsWith(".response_schema") ? sample.frame?.result : sample.frame?.params;
    const errors = validateAgainstSchema(instance ?? null, node);
    for (const error of errors) {
      push(`sample_frames.${name} violates ${ref}: ${error}`);
    }
  }

  const fixtureText = JSON.stringify(fixture);
  for (const pattern of CREDENTIAL_PATTERNS) {
    const match = fixtureText.match(pattern);
    if (match) {
      push(`credential-like content in fixture: /${pattern.source}/ matched "${redact(match[0])}"`);
    }
  }

  return issues;
}

function redact(value) {
  return value.length > 12 ? `${value.slice(0, 6)}…${value.slice(-3)}` : "…";
}

async function main() {
  const fixturePath = process.argv[2] ?? "fixtures/codex-interaction/protocol-0.145.0.json";
  let fixture;
  try {
    fixture = JSON.parse(await readFile(fixturePath, "utf8"));
  } catch (error) {
    console.error(`cannot read fixture ${fixturePath}: ${error.message}`);
    process.exitCode = 2;
    return;
  }
  const name = fixturePath.replaceAll("\\", "/").split("/").pop();
  const issues = checkProtocolFixture(fixture, name);
  if (issues.length > 0) {
    console.error(`protocol fixture check FAILED (${fixturePath}):`);
    for (const issue of issues) {
      console.error(`  - ${issue}`);
    }
    process.exitCode = 1;
    return;
  }
  const notificationCount = Object.keys(fixture.notifications ?? {}).length;
  console.log(
    `protocol fixture check passed: ${fixturePath} (codex-cli ${fixture.meta.cli_version}, ` +
      `${notificationCount} notifications, ${Object.keys(fixture.sample_frames ?? {}).length} sample frames)`,
  );
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main();
}

#!/usr/bin/env node

import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";

function parseArgs(argv) {
  const options = { mode: "check" };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (!argument.startsWith("--")) throw new Error(`unexpected argument: ${argument}`);
    const key = argument.slice(2);
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) throw new Error(`missing value for --${key}`);
    options[key] = value;
    index += 1;
  }
  for (const required of ["document", "freeze", "report"]) {
    if (!options[required]) throw new Error(`missing --${required}`);
  }
  if (!new Set(["compute", "check"]).has(options.mode)) {
    throw new Error("--mode must be compute or check");
  }
  return options;
}

function extractMarked(text, name) {
  const start = `<!-- AI_WORKLIST_${name}_START -->`;
  const end = `<!-- AI_WORKLIST_${name}_END -->`;
  const startIndex = text.indexOf(start);
  const endIndex = text.indexOf(end);
  if (startIndex < 0 || endIndex < 0 || endIndex <= startIndex) {
    throw new Error(`missing or invalid ${name} markers`);
  }
  return text.slice(startIndex + start.length, endIndex);
}

function normalize(text) {
  return text
    .replaceAll("\r\n", "\n")
    .replaceAll("\r", "\n")
    .replace(/^- \[[ xX]\] (\*\*M\d+-\d{2}\*\*.*?证据：).*$/gmu, "- [ ] $1<volatile>")
    .split("\n")
    .map((line) => line.trimEnd())
    .join("\n")
    .trim();
}

function sha256(text) {
  return createHash("sha256").update(text, "utf8").digest("hex");
}

function unique(values) {
  return [...new Set(values)];
}

function duplicates(values) {
  const seen = new Set();
  const repeated = new Set();
  for (const value of values) {
    if (seen.has(value)) repeated.add(value);
    seen.add(value);
  }
  return [...repeated];
}

function parseTaskCards(contract) {
  const matches = [...contract.matchAll(/^### (M\d+-\d{2})\s+(.+)$/gmu)];
  return matches.map((match, index) => {
    const start = match.index;
    const end = matches[index + 1]?.index ?? contract.length;
    const body = contract.slice(start, end);
    const dependencyLine = body.match(/^- 依赖：(.+)$/mu)?.[1] ?? "";
    const dependsOn = dependencyLine === "无。" || dependencyLine === "无"
      ? []
      : unique([...dependencyLine.matchAll(/M\d+-\d{2}/gu)].map((item) => item[0]));
    const assertions = unique([...body.matchAll(/`(M\d+-\d{2}\.A\d+)`/gu)].map((item) => item[1]));
    return { id: match[1], title: match[2], dependsOn, assertions };
  });
}

function detectCycle(cards) {
  const graph = new Map(cards.map((card) => [card.id, card.dependsOn]));
  const visiting = new Set();
  const visited = new Set();
  function visit(id) {
    if (visiting.has(id)) return true;
    if (visited.has(id)) return false;
    visiting.add(id);
    for (const dependency of graph.get(id) ?? []) {
      if (visit(dependency)) return true;
    }
    visiting.delete(id);
    visited.add(id);
    return false;
  }
  return [...graph.keys()].some(visit);
}

function yamlSection(text, name, nextName) {
  const start = text.indexOf(`${name}:`);
  const end = text.indexOf(`\n${nextName}:`, start + name.length + 1);
  return start >= 0 && end > start ? text.slice(start, end) : "";
}

function yamlScalar(section, key) {
  return section.match(new RegExp(`^\\s*${key}:\\s*([^#\\n]+)`, "mu"))?.[1].trim() ?? null;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const [documentText, freezeText] = await Promise.all([
    readFile(options.document, "utf8"),
    readFile(options.freeze, "utf8"),
  ]);
  const normative = normalize(extractMarked(documentText, "NORMATIVE"));
  const contract = normalize(extractMarked(documentText, "CONTRACT"));
  const normativeDigest = sha256(normative);
  const worklistDigest = sha256(contract);
  const issues = [];

  const requirementIds = [...normative.matchAll(/\*\*(R-[A-Z]+-\d{2})（(?:MUST|SHOULD|MAY)）\*\*/gu)].map((match) => match[1]);
  for (const id of duplicates(requirementIds)) issues.push(`duplicate requirement id: ${id}`);

  const checklistIds = [...contract.matchAll(/^- \[[ xX]\] \*\*(M\d+-\d{2})\*\*/gmu)].map((match) => match[1]);
  for (const id of duplicates(checklistIds)) issues.push(`duplicate checklist task id: ${id}`);

  const cards = parseTaskCards(contract);
  const cardIds = cards.map((card) => card.id);
  for (const id of duplicates(cardIds)) issues.push(`duplicate task card id: ${id}`);
  for (const id of checklistIds.filter((id) => !cardIds.includes(id))) issues.push(`missing task card: ${id}`);
  for (const id of cardIds.filter((id) => !checklistIds.includes(id))) issues.push(`orphan task card: ${id}`);

  const cardIdSet = new Set(cardIds);
  const definedAssertions = new Set(cards.flatMap((card) => card.assertions));
  for (const card of cards) {
    if (card.assertions.length === 0) issues.push(`task has no assertion: ${card.id}`);
    for (const dependency of card.dependsOn) {
      if (!cardIdSet.has(dependency)) issues.push(`unknown dependency: ${card.id} -> ${dependency}`);
      if (dependency === card.id) issues.push(`self dependency: ${card.id}`);
    }
  }
  if (detectCycle(cards)) issues.push("task dependency graph contains a cycle");

  const traceRows = [...normative.matchAll(/^\| (R-[A-Z]+-\d{2}) \|[^\n]+$/gmu)].map((match) => match[0]);
  const tracedRequirements = traceRows.map((row) => row.match(/^\| (R-[A-Z]+-\d{2}) /u)?.[1]).filter(Boolean);
  for (const id of requirementIds.filter((id) => !tracedRequirements.includes(id))) issues.push(`requirement missing trace row: ${id}`);
  for (const row of traceRows) {
    const requirement = row.match(/^\| (R-[A-Z]+-\d{2}) /u)?.[1] ?? "unknown";
    for (const taskId of unique([...row.matchAll(/M\d+-\d{2}/gu)].map((match) => match[0]))) {
      if (!cardIdSet.has(taskId)) issues.push(`trace row ${requirement} references unknown task: ${taskId}`);
    }
    for (const assertionId of unique([...row.matchAll(/M\d+-\d{2}\.A\d+/gu)].map((match) => match[0]))) {
      if (!definedAssertions.has(assertionId)) issues.push(`trace row ${requirement} references undefined assertion: ${assertionId}`);
    }
  }

  const normativeSection = yamlSection(freezeText, "normative_input", "worklist");
  const worklistSection = yamlSection(freezeText, "worklist", "completion_gate");
  const gateSection = yamlSection(freezeText, "completion_gate", "material_change_triggers");
  const freezeState = {
    status: yamlScalar(freezeText, "status"),
    normativeDigest: yamlScalar(normativeSection, "digest"),
    worklistDigest: yamlScalar(worklistSection, "digest"),
    taskCount: Number(yamlScalar(worklistSection, "task_count")),
    requiredTaskCount: Number(yamlScalar(worklistSection, "required_task_count")),
    gatePassed: yamlScalar(gateSection, "passed"),
    blockingIssues: yamlScalar(gateSection, "blocking_issues"),
    majorIssues: yamlScalar(gateSection, "major_issues"),
  };

  if (options.mode === "check") {
    if (freezeState.status !== "frozen") issues.push(`freeze status is ${freezeState.status ?? "missing"}, expected frozen`);
    if (freezeState.normativeDigest !== normativeDigest) issues.push("normative digest mismatch");
    if (freezeState.worklistDigest !== worklistDigest) issues.push("worklist digest mismatch");
    if (freezeState.taskCount !== checklistIds.length) issues.push("freeze task_count mismatch");
    if (freezeState.requiredTaskCount !== checklistIds.length) issues.push("freeze required_task_count mismatch");
    if (freezeState.gatePassed !== "true") issues.push("freeze completion_gate.passed is not true");
    if (freezeState.blockingIssues !== "0") issues.push("freeze blocking_issues is not 0");
    if (freezeState.majorIssues !== "0") issues.push("freeze major_issues is not 0");
  }

  const report = {
    schema_version: "ai-worklist-gate.v1",
    mode: options.mode,
    document: options.document.replaceAll("\\", "/"),
    freeze: options.freeze.replaceAll("\\", "/"),
    passed: issues.length === 0,
    counts: {
      requirements: unique(requirementIds).length,
      checklist_tasks: checklistIds.length,
      task_cards: cards.length,
      assertions: definedAssertions.size,
    },
    digests: {
      normative: normativeDigest,
      worklist: worklistDigest,
    },
    issues,
  };
  await mkdir(path.dirname(options.report), { recursive: true });
  await writeFile(options.report, `${JSON.stringify(report, null, 2)}\n`, "utf8");
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
  if (!report.passed) process.exitCode = 1;
}

main().catch((error) => {
  process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
  process.exitCode = 2;
});

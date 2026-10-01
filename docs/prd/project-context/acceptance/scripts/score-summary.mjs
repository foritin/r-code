#!/usr/bin/env node
// Appendix-A score summary + M1a verdict (project-context acceptance).
// Joins the two baseline arms with the human score sheets and applies the
// PRD §7/M1a gate: instruction group (A) token total must drop >=10% on the
// on arm with completion mean not decreased. Group B is control-only.
//
// Usage:
//   node docs/prd/project-context/acceptance/scripts/score-summary.mjs
//
// Inputs (see README for locations):
//   artifacts/baseline-<group>-<off|on>.json
//   sandbox/project-context-acceptance/scores/scores-<group>-<off|on>.json
// Output:
//   artifacts/verdict-m1a.json + stdout table

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import path from "node:path";
import { fileURLToPath } from "node:url";
import process from "node:process";

const ACCEPT_ROOT = path.resolve(dirname(fileURLToPath(import.meta.url)), "..");
const ART_ROOT = path.join(ACCEPT_ROOT, "artifacts");
const SCORE_ROOT = path.resolve(
  ACCEPT_ROOT, "../../../..", "sandbox", "project-context-acceptance", "scores",
);

function loadBaseline(group, arm) {
  const file = path.join(ART_ROOT, `baseline-${group}-${arm}.json`);
  if (!existsSync(file)) return null;
  return JSON.parse(readFileSync(file, "utf8"));
}

function loadScores(group, arm) {
  const file = path.join(SCORE_ROOT, `scores-${group}-${arm}.json`);
  if (!existsSync(file)) return null;
  const parsed = JSON.parse(readFileSync(file, "utf8"));
  const map = new Map(parsed.scores.map((row) => [row.taskId, row.score]));
  return { scoredBy: parsed.scoredBy ?? "", map };
}

function armMetrics(group, arm) {
  const baseline = loadBaseline(group, arm);
  const scores = loadScores(group, arm);
  if (!baseline) return null;
  const rows = baseline.rows.map((row) => ({
    taskId: row.taskId,
    tokens: row.usage?.totalTokens ?? null,
    wallMs: row.wallMs ?? null,
    settled: row.settled,
    // Unscored or unsettled tasks count 0 per the README judging rule.
    score: row.settled ? scores?.map.get(row.taskId) ?? 0 : 0,
  }));
  const scored = scores != null;
  return {
    group, arm, rows, scored,
    totalTokens: rows.reduce((s, r) => s + (r.tokens ?? 0), 0),
    meanWallMs: rows.length ? rows.reduce((s, r) => s + (r.wallMs ?? 0), 0) / rows.length : 0,
    meanScore: rows.length ? rows.reduce((s, r) => s + r.score, 0) / rows.length : 0,
  };
}

function fmtPct(value) {
  return `${(value * 100).toFixed(1)}%`;
}

function compare(group) {
  const off = armMetrics(group, "off");
  const on = armMetrics(group, "on");
  if (!off || !on) {
    return { group, status: "incomplete", missing: [!off && "baseline-off", !on && "baseline-on"].filter(Boolean) };
  }
  const tokenDelta = off.totalTokens > 0 ? 1 - on.totalTokens / off.totalTokens : null;
  const scoreDelta = on.meanScore - off.meanScore;
  const rows = off.rows.map((offRow) => {
    const onRow = on.rows.find((r) => r.taskId === offRow.taskId);
    return {
      taskId: offRow.taskId,
      offTokens: offRow.tokens,
      onTokens: onRow?.tokens ?? null,
      offScore: offRow.score,
      onScore: onRow?.score ?? null,
    };
  });
  const isGateGroup = group === "a";
  const gate = isGateGroup
    ? {
        tokenDropAtLeast10pct: tokenDelta != null && tokenDelta >= 0.10,
        completionNotDecreased: scoreDelta >= 0,
      }
    : null;
  return {
    group,
    status: "complete",
    controlOnly: !isGateGroup,
    off: { totalTokens: off.totalTokens, meanScore: off.meanScore, meanWallMs: off.meanWallMs, humanScored: off.scored },
    on: { totalTokens: on.totalTokens, meanScore: on.meanScore, meanWallMs: on.meanWallMs, humanScored: on.scored },
    tokenDelta,
    scoreDelta,
    gate,
    verdict: isGateGroup
      ? (gate.tokenDropAtLeast10pct && gate.completionNotDecreased && off.scored && on.scored ? "pass" : "fail")
      : "control-reported",
    rows,
  };
}

const comparisons = [compare("a"), compare("b")];
const verdict = {
  generatedAt: new Date().toISOString(),
  milestone: "M1a",
  gate: "A 组（注入组）on 臂总 token 较 off 臂下降 >=10% 且完成度均分不降（附录 A.3）",
  missingInputs: comparisons.flatMap((c) => (c.status === "incomplete" ? c.missing.map((m) => `${c.group}:${m}`) : [])),
  humanScoringPending: comparisons.some((c) => c.status === "complete" && (!c.off.humanScored || !c.on.humanScored)),
  comparisons,
};

for (const c of comparisons) {
  if (c.status === "incomplete") {
    console.log(`group ${c.group.toUpperCase()}: incomplete (${c.missing.join(", ")})`);
    continue;
  }
  console.log(`\ngroup ${c.group.toUpperCase()} ${c.controlOnly ? "(control)" : "(gate)"}`);
  console.log(`  tokens   off=${c.off.totalTokens}  on=${c.on.totalTokens}  delta=${c.tokenDelta == null ? "n/a" : fmtPct(c.tokenDelta)}`);
  console.log(`  score    off=${c.off.meanScore.toFixed(2)}  on=${c.on.meanScore.toFixed(2)}  delta=${c.scoreDelta >= 0 ? "+" : ""}${c.scoreDelta.toFixed(2)}${(!c.off.humanScored || !c.on.humanScored) ? "  (人工评分未落盘，按 0 分口径)" : ""}`);
  console.log(`  wall(ms) off=${Math.round(c.off.meanWallMs)}  on=${Math.round(c.on.meanWallMs)}`);
  if (c.gate) {
    console.log(`  gate: token>=10%↓ ${c.gate.tokenDropAtLeast10pct} / completion↑ ${c.gate.completionNotDecreased} -> ${c.verdict}`);
  }
}
const out = path.join(ART_ROOT, "verdict-m1a.json");
mkdirSync(ART_ROOT, { recursive: true });
writeFileSync(out, `${JSON.stringify(verdict, null, 2)}\n`);
console.log(`\nverdict -> ${out}`);

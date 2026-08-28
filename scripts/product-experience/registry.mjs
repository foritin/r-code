// product-experience 断言注册表装配层：
//   registry.generated.json —— 由 generate_registry_from_prd.py 从 PRD §11 机械提取（唯一枚举源）
//   wiring.mjs              —— 断言 → 当前可执行验证手段的接线
//
// 本文件只做合并与结构校验，不出现任何手工维护的任务/断言 ID 枚举。

import { createHash } from "node:crypto";

import generated from "./registry.generated.json" with { type: "json" };
import { WIRING } from "./wiring.mjs";

export const MILESTONES = ["D0", "M0", "M1", "M2", "M3", "M4", "M5", "M6", "M7", "M8", "M9"];
export const PROFILES = ["implementation", "candidate", "production"];

const wiredIds = new Set(Object.keys(WIRING));

function sha256(text) {
  return createHash("sha256").update(text).digest("hex");
}

// 生成 JSON 的顺序稳定，直接遍历即为稳定注册序。
// buildRegistry 独立导出：自测/合成 fixture 用它构造确定性注册表，
// 不再依赖真实注册表的瞬态进度（如某任务恰好未接线）。
export function buildRegistry(generatedSource, wiring = WIRING) {
  return Object.fromEntries(
    Object.entries(generatedSource.tasks).map(([taskId, card]) => [
      taskId,
      {
        title: card.title,
        milestone: card.milestone,
        requirement_refs: card.requirement_refs,
        depends_on: card.depends_on,
        baseline_done: generatedSource.baseline_done.includes(taskId),
        assertions: card.assertions.map((a) => {
          const w = wiring[a.id];
          if (w) {
            if (w.id !== a.id) {
              throw new Error(`wiring id mismatch: ${w.id} != ${a.id}`);
            }
            return {
              ...a,
              ...w,
              not_implemented: false,
            };
          }
          return {
            ...a,
            type: "not_implemented",
            command: null,
            profiles: ["implementation"],
            note: "待对应里程碑实施时接线；在此之前为显式 required 失败",
            not_implemented: true,
          };
        }),
      },
    ]),
  );
}

export const REGISTRY = buildRegistry(generated);

/** 已接线但 registry 中不存在的 id（防止接线表拼错后静默失效）。 */
const ORPHAN_WIRING = [...wiredIds].filter((id) => !REGISTRY[id.split(".")[0]]);

/** 结构校验：返回 issue 字符串数组，空数组即合法。 */
export function validateRegistry() {
  const issues = [];
  for (const id of ORPHAN_WIRING) issues.push(`wiring 指向不存在的断言: ${id}`);
  const seenAssertion = new Map();
  for (const [tid, task] of Object.entries(REGISTRY)) {
    if (!task.milestone || !MILESTONES.includes(task.milestone)) {
      issues.push(`${tid}: 非法里程碑 ${task.milestone}`);
    }
    if (!tid.startsWith(`${task.milestone}-`)) {
      issues.push(`${tid}: 任务 ID 与里程碑前缀不一致`);
    }
    if (task.assertions.length === 0) {
      issues.push(`${tid}: 无 required 断言`);
    }
    for (const a of task.assertions) {
      if (seenAssertion.has(a.id)) {
        issues.push(`重复断言 ID: ${a.id}（另见 ${seenAssertion.get(a.id)}）`);
      } else {
        seenAssertion.set(a.id, tid);
      }
      if (!a.not_implemented) {
        if (a.profiles.length === 0 || a.profiles.some((p) => !PROFILES.includes(p))) {
          issues.push(`${a.id}: 非法 profiles ${JSON.stringify(a.profiles)}`);
        }
      }
    }
    for (const dep of task.depends_on) {
      if (!(dep in REGISTRY)) issues.push(`${tid}: 未知依赖 ${dep}`);
    }
  }
  // 与生成源的一致性计数合同
  if (Object.keys(REGISTRY).length !== generated.task_count) {
    issues.push(`任务数漂移: registry=${Object.keys(REGISTRY).length} 生成源=${generated.task_count}`);
  }
  const assertionTotal = [...seenAssertion.keys()].length;
  if (assertionTotal !== generated.assertion_count) {
    issues.push(`断言数漂移: registry=${assertionTotal} 生成源=${generated.assertion_count}`);
  }
  issues.push(...findCycles());
  return issues;
}

/** 依赖环检测（迭代式三色 DFS）。tasks 可注入以便自测；默认读全局注册表。
 *  返回 `A -> B -> A` 形式的环描述数组。 */
export function findCycles(tasksInput) {
  const tasks = tasksInput ?? REGISTRY;
  const WHITE = 0;
  const GRAY = 1;
  const BLACK = 2;
  const color = new Map();
  const cycles = [];
  const stackPath = [];

  function visit(node) {
    color.set(node, GRAY);
    stackPath.push(node);
    for (const dep of tasks[node]?.depends_on ?? []) {
      if (!(dep in tasks)) continue; // 未知依赖由 validateRegistry 单独报告
      const c = color.get(dep) ?? WHITE;
      if (c === GRAY) {
        const startIdx = stackPath.indexOf(dep);
        cycles.push([...stackPath.slice(startIdx), dep].join(" -> "));
      } else if (c === WHITE) {
        visit(dep);
      }
    }
    stackPath.pop();
    color.set(node, BLACK);
  }

  for (const tid of Object.keys(tasks)) {
    if ((color.get(tid) ?? WHITE) === WHITE) visit(tid);
  }
  return cycles.map((c) => `依赖环: ${c}`);
}

/** --through <M>：里程碑 <= M 的全部任务。 */
export function tasksThroughMilestone(milestone) {
  const limit = MILESTONES.indexOf(milestone);
  if (limit < 0) return null;
  return Object.keys(REGISTRY).filter(
    (t) => MILESTONES.indexOf(REGISTRY[t].milestone) <= limit,
  );
}
/** 注册表内容摘要：同输入必同输出（不含绝对路径）。 */
export function registryDigest() {
  const stripped = {
    task_count: generated.task_count,
    assertion_count: generated.assertion_count,
    source_document: generated.source_document,
    source_sha256: generated.source_sha256,
  };
  return sha256(JSON.stringify(stripped));
}

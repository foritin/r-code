import type { ChangeDiff, FileChange, PlanItemState, PlanView } from "../../lib/types";
import type { PlanStep } from "./model";

export type SessionSummaryStepState =
  | "completed"
  | "current"
  | "pending"
  | "blocked"
  | "failed"
  | "cancelled";

export interface SessionSummaryStep {
  id: string;
  label: string;
  detail: string | null;
  state: SessionSummaryStepState;
}

export interface SessionChangeStat {
  additions: number | null;
  deletions: number | null;
  available: boolean;
}

function persistedStepState(state: PlanItemState): SessionSummaryStepState {
  switch (state) {
    case "completed":
      return "completed";
    case "in_progress":
      return "current";
    case "blocked":
      return "blocked";
    case "failed":
      return "failed";
    case "cancelled":
      return "cancelled";
    case "proposed":
    case "pending":
      return "pending";
  }
}

/**
 * Reconcile the lightweight live plan emitted by normal agent runs with the persisted Plan
 * aggregate used by explicit planning mode. Live steps win while present because they update on
 * every `plan` event; the persisted view remains the fallback after reloads and for HITL plans.
 */
export function sessionSummarySteps(
  liveSteps: readonly PlanStep[],
  planView: PlanView | null,
  running: boolean,
): SessionSummaryStep[] {
  if (liveSteps.length > 0) {
    const current = liveSteps.findIndex((step) => !step.completed);
    return liveSteps.map((step, index) => ({
      id: `live-${index}`,
      label: step.description.trim() || `步骤 ${index + 1}`,
      detail: null,
      state: step.completed
        ? "completed"
        : running && index === current
          ? "current"
          : "pending",
    }));
  }

  if (!planView?.items.length) return [];
  const ordered = [...planView.items].sort(
    (left, right) => left.ordinal - right.ordinal || left.id.localeCompare(right.id),
  );
  const steps = ordered.map((item, index): SessionSummaryStep => {
    const title = item.title.trim();
    const description = item.description.trim();
    const label = title || description || `步骤 ${index + 1}`;
    return {
      id: item.id,
      label,
      detail: description && description !== label ? description : null,
      state: persistedStepState(item.state),
    };
  });

  if (
    running
    && !steps.some((step) => step.state === "current")
    && !steps.some((step) => step.state === "blocked" || step.state === "failed")
  ) {
    const firstPending = steps.findIndex((step) => step.state === "pending");
    if (firstPending >= 0) steps[firstPending] = { ...steps[firstPending], state: "current" };
  }
  return steps;
}

export function currentStepNumber(steps: readonly SessionSummaryStep[]): number {
  if (steps.length === 0) return 0;
  const active = steps.findIndex((step) =>
    step.state === "current" || step.state === "blocked" || step.state === "failed"
  );
  if (active >= 0) return active + 1;
  const firstPending = steps.findIndex((step) => step.state === "pending");
  return firstPending >= 0 ? firstPending + 1 : steps.length;
}

/** One row per path, keeping the newest snapshot so polling never inflates the file count. */
export function latestSessionChanges(changes: readonly FileChange[]): FileChange[] {
  const byPath = new Map<string, FileChange>();
  for (const change of changes) {
    const path = change.path.replaceAll("\\", "/");
    const normalized = { ...change, path };
    const current = byPath.get(path);
    const currentIsSnapshot = Boolean(current?.run_id && current.tool_call_id == null);
    const nextIsSnapshot = Boolean(normalized.run_id && normalized.tool_call_id == null);
    if (
      !current
      || (nextIsSnapshot && !currentIsSnapshot)
      || (nextIsSnapshot === currentIsSnapshot && current.created_at <= normalized.created_at)
    ) {
      byPath.set(path, normalized);
    }
  }
  return [...byPath.values()].sort(
    (left, right) => right.created_at.localeCompare(left.created_at) || left.path.localeCompare(right.path),
  );
}

export function changeFingerprint(change: FileChange): string {
  return [
    change.path,
    change.change_type,
    change.before_hash ?? "",
    change.after_hash ?? "",
    change.id,
  ].join("\u0000");
}

export function changeStatFromDiff(diff: ChangeDiff): SessionChangeStat {
  // The existing preview deliberately caps very large files. Counting those rendered rows would
  // turn the cap into a fake Session total, so omit the aggregate rather than present bad data.
  if (!diff.supported || !diff.lines || diff.truncated) {
    return { additions: null, deletions: null, available: false };
  }
  let additions = 0;
  let deletions = 0;
  for (const line of diff.lines) {
    if (line.kind === "add") additions += 1;
    else if (line.kind === "del") deletions += 1;
  }
  return { additions, deletions, available: true };
}

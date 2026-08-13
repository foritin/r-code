/**
 * 页面展示层的数据适配。
 *
 * 任务房间和侧栏使用原始任务记录推导展示语义。项目仪表盘则直接使用后端的
 * `WorkspaceDashboard` 聚合响应，避免逐任务详情请求和前端统计口径漂移。
 */
import type { AgentRun, FileChange, Task, TaskDetail, TaskState, Workspace } from "./types";

export type VisualTaskState = "running" | "attention" | "review" | "done" | "stopped" | "idle";

export interface DiffSummary {
  files: number;
  created: number;
  modified: number;
  removed: number;
  renamed: number;
}

export interface ProjectSummary {
  taskCount: number;
  runningCount: number;
  attentionCount: number;
  reviewCount: number;
  subagentCount: number;
  completedRecently: number;
}

export const PARTIAL_SUCCESS_RUN_SUMMARY =
  "部分完成：修改存在但运行或最终总结失败，请审阅工作区改动。";

/** 最新主运行有改动可审阅，但没有完整成功结束。 */
export function isPartialSuccess(detail?: TaskDetail): boolean {
  const latestMainRun = detail?.runs.find((run) => run.agent_kind === "main");
  return latestMainRun?.review_state === "pending"
    && latestMainRun.summary?.trim() === PARTIAL_SUCCESS_RUN_SUMMARY;
}

export function reviewAttentionDescription(detail?: TaskDetail): string {
  const count = detail?.changes.length ?? 0;
  return isPartialSuccess(detail)
    ? `修改存在但总结失败 · ${count} 个文件等待审核`
    : `${count} 个文件等待审核`;
}

export function taskTitle(task: Task): string {
  return task.title.trim() || task.goal.trim() || "未命名任务";
}

export function workspaceName(path: string | null, workspaces: readonly Workspace[]): string {
  if (!path) return "未归属项目";
  return workspaces.find((workspace) => workspace.canonical_path === path)?.display_name
    ?? path.split(/[\\/]/).filter(Boolean).pop()
    ?? path;
}

export function isTaskLive(task: Task, detail?: TaskDetail): boolean {
  return task.state === "in_progress"
    || task.state === "exploring"
    || detail?.runs.some((run) => run.ended_at === null) === true;
}

export function pendingPermissionCount(detail?: TaskDetail): number {
  return detail?.permissions.filter((permission) => permission.decision === "pending").length ?? 0;
}

export function visualTaskState(task: Task, detail?: TaskDetail): VisualTaskState {
  if (pendingPermissionCount(detail) > 0) return "attention";
  if (task.state === "review_ready") return "review";
  if (isTaskLive(task, detail)) return "running";
  if (task.state === "interrupted") return "stopped";
  if (task.state === "idle") return "done";
  return "idle";
}

export function taskStateLabel(state: TaskState, detail?: TaskDetail): string {
  if (pendingPermissionCount(detail) > 0) return "等待你的处理";
  if (state === "exploring") return "正在分析";
  if (state === "in_progress") return "正在执行";
  if (state === "review_ready") return "等待审查";
  if (state === "interrupted") return "已中止";
  if (state === "archived") return "已归档";
  return "已完成";
}

export function activeRun(detail?: TaskDetail): AgentRun | undefined {
  return detail?.runs.find((run) => run.ended_at === null)
    ?? detail?.runs.filter((run) => run.agent_kind === "main").slice(-1)[0]
    ?? detail?.runs.slice(-1)[0];
}

/** 页面中没有后端当前动作字段时，使用 task state 的可解释回退。 */
export function taskActivity(task: Task, detail?: TaskDetail): string {
  const permission = detail?.permissions.find((item) => item.decision === "pending");
  if (permission) return `等待授权 · ${permission.tool_name}`;
  const run = activeRun(detail);
  if (run?.summary?.trim()) return run.summary.trim();
  if (task.state === "exploring") return "梳理代码与执行路径";
  if (task.state === "in_progress") return "正在推进任务";
  if (task.state === "review_ready") return "变更已准备好审查";
  if (task.state === "interrupted") return "任务已停止";
  return "等待下一步";
}

export function taskAgentLabel(detail?: TaskDetail): string {
  const run = activeRun(detail);
  if (!run) return "主代理";
  if (run.agent_label?.trim()) return run.agent_label.trim();
  return run.agent_kind === "subagent" ? "子代理" : "主代理";
}

export function diffSummary(changes: readonly FileChange[] | undefined): DiffSummary {
  const result: DiffSummary = { files: changes?.length ?? 0, created: 0, modified: 0, removed: 0, renamed: 0 };
  for (const change of changes ?? []) {
    if (change.change_type === "create") result.created += 1;
    else if (change.change_type === "delete") result.removed += 1;
    else if (change.change_type === "rename") result.renamed += 1;
    else result.modified += 1;
  }
  return result;
}

export function projectSummary(tasks: readonly Task[], details: Record<string, TaskDetail>): ProjectSummary {
  const now = Date.now();
  const hourAgo = now - 60 * 60 * 1000;
  let runningCount = 0;
  let attentionCount = 0;
  let reviewCount = 0;
  let subagentCount = 0;
  let completedRecently = 0;
  for (const task of tasks) {
    const detail = details[task.id];
    const state = visualTaskState(task, detail);
    if (state === "running") runningCount += 1;
    if (state === "attention") attentionCount += 1;
    if (state === "review") reviewCount += 1;
    subagentCount += detail?.runs.filter((run) => run.agent_kind === "subagent" && run.ended_at === null).length ?? 0;
    if (task.state === "idle" && Date.parse(task.updated_at) >= hourAgo) completedRecently += 1;
  }
  return { taskCount: tasks.length, runningCount, attentionCount, reviewCount, subagentCount, completedRecently };
}

export function sortTasksByUrgency(tasks: readonly Task[], details: Record<string, TaskDetail>): Task[] {
  const weight: Record<VisualTaskState, number> = {
    attention: 0,
    review: 1,
    running: 2,
    stopped: 3,
    done: 4,
    idle: 5,
  };
  return [...tasks].sort((left, right) => {
    const leftWeight = weight[visualTaskState(left, details[left.id])];
    const rightWeight = weight[visualTaskState(right, details[right.id])];
    return leftWeight - rightWeight || right.updated_at.localeCompare(left.updated_at);
  });
}

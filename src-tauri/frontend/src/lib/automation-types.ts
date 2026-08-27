import type { TaskAgentEngine } from "./types";

export const HOURLY_INTERVAL_MINUTES = 60 as const;

export interface ExecutionProfile {
  agent_engine: TaskAgentEngine;
  provider_name: string;
  model: string;
  reasoning_effort: string | null;
}

export type AutomationPermission = "read_only" | "isolated_write";
export type AutomationDefinitionState = "active" | "paused" | "completed";
export type AutomationWeekday =
  | "monday"
  | "tuesday"
  | "wednesday"
  | "thursday"
  | "friday"
  | "saturday"
  | "sunday";

export type ScheduleSpec =
  | { kind: "once"; run_at_utc: string }
  | {
      kind: "hourly";
      anchor_at_utc: string;
      interval_minutes: typeof HOURLY_INTERVAL_MINUTES;
    }
  | { kind: "daily"; local_time: string }
  | { kind: "weekdays"; local_time: string }
  | { kind: "weekly"; weekday: AutomationWeekday; local_time: string };

export interface AutomationDefinition {
  id: string;
  name: string;
  workspace_path: string;
  prompt: string;
  execution_profile: ExecutionProfile;
  schedule: ScheduleSpec;
  /** IANA identifier such as `Asia/Shanghai`, never a fixed offset or OS display name. */
  timezone: string;
  permission: AutomationPermission;
  base_ref: string | null;
  state: AutomationDefinitionState;
  next_run_at_utc: string | null;
  created_at: string;
  updated_at: string;
}

export interface AutomationDefinitionSnapshot {
  definition_id: string;
  name: string;
  workspace_path: string;
  prompt: string;
  execution_profile: ExecutionProfile;
  schedule: ScheduleSpec;
  timezone: string;
  permission: AutomationPermission;
  base_ref: string | null;
  definition_updated_at: string;
}

export type RunTrigger = "scheduled" | "catch_up" | "manual";
export type RunStatus =
  | "queued"
  | "running"
  | "waiting_approval"
  | "succeeded"
  | "failed"
  | "skipped"
  | "cancelled";

export interface AutomationRun {
  id: string;
  automation_id: string;
  task_id: string | null;
  trigger: RunTrigger;
  scheduled_for: string;
  definition_snapshot: AutomationDefinitionSnapshot;
  status: RunStatus;
  idempotency_key: string;
  lease_owner: string | null;
  lease_expires_at: string | null;
  missed_count: number;
  started_at: string | null;
  finished_at: string | null;
  error_code: string | null;
}

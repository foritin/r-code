/** Browser contract v1. Keep in sync with `src-tauri/src/browser` and the shared JSON fixture. */
export const BROWSER_CONTRACT_SCHEMA_VERSION = 1 as const;
export const MAX_BROWSER_TIMEOUT_MS = 30_000 as const;

export const BROWSER_TOOL_NAMES = [
  "open",
  "navigate",
  "snapshot",
  "screenshot",
  "click",
  "type",
  "select",
  "press",
  "scroll",
  "wait",
  "tabs",
  "console",
  "network-errors",
  "close",
] as const;

export type BrowserToolName = (typeof BROWSER_TOOL_NAMES)[number];
export type BrowserTargetPlatform = "windows" | "macos" | "linux";
export type BrowserTargetArch = "x86_64" | "aarch64";
export type BrowserRuntimeState =
  | "not_installed"
  | "installing"
  | "ready"
  | "repair_required";
export type BrowserProcessState =
  | "not_started"
  | "starting"
  | "running"
  | "stopping"
  | "stopped"
  | "crashed";
export type BrowserTabState = "loading" | "ready" | "closed" | "crashed";
export type BrowserPermissionCapability = "browse" | "interact";
export type BrowserPermissionScope = "once" | "task";

export interface BrowserRuntimeManifest {
  schema_version: number;
  runtime_version: string;
  target_platform: BrowserTargetPlatform;
  target_arch: BrowserTargetArch;
  wrapper_version: string;
  node_version: string;
  playwright_version: string;
  chromium_revision: string;
  asset_url: string;
  asset_size: number;
  sha256: string;
}

export interface BrowserScreenshotRef {
  screenshot_id: string;
  path: string;
  media_type: string;
  width: number;
  height: number;
  captured_at: string;
}

export interface BrowserSession {
  session_id: string;
  task_id: string;
  profile_path: string;
  runtime_version: string;
  process_state: BrowserProcessState;
  active_tab_id: string | null;
  last_url: string | null;
  last_screenshot: BrowserScreenshotRef | null;
}

export interface BrowserTab {
  tab_id: string;
  session_id: string;
  opener_tab_id: string | null;
  url: string;
  title: string;
  state: BrowserTabState;
  active: boolean;
  created_at: string;
  updated_at: string;
}

export interface BrowserOrigin {
  scheme: "http" | "https";
  host: string;
  effective_port: number;
}

export interface BrowserPermissionGrant {
  task_id: string;
  origin: BrowserOrigin;
  capability: BrowserPermissionCapability;
  scope: BrowserPermissionScope;
  granted_at: string;
  revoked_at: string | null;
}

export interface BrowserPermissionRequest {
  request_id: string;
  task_id: string;
  session_id: string;
  tab_id: string | null;
  origin: BrowserOrigin;
  capability: BrowserPermissionCapability;
  requested_at: string;
}

export type BrowserNavigationTarget =
  | { kind: "url"; url: string }
  | { kind: "workspace_file"; path: string };

export type BrowserElementTarget =
  | { kind: "css"; selector: string }
  | { kind: "text"; text: string; exact?: boolean }
  | { kind: "snapshot_ref"; reference: string };

export type BrowserLoadState = "dom_content_loaded" | "load" | "network_idle";
export type BrowserWaitCondition =
  | { kind: "selector"; selector: string }
  | { kind: "text"; text: string; exact?: boolean }
  | { kind: "url"; url: string }
  | { kind: "load_state"; state: BrowserLoadState };

export type BrowserToolRequest =
  | { tool: "open"; input: { target?: BrowserNavigationTarget } }
  | {
      tool: "navigate";
      input: { session_id: string; tab_id?: string; target: BrowserNavigationTarget };
    }
  | { tool: "snapshot"; input: BrowserTabInput }
  | { tool: "screenshot"; input: BrowserTabInput & { full_page?: boolean } }
  | { tool: "click"; input: BrowserTargetInput }
  | {
      tool: "type";
      input: BrowserTargetInput & { text: string; clear?: boolean };
    }
  | {
      tool: "select";
      input: BrowserTargetInput & { values: string[] };
    }
  | {
      tool: "press";
      input: BrowserTabInput & { target?: BrowserElementTarget; key: string };
    }
  | {
      tool: "scroll";
      input: BrowserTabInput & {
        target?: BrowserElementTarget;
        delta_x: number;
        delta_y: number;
      };
    }
  | {
      tool: "wait";
      input: BrowserTabInput & {
        condition: BrowserWaitCondition;
        timeout_ms?: number;
      };
    }
  | { tool: "tabs"; input: BrowserSessionInput }
  | { tool: "console"; input: BrowserTabInput }
  | { tool: "network-errors"; input: BrowserTabInput }
  | { tool: "close"; input: BrowserTabInput };

export interface BrowserSessionInput {
  session_id: string;
}

export interface BrowserTabInput extends BrowserSessionInput {
  tab_id?: string;
}

export interface BrowserTargetInput extends BrowserTabInput {
  target: BrowserElementTarget;
}

export interface BrowserActionMetadata {
  session_id: string;
  tab_id: string | null;
  url: string | null;
  action_id: string;
  timestamp: string;
}

export type BrowserElementValue =
  | { state: "missing" }
  | { state: "visible"; text: string }
  | { state: "redacted" };

export interface BrowserSnapshotElement {
  reference: string;
  role: string;
  name: string;
  value: BrowserElementValue;
  disabled: boolean;
}

export interface BrowserSnapshot {
  snapshot_id: string;
  text: string;
  elements: BrowserSnapshotElement[];
  truncated: boolean;
}

export interface BrowserConsoleEntry {
  level: string;
  text: string;
  timestamp: string;
  redacted: boolean;
}

export interface BrowserNetworkError {
  method: string;
  url: string;
  error_text: string;
  timestamp: string;
  redacted: boolean;
}

export type BrowserToolResult =
  | {
      tool: "open";
      output: BrowserActionMetadata & { session: BrowserSession; tab: BrowserTab };
    }
  | {
      tool: "navigate";
      output: BrowserActionMetadata & { tab: BrowserTab };
    }
  | {
      tool: "snapshot";
      output: BrowserActionMetadata & { snapshot: BrowserSnapshot };
    }
  | {
      tool: "screenshot";
      output: BrowserActionMetadata & { screenshot: BrowserScreenshotRef };
    }
  | { tool: "click" | "type" | "select" | "press" | "scroll"; output: BrowserActionMetadata }
  | {
      tool: "wait";
      output: BrowserActionMetadata & {
        satisfied: boolean;
        load_state: BrowserLoadState | null;
      };
    }
  | {
      tool: "tabs";
      output: BrowserActionMetadata & { tabs: BrowserTab[] };
    }
  | {
      tool: "console";
      output: BrowserActionMetadata & { entries: BrowserConsoleEntry[]; truncated: boolean };
    }
  | {
      tool: "network-errors";
      output: BrowserActionMetadata & { errors: BrowserNetworkError[]; truncated: boolean };
    }
  | {
      tool: "close";
      output: BrowserActionMetadata & { process_state: BrowserProcessState };
    };

export type BrowserEvent =
  | { type: "runtime_state_changed"; runtime_version: string | null; state: BrowserRuntimeState }
  | { type: "session_state_changed"; state: BrowserProcessState }
  | { type: "tab_opened" | "tab_updated"; tab: BrowserTab }
  | { type: "tab_closed"; tab_id: string }
  | BrowserActionEvent
  | { type: "permission_required"; request: BrowserPermissionRequest }
  | { type: "permission_granted" | "permission_revoked"; grant: BrowserPermissionGrant }
  | { type: "screenshot_captured"; screenshot: BrowserScreenshotRef }
  | { type: "console_entry"; tab_id: string; entry: BrowserConsoleEntry }
  | { type: "network_error"; tab_id: string; error: BrowserNetworkError };

export type BrowserActionEvent =
  | {
      type: "action_started" | "action_completed";
      action_id: string;
      tool: BrowserToolName;
      tab_id: string | null;
      url: string | null;
    }
  | {
      type: "action_failed";
      action_id: string;
      tool: BrowserToolName;
      tab_id: string | null;
      url: string | null;
      error_code: string;
    };

export interface BrowserEventEnvelope {
  schema_version: number;
  event_id: string;
  task_id: string;
  session_id: string | null;
  occurred_at: string;
  event: BrowserEvent;
}

export interface BrowserToolContract {
  name: BrowserToolName;
  description: string;
  capability: BrowserPermissionCapability;
  input_schema: Record<string, unknown>;
}

export interface BrowserAgentContract {
  schema_version: number;
  tools: BrowserToolContract[];
}

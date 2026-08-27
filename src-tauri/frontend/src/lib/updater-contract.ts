/** Cross-layer application updater contract owned by `src-tauri/src/updater`. */
export const APPLICATION_UPDATER_STATE_EVENT = "application-updater-state" as const;
export const APPLICATION_UPDATER_AUTO_CHECK_HOURS = 6 as const;

export type UpdaterPhase =
  | "idle"
  | "checking"
  | "up_to_date"
  | "available"
  | "downloading"
  | "downloaded"
  | "installing"
  | "restart_pending"
  | "failed";

export type UpdaterOperation = "check" | "download" | "install" | "restart";

export interface UpdaterRelease {
  version: string;
  notes: string | null;
  published_at: string | null;
}

export interface UpdaterDownloadProgress {
  downloaded_bytes: number;
  total_bytes: number | null;
  percent: number | null;
}

export interface UpdaterSnapshot {
  current_version: string;
  state: UpdaterPhase;
  release: UpdaterRelease | null;
  progress: UpdaterDownloadProgress;
  last_check_at: string | null;
  next_auto_check_at: string | null;
  error_code: string | null;
  error_args: Record<string, unknown>;
  failed_operation: UpdaterOperation | null;
}

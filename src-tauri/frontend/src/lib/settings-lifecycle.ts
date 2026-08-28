// M2-03.A10：12 页 Settings 共用 lifecycle reducer（R-SET-09）。
//
// 合同：
// - 读取：uninitialized → loading → ready | failed；有 last-good 时失败落
//   stale_last_good（只读展示 + retry），无快照失败落 failed；retry 经 retrying 回 loading。
// - 草稿：clean | dirty | saving；保存失败保留 persisted snapshot 与 dirty draft；
//   离开 dirty 页必须显式 discard。
// - 刷新/轮询检测到更新的 Host revision：clean 页静默采用；dirty 页进入
//   stale_last_good 并保留草稿——任何路径都不得静默覆盖草稿。
// - CAS 冲突（提交时 base revision 已过期）：同时保留 local digest 与 fresh 快照，
//   三路恢复 discardLocal / reapplyLocal / mergeAccept 都返回**新的 base revision**，
//   每条路径离开冲突态；merge preview 只读预览不落库。

export type LoadPhase =
  | "uninitialized"
  | "loading"
  | "ready"
  | "stale_last_good"
  | "failed"
  | "retrying";

export type DraftPhase = "clean" | "dirty" | "saving";

export interface SettingsSnapshot<TValue = unknown> {
  revision: string;
  value: TValue;
}

export type SettingsLifecycleState<TValue = unknown> = {
  load: LoadPhase;
  draftPhase: DraftPhase;
  /** Host 权威值（最近一次 ACK 的快照）。 */
  persisted: SettingsSnapshot<TValue> | null;
  /** 本地草稿；dirty 时与 persisted 的差异即为未保存内容。 */
  draft: TValue | null;
  /** 提交所依据的 base revision；保存成功后与 persisted.revision 同步。 */
  baseRevision: string | null;
  error: string | null;
  conflict: {
    localDigest: string;
    localValue: TValue;
    fresh: SettingsSnapshot<TValue>;
  } | null;
  /** 冲突恢复或保存成功后自增；三路恢复都必须产生新的 base revision。 */
  baseEpoch: number;
};

export type SettingsEvent<TValue> =
  | { type: "LOAD_START" }
  | { type: "LOAD_SUCCESS"; snapshot: SettingsSnapshot<TValue> }
  | { type: "LOAD_FAILURE"; error: string }
  | { type: "RETRY" }
  | { type: "EDIT_DRAFT"; draft: TValue }
  | { type: "DISCARD_DRAFT" }
  | { type: "SAVE_START" }
  | { type: "SAVE_SUCCESS"; revision: string }
  | { type: "SAVE_FAILURE"; error: string }
  /** 后台刷新发现更新 revision：clean 静默采用；dirty 转 stale_last_good 保留草稿。 */
  | { type: "REMOTE_REFRESH"; snapshot: SettingsSnapshot<TValue> }
  /** 提交返回 CAS 冲突：Host revision ≠ baseRevision。 */
  | {
      type: "CAS_CONFLICT";
      localDigest: string;
      fresh: SettingsSnapshot<TValue>;
    }
  | { type: "CONFLICT_DISCARD_LOCAL" }
  /** local 草稿整体 reapply 到 fresh 之上，产生新 base revision。 */
  | { type: "CONFLICT_REAPPLY_LOCAL"; revision: string }
  /** 字段级合并结果被接受，产生新 base revision。 */
  | { type: "CONFLICT_MERGE_ACCEPT"; merged: TValue; revision: string };

export function initialSettingsLifecycle<TValue>(): SettingsLifecycleState<TValue> {
  return {
    load: "uninitialized",
    draftPhase: "clean",
    persisted: null,
    draft: null,
    baseRevision: null,
    error: null,
    conflict: null,
    baseEpoch: 0,
  };
}

export function reduceSettingsLifecycle<TValue>(
  state: SettingsLifecycleState<TValue>,
  event: SettingsEvent<TValue>,
): SettingsLifecycleState<TValue> {
  switch (event.type) {
    case "LOAD_START":
      // 已有持久值的失败重试走 stale/retry 语义；首载才进入无快照 loading。
      return state.persisted
        ? { ...state, load: "retrying", error: null }
        : { ...state, load: "loading", error: null };

    case "LOAD_SUCCESS": {
      // dirty 草稿不可被读取结果覆盖：成功读取只更新持久值，草稿保持。
      return {
        ...state,
        load: "ready",
        persisted: event.snapshot,
        baseRevision: event.snapshot.revision,
        error: null,
      };
    }

    case "LOAD_FAILURE":
      return state.persisted
        ? { ...state, load: "stale_last_good", error: event.error }
        : { ...state, load: "failed", error: event.error };

    case "RETRY":
      return state.load === "failed" || state.load === "stale_last_good"
        ? { ...state, load: "retrying", error: null }
        : state;

    case "EDIT_DRAFT":
      return { ...state, draftPhase: "dirty", draft: event.draft };

    case "DISCARD_DRAFT":
      return { ...state, draftPhase: "clean", draft: null };

    case "SAVE_START":
      return { ...state, draftPhase: "saving" };

    case "SAVE_SUCCESS": {
      const saved = state.draft as TValue;
      return {
        ...state,
        draftPhase: "clean",
        draft: null,
        persisted: { revision: event.revision, value: saved },
        baseRevision: event.revision,
        baseEpoch: state.baseEpoch + 1,
        error: null,
      };
    }

    case "SAVE_FAILURE":
      // 失败：持久快照原封不动，dirty draft 保留等待修复重试或显式 discard。
      return { ...state, draftPhase: "dirty", error: event.error };

    case "REMOTE_REFRESH": {
      if (state.draftPhase === "clean" || state.draftPhase === "saving") {
        return {
          ...state,
          load: "ready",
          persisted: event.snapshot,
          baseRevision: event.snapshot.revision,
        };
      }
      // dirty：不覆盖草稿，标记 last-good 已过期。
      return {
        ...state,
        load: "stale_last_good",
        persisted: event.snapshot,
      };
    }

    case "CAS_CONFLICT":
      return {
        ...state,
        draftPhase: "dirty",
        conflict: {
          localDigest: event.localDigest,
          localValue: state.draft as TValue,
          fresh: event.fresh,
        },
      };

    case "CONFLICT_DISCARD_LOCAL":
      return {
        ...state,
        draftPhase: "clean",
        draft: null,
        persisted: state.conflict?.fresh ?? state.persisted,
        baseRevision: state.conflict?.fresh.revision ?? state.baseRevision,
        conflict: null,
        baseEpoch: state.baseEpoch + 1,
      };

    case "CONFLICT_REAPPLY_LOCAL":
      return {
        ...state,
        draftPhase: "clean",
        draft: null,
        persisted: { revision: event.revision, value: (state.conflict?.localValue ?? state.draft) as TValue },
        baseRevision: event.revision,
        conflict: null,
        baseEpoch: state.baseEpoch + 1,
      };

    case "CONFLICT_MERGE_ACCEPT":
      return {
        ...state,
        draftPhase: "clean",
        draft: null,
        persisted: { revision: event.revision, value: event.merged },
        baseRevision: event.revision,
        conflict: null,
        baseEpoch: state.baseEpoch + 1,
      };

    default:
      return state;
  }
}

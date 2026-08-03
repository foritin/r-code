import { useMemo } from "react";
import { useSyncHealthStore } from "../../store/sync-health";
import { IconAlert, IconRefresh } from "../icons";

export function SyncHealthBanner() {
  const issueMap = useSyncHealthStore((state) => state.issues);
  const issues = useMemo(
    () => Object.values(issueMap).sort((left, right) => right.failedAt - left.failedAt),
    [issueMap],
  );
  if (issues.length === 0) return null;

  const latest = issues[0];
  const summary = issues.length === 1 ? latest.label : `${latest.label}等 ${issues.length} 项`;
  return (
    <div className="sync-health-banner" role="alert" title={latest.message}>
      <IconAlert width={14} height={14} />
      <span><strong>数据可能已过期</strong><small>{summary}刷新失败</small></span>
      <button
        type="button"
        className="text-link"
        onClick={() => window.dispatchEvent(new Event("r-code:refresh-now"))}
      >
        <IconRefresh width={13} height={13} /> 重试
      </button>
    </div>
  );
}

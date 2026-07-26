/**
 * 运行期间常驻的可观察活动条。
 * 只呈现 reducer 已归类的工具、权限和引导活动，不展示模型私有推理。
 */
import { useEffect, useState } from "react";
import type { ActivityTraceState } from "./activity";

interface Props {
  state: ActivityTraceState;
  running: boolean;
}

export function ActivityStrip({ state, running }: Props) {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (!running || state.startedAt == null) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [running, state.startedAt]);

  if (!running) return null;

  const elapsed = elapsedLabel(state.startedAt, now);
  const recent = [...state.recentActivities].reverse();

  return (
    <div className={"activity-strip phase-" + state.phase} role="status" aria-live="polite">
      <div className="activity-current">
        <span className="activity-lamp" aria-hidden="true" />
        <span className="activity-label">{state.label}</span>
        <span className="activity-elapsed">{elapsed}</span>
      </div>
      {recent.length > 0 && (
        <div className="activity-trace" aria-label="近期可观察活动">
          {recent.map((item) => (
            <span className={"activity-item kind-" + item.kind} key={item.id}>
              {item.label}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

function elapsedLabel(startedAt: number | null, now: number): string {
  if (startedAt == null) return "刚刚开始";
  const seconds = Math.max(0, Math.floor((now - startedAt) / 1000));
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  return minutes > 0 ? `${minutes}分${String(rest).padStart(2, "0")}秒` : `${rest}秒`;
}

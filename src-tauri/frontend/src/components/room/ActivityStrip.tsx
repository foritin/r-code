/**
 * 运行期间常驻的状态条 —— 只回答「现在在做什么、做了多久」。
 * 逐条工具动作由时间线（verb + target + 结果）承担，此处不再重复罗列，
 * 避免同一件事在时间线、活动条、画布 live 标签里各说一遍。
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

  return (
    <div className={"activity-strip phase-" + state.phase} role="status" aria-live="polite">
      <span className="activity-lamp" aria-hidden="true" />
      <span className="activity-label">{state.label}</span>
      <span className="activity-elapsed" title="本次运行已进行时长">
        {elapsed}
      </span>
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

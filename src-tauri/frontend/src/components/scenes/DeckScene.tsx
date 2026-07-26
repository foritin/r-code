/**
 * Deck —— 舰队态势监控页（纯监控，无输入框；发起会话回 Home）。
 * 结构照 fusion-obsidian.html:735-920：
 *   态势带 → Needs You 通道 → Fleet（cards/rows 密度切换）→ Settled strip。
 * 轮询：usePoll 2s，刷新 tasks + 未完结任务 details（refreshDetails 并发限 4）；
 * 首轮额外补一轮完结任务 detail（settled strip 的 diffstat 数据源，之后静态缓存）。
 * 密度：store.app.deckDensity，写 data-density 到场景根；rows 时 CSS 隐藏
 * needs-lane / fleet-cards / settled-wrap（DOM 保留，切换不丢行内状态）。
 */
import { useEffect, useRef, useState } from "react";
import { useAppStore } from "../../store/app";
import { selectNeedsYou, selectRunning, useTasksStore } from "../../store/tasks";
import { usePoll } from "../../lib/poll";
import { onAgentEvent } from "../../lib/ipc";
import {
  computeGauges,
  isLiveTask,
  recentEventCount,
  settledItems,
} from "../../lib/deck";
import { SituationBand } from "../deck/SituationBand";
import { NeedsLane } from "../deck/NeedsLane";
import { FleetCards } from "../deck/FleetCards";
import { FleetRows } from "../deck/FleetRows";
import { SettledStrip } from "../deck/SettledStrip";

export function DeckScene() {
  const density = useAppStore((s) => s.deckDensity);
  const setDeckDensity = useAppStore((s) => s.setDeckDensity);

  const tasks = useTasksStore((s) => s.tasks);
  const details = useTasksStore((s) => s.details);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const refreshDetails = useTasksStore((s) => s.refreshDetails);
  const needsYou = useTasksStore(selectNeedsYou);
  const running = useTasksStore(selectRunning);

  const [error, setError] = useState<string | null>(null);
  const primedRef = useRef(false);

  // 子代理生命周期到达时立即刷新运行树；普通 token 增量不触发全局查询，避免 Deck
  // 与 Room 的实时流竞争。
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    onAgentEvent((taskId, event) => {
      const isSubagentLifecycle =
        event.type === "scoped" && event.event.type === "subagent_lifecycle";
      if (!isSubagentLifecycle && event.type !== "state") return;
      void refreshDetail(taskId);
      if (event.type === "state") void refreshTasks();
    })
      .then((un) => {
        if (disposed) un();
        else unlisten = un;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [refreshDetail, refreshTasks]);

  usePoll(async () => {
    try {
      await refreshTasks();
      const st = useTasksStore.getState();
      const liveIds = st.tasks.filter((task) => isLiveTask(task, st.details[task.id])).map((t) => t.id);
      await refreshDetails(liveIds);
      if (!primedRef.current) {
        primedRef.current = true;
        const rest = st.tasks.filter((t) => !isLiveTask(t)).map((t) => t.id);
        if (rest.length > 0) await refreshDetails(rest);
      }
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, 2000);

  const now = Date.now();
  const gauges = computeGauges(tasks, details, needsYou.length, now);
  const eventSeed = recentEventCount(details, 60_000, now);
  const settled = settledItems(tasks, details, 10);

  /** 操作（grant/deny/accept/rollback）后的定点刷新。 */
  const refreshOne = async (taskId: string) => {
    await Promise.all([refreshTasks(), refreshDetail(taskId)]);
  };

  return (
    <div className="scene scene-deck pane pane-lit" data-density={density}>
      {error && (
        <div className="deck-error" role="alert">
          <span>{error}</span>
          <button className="btn ghost" onClick={() => setError(null)}>
            ✕
          </button>
        </div>
      )}
      <div className="deck-scroll">
        <SituationBand gauges={gauges} eventSeed={eventSeed} />

        <NeedsLane items={needsYou} onRefresh={refreshOne} onError={setError} />

        <div className="zone-head has-aux">
          <span>进行中的任务</span>
          <span className="n">{running.length}</span>
          <span className="zh-rule" />
          <div className="density">
            <button
              className={density === "cards" ? "on" : ""}
              onClick={() => setDeckDensity("cards")}
            >
              ▦ 卡片
            </button>
            <button
              className={density === "rows" ? "on" : ""}
              onClick={() => setDeckDensity("rows")}
            >
              ☰ 列表
            </button>
          </div>
        </div>

        <FleetCards tasks={running} />
        <FleetRows onRefresh={refreshOne} onError={setError} />
        <SettledStrip items={settled} />
      </div>
    </div>
  );
}

import { useEffect, useState } from "react";
import { contextCurrent, contextSettingsUpdate, type ContextCurrentView } from "../../lib/ipc";

/**
 * FR-1 (M1a-08)：/context 面板——最近一次 run 注入的指令文件、预算占用、
 * 来源标注与记忆注入摘要；跳过/被裁剪条目带标注。可一键对本项目关闭注入
 * （PRD §8 配置项，落 daemon 侧 context settings）。
 */
export function ContextPanel({
  taskId,
  workspacePath,
  onClose,
}: {
  taskId: string;
  workspacePath: string | null;
  onClose: () => void;
}) {
  const [view, setView] = useState<ContextCurrentView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [toggling, setToggling] = useState(false);

  useEffect(() => {
    let alive = true;
    contextCurrent(taskId)
      .then((data) => {
        if (alive) setView(data);
      })
      .catch((cause: unknown) => {
        if (alive) setError(String(cause ?? "读取失败"));
      });
    return () => {
      alive = false;
    };
  }, [taskId]);

  const toggleInjection = async (next: boolean) => {
    if (!workspacePath) return;
    setToggling(true);
    try {
      await contextSettingsUpdate(workspacePath, next);
      const data = await contextCurrent(taskId);
      setView(data);
    } catch (cause: unknown) {
      setError(String(cause ?? "切换失败"));
    } finally {
      setToggling(false);
    }
  };

  const layerLabel: Record<string, string> = {
    global: "全局",
    "repo-foreign": "外来",
    "repo-own": "自有",
    subdir: "子目录",
  };
  const statusLabel: Record<string, string> = {
    injected: "",
    skipped_oversize: "跳过（超过 4 MiB）",
    trimmed: "被裁剪（预算）",
    dropped_budget: "去重/预算淘汰",
    dropped_disabled: "注入已关闭",
  };

  return (
    <div className="slash-menu" role="dialog" aria-label="上下文注入详情">
      <div className="slash-command-list" style={{ maxHeight: "60vh", overflow: "auto" }}>
        {error && <div className="panel-note" role="alert">{error}</div>}
        {!view && !error && <div className="panel-note">读取中…</div>}
        {view && (
          <>
            <div className="panel-note">
              {view.injectionEnabled
                ? `指令注入已开启 · 本次 run 注入 ${view.instructions.filter((entry) => entry.status === "injected").length} 条 · ${view.instructionsBytes} / ${view.totalBudgetBytes} 字节（JIT 额度 ${view.jitAllowanceBytes}）`
                : "指令注入已对本项目关闭（新 run 不再注入）"}
              {view.memory
                ? ` · 记忆快照 ${view.memory.snapshotHash.slice(0, 8)}（${view.memory.entryCount} 条 · ${view.memory.chars} 字符）`
                : " · 无记忆注入"}
            </div>
            {view.instructions.length === 0 && (
              <div className="panel-note">本次 run 没有可注入的项目指令文件。</div>
            )}
            {view.instructions.map((entry) => (
              <div key={`${entry.layer}:${entry.path}`} className="slash-command-item">
                <span className="slash-command-name">
                  [{layerLabel[entry.layer] ?? entry.layer}] {entry.path.split(/[\\/]/).pop()}
                </span>
                <span className="slash-command-hint">
                  {entry.bytes} B · {entry.sha256 || "—"}
                  {statusLabel[entry.status] ? ` · ${statusLabel[entry.status]}` : ""}
                </span>
              </div>
            ))}
            {view.jitBatches.length > 0 && (
              <div className="panel-note">
                运行中 JIT 注入 {view.jitBatches.length} 批：累计{" "}
                {view.jitBatches.map((batch) => batch.paths.join("、")).filter(Boolean).join("；")}
              </div>
            )}
            {workspacePath && (
              <div className="slash-command-item">
                <button
                  type="button"
                  disabled={toggling}
                  onClick={() => toggleInjection(!view.injectionEnabled)}
                >
                  {view.injectionEnabled ? "对本项目关闭指令注入" : "对本项目开启指令注入"}
                </button>
              </div>
            )}
            {!workspacePath && <div className="panel-note">附加项目后可配置注入开关。</div>}
          </>
        )}
        <div className="slash-command-item">
          <button type="button" onClick={onClose}>
            关闭
          </button>
        </div>
      </div>
    </div>
  );
}

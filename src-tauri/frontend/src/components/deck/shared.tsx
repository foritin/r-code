/**
 * Deck 共享小组件：DiffStat（变更统计）与 VerifyRow（fleet card 验证行）。
 */
import type { FileChange, TaskDetail } from "../../lib/types";
import { latestVerification } from "../../lib/deck";

/** 变更统计：总数 + 按 change_type 分列（复用 .diffstat 原语配色）。 */
export function DiffStat({
  changes,
  className,
}: {
  changes: FileChange[];
  className?: string;
}) {
  const cls = `diffstat${className ? ` ${className}` : ""}`;
  const total = changes.length;
  if (total === 0) {
    return (
      <span className={cls}>
        <span className="dim">no changes</span>
      </span>
    );
  }
  const count = (k: FileChange["change_type"]) =>
    changes.filter((c) => c.change_type === k).length;
  const [created, modified, deleted, renamed] = [
    count("create"),
    count("modify"),
    count("delete"),
    count("rename"),
  ];
  return (
    <span className={cls}>
      <span className="dim">{total} files</span>
      {created > 0 && (
        <>
          {" · "}
          <span className="add">{created} new</span>
        </>
      )}
      {modified > 0 && (
        <>
          {" · "}
          <span className="dim">{modified} mod</span>
        </>
      )}
      {renamed > 0 && (
        <>
          {" · "}
          <span className="dim">{renamed} ren</span>
        </>
      )}
      {deleted > 0 && (
        <>
          {" · "}
          <span className="del">{deleted} del</span>
        </>
      )}
    </span>
  );
}

/**
 * 验证行：有 running verification → sweep 不确定动画；
 * 否则确定性进度（宽度由结论推出）+ 结论文本。
 */
export function VerifyRow({ detail }: { detail: TaskDetail | undefined }) {
  const running = detail?.verifications.find((v) => v.status === "running");
  if (running) {
    return (
      <div className="verify">
        <span className="vcmd">{running.command}</span>
        <div className="vbar">
          <i />
        </div>
        <span>running</span>
      </div>
    );
  }

  const v = latestVerification(detail);
  if (!v) {
    return (
      <div className="verify">
        <span className="vcmd">verification armed</span>
        <div className="vbar det">
          <i style={{ width: "0%" }} />
        </div>
        <span>—</span>
      </div>
    );
  }

  const failed = v.status === "failed" || v.status === "timeout";
  const pct = v.status === "passed" || failed ? 100 : 50;
  return (
    <div className="verify">
      <span className="vcmd">{v.command}</span>
      <div className={`vbar det${failed ? " fail" : ""}`}>
        <i style={{ width: `${pct}%` }} />
      </div>
      <span>{v.status}</span>
    </div>
  );
}

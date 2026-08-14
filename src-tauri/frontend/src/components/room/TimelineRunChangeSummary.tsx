import { useState } from "react";
import type { FileChange } from "../../lib/types";
import { useAppStore } from "../../store/app";
import { IconChevronDown, IconFile } from "../icons";
import { changeTypeLabel, pathParts, useRunChangeStats } from "./run-change-stats";

interface Props {
  taskId: string;
  runId: string;
  changes: readonly FileChange[];
}

/** Final, run-scoped artifact receipt rendered immediately after that turn's final response. */
export function TimelineRunChangeSummary({ taskId, runId, changes }: Props) {
  const openWorkbenchFile = useAppStore((state) => state.openWorkbenchFile);
  const [expanded, setExpanded] = useState(true);
  const [showAll, setShowAll] = useState(false);
  const {
    files,
    statsByPath,
    additions,
    deletions,
    statsPending,
    hasKnownStats,
  } = useRunChangeStats(taskId, runId, changes);

  if (files.length === 0) return null;
  const detailId = `timeline-run-changes-${runId}`;
  const visibleFiles = showAll ? files : files.slice(0, 3);
  const hiddenCount = files.length - visibleFiles.length;

  return (
    <section className="timeline-run-changes" data-run-id={runId}>
      <button
        type="button"
        className="timeline-run-changes-head ring-inset"
        aria-expanded={expanded}
        aria-controls={detailId}
        onClick={() => setExpanded((current) => !current)}
      >
        <span className="timeline-run-changes-icon" aria-hidden="true"><IconFile width={18} height={18} /></span>
        <span className="timeline-run-changes-copy">
          <strong>已编辑 {files.length} 个文件</strong>
          {hasKnownStats ? (
            <small aria-label={`新增 ${additions} 行，删除 ${deletions} 行`}>
              <b className="add">+{additions}</b>
              <b className="del">−{deletions}</b>
            </small>
          ) : statsPending ? (
            <small>正在统计变更行数…</small>
          ) : null}
        </span>
        <IconChevronDown className="timeline-run-changes-chevron" width={14} height={14} aria-hidden="true" />
      </button>
      {expanded && (
        <div className="timeline-run-change-list" id={detailId}>
          {visibleFiles.map((file) => {
            const parts = pathParts(file.path);
            const stat = statsByPath[file.path];
            return (
              <button
                type="button"
                className="timeline-run-change-row"
                key={file.path}
                title={`在文件工作台打开 ${file.path}`}
                onClick={() => openWorkbenchFile(taskId, file.path)}
              >
                <span className="timeline-run-change-path">
                  <strong>{parts.name}</strong>
                  {parts.directory && <small>{parts.directory}</small>}
                </span>
                {stat?.available ? (
                  <span className="timeline-run-change-stat" aria-label={`新增 ${stat.additions ?? 0} 行，删除 ${stat.deletions ?? 0} 行`}>
                    <b className="add">+{stat.additions ?? 0}</b>
                    <b className="del">−{stat.deletions ?? 0}</b>
                  </span>
                ) : stat ? (
                  <span className="timeline-run-change-kind">{changeTypeLabel(file)}</span>
                ) : (
                  <span className="timeline-run-change-pending" aria-label="正在统计">···</span>
                )}
              </button>
            );
          })}
          {hiddenCount > 0 && (
            <button
              type="button"
              className="timeline-run-change-more"
              aria-expanded={showAll}
              onClick={() => setShowAll(true)}
            >
              再显示 {hiddenCount} 个文件
            </button>
          )}
        </div>
      )}
    </section>
  );
}

import { useEffect, useMemo, useState } from "react";
import { changeDiff } from "../../lib/ipc";
import type { FileChange } from "../../lib/types";
import {
  changeFingerprint,
  changeStatFromDiff,
  latestSessionChanges,
  type SessionChangeStat,
} from "./session-run-summary-model";

const MAX_CONCURRENT_DIFFS = 6;
const MAX_CACHED_DIFFS = 512;

interface ScheduledDiff {
  work: () => Promise<SessionChangeStat>;
  resolve: (value: SessionChangeStat) => void;
}

export interface RunChangeStats {
  files: FileChange[];
  statsByPath: Record<string, SessionChangeStat>;
  additions: number;
  deletions: number;
  statsPending: boolean;
  hasKnownStats: boolean;
}

export interface PathParts {
  name: string;
  directory: string | null;
}

const statCache = new Map<string, SessionChangeStat>();
const inFlight = new Map<string, Promise<SessionChangeStat>>();
const queue: ScheduledDiff[] = [];
let activeDiffs = 0;

function remember(key: string, stat: SessionChangeStat): void {
  statCache.delete(key);
  statCache.set(key, stat);
  while (statCache.size > MAX_CACHED_DIFFS) {
    const oldest = statCache.keys().next().value as string | undefined;
    if (oldest == null) break;
    statCache.delete(oldest);
  }
}

function drainQueue(): void {
  while (activeDiffs < MAX_CONCURRENT_DIFFS && queue.length > 0) {
    const job = queue.shift();
    if (!job) return;
    activeDiffs += 1;
    void job.work()
      .then(job.resolve)
      .finally(() => {
        activeDiffs -= 1;
        drainQueue();
      });
  }
}

function scheduleDiff(work: () => Promise<SessionChangeStat>): Promise<SessionChangeStat> {
  return new Promise((resolve) => {
    queue.push({ work, resolve });
    drainQueue();
  });
}

function statKey(taskId: string, runId: string, change: FileChange): string {
  return `${taskId}\u0000${runId}\u0000${changeFingerprint(change)}`;
}

function loadChangeStat(taskId: string, runId: string, change: FileChange): Promise<SessionChangeStat> {
  const key = statKey(taskId, runId, change);
  const cached = statCache.get(key);
  if (cached) return Promise.resolve(cached);
  const pending = inFlight.get(key);
  if (pending) return pending;

  const request = scheduleDiff(async () => {
    try {
      return changeStatFromDiff(await changeDiff(taskId, change.path, runId));
    } catch {
      return { additions: null, deletions: null, available: false };
    }
  }).then((stat) => {
    remember(key, stat);
    inFlight.delete(key);
    return stat;
  });
  inFlight.set(key, request);
  return request;
}

/**
 * Shared run-scoped diffstat loader. A global six-request scheduler prevents a long history from
 * turning one render into dozens of simultaneous IPC/blob reads; the fingerprint cache is shared
 * by the live strip and the archived Timeline card.
 */
export function useRunChangeStats(
  taskId: string,
  runId: string,
  changes: readonly FileChange[],
): RunChangeStats {
  const files = useMemo(() => latestSessionChanges(changes), [changes]);
  const fingerprint = files.map(changeFingerprint).join("\u0001");
  const [statsByPath, setStatsByPath] = useState<Record<string, SessionChangeStat>>({});

  useEffect(() => {
    let cancelled = false;
    const initial: Record<string, SessionChangeStat> = {};
    for (const change of files) {
      const cached = statCache.get(statKey(taskId, runId, change));
      if (cached) initial[change.path] = cached;
    }
    setStatsByPath(initial);

    for (const change of files) {
      if (initial[change.path]) continue;
      void loadChangeStat(taskId, runId, change).then((stat) => {
        if (cancelled) return;
        setStatsByPath((current) => ({ ...current, [change.path]: stat }));
      });
    }
    return () => { cancelled = true; };
    // The semantic fingerprint avoids reloading when the 2s detail poll only replaces arrays.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [taskId, runId, fingerprint]);

  const allStats = files.map((file) => statsByPath[file.path]);
  const resolvedStats = allStats.filter(
    (stat): stat is SessionChangeStat => stat?.available === true,
  );
  const additions = resolvedStats.reduce((total, stat) => total + (stat.additions ?? 0), 0);
  const deletions = resolvedStats.reduce((total, stat) => total + (stat.deletions ?? 0), 0);
  const statsPending = allStats.some((stat) => stat == null);
  const hasKnownStats = files.length > 0
    && !statsPending
    && allStats.every((stat) => stat?.available === true);

  return { files, statsByPath, additions, deletions, statsPending, hasKnownStats };
}

export function pathParts(path: string): PathParts {
  const normalized = path.replaceAll("\\", "/");
  const separator = normalized.lastIndexOf("/");
  if (separator < 0) return { name: normalized, directory: null };
  return {
    name: normalized.slice(separator + 1) || normalized,
    directory: normalized.slice(0, separator) || null,
  };
}

export function changeTypeLabel(change: FileChange): string {
  switch (change.change_type) {
    case "create": return "新增";
    case "modify": return "修改";
    case "delete": return "删除";
    case "rename": return "重命名";
  }
}

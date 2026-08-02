import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import { createPortal } from "react-dom";
import { copyText } from "../../lib/clipboard";
import { localFileTarget, revealLocalPath, type LocalFileTarget } from "../../lib/ipc";
import { useAppStore } from "../../store/app";
import { pushToast } from "../../store/toast";

export interface FileContextMenuTarget {
  workspacePath: string;
  path: string;
  x: number;
  y: number;
  isDirectory?: boolean;
}

export interface FileTaskTarget {
  id: string;
  title: string;
}

interface Props {
  target: FileContextMenuTarget | null;
  tasks: readonly FileTaskTarget[];
  onDismiss: () => void;
  onTaskSelected?: (task: FileTaskTarget) => void;
}

type Panel = "root" | "tasks" | "open-with";
const VIEWPORT_MARGIN = 8;
const ITEM_SELECTOR = '[role="menuitem"]:not([disabled])';

async function resolveWorkspaceFile(target: FileContextMenuTarget): Promise<LocalFileTarget> {
  const resolved = await localFileTarget(target.workspacePath, target.path);
  if (resolved.scope !== "workspace" || resolved.is_directory || !resolved.relative_path) {
    throw new Error("目标不是当前工作区中的文件");
  }
  return resolved;
}

/** Pointer-positioned, file-only actions shared by both file trees. */
export function FileContextMenu({ target, tasks, onDismiss, onTaskSelected }: Props) {
  const menuRef = useRef<HTMLDivElement | null>(null);
  const [panel, setPanel] = useState<Panel>("root");
  const [busy, setBusy] = useState(false);
  const [style, setStyle] = useState<CSSProperties>({ position: "fixed", visibility: "hidden" });

  const reposition = useCallback(() => {
    const menu = menuRef.current;
    if (!target || !menu) return;
    const viewport = window.visualViewport;
    const top = viewport?.offsetTop ?? 0;
    const left = viewport?.offsetLeft ?? 0;
    const width = viewport?.width ?? document.documentElement.clientWidth;
    const height = viewport?.height ?? document.documentElement.clientHeight;
    const right = left + width;
    const bottom = top + height;
    const rect = menu.getBoundingClientRect();
    const maxWidth = Math.max(0, width - VIEWPORT_MARGIN * 2);
    const maxHeight = Math.max(0, height - VIEWPORT_MARGIN * 2);
    const menuWidth = Math.min(rect.width, maxWidth);
    const menuHeight = Math.min(rect.height, maxHeight);
    setStyle({
      position: "fixed",
      visibility: "visible",
      left: Math.max(left + VIEWPORT_MARGIN, Math.min(target.x, right - VIEWPORT_MARGIN - menuWidth)),
      top: Math.max(top + VIEWPORT_MARGIN, Math.min(target.y, bottom - VIEWPORT_MARGIN - menuHeight)),
      maxWidth,
      maxHeight,
    });
  }, [target]);

  useEffect(() => {
    setPanel("root");
    setBusy(false);
    setStyle({ position: "fixed", visibility: "hidden" });
  }, [target?.path, target?.workspacePath, target?.x, target?.y]);

  useLayoutEffect(() => {
    reposition();
    const schedule = () => requestAnimationFrame(reposition);
    window.addEventListener("resize", schedule);
    window.visualViewport?.addEventListener("resize", schedule);
    window.visualViewport?.addEventListener("scroll", schedule);
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(schedule);
    if (menuRef.current) observer?.observe(menuRef.current);
    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", schedule);
      window.visualViewport?.removeEventListener("resize", schedule);
      window.visualViewport?.removeEventListener("scroll", schedule);
    };
  }, [panel, reposition]);

  useEffect(() => {
    if (!target || target.isDirectory) return;
    const onPointerDown = (event: PointerEvent) => {
      if (!menuRef.current?.contains(event.target as Node)) onDismiss();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onDismiss();
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKeyDown);
    requestAnimationFrame(() => menuRef.current?.querySelector<HTMLElement>(ITEM_SELECTOR)?.focus());
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [onDismiss, panel, target]);

  if (!target || target.isDirectory || typeof document === "undefined") return null;

  const finish = async (action: () => Promise<void>) => {
    if (busy) return;
    setBusy(true);
    try {
      await action();
    } finally {
      onDismiss();
    }
  };

  const addToTask = (task: FileTaskTarget) => void finish(async () => {
    try {
      const resolved = await resolveWorkspaceFile(target);
      useAppStore.getState().queueTaskFileReference(task.id, resolved.relative_path!);
      onTaskSelected?.(task);
      pushToast({ kind: "success", title: "已添加文件引用", body: task.title });
    } catch (cause) {
      pushToast({ kind: "error", title: "无法添加文件引用", body: String(cause) });
    }
  });

  const copyPath = () => void finish(async () => {
    try {
      const resolved = await resolveWorkspaceFile(target);
      if (!await copyText(resolved.absolute_path)) throw new Error("剪贴板不可用");
      pushToast({ kind: "success", title: "已复制文件路径", body: resolved.absolute_path });
    } catch (cause) {
      pushToast({ kind: "error", title: "无法复制文件路径", body: String(cause) });
    }
  });

  const reveal = () => void finish(async () => {
    try {
      const resolved = await resolveWorkspaceFile(target);
      await revealLocalPath(resolved.absolute_path);
      pushToast({ kind: "success", title: "已在文件管理器中显示", body: resolved.absolute_path });
    } catch (cause) {
      pushToast({ kind: "error", title: "无法在文件管理器中显示", body: String(cause) });
    }
  });

  const onKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Tab") {
      onDismiss();
      return;
    }
    if (event.key === "ArrowLeft" && panel !== "root") {
      event.preventDefault();
      setPanel("root");
      return;
    }
    const items = Array.from(menuRef.current?.querySelectorAll<HTMLButtonElement>(ITEM_SELECTOR) ?? []);
    if (items.length === 0) return;
    const current = items.indexOf(document.activeElement as HTMLButtonElement);
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const delta = event.key === "ArrowDown" ? 1 : -1;
      items[current < 0 ? (delta > 0 ? 0 : items.length - 1) : (current + delta + items.length) % items.length].focus();
    } else if (event.key === "Home" || event.key === "End") {
      event.preventDefault();
      items[event.key === "Home" ? 0 : items.length - 1].focus();
    }
  };

  return createPortal(
    <div
      ref={menuRef}
      className="file-context-menu"
      role="menu"
      aria-label="文件操作"
      style={style}
      onKeyDown={onKeyDown}
    >
      {panel === "root" ? (
        <>
          <span className="file-context-menu-path">{target.path}</span>
          <button
            type="button"
            role="menuitem"
            disabled={busy || tasks.length === 0}
            onClick={() => tasks.length === 1 ? addToTask(tasks[0]) : setPanel("tasks")}
          >
            <span>添加到任务</span><small>{tasks.length === 0 ? "没有可用任务" : tasks.length === 1 ? tasks[0].title : `${tasks.length} 个任务`}</small>
          </button>
          <button type="button" role="menuitem" disabled={busy} onClick={copyPath}>复制路径</button>
          <button type="button" role="menuitem" disabled={busy} onClick={() => setPanel("open-with")}>
            <span>打开方式</span><small>文件管理器</small>
          </button>
        </>
      ) : panel === "tasks" ? (
        <>
          <button type="button" role="menuitem" className="file-context-menu-back" onClick={() => setPanel("root")}>返回</button>
          <span className="file-context-menu-heading">添加到任务</span>
          {tasks.map((task) => <button type="button" role="menuitem" disabled={busy} onClick={() => addToTask(task)} key={task.id}>{task.title}</button>)}
        </>
      ) : (
        <>
          <button type="button" role="menuitem" className="file-context-menu-back" onClick={() => setPanel("root")}>返回</button>
          <span className="file-context-menu-heading">打开方式</span>
          <button type="button" role="menuitem" disabled={busy} onClick={reveal}>在文件管理器中显示</button>
        </>
      )}
    </div>,
    document.body,
  );
}

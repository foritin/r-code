import { useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getVersion } from "@tauri-apps/api/app";
import { useAppStore } from "../../store/app";
import { useTasksStore, selectRunning, selectNeedsYou } from "../../store/tasks";
import { elapsedSince } from "../../lib/format";
import { IconClose, IconMaximize, IconMinimize } from "../icons";

/**
 * MenuBar（40px）— 桌面标准顶栏：品牌 + 文件/编辑/视图/帮助 + 场景上下文 +
 * Room state-chip + 窗控（无边框窗口）。
 * 菜单为 React 下拉：单击展开，展开后悬停切换，Esc/点击外部关闭，↑↓⏎ 可导航。
 */

interface MenuItem {
  label?: string;
  shortcut?: string;
  disabled?: boolean;
  separator?: boolean;
  action?: () => void;
}

export function MenuBar() {
  const scene = useAppStore((s) => s.scene);
  const setScene = useAppStore((s) => s.setScene);
  const goHome = useAppStore((s) => s.goHome);
  const currentTaskId = useAppStore((s) => s.currentTaskId);
  const setCanvasTab = useAppStore((s) => s.setCanvasTab);
  const toggleSearch = useAppStore((s) => s.toggleSearch);
  const zoomIn = useAppStore((s) => s.zoomIn);
  const zoomOut = useAppStore((s) => s.zoomOut);
  const zoomReset = useAppStore((s) => s.zoomReset);

  const tasks = useTasksStore((s) => s.tasks);
  const running = useTasksStore(selectRunning);
  const needsYou = useTasksStore(selectNeedsYou);
  const workspaces = useTasksStore((s) => s.workspaces);

  const [open, setOpen] = useState<string | null>(null);
  const [about, setAbout] = useState<string | null>(null);
  const barRef = useRef<HTMLDivElement>(null);

  // 点击外部关闭菜单
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (barRef.current && !barRef.current.contains(e.target as Node)) setOpen(null);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(null);
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const currentTask = tasks.find((t) => t.id === currentTaskId);
  const workspaceName = (path: string | null) => {
    if (!path) return "聊天";
    return workspaces.find((w) => w.canonical_path === path)?.display_name ?? path.split(/[\\/]/).pop() ?? path;
  };

  const ctx =
    scene === "home"
      ? "新对话"
      : scene === "deck"
        ? `活动：${running.length} 个进行中，${needsYou.length} 项待处理`
        : scene === "room" && currentTask
          ? `${workspaceName(currentTask.workspace_path)} › ${currentTask.title}`
          : scene === "inbox"
            ? `待处理：${needsYou.length} 项`
            : scene === "projects"
              ? "文件夹"
              : scene === "editor"
                ? "文件预览"
                : scene === "settings"
                  ? "设置"
                  : "";

  const execEdit = (cmd: "cut" | "copy" | "paste" | "selectAll") => () => {
    document.execCommand(cmd);
  };

  const showAbout = async () => {
    try {
      setAbout(await getVersion());
    } catch {
      setAbout("dev");
    }
  };

  const menus: { key: string; title: string; items: MenuItem[] }[] = [
    {
      key: "file",
      title: "文件",
      items: [
        { label: "新建会话", shortcut: "Ctrl N", action: goHome },
        { label: "打开工作区…", action: () => setScene("projects") },
        { separator: true },
        { label: "设置", shortcut: "Ctrl ,", action: () => setScene("settings") },
        { separator: true },
        { label: "退出", action: () => void getCurrentWindow().close() },
      ],
    },
    {
      key: "edit",
      title: "编辑",
      items: [
        { label: "剪切", shortcut: "Ctrl X", action: execEdit("cut") },
        { label: "复制", shortcut: "Ctrl C", action: execEdit("copy") },
        { label: "粘贴", shortcut: "Ctrl V", action: execEdit("paste") },
        { label: "全选", shortcut: "Ctrl A", action: execEdit("selectAll") },
      ],
    },
    {
      key: "view",
      title: "视图",
      items: [
        { label: "新对话", action: goHome },
        { label: "活动", action: () => setScene("deck") },
        { label: "待处理", action: () => setScene("inbox") },
        { label: "文件夹", action: () => setScene("projects") },
        { label: "文件预览", shortcut: "Ctrl E", action: () => setScene("editor") },
        { label: "搜索", shortcut: "Ctrl K", action: toggleSearch },
        { separator: true },
        { label: "查看变更", disabled: scene !== "room", action: () => setCanvasTab("changes") },
        { label: "查看验证", disabled: scene !== "room", action: () => setCanvasTab("review") },
        { separator: true },
        { label: "放大", shortcut: "Ctrl +", action: zoomIn },
        { label: "缩小", shortcut: "Ctrl −", action: zoomOut },
        { label: "重置缩放", shortcut: "Ctrl 0", action: zoomReset },
      ],
    },
    {
      key: "help",
      title: "帮助",
      items: [
        { label: "查看日志", action: () => setScene("settings") },
        { label: "支持包", action: () => setScene("settings") },
        { separator: true },
        { label: "关于 R-Code", action: () => void showAbout() },
      ],
    },
  ];

  return (
    <header className="menubar" ref={barRef}>
      <button className="brand" onClick={goHome} title="R-Code — 回到新对话">
        R-Code
      </button>
      <nav className="menus" aria-label="菜单栏">
        {menus.map((m) => (
          <div className="menu" key={m.key}>
            <button
              className={"menu-title" + (open === m.key ? " on" : "")}
              onClick={() => setOpen(open === m.key ? null : m.key)}
              onMouseEnter={() => {
                if (open && open !== m.key) setOpen(m.key);
              }}
              aria-expanded={open === m.key}
              aria-haspopup="menu"
            >
              {m.title}
            </button>
            {open === m.key && (
              <div className="dropdown" role="menu">
                {m.items.map((it, i) =>
                  it.separator ? (
                    <div className="sep" key={i} />
                  ) : (
                    <button
                      className="mi"
                      key={i}
                      role="menuitem"
                      disabled={it.disabled}
                      onClick={() => {
                        setOpen(null);
                        it.action?.();
                      }}
                    >
                      <span>{it.label}</span>
                      {it.shortcut && <span className="sc">{it.shortcut}</span>}
                    </button>
                  )
                )}
              </div>
            )}
          </div>
        ))}
      </nav>
      <span className="ctx">{ctx}</span>
      {scene === "room" && currentTask && <RoomStateChip taskId={currentTask.id} />}
      <span className="spacer" />
      <span className="winctl">
        <button
          className="wc"
          onClick={() => void getCurrentWindow().minimize()}
          aria-label="最小化"
          title="最小化"
        >
          <IconMinimize />
        </button>
        <button
          className="wc"
          onClick={() => void getCurrentWindow().toggleMaximize()}
          aria-label="最大化或还原"
          title="最大化或还原"
        >
          <IconMaximize />
        </button>
        <button
          className="wc close"
          onClick={() => void getCurrentWindow().close()}
          aria-label="关闭"
          title="关闭"
        >
          <IconClose />
        </button>
      </span>
      {about && (
        <div className="about-backdrop" onClick={() => setAbout(null)}>
          <div className="about pane" onClick={(e) => e.stopPropagation()}>
            <div className="about-brand">R-Code</div>
            <div className="about-ver">版本 {about}</div>
            <div className="about-desc">本地优先的编码 agent 驾驶舱。</div>
            <button className="btn" onClick={() => setAbout(null)}>
              关闭
            </button>
          </div>
        </div>
      )}
    </header>
  );
}

/** Room 状态芯片：运行中任务显示状态。 */
function RoomStateChip({ taskId }: { taskId: string }) {
  const detail = useTasksStore((s) => s.details[taskId]);
  if (!detail) return null;
  const { task, runs, permissions } = detail;
  const pending = permissions.filter((p) => p.decision === "pending").length;
  const activeRun = runs.find((r) => r.ended_at === null);

  if (pending > 0) {
    return (
      <span className="state-chip warn">
        <i />
        有 {pending} 项待处理
      </span>
    );
  }
  if (task.state === "review_ready") {
    return (
      <span className="state-chip warn">
        <i />
        等待验收
      </span>
    );
  }
  if (activeRun) {
    return (
      <span className="state-chip">
        <i />
        正在执行（{elapsedSince(activeRun.started_at)}）
      </span>
    );
  }
  return null;
}

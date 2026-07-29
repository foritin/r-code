import { useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useAppStore } from "../../store/app";
import { selectNeedsYou, useTasksStore } from "../../store/tasks";
import { notificationList, notificationMarkAllRead, notificationMarkRead } from "../../lib/ipc";
import type { Notification, NotificationPage } from "../../lib/types";
import {
  IconBell,
  IconClose,
  IconHelp,
  IconMaximize,
  IconMinimize,
  IconSearch,
  IconSidebar,
} from "../icons";

/**
 * 全局顶栏只放跨页面动作：搜索、通知、帮助与窗口控制。
 * 项目、对话、外观和设置都在左栏或设置页保留唯一入口，避免重复导航。
 * 项目内才出现的活动流由 DashboardScene 自己持有，避免跨页面残留错误的右栏。
 */
export function MenuBar() {
  const setScene = useAppStore((s) => s.setScene);
  const openRoom = useAppStore((s) => s.openRoom);
  const railCollapsed = useAppStore((s) => s.railCollapsed);
  const toggleRail = useAppStore((s) => s.toggleRail);
  const toggleSearch = useAppStore((s) => s.toggleSearch);
  const needsYou = useTasksStore(selectNeedsYou);
  const [notificationOpen, setNotificationOpen] = useState(false);
  const [notificationPage, setNotificationPage] = useState<NotificationPage | null>(null);
  const notificationRef = useRef<HTMLDivElement>(null);

  const refreshNotifications = async () => {
    try {
      setNotificationPage(await notificationList());
    } catch {
      // 顶栏通知不应阻断主工作流；下一次轮询会再试。
    }
  };

  useEffect(() => {
    void refreshNotifications();
    const timer = window.setInterval(() => void refreshNotifications(), 15_000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    if (!notificationOpen) return;
    const onPointerDown = (event: PointerEvent) => {
      if (!notificationRef.current?.contains(event.target as Node)) setNotificationOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setNotificationOpen(false);
    };
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [notificationOpen]);

  const openNotification = async (notification: Notification) => {
    try {
      await notificationMarkRead(notification.id);
    } finally {
      setNotificationPage((page) => page ? {
        ...page,
        unread_count: Math.max(0, page.unread_count - (notification.read_at ? 0 : 1)),
        notifications: page.notifications.map((item) => item.id === notification.id ? { ...item, read_at: item.read_at ?? new Date().toISOString() } : item),
      } : page);
    }
    setNotificationOpen(false);
    if (notification.task_id) openRoom(notification.task_id);
  };

  const markAllNotificationsRead = async () => {
    try {
      await notificationMarkAllRead();
      setNotificationPage((page) => page ? {
        ...page,
        unread_count: 0,
        notifications: page.notifications.map((item) => ({ ...item, read_at: item.read_at ?? new Date().toISOString() })),
      } : page);
    } catch {
      // 不在菜单里展示瞬态错误，保持可再次尝试。
    }
  };

  const unreadNotifications = notificationPage?.unread_count ?? needsYou.length;

  return (
    <header className="menubar app-topbar">
      <button className="top-icon desktop-sidebar-toggle" onClick={toggleRail} aria-label={railCollapsed ? "展开侧边栏" : "收起侧边栏"} title={railCollapsed ? "展开侧边栏" : "收起侧边栏"}>
        <IconSidebar width={16} height={16} />
      </button>
      <button className="top-icon compact-search-toggle" onClick={toggleSearch} aria-label="搜索任务、文件和对话" title="搜索">
        <IconSearch width={16} height={16} />
      </button>

      <div className="topbar-spacer" />
      <div className="top-actions" aria-label="全局操作">
        <div className="notification-menu-wrap" ref={notificationRef}>
          <button
            className={`top-icon has-badge${notificationOpen ? " active" : ""}`}
            onClick={() => { setNotificationOpen((open) => !open); if (!notificationOpen) void refreshNotifications(); }}
            title="通知中心"
            aria-label={unreadNotifications > 0 ? `通知中心，${unreadNotifications} 条未读` : "通知中心，无未读通知"}
            aria-expanded={notificationOpen}
            aria-haspopup="dialog"
          >
            <IconBell />
            {unreadNotifications > 0 && <b>{unreadNotifications > 9 ? "9+" : unreadNotifications}</b>}
          </button>
          {notificationOpen && (
            <section className="notification-menu" role="dialog" aria-label="通知中心">
              <header className="notification-menu-head"><div><strong>通知</strong><span>{unreadNotifications ? `${unreadNotifications} 条未读` : "已全部读完"}</span></div><button className="text-link" disabled={!unreadNotifications} onClick={() => void markAllNotificationsRead()}>全部已读</button></header>
              <div className="notification-menu-list">
                {!notificationPage?.notifications.length ? <p>暂时没有通知。</p> : notificationPage.notifications.map((notification) => <button className={`notification-menu-item${notification.read_at ? " read" : ""}`} key={notification.id} onClick={() => void openNotification(notification)}><i className={notification.kind} /><span><strong>{notification.title}</strong><small>{notification.body}</small></span><time>{notification.read_at ? "已读" : "未读"}</time></button>)}
              </div>
              <button className="notification-menu-inbox" onClick={() => { setNotificationOpen(false); setScene("inbox"); }}>打开待处理</button>
            </section>
          )}
        </div>
        <button className="top-icon top-action-help" onClick={() => window.dispatchEvent(new Event("r-code:shortcuts"))} title="快捷键与帮助">
          <IconHelp />
        </button>
      </div>

      <span className="winctl app-window-controls" aria-label="窗口控制">
        <button className="wc" onClick={() => void getCurrentWindow().minimize().catch(() => {})} aria-label="最小化"><IconMinimize /></button>
        <button className="wc" onClick={() => void getCurrentWindow().toggleMaximize().catch(() => {})} aria-label="最大化或还原"><IconMaximize /></button>
        <button className="wc close" onClick={() => void getCurrentWindow().close().catch(() => {})} aria-label="关闭"><IconClose /></button>
      </span>
    </header>
  );
}

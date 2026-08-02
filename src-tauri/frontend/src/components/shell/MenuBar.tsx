import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useAppStore } from "../../store/app";
import { selectNeedsYou, useTasksStore } from "../../store/tasks";
import { notificationList, notificationMarkAllRead, notificationMarkRead } from "../../lib/ipc";
import type { Notification, NotificationPage } from "../../lib/types";
import { requestOnboarding } from "../../lib/onboarding";
import { isMacPlatform, keyLabel } from "../../lib/keys";
import { Menu, MenuItem, MenuSeparator } from "../ui/Menu";
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
  const macOS = isMacPlatform();
  const setScene = useAppStore((s) => s.setScene);
  const goHome = useAppStore((s) => s.goHome);
  const goBack = useAppStore((s) => s.goBack);
  const goForward = useAppStore((s) => s.goForward);
  const canGoBack = useAppStore((s) => s.navigationBack.length > 0);
  const canGoForward = useAppStore((s) => s.navigationForward.length > 0);
  const setSettingsPane = useAppStore((s) => s.setSettingsPane);
  const openRoom = useAppStore((s) => s.openRoom);
  const railCollapsed = useAppStore((s) => s.railCollapsed);
  const toggleRail = useAppStore((s) => s.toggleRail);
  const toggleSearch = useAppStore((s) => s.toggleSearch);
  const zoomReset = useAppStore((s) => s.zoomReset);
  const needsYou = useTasksStore(selectNeedsYou);
  const [notificationOpen, setNotificationOpen] = useState(false);
  const [notificationPage, setNotificationPage] = useState<NotificationPage | null>(null);

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

  const showShortcuts = () => window.dispatchEvent(new Event("r-code:shortcuts"));

  return (
    <header className="menubar app-topbar">
      <button className="top-icon desktop-sidebar-toggle" onClick={toggleRail} aria-label={railCollapsed ? "展开侧边栏" : "收起侧边栏"} title={railCollapsed ? "展开侧边栏" : "收起侧边栏"}>
        <IconSidebar width={16} height={16} />
      </button>
      <nav className="desktop-navigation" aria-label="桌面导航">
        <div className="desktop-history-actions" aria-label="浏览历史">
          <button className="desktop-nav-button desktop-history-button" type="button" onClick={goBack} disabled={!canGoBack} aria-label="后退" title="后退">
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="m15 18-6-6 6-6" /></svg>
          </button>
          <button className="desktop-nav-button desktop-history-button" type="button" onClick={goForward} disabled={!canGoForward} aria-label="前进" title="前进">
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="m9 18 6-6-6-6" /></svg>
          </button>
        </div>
        <div className="desktop-app-menus">
          <Menu
            label="文件"
            menuClassName="desktop-menu-popover"
            trigger={<button className="desktop-nav-button desktop-menu-trigger" type="button">文件</button>}
          >
            {({ close }) => <>
              <MenuItem close={close} shortcut={keyLabel("new")} onSelect={goHome}>新建任务</MenuItem>
              <MenuItem close={close} onSelect={() => setScene("projects")}>打开项目…</MenuItem>
              <MenuItem close={close} onSelect={() => setScene("editor")}>当前项目文件</MenuItem>
              <MenuSeparator />
              <MenuItem close={close} onSelect={() => void getCurrentWindow().close().catch(() => {})}>关闭窗口</MenuItem>
            </>}
          </Menu>
          <Menu
            label="编辑"
            menuClassName="desktop-menu-popover"
            trigger={<button className="desktop-nav-button desktop-menu-trigger" type="button">编辑</button>}
          >
            {({ close }) => <>
              <MenuItem close={close} shortcut={keyLabel("search")} onSelect={toggleSearch}>查找</MenuItem>
              <MenuItem close={close} shortcut={keyLabel("toggleRail")} onSelect={toggleRail}>切换左侧边栏</MenuItem>
              <MenuItem close={close} onSelect={() => setScene("editor")}>编辑当前项目文件</MenuItem>
            </>}
          </Menu>
          <Menu
            label="视图"
            menuClassName="desktop-menu-popover"
            trigger={<button className="desktop-nav-button desktop-menu-trigger" type="button">视图</button>}
          >
            {({ close }) => <>
              <MenuItem close={close} onSelect={() => setScene("conversations")}>对话</MenuItem>
              <MenuItem close={close} onSelect={() => setScene("inbox")}>待处理</MenuItem>
              <MenuItem close={close} onSelect={() => setScene("deck")}>活动</MenuItem>
              <MenuItem close={close} onSelect={() => setScene("knowledge")}>知识与指令</MenuItem>
              <MenuSeparator />
              <MenuItem close={close} shortcut={keyLabel("zoomReset")} onSelect={zoomReset}>重置缩放</MenuItem>
            </>}
          </Menu>
          <Menu
            label="帮助"
            menuClassName="desktop-menu-popover"
            trigger={<button className="desktop-nav-button desktop-menu-trigger" type="button">帮助</button>}
          >
            {({ close }) => <>
              <MenuItem close={close} onSelect={requestOnboarding}>首次设置</MenuItem>
              <MenuSeparator />
              <MenuItem close={close} shortcut={keyLabel("shortcuts")} onSelect={showShortcuts}>快捷键</MenuItem>
              <MenuItem close={close} onSelect={() => setSettingsPane("diagnostics")}>诊断与支持</MenuItem>
              <MenuItem close={close} onSelect={() => setSettingsPane("codex")}>Codex 协作</MenuItem>
            </>}
          </Menu>
        </div>
      </nav>
      <button className="top-icon compact-search-toggle" onClick={toggleSearch} aria-label="搜索任务、文件和对话" title="搜索">
        <IconSearch width={16} height={16} />
      </button>

      <div className="topbar-spacer" />
      <div className="top-actions" aria-label="全局操作">
        <Menu
          className="notification-menu-wrap"
          role="dialog"
          label="通知中心"
          placement="down"
          align="right"
          menuClassName="notification-menu"
          scroll
          onOpenChange={(open) => {
            setNotificationOpen(open);
            if (open) void refreshNotifications();
          }}
          trigger={
            <button
              className={`top-icon has-badge${notificationOpen ? " active" : ""}`}
              title="通知中心"
              aria-label={unreadNotifications > 0 ? `通知中心，${unreadNotifications} 条未读` : "通知中心，无未读通知"}
            >
              <IconBell />
              {unreadNotifications > 0 && <b>{unreadNotifications > 9 ? "9+" : unreadNotifications}</b>}
            </button>
          }
        >
          {({ close }) => <>
            <header className="notification-menu-head"><div><strong>通知</strong><span>{unreadNotifications ? `${unreadNotifications} 条未读` : "已全部读完"}</span></div><button className="text-link" disabled={!unreadNotifications} onClick={() => void markAllNotificationsRead()}>全部已读</button></header>
            <div className="notification-menu-list">
              {!notificationPage?.notifications.length ? <p>暂时没有通知。</p> : notificationPage.notifications.map((notification) => <button className={`notification-menu-item${notification.read_at ? " read" : ""}`} key={notification.id} onClick={() => { close(); void openNotification(notification); }}><i className={notification.kind} /><span><strong>{notification.title}</strong><small>{notification.body}</small></span><time>{notification.read_at ? "已读" : "未读"}</time></button>)}
            </div>
            <button className="notification-menu-inbox" onClick={() => { close(); setScene("inbox"); }}>打开待处理</button>
          </>}
        </Menu>
        <button className="top-icon top-action-help" onClick={showShortcuts} title="快捷键与帮助">
          <IconHelp />
        </button>
      </div>

      {!macOS && (
        <span className="winctl app-window-controls" aria-label="窗口控制">
          <button className="wc" onClick={() => void getCurrentWindow().minimize().catch(() => {})} aria-label="最小化"><IconMinimize /></button>
          <button className="wc" onClick={() => void getCurrentWindow().toggleMaximize().catch(() => {})} aria-label="最大化或还原"><IconMaximize /></button>
          <button className="wc close" onClick={() => void getCurrentWindow().close().catch(() => {})} aria-label="关闭"><IconClose /></button>
        </span>
      )}
    </header>
  );
}

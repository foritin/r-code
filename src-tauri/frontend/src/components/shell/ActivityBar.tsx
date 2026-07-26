import { useAppStore, type Scene } from "../../store/app";
import { useTasksStore, selectNeedsYou } from "../../store/tasks";
import {
  IconBrand,
  IconDeck,
  IconEditor,
  IconHome,
  IconInbox,
  IconProjects,
  IconSearch,
  IconSessions,
  IconSettings,
} from "../icons";

/**
 * ActivityBar（52px）— 品牌 → Home / Deck(NEW) / Sessions / Inbox(徽章) /
 * Projects / Search(Ctrl K) →〔spacer〕→ Editor(Ctrl E) / Settings。
 * 结构照 fusion-obsidian.html:632-663。
 */
export function ActivityBar() {
  const scene = useAppStore((s) => s.scene);
  const setScene = useAppStore((s) => s.setScene);
  const goHome = useAppStore((s) => s.goHome);
  const toggleSearch = useAppStore((s) => s.toggleSearch);
  const setRailTab = useAppStore((s) => s.setRailTab);
  const needsCount = useTasksStore((s) => selectNeedsYou(s).length);

  const item = (
    key: string,
    label: string,
    icon: React.ReactNode,
    onClick: () => void,
    active: boolean,
    extra?: React.ReactNode
  ) => (
    <button
      key={key}
      className={`act-item${active ? " active" : ""}`}
      onClick={onClick}
      title={label}
      aria-label={label}
    >
      {icon}
      {extra}
    </button>
  );

  const go = (s: Scene) => () => setScene(s);

  return (
    <nav className="activity" aria-label="主导航">
      <button className="act-item brand" onClick={goHome} title="R-Code">
        <IconBrand width={20} height={20} />
      </button>
      {item("home", "Home", <IconHome />, goHome, scene === "home")}
      {item(
        "deck",
        "Deck — 舰队态势",
        <IconDeck />,
        go("deck"),
        scene === "deck"
      )}
      {item(
        "sessions",
        "Sessions（Rail 面板）",
        <IconSessions />,
        () => setRailTab("sessions"),
        false
      )}
      {item(
        "inbox",
        "Needs attention",
        <IconInbox />,
        go("inbox"),
        scene === "inbox",
        needsCount > 0 ? <span className="badge">{needsCount}</span> : undefined
      )}
      {item("projects", "Projects", <IconProjects />, go("projects"), scene === "projects")}
      {item("search", "Search（Ctrl K）", <IconSearch />, toggleSearch, false)}
      <span className="spacer" />
      {item("editor", "Editor（Ctrl E）", <IconEditor />, go("editor"), scene === "editor")}
      {item("settings", "Settings", <IconSettings />, go("settings"), scene === "settings")}
    </nav>
  );
}

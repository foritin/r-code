import {
  CalendarBlank,
  ChatCircleText,
  CheckCircle,
  FileText,
  Globe,
  ShieldCheck,
  Table,
  TerminalWindow,
  WarningCircle,
} from "@phosphor-icons/react";
import { levelMeta } from "../data.js";

const iconForType = {
  message: ChatCircleText,
  plan: ChatCircleText,
  web: Globe,
  spreadsheet: Table,
  decision: ShieldCheck,
  document: FileText,
  verification: CheckCircle,
  email: ChatCircleText,
  terminal: TerminalWindow,
  calendar: CalendarBlank,
  approval: ShieldCheck,
};

export function EventIcon({ type, size = 18, weight = "regular" }) {
  const Icon = iconForType[type] ?? FileText;
  return <Icon size={size} weight={weight} aria-hidden="true" />;
}

export function LevelMark({ level, compact = false }) {
  const meta = levelMeta[level] ?? levelMeta.missing;
  const Icon = level === "missing" || level === "observed" ? WarningCircle : CheckCircle;
  return (
    <span className={`level-mark level-${meta.tone} ${compact ? "compact" : ""}`}>
      <Icon size={compact ? 12 : 14} weight={level === "verified" ? "fill" : "regular"} />
      <span>{meta.label}</span>
    </span>
  );
}

export function AppBadge({ app }) {
  return <span className="app-badge">{app}</span>;
}

export function StatusDot({ status }) {
  return <span className={`status-dot status-${status}`} aria-hidden="true" />;
}


import { Funnel, MagnifyingGlass, WarningCircle } from "@phosphor-icons/react";
import { useMemo, useState } from "react";
import { formatDuration } from "../data.js";
import { ArtifactStage } from "./ArtifactStage.jsx";
import { EvidenceDrawer } from "./EvidenceDrawer.jsx";
import { EventIcon, LevelMark } from "./ui.jsx";

const filters = [
  ["all", "全部"],
  ["changed", "发生了什么变化？"],
  ["decision", "做了哪些判断？"],
  ["attention", "哪里需要注意？"],
  ["unverified", "哪些尚未验证？"],
];

function matchFilter(event, filter) {
  if (filter === "changed") return ["document", "spreadsheet", "email", "calendar"].includes(event.type);
  if (filter === "decision") return ["decision", "approval", "plan"].includes(event.type);
  if (filter === "attention") return event.status === "attention";
  if (filter === "unverified") return ["observed", "inferred", "missing"].includes(event.level);
  return true;
}

export function Explore({ scenario, event, index, onEvent, onDepth, onToast, answer, onAsk }) {
  const [filter, setFilter] = useState("all");
  const [search, setSearch] = useState("");
  const visible = useMemo(
    () => scenario.events.filter((item) => matchFilter(item, filter) && `${item.label} ${item.detail} ${item.app}`.toLowerCase().includes(search.toLowerCase())),
    [scenario, filter, search],
  );
  return (
    <main className="explore-depth">
      <aside className="explore-list">
        <div className="explore-tools">
          <label><MagnifyingGlass size={15} /><input value={search} onChange={(e) => setSearch(e.target.value)} placeholder="搜索事件、应用或证据" /></label>
          <div className="question-filters">
            <span><Funnel size={14} />沿问题筛选</span>
            {filters.map(([id, label]) => <button key={id} className={filter === id ? "active" : ""} onClick={() => setFilter(id)}>{label}</button>)}
          </div>
        </div>
        <div className="event-list" aria-label="All replay events">
          {visible.map((item) => {
            const eventIndex = scenario.events.indexOf(item);
            return (
              <button key={item.id} className={eventIndex === index ? "active" : ""} onClick={() => onEvent(eventIndex)}>
                <time>{formatDuration(item.actual)}</time>
                <span className={`event-list-icon status-${item.status}`}><EventIcon type={item.type} size={15} /></span>
                <span><strong>{item.label}</strong><small>{item.app} · {item.chapter}</small></span>
                <LevelMark level={item.level} compact />
              </button>
            );
          })}
          {!visible.length ? <div className="empty-list"><WarningCircle size={20} /><strong>没有匹配事件</strong><span>换一个问题或搜索词。</span></div> : null}
        </div>
      </aside>
      <section className="explore-inspector">
        <ArtifactStage event={event} compact />
        <div className="surrounding-context">
          <span>周围上下文</span>
          <div>
            {scenario.events.slice(Math.max(0, index - 1), index + 2).map((item) => (
              <button key={item.id} className={item.id === event.id ? "active" : ""} onClick={() => onEvent(scenario.events.indexOf(item))}>
                <EventIcon type={item.type} size={14} />
                <span><small>{formatDuration(item.actual)}</small><strong>{item.label}</strong></span>
              </button>
            ))}
          </div>
        </div>
      </section>
      <EvidenceDrawer event={event} onVerify={() => onDepth("verify")} onToast={onToast} answer={answer} onAsk={onAsk} />
    </main>
  );
}

import { CaretRight } from "@phosphor-icons/react";
import { ArtifactStage } from "./ArtifactStage.jsx";
import { EvidenceDrawer } from "./EvidenceDrawer.jsx";
import { SummaryBand } from "./SummaryBand.jsx";
import { EventIcon, LevelMark, StatusDot } from "./ui.jsx";
import { formatDuration } from "../data.js";

export function Recap({ scenario, event, index, onEvent, onPlay, onDepth, onToast, answer, onAsk }) {
  return (
    <main className="recap-depth">
      <SummaryBand scenario={scenario} onPlay={onPlay} />
      <div className="recap-workspace">
        <aside className="chapter-rail" aria-label="Semantic chapters">
          <div className="rail-heading"><span>故事章节</span><small>{scenario.events.length}</small></div>
          <div className="chapter-list">
            {scenario.events.map((item, eventIndex) => (
              <button key={item.id} className={eventIndex === index ? "active" : ""} onClick={() => onEvent(eventIndex)}>
                <time>{formatDuration(item.story)}</time>
                <span className={`chapter-icon status-${item.status}`}><EventIcon type={item.type} size={15} /></span>
                <span><strong>{item.chapter}</strong><small>{item.label}</small></span>
                {eventIndex === index ? <CaretRight size={14} /> : <StatusDot status={item.status} />}
              </button>
            ))}
          </div>
          <div className="coverage-legend">
            <span>证据语言</span>
            {["verified", "recorded", "observed", "inferred", "missing"].map((level) => <LevelMark key={level} level={level} compact />)}
          </div>
        </aside>
        <ArtifactStage event={event} />
        <EvidenceDrawer event={event} onVerify={() => onDepth("verify")} onToast={onToast} answer={answer} onAsk={onAsk} />
      </div>
    </main>
  );
}

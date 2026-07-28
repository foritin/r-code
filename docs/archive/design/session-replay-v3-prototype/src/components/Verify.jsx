import { CheckCircle, Export, FileText, LockKey, SealCheck, WarningCircle } from "@phosphor-icons/react";
import { ArtifactStage } from "./ArtifactStage.jsx";
import { EvidenceDrawer } from "./EvidenceDrawer.jsx";
import { EventIcon, LevelMark } from "./ui.jsx";
import { formatDuration } from "../data.js";

export function Verify({ scenario, event, index, onEvent, onToast, answer, onAsk }) {
  const claims = scenario.events.filter((item) => item.evidence.length > 0);
  const verifiedCount = claims.filter((item) => item.level === "verified").length;
  return (
    <main className="verify-depth">
      <aside className="claim-list">
        <header>
          <span>全部主张与证据</span>
          <small>{claims.length} claims</small>
        </header>
        {claims.map((item) => {
          const eventIndex = scenario.events.indexOf(item);
          return (
            <button key={item.id} className={eventIndex === index ? "active" : ""} onClick={() => onEvent(eventIndex)}>
              <time>{formatDuration(item.actual)}</time>
              <span><EventIcon type={item.type} size={15} /></span>
              <span><strong>{item.label}</strong><LevelMark level={item.level} compact /></span>
            </button>
          );
        })}
      </aside>
      <section className="verify-workspace">
        <div className="verify-title">
          <div><span>核验当前主张</span><h1>{event.label}</h1></div>
          <button className="quiet-button" onClick={() => onToast("已打开完整证据表：所有字段均保留当前筛选。") }><FileText size={15} />全部证据</button>
        </div>
        <ArtifactStage event={event} compact />
        <div className="verify-chain">
          <header><span>证据链</span><small>Only explicit relationships</small></header>
          <div className="chain-flow">
            <div><span><FileText size={17} /></span><small>Claim</small><strong>{event.label}</strong></div>
            <i />
            <div><span><LockKey size={17} /></span><small>Evidence</small><strong>{event.evidence.length} direct items</strong></div>
            <i />
            <div className={event.level === "verified" ? "verified" : "attention"}>
              <span>{event.level === "verified" ? <SealCheck size={17} /> : <WarningCircle size={17} />}</span>
              <small>Disposition</small>
              <strong>{event.level === "verified" ? "Verified" : "Boundary shown"}</strong>
            </div>
          </div>
        </div>
      </section>
      <div className="verify-side">
        <section className="receipt-card">
          <header><SealCheck size={21} weight="duotone" /><span>Replay evidence receipt</span></header>
          <div className="receipt-score"><strong>{verifiedCount}/{claims.length}</strong><span>claims directly verified</span></div>
          <dl>
            <div><dt>Task</dt><dd>{scenario.shortTitle}</dd></div>
            <div><dt>Sources</dt><dd>{scenario.source}</dd></div>
            <div><dt>Integrity</dt><dd>Ledger sealed</dd></div>
            <div><dt>Redactions</dt><dd>2 policy fields</dd></div>
          </dl>
          <button className="primary-button" onClick={() => onToast("审计凭证已导出为 HTML + JSON 证据包。") }><Export size={15} />导出凭证</button>
          <small><CheckCircle size={12} weight="fill" /> Narratives remain separate from evidence</small>
        </section>
        <EvidenceDrawer event={event} expanded onToast={onToast} answer={answer} onAsk={onAsk} />
      </div>
    </main>
  );
}

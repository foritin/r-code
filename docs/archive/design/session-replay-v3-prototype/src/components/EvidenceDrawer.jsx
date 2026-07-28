import {
  ArrowRight,
  ChatCircleText,
  CheckCircle,
  Copy,
  Export,
  Hash,
  Link,
  LockKey,
  Question,
  X,
} from "@phosphor-icons/react";
import { LevelMark } from "./ui.jsx";

export function EvidenceDrawer({ event, expanded = false, onClose, onVerify, onToast, answer, onAsk }) {
  const submit = (formEvent) => {
    formEvent.preventDefault();
    const value = new FormData(formEvent.currentTarget).get("question")?.toString().trim();
    if (value) onAsk(value);
  };
  return (
    <aside className={`evidence-drawer ${expanded ? "expanded" : ""}`} aria-label="Evidence for selected moment">
      <header className="drawer-header">
        <div>
          <span>当前主张</span>
          <LevelMark level={event.level} compact />
        </div>
        {onClose ? <button className="icon-button" onClick={onClose} aria-label="Close evidence"><X size={16} /></button> : null}
      </header>
      <div className="claim-copy">
        <strong>{event.label}</strong>
        <p>{event.detail}</p>
      </div>
      <section className="evidence-section">
        <div className="section-label"><span>直接证据</span><small>{event.evidence.length}</small></div>
        <div className="evidence-items">
          {event.evidence.map((item, index) => (
            <button key={item} onClick={() => onToast(`已定位证据：${item}`)}>
              <span><CheckCircle size={15} weight={event.level === "verified" ? "fill" : "regular"} /></span>
              <span><strong>{item}</strong><small>Evidence {String(index + 1).padStart(2, "0")}</small></span>
              <ArrowRight size={14} />
            </button>
          ))}
        </div>
      </section>
      <section className="evidence-section provenance-section">
        <div className="section-label"><span>来源与完整性</span></div>
        <dl>
          <div><dt><Hash size={14} /> Integrity</dt><dd>{event.hash}</dd></div>
          <div><dt><LockKey size={14} /> Reversibility</dt><dd>{event.reversible}</dd></div>
          <div><dt><Link size={14} /> Source</dt><dd>{event.app}</dd></div>
        </dl>
      </section>
      {event.relations?.length ? (
        <section className="evidence-section relation-section">
          <div className="section-label"><span>明确关系</span></div>
          {event.relations.map((relation) => (
            <button key={`${relation.type}-${relation.value}`} onClick={() => onToast(`关系已由事件 ID 支持：${relation.type}`)}>
              <small>{relation.type}</small><strong>{relation.value}</strong><ArrowRight size={13} />
            </button>
          ))}
        </section>
      ) : null}
      {event.level === "inferred" || event.level === "observed" ? (
        <div className="boundary-note">
          <Question size={16} />
          <p>{event.level === "inferred" ? "这是引用事实生成的叙事，不是 Agent 的隐藏推理。" : "这一步由终端或文件系统观察，记录无法确认应用内部语义。"}</p>
        </div>
      ) : null}
      <form className="ask-replay" onSubmit={submit}>
        <label htmlFor="ask-input"><ChatCircleText size={16} /> 询问这段回放</label>
        <div>
          <input id="ask-input" name="question" placeholder="为什么发生这一步？" autoComplete="off" />
          <button type="submit" aria-label="Ask replay"><ArrowRight size={16} /></button>
        </div>
      </form>
      {answer ? (
        <div className="replay-answer">
          <strong>基于记录的回答</strong>
          <p>{answer}</p>
          <small><Link size={12} /> 引用 {event.id} · {event.evidence[0]}</small>
        </div>
      ) : null}
      <footer className="drawer-actions">
        <button className="quiet-button" onClick={() => onToast("证据引用已复制。") }><Copy size={15} />复制引用</button>
        {onVerify ? <button className="primary-button" onClick={onVerify}><Export size={15} />进入核验</button> : null}
      </footer>
    </aside>
  );
}

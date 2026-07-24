import {
  ArrowRight,
  CalendarBlank,
  Check,
  CheckCircle,
  Envelope,
  FileText,
  Globe,
  Paperclip,
  ShieldCheck,
  Table,
  TerminalWindow,
  WarningCircle,
} from "@phosphor-icons/react";
import { AppBadge, EventIcon, LevelMark } from "./ui.jsx";

function EmptyArtifact({ event }) {
  return (
    <div className="empty-artifact">
      <span className={`hero-event-icon status-${event.status}`}>
        <EventIcon type={event.type} size={34} weight="duotone" />
      </span>
      <AppBadge app={event.app} />
      <h2>{event.label}</h2>
      <p>{event.detail}</p>
      <LevelMark level={event.level} />
    </div>
  );
}

function DocumentArtifact({ artifact }) {
  const highlight = (body) => {
    const hits = artifact.after?.highlights ?? [];
    if (!hits.length) return body;
    const parts = body.split(new RegExp(`(${hits.join("|")})`, "gi"));
    return parts.map((part, index) =>
      hits.some((hit) => hit.toLowerCase() === part.toLowerCase()) ? <mark key={index}>{part}</mark> : part,
    );
  };
  return (
    <div className="document-compare">
      <article className="paper-sheet before-paper">
        <header>
          <FileText size={16} />
          <span>之前 · 版本 2</span>
        </header>
        <small>{artifact.title}</small>
        <h3>{artifact.before.heading}</h3>
        <p>{artifact.before.body}</p>
        <footer>Draft · Internal</footer>
      </article>
      <div className="compare-arrow"><ArrowRight size={22} /></div>
      <article className="paper-sheet after-paper">
        <header>
          <CheckCircle size={16} weight="fill" />
          <span>之后 · 版本 3</span>
        </header>
        <small>{artifact.title}</small>
        <h3>{artifact.after.heading}</h3>
        <p>{highlight(artifact.after.body)}</p>
        <footer>Saved · Evidence linked</footer>
      </article>
    </div>
  );
}

function SheetArtifact({ artifact }) {
  return (
    <div className="sheet-artifact">
      <header>
        <span><Table size={18} weight="duotone" /> {artifact.title}</span>
        <small>Formula check passed</small>
      </header>
      <table>
        <thead>
          <tr>{artifact.columns.map((column) => <th key={column}>{column}</th>)}</tr>
        </thead>
        <tbody>
          {artifact.rows.map((row, rowIndex) => (
            <tr key={row.join("-")}>
              {row.map((cell, cellIndex) => (
                <td key={`${cell}-${cellIndex}`} className={artifact.changed.includes(`${rowIndex}-${cellIndex}`) ? "changed" : ""}>
                  {cell}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      <footer><CheckCircle size={15} weight="fill" /> Changed cells retain before/after versions</footer>
    </div>
  );
}

function WebArtifact({ artifact }) {
  return (
    <article className="web-artifact">
      <header>
        <Globe size={17} />
        <span>Captured source</span>
        <small>Snapshot immutable</small>
      </header>
      <div className="browser-address">research.example/report/q2-enterprise-ai</div>
      <div className="web-copy">
        <small>{artifact.source}</small>
        <h2>{artifact.title}</h2>
        <blockquote>{artifact.excerpt}</blockquote>
        <footer><CheckCircle size={15} weight="fill" /> {artifact.citation}</footer>
      </div>
    </article>
  );
}

function ClaimArtifact({ artifact }) {
  return (
    <article className="claim-artifact">
      <header><ShieldCheck size={20} weight="duotone" /> {artifact.title}</header>
      <blockquote>{artifact.claim}</blockquote>
      <div className="citation-stack">
        {artifact.citations.map((citation, index) => <span key={citation}><b>{index + 1}</b>{citation}</span>)}
      </div>
      <footer><WarningCircle size={16} /> {artifact.limitation}</footer>
    </article>
  );
}

function EmailArtifact({ artifact }) {
  return (
    <article className="email-artifact">
      <header>
        <Envelope size={18} weight="duotone" />
        <span>{artifact.state}</span>
      </header>
      <div className="email-meta"><span>To</span><strong>{artifact.to}</strong></div>
      <h2>{artifact.title}</h2>
      <p>{artifact.body}</p>
      <div className="attachment-row">
        {artifact.attachments.map((file) => <span key={file}><Paperclip size={14} />{file}</span>)}
      </div>
    </article>
  );
}

function CalendarArtifact({ artifact }) {
  return (
    <article className="calendar-artifact">
      <div className="calendar-date"><span>JUL</span><strong>23</strong></div>
      <div>
        <small>{artifact.state}</small>
        <h2>{artifact.title}</h2>
        <p>{artifact.date}</p>
        <div className="people-row">{artifact.people.map((person) => <span key={person}>{person}</span>)}</div>
      </div>
      <CalendarBlank size={30} weight="duotone" />
    </article>
  );
}

function ApprovalArtifact({ artifact }) {
  return (
    <article className="approval-artifact">
      <header><ShieldCheck size={22} weight="duotone" /><span>{artifact.title}</span></header>
      <strong className="approval-decision"><CheckCircle size={22} weight="fill" />{artifact.decision}</strong>
      <div className="approval-amount">{artifact.amount}</div>
      <dl>
        <div><dt>Approved by</dt><dd>{artifact.approver}</dd></div>
        <div><dt>Policy / receipt</dt><dd>{artifact.policy}</dd></div>
      </dl>
    </article>
  );
}

function TerminalArtifact({ artifact }) {
  return (
    <article className="terminal-artifact">
      <header><TerminalWindow size={17} />{artifact.title}<span>OBSERVED</span></header>
      <pre>{artifact.lines.map((line, index) => <code key={line} className={index === artifact.lines.length - 1 ? "warn" : ""}>{line}{"\n"}</code>)}</pre>
      <footer>Terminal pixels do not prove application state.</footer>
    </article>
  );
}

function VerificationArtifact({ artifact }) {
  return (
    <article className="verification-artifact">
      <header><CheckCircle size={20} weight="duotone" />{artifact.title}</header>
      <div className="check-list">
        {artifact.checks.map((check) => (
          <div key={check.label} className={check.ok ? "ok" : "attention"}>
            <span>{check.ok ? <Check size={15} weight="bold" /> : <WarningCircle size={16} />}</span>
            <strong>{check.label}</strong>
            <small>{check.note ?? "Passed"}</small>
          </div>
        ))}
      </div>
    </article>
  );
}

export function ArtifactStage({ event, compact = false }) {
  const artifact = event.artifact;
  let body = <EmptyArtifact event={event} />;
  if (artifact?.kind === "document") body = <DocumentArtifact artifact={artifact} />;
  if (artifact?.kind === "sheet") body = <SheetArtifact artifact={artifact} />;
  if (artifact?.kind === "web") body = <WebArtifact artifact={artifact} />;
  if (artifact?.kind === "claim") body = <ClaimArtifact artifact={artifact} />;
  if (artifact?.kind === "email") body = <EmailArtifact artifact={artifact} />;
  if (artifact?.kind === "calendar") body = <CalendarArtifact artifact={artifact} />;
  if (artifact?.kind === "approval") body = <ApprovalArtifact artifact={artifact} />;
  if (artifact?.kind === "terminal") body = <TerminalArtifact artifact={artifact} />;
  if (artifact?.kind === "verification") body = <VerificationArtifact artifact={artifact} />;

  return (
    <section className={`artifact-stage ${compact ? "compact" : ""}`} aria-label={`Artifact for ${event.label}`}>
      <div className="stage-heading">
        <div>
          <EventIcon type={event.type} size={15} weight="duotone" />
          <strong>{event.label}</strong>
        </div>
        <div><AppBadge app={event.app} /><LevelMark level={event.level} compact /></div>
      </div>
      <div className={`artifact-canvas artifact-${artifact?.kind ?? "empty"}`}>{body}</div>
    </section>
  );
}

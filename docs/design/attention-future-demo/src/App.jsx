import { useEffect, useRef, useState } from 'react';
import {
  ArrowCounterClockwise,
  Bell,
  BracketsCurly,
  CaretDown,
  CaretLeft,
  CaretRight,
  Check,
  CheckCircle,
  ChatCircleDots,
  ClockCounterClockwise,
  Copy,
  FileCode,
  FileText,
  Folder,
  GearSix,
  GitBranch,
  GitPullRequest,
  Globe,
  House,
  ListMagnifyingGlass,
  LockKey,
  MagnifyingGlass,
  Paperclip,
  Pause,
  Play,
  Question,
  Robot,
  Rows,
  ShieldCheck,
  SidebarSimple,
  TerminalWindow,
  UsersThree,
  Warning,
  X,
  XCircle,
} from '@phosphor-icons/react';
import {
  agents,
  attentionRows,
  DEMO_DURATION,
  DIRECTIONS,
  SCENES,
  workingRows,
} from './demoData.js';

const statusLabels = {
  running: 'Running',
  working: 'Working',
  needs_input: 'Needs input',
  needs_permission: 'Needs permission',
  completed: 'Completed',
  failed: 'Failed',
  unread: 'Unread',
  paused: 'Paused',
};

function Status({ state, label = true }) {
  return (
    <span className={`status status-${state}`}>
      <span className="status-mark" aria-hidden="true" />
      {label && <span>{statusLabels[state]}</span>}
    </span>
  );
}

function AppTopbar({ direction, sceneIndex, onNext }) {
  const title = sceneIndex === 0
    ? 'Home'
    : direction === 'command'
    ? 'Attention'
    : direction === 'rooms'
      ? 'Fix flaky compiler tests'
      : 'compiler-lab';

  return (
    <div className="app-topbar">
      <div className="traffic-lights" aria-hidden="true"><i /><i /><i /></div>
      <div className="app-brand">Charter</div>
      <div className="top-divider" />
      <strong className="app-title">{title}</strong>
      {direction === 'terminal' && sceneIndex > 0 && (
        <div className="workspace-meta">
          <span><Folder /> ~/Projects/compiler-lab</span>
          <span><GitBranch /> main</span>
          <span><GitPullRequest /> PR #184</span>
          <span><Globe /> localhost:4173 <i className="online-dot" /></span>
        </div>
      )}
      {direction === 'rooms' && sceneIndex === 1 && <span className="top-restored">Session ready · restored context</span>}
      <div className="top-spacer" />
      <button className="next-attention" onClick={onNext} type="button">Next attention <kbd>⌘⇧]</kbd></button>
      <button className="top-icon" aria-label="Search" type="button"><MagnifyingGlass /></button>
      <button className="top-icon has-alert" aria-label="Notifications" type="button"><Bell /></button>
      <div className="avatar" aria-label="Avery Morgan">AM</div>
    </div>
  );
}

const mainProjects = [
  ['compiler-lab', 'working'],
  ['api-gateway', 'needs_input'],
  ['web-dashboard', 'failed'],
  ['docs-site', 'unread'],
];

const sessionPrompt = 'Investigate the intermittent compiler test failures on CI, fix the root cause, and open a review-ready PR.';

const homeDirectionCopy = {
  command: {
    title: 'Attention returns to Home',
    detail: 'The session opens in the shared Attention view. New checkpoints return to Home and Inbox.',
  },
  rooms: {
    title: 'A durable Task Room opens',
    detail: 'The conversation, agent events, approvals, layout and terminal history stay together.',
  },
  terminal: {
    title: 'The Terminal Workbench opens',
    detail: 'The session lands beside code with semantic prompts, search and exact attention jumps.',
  },
};

function LeftRail({ direction, sceneIndex }) {
  if (direction === 'terminal') {
    return (
      <aside className="left-rail workspace-rail">
        <nav className="rail-nav compact">
          <button type="button" aria-label="Home"><House /></button>
          <button className="active" type="button" aria-label="Files"><Folder /></button>
          <button type="button" aria-label="Source control"><GitBranch /></button>
          <button type="button" aria-label="Search"><MagnifyingGlass /></button>
        </nav>
        <div className="rail-content terminal-tree">
          <div className="rail-heading"><span>Workspace</span><SidebarSimple /></div>
          <div className="tree-project"><CaretDown /> compiler-lab <Status state="working" label={false} /></div>
          <div className={`tree-agent ${sceneIndex === 3 ? 'selected attention-halo' : ''}`}>
            <Status state={sceneIndex === 3 ? 'needs_permission' : sceneIndex === 4 ? 'completed' : 'working'} label={false} />
            <span>Codex 7f3a…</span>
            <small>{sceneIndex === 3 ? 'Needs permission' : sceneIndex === 4 ? 'Completed' : 'Working'}</small>
          </div>
          <div className="tree-agent"><Status state="working" label={false} /><span>Planner</span><small>Working</small></div>
          <div className="tree-agent"><Status state="unread" label={false} /><span>Docs writer</span><small>Unread</small></div>
          <div className="rail-heading section"><span>Project</span></div>
          {[
            ['src', Folder], ['tests', Folder], ['bench', Folder], ['package.json', BracketsCurly],
            ['README.md', FileText], ['tsconfig.json', BracketsCurly],
          ].map(([name, Icon]) => <div className="file-row" key={name}><Icon /><span>{name}</span></div>)}
          <div className="rail-heading section"><span>Terminals</span></div>
          <div className="terminal-row selected"><Status state={sceneIndex === 3 ? 'needs_permission' : 'working'} label={false} /> codex 7f3a…</div>
          <div className="terminal-row"><Status state="working" label={false} /> node dev</div>
        </div>
      </aside>
    );
  }

  return (
    <aside className="left-rail">
      <nav className="rail-nav">
        <button className={direction === 'home' ? 'active' : ''} type="button"><House /> Home</button>
        <button className={direction === 'command' ? 'active' : ''} type="button"><Bell /> Inbox <span className="count-badge">3</span></button>
        <button type="button"><MagnifyingGlass /> Search</button>
        <button type="button"><ClockCounterClockwise /> Timeline</button>
        <button type="button"><GearSix /> Settings</button>
      </nav>
      <div className="rail-section-title">Projects <span>+</span></div>
      <div className="project-list">
        {mainProjects.map(([name, state], index) => (
          <div className={`project-row ${index === 0 ? 'selected' : ''}`} key={name}>
            <Folder /><span>{name}</span><Status state={state} label={false} />
          </div>
        ))}
      </div>
      {direction === 'rooms' && (
        <div className="room-list">
          <div className="rail-section-title">Task rooms</div>
          <div className="room-row selected"><span>Fix flaky compiler tests</span><Status state={sceneIndex >= 4 ? 'completed' : sceneIndex === 3 ? 'needs_permission' : 'needs_input'} label={false} /></div>
          <div className="room-row"><span>Add incremental parsing</span><Status state="completed" label={false} /></div>
          <div className="room-row"><span>Refactor AST cache</span><Status state="needs_permission" label={false} /></div>
        </div>
      )}
      <div className="rail-footer"><ClockCounterClockwise /> Closed {direction === 'rooms' ? 'room' : 'session'} history</div>
    </aside>
  );
}

function SectionHeading({ children, count, detail }) {
  return (
    <div className="section-heading">
      <strong>{children}</strong>
      {count !== undefined && <span className="section-count">{count}</span>}
      {detail && <small>{detail}</small>}
    </div>
  );
}

function HomeSurface({ direction, time, draft, draftTouched, onDraft, onStart }) {
  const typeProgress = Math.max(0, Math.min(1, (time - 0.35) / 4.15));
  const demoPrompt = sessionPrompt.slice(0, Math.floor(sessionPrompt.length * typeProgress));
  const visiblePrompt = draftTouched ? draft : demoPrompt;

  return (
    <section className="home-surface surface-enter">
      <div className="home-hero">
        <h2>What should we build?</h2>
        <p>Start a Chat Session. Charter keeps the conversation, workspace and Agent state together.</p>
      </div>
      <div className={`home-composer ${time >= 4.7 ? 'ready' : ''}`}>
        <div className="home-chiprow">
          <button type="button"><Folder /> compiler-lab <CaretDown /></button>
          <button type="button"><Robot /> Codex <CaretDown /></button>
          <button type="button"><ShieldCheck /> Review commands <CaretDown /></button>
        </div>
        <textarea
          aria-label="Chat Session request"
          onChange={(event) => onDraft(event.target.value)}
          placeholder="Describe a task, ask a question, or paste an error…"
          value={visiblePrompt}
        />
        <div className="home-composer-footer">
          <button className="home-attach" type="button"><Paperclip /> Add context</button>
          <span><GitBranch /> main</span>
          <span><Folder /> ~/Projects/compiler-lab</span>
          <button className="home-start" disabled={!visiblePrompt.trim()} onClick={onStart} type="button">
            Start Chat Session <CaretRight weight="bold" />
          </button>
        </div>
      </div>
      <div className="home-mission">
        <div className="home-section">
          <div className="home-section-head"><div><strong>Needs you now</strong><span>3 checkpoints across projects</span></div><button type="button">Open Inbox</button></div>
          <div className="home-attention-list">
            {attentionRows.slice(0, 2).map((item) => (
              <button className="home-attention-row" key={item.id} type="button">
                <Status state={item.status} label={false} />
                <span><strong>{item.title}</strong><small>{item.project} · {item.subtitle}</small></span>
                <Status state={item.status} />
                <CaretRight />
              </button>
            ))}
          </div>
        </div>
        <div className="home-section">
          <div className="home-section-head"><div><strong>Running & restorable</strong><span>Native sessions keep cwd and scrollback</span></div><button type="button">View all</button></div>
          <div className="home-running-list">
            <div><Status state="working" label={false} /><span><strong>Compiler fix session</strong><small>compiler-lab · native-c3d4</small></span><time>14m</time></div>
            <div><Status state="working" label={false} /><span><strong>API contract refactor</strong><small>api-gateway · native-8d1b</small></span><time>22m</time></div>
            <div><Status state="completed" label={false} /><span><strong>Dashboard UX polish</strong><small>web-dashboard · restorable</small></span><time>1h</time></div>
          </div>
        </div>
      </div>
      <div className="home-launch-cue" aria-hidden="true">
        <ChatCircleDots weight="fill" />
        <span><strong>{homeDirectionCopy[direction].title}</strong><small>{homeDirectionCopy[direction].detail}</small></span>
      </div>
    </section>
  );
}

function HomeContext({ direction }) {
  return (
    <aside className="context-panel home-context inspector-enter">
      <div className="context-kicker">New Chat Session</div>
      <h3>Session setup</h3>
      <p className="context-lead">Your first message creates a durable session. Agent work can continue without replacing Home as the product entry.</p>
      <div className="context-section key-values">
        <strong>Workspace context</strong>
        <dl>
          <dt>Project</dt><dd>compiler-lab</dd>
          <dt>CWD</dt><dd>~/Projects/compiler-lab</dd>
          <dt>Branch</dt><dd>main</dd>
          <dt>Agent</dt><dd>Codex</dd>
          <dt>Gateway</dt><dd>Review commands <ShieldCheck /></dd>
        </dl>
      </div>
      <div className="context-section home-next-step">
        <strong>After you start</strong>
        <div><ChatCircleDots weight="fill" /><span><b>{homeDirectionCopy[direction].title}</b><small>{homeDirectionCopy[direction].detail}</small></span></div>
      </div>
      <div className="context-section">
        <strong>Recovered automatically</strong>
        <div className="home-recovery-list"><span><Check /> Native session ID</span><span><Check /> CWD and branch</span><span><Check /> Layout and scrollback</span><span><Check /> Attention history</span></div>
      </div>
      <button className="secondary-action full" type="button"><ClockCounterClockwise /> Browse closed sessions</button>
    </aside>
  );
}

function AttentionRow({ item, selected, nested = false }) {
  const Icon = item.status === 'needs_permission'
    ? ShieldCheck
    : item.status === 'needs_input'
      ? Question
      : item.status === 'unread'
        ? CheckCircle
        : item.status === 'failed'
          ? XCircle
          : Robot;
  return (
    <div className={`attention-row ${selected ? 'selected attention-halo' : ''} ${nested ? 'nested' : ''}`}>
      <span className="row-time">{item.time}</span>
      <span className={`state-icon state-${item.status}`}><Icon weight="bold" /></span>
      <span className="row-copy"><strong>{item.title}</strong><small>{item.subtitle}</small></span>
      <span className="row-session">{item.session}</span>
      <span className="row-project">{item.project}</span>
      <Status state={item.status} />
    </div>
  );
}

function SessionTable({ compact = false }) {
  const rows = [
    ['Compiler fix session', '~/work/compiler-lab', 'main', '5173', 'native-c3d4', 'Running'],
    ['API contract refactor', '~/work/api-gateway', 'feature/api-v2', '4000', 'native-8d1b', 'Running'],
    ['Dashboard UX polish', '~/work/web-dashboard', 'ui/fixes', '3000', 'native-5f9e', 'Failed'],
    ['Data pipeline backfill', '~/work/data-pipeline', 'main', '5432', 'native-2b7d', 'Completed'],
  ];
  return (
    <div className={`session-table ${compact ? 'compact' : ''}`}>
      <div className="session-tools"><strong>Running & restorable sessions</strong><div className="session-search"><MagnifyingGlass /> Search sessions</div><button type="button"><Rows /> Restore layout</button></div>
      <div className="session-head"><span>Session</span><span>CWD</span><span>Branch</span><span>Port</span><span>Native session ID</span><span>State</span></div>
      {rows.map((row) => (
        <div className="session-data" key={row[0]}>
          {row.slice(0, 5).map((cell) => <span key={cell}>{cell}</span>)}
          <Status state={row[5].toLowerCase()} />
        </div>
      ))}
    </div>
  );
}

function AgentCluster() {
  return (
    <div className="agent-cluster">
      {agents.map((agent, index) => (
        <div className="agent-line" key={agent.name} style={{ '--delay': `${index * 90}ms` }}>
          <span className="agent-node"><Robot weight="fill" /></span>
          <span><strong>{agent.name}</strong><small>{agent.detail}</small></span>
          <Status state={agent.status} />
        </div>
      ))}
    </div>
  );
}

function AttentionSurface({ sceneIndex }) {
  const selectedId = sceneIndex === 1 ? 'input' : sceneIndex === 4 ? 'unread' : 'permission';
  return (
    <section className="attention-surface surface-enter" key={`attention-${sceneIndex}`}>
      <div className="surface-title"><div><h2>Attention</h2><p>Only the moments that need you.</p></div><span>Today · Jul 15</span></div>
      <SectionHeading count={3} detail="Action required">Needs you now</SectionHeading>
      <div className="attention-list">{attentionRows.map((item) => <AttentionRow item={item} selected={item.id === selectedId} key={item.id} />)}</div>
      <SectionHeading count={2} detail="In progress">Still working</SectionHeading>
      <div className="attention-list">
        {workingRows.map((item, index) => (
          <div key={item.id}>
            <AttentionRow item={item} selected={sceneIndex === 2 && index === 1} />
            {sceneIndex === 2 && index === 1 && <AgentCluster />}
          </div>
        ))}
      </div>
      {sceneIndex >= 4 && (
        <>
          <SectionHeading count={2}>Recently finished</SectionHeading>
          <div className="attention-list compact-list">
            <AttentionRow item={{ id: 'failed', time: '10:35', title: 'Type errors in user.service.ts', subtitle: 'Failed after 3m', project: 'web-dashboard', session: 'native-5f9e', status: 'failed' }} />
            <AttentionRow item={{ id: 'done', time: '10:12', title: 'Backfill 3 days of events', subtitle: 'Completed in 18m', project: 'data-pipeline', session: 'native-2b7d', status: 'completed' }} />
          </div>
        </>
      )}
      <SessionTable compact={sceneIndex < 4} />
    </section>
  );
}

function PermissionInspector({ selected = 'permission', onApprove, approved }) {
  if (selected === 'unread') {
    return (
      <aside className="context-panel inspector-enter">
        <div className="context-kicker">Unread completion</div><h3>PR #124 opened</h3>
        <p className="context-lead">Docs writer completed the task and opened a review-ready pull request.</p>
        <div className="summary-block"><CheckCircle weight="fill" /><span><strong>6 files changed</strong><small>Tests and verification passed</small></span></div>
        <button className="primary-action full" type="button">Open review</button>
        <button className="secondary-action full" type="button">Mark as read</button>
      </aside>
    );
  }
  if (selected === 'input') {
    return (
      <aside className="context-panel inspector-enter">
        <div className="context-kicker">Input requested · API contract refactor</div>
        <h3>Which API version should we target?</h3>
        <p className="context-lead">The agent found an incompatible response shape and paused at the exact decision point.</p>
        <div className="choice-stack"><button className="choice selected" type="button"><Check /> Target stable v2</button><button className="choice" type="button">Keep v1 compatibility</button></div>
        <button className="primary-action full" type="button">Reply and continue</button>
        <div className="prompt-footer"><CaretLeft /> Previous prompt <span>2 of 4</span> Next prompt <CaretRight /></div>
      </aside>
    );
  }
  return (
    <aside className="context-panel inspector-enter">
      <div className="context-kicker">Permission request · External Claude session</div>
      <h3>{approved ? 'Command approved once' : 'Allow npm test in /compiler-lab?'}</h3>
      <Status state={approved ? 'completed' : 'needs_permission'} />
      <p className="context-lead">{approved ? 'The exact command is running. No broader permission was granted.' : 'Approval is required by your workspace policy.'}</p>
      <div className="context-section"><strong>Exact command</strong><code><span>10:42:18</span>$ npm test</code></div>
      <div className="context-section key-values">
        <strong>Gateway & scope</strong>
        <dl><dt>via</dt><dd>Charter Gateway <ShieldCheck /></dd><dt>CWD</dt><dd>/compiler-lab</dd><dt>Policy</dt><dd>Workspace · Allow once</dd><dt>Not allowed</dt><dd>read screen, send keys</dd></dl>
      </div>
      <div className="mini-terminal"><span>10:41:52  $ git status</span><span>10:42:05  $ npm test -- --watch</span><em>No tests found, exiting with code 1</em></div>
      <button className="text-link" type="button"><MagnifyingGlass /> Search terminal scrollback…</button>
      <div className="decision-actions">
        <button className="primary-action" onClick={onApprove} type="button">{approved ? <><Check /> Approved</> : <><ShieldCheck /> Approve once</>}</button>
        <button className="secondary-action" type="button">Deny</button>
      </div>
      <div className="prompt-footer"><CaretLeft /> Previous prompt <span>1 of 3</span> Next prompt <CaretRight /></div>
    </aside>
  );
}

function RestoreBanner({ resumed, onResume }) {
  return (
    <div className={`restore-banner ${resumed ? 'resolved' : ''}`}>
      <ClockCounterClockwise weight="bold" />
      <span><strong>{resumed ? 'Agents resumed from saved sessions' : 'Restored where you left off'}</strong><small>Layout, scroll position, and native agent sessions restored at 9:14 AM.</small></span>
      <button className="primary-action" onClick={onResume} type="button">{resumed ? <><Check /> Resumed</> : 'Resume agents'}</button>
      <button className="secondary-action" type="button">Keep paused</button>
      <button className="icon-only" type="button" aria-label="Dismiss"><X /></button>
    </div>
  );
}

function CommandBlock({ active = false, success = false }) {
  return (
    <div className={`command-block ${active ? 'active' : ''}`}>
      <div className="command-head"><span className="prompt-mark" /><strong>codex 7f3a…</strong><span>9:04:11 AM</span><span className="gateway"><ShieldCheck /> via Charter Gateway</span></div>
      <pre><b>$ pnpm -C compiler test --reporter=dot</b>{'\n'}........................F.....F.........F{'\n'}{success ? '✓ 845 passed, 0 failed, 12 skipped' : '3 failed, 842 passed, 12 skipped'}</pre>
      <button type="button" className="select-output">Select whole output</button>
    </div>
  );
}

function SemanticTerminal({ compact = false, success = false, sceneIndex = 0 }) {
  const permission = sceneIndex === 3;
  return (
    <div className={`semantic-terminal ${compact ? 'compact' : ''}`}>
      <div className="terminal-tabs"><span className="active"><TerminalWindow /> codex 7f3a… <Status state={permission ? 'needs_permission' : success ? 'completed' : 'working'} label={false} /></span><span><TerminalWindow /> bash</span><span><Globe /> node dev</span><span className="terminal-tab-spacer" /><button type="button"><ClockCounterClockwise /> Closed terminals</button></div>
      {!compact && <div className="restore-strip"><span>Restored · codex 7f3a…</span><span>~/Projects/compiler-lab</span><span>Last active 11:42 AM</span><button type="button">Resume agent</button></div>}
      <div className="terminal-search"><div><MagnifyingGlass /> Search output <span>4 matches</span></div><button type="button"><CaretLeft /> Previous prompt</button><button type="button">Next prompt <CaretRight /></button><span className="read-only"><LockKey /> Read-only</span></div>
      <div className="terminal-output">
        <div className="semantic-block"><div className="semantic-head"><span className="prompt-mark" /> codex 7f3a… <time>11:32:18</time><Copy /></div><pre><b>$ git status --sb</b>{'\n'}## main...origin/main{'\n'} M src/ir.ts{'\n'} M src/optimizer.ts</pre></div>
        <div className={`semantic-block ${sceneIndex >= 2 ? 'highlighted' : ''}`}><div className="semantic-head"><span className="prompt-mark" /> codex 7f3a… <time>11:34:09</time><span className="duration">02:14</span></div><pre><b>$ <mark>npm test</mark></b>{'\n'}&gt; compiler-lab@0.1.0 test{'\n'}&gt; vitest run{'\n'}{success ? '✓ 845 tests passed · PR #184 opened' : permission ? '? Charter Gateway needs permission to run this command.\n  Waiting for your response…' : '✓ 842 passed · 3 skipped'}</pre>{permission && <div className="terminal-state"><Status state="needs_permission" /> Waiting for permission</div>}</div>
      </div>
    </div>
  );
}

function RoomTimeline({ sceneIndex, approved, onApprove, resumed, onResume }) {
  return (
    <section className="room-surface surface-enter" key={`room-${sceneIndex}`}>
      <RestoreBanner resumed={resumed} onResume={onResume} />
      <div className="room-date">Today, July 15, 2026</div>
      <div className="event-stream">
        <div className="event"><time>9:02 AM</time><span className="event-dot" /><div className="event-card"><div className="event-author">You</div><p>We’re seeing intermittent failures in the compiler tests on CI. Investigate and propose a fix.</p></div></div>
        <div className="event"><time>9:03 AM</time><span className="event-dot purple" /><div className="event-card"><div className="event-author">Claude 9f3a… <small>reasoning</small></div><p>Analyzing parser recovery and incremental cache invalidation.</p></div></div>
        <div className="event command-event"><time>9:04 AM</time><span className="event-dot green" /><CommandBlock active={sceneIndex === 0} success={sceneIndex === 4} /></div>
        {sceneIndex >= 2 && agents.map((agent, index) => (
          <div className="event agent-event" key={agent.name} style={{ '--delay': `${index * 90}ms` }}><time>{agent.time} AM</time><span className="event-dot blue" /><div className="event-card"><div className="event-author">{agent.name} <small>sub-agent</small></div><p>{agent.detail}</p></div></div>
        ))}
        {sceneIndex === 1 && (
          <div className="event checkpoint attention-halo"><time>9:14 AM</time><span className="event-dot blue focus" /><div className="checkpoint-card"><Status state="needs_input" /><h3>Which API version should we target?</h3><div className="checkpoint-actions"><button type="button">Target stable v2</button><button type="button">Keep v1 compatibility</button></div><div className="reply-line">Reply to continue <CaretRight /></div></div></div>
        )}
        {sceneIndex === 3 && (
          <div className="event checkpoint attention-halo"><time>9:18 AM</time><span className="event-dot orange focus" /><div className="checkpoint-card"><Status state={approved ? 'completed' : 'needs_permission'} /><h3>{approved ? 'npm test approved and running' : 'Allow npm test in /compiler-lab?'}</h3><code>$ npm test · cwd /compiler-lab · via Charter Gateway</code><div className="checkpoint-actions"><button onClick={onApprove} type="button">{approved ? 'Approved once' : 'Approve once'}</button><button type="button">Deny</button></div></div></div>
        )}
        {sceneIndex >= 4 && (
          <div className="event checkpoint complete-checkpoint"><time>9:21 AM</time><span className="event-dot green focus" /><div className="checkpoint-card"><Status state="unread" /><h3>Implementation complete · Review ready</h3><p>6 files changed · 845 tests passed · PR #184 opened</p><div className="checkpoint-actions"><button type="button">Open review</button><button type="button">Mark as read</button></div></div></div>
        )}
      </div>
      <SemanticTerminal compact success={sceneIndex >= 4} sceneIndex={sceneIndex} />
    </section>
  );
}

function SessionContext({ sceneIndex }) {
  return (
    <aside className="context-panel room-context inspector-enter">
      <div className="context-kicker">Session context</div><h3>Fix flaky compiler tests</h3>
      <div className="key-values"><dl><dt>Status</dt><dd><Status state={sceneIndex >= 4 ? 'unread' : sceneIndex === 3 ? 'needs_permission' : sceneIndex === 2 ? 'working' : 'needs_input'} /></dd><dt>Room ID</dt><dd>room_01HZ6YQW…</dd><dt>Restored</dt><dd>Today, 9:14 AM</dd></dl></div>
      <div className="context-section"><strong>Agents</strong><div className="context-agent"><TerminalWindow /><span><b>codex 7f3a…</b><small>cwd ~/projects/compiler-suite</small></span><Status state={sceneIndex >= 4 ? 'completed' : 'working'} label={false} /></div><div className="context-agent"><Robot /><span><b>claude 9f3a…</b><small>cwd ~/projects/compiler-suite</small></span><Status state={sceneIndex >= 4 ? 'completed' : 'working'} label={false} /></div></div>
      <div className="context-section"><strong>Sub-agents <span className="soft-count">3</span></strong>{agents.map((agent) => <div className="subagent-mini" key={agent.name}><Robot /><span><b>{agent.name}</b><small>{agent.detail}</small></span><Status state={agent.status} label={false} /></div>)}</div>
      <div className="context-section"><strong>Workspace</strong><div className="workspace-kv"><span><GitBranch /> feat/fix-flaky-tests</span><span><GitPullRequest /> PR #184</span><span><Folder /> ~/projects/compiler-suite</span><span><Globe /> 5173</span></div></div>
      <div className="layout-thumb"><div /><div /><div /><div /><div /></div>
      <button className="text-link" type="button"><ClockCounterClockwise /> Closed room history</button>
    </aside>
  );
}

function CodeEditor({ sceneIndex }) {
  return (
    <div className="editor-pane">
      <div className="editor-tabs"><span className="active"><FileCode /> ir.ts <X /></span><span><FileCode /> optimizer.ts <X /></span><span><FileCode /> constant-fold.ts <X /></span><span><BracketsCurly /> package.json <X /></span></div>
      <div className="breadcrumbs-row">src <CaretRight /> ir.ts <CaretRight /> foldConstants</div>
      <div className="code-area"><div className="line-numbers">142<br />143<br />144<br />145<br />146<br />147<br />148<br />149<br />150<br />151<br />152<br />153</div><pre><span className="kw">export function</span> <span className="fn">foldConstants</span>(node: Expr): Expr {'{'}{'\n'}  <span className="kw">switch</span> (node.kind) {'{'}{'\n'}    <span className="kw">case</span> <span className="str">'Binary'</span>: {'{'}{'\n'}      <span className="kw">const</span> left = <span className="fn">foldConstants</span>(node.left);{'\n'}      <span className="kw">const</span> right = <span className="fn">foldConstants</span>(node.right);{'\n'}      <span className={sceneIndex >= 2 ? 'active-code' : ''}><span className="kw">if</span> (left.kind === <span className="str">'Const'</span> &amp;&amp; right.kind === <span className="str">'Const'</span>) {'{'}</span>{'\n'}        <span className="kw">return</span> {'{'} kind: <span className="str">'Const'</span>, value: <span className="fn">evalBinary</span>(node.op, left.value, right.value) {'}'};{'\n'}      {'}'}{'\n'}      <span className="kw">return</span> {'{'} ...node, left, right {'}'};{'\n'}    {'}'}{'\n'}    <span className="kw">case</span> <span className="str">'Unary'</span>: {'{'}{'\n'}      <span className="kw">const</span> arg = <span className="fn">foldConstants</span>(node.arg);</pre></div>
      <div className="editor-status">Ln 149, Col 1 <span>Spaces: 2</span><span>UTF-8</span><span>TypeScript</span><i className="online-dot" /></div>
    </div>
  );
}

function AttentionSwitcher() {
  return (
    <div className="attention-switcher attention-halo"><div className="switcher-head"><ListMagnifyingGlass /> Next attention <kbd>⌘⇧]</kbd></div><div className="switcher-row selected"><Status state="needs_input" label={false} /><span><b>Which API version should we target?</b><small>api-gateway · exact prompt 2 of 4</small></span><CaretRight /></div><div className="switcher-row"><Status state="needs_permission" label={false} /><span><b>Allow npm test?</b><small>compiler-lab · via Gateway</small></span><CaretRight /></div></div>
  );
}

function WorkspaceSurface({ sceneIndex }) {
  return (
    <section className="workspace-surface surface-enter" key={`workspace-${sceneIndex}`}>
      <CodeEditor sceneIndex={sceneIndex} /><SemanticTerminal sceneIndex={sceneIndex} success={sceneIndex >= 4} />
      {sceneIndex === 1 && <AttentionSwitcher />}
      {sceneIndex === 2 && <div className="agent-toast"><UsersThree /><span><strong>3 sub-agents are working</strong><small>Planner finished · Test generator finished · Bench runner active</small></span><CaretRight /></div>}
      {sceneIndex >= 4 && <div className="completion-toast"><CheckCircle weight="fill" /><span><strong>Long command completed</strong><small>845 tests passed · terminal saved to history</small></span><button type="button">Open review</button></div>}
    </section>
  );
}

function WorkspaceInspector({ sceneIndex, approved, onApprove }) {
  if (sceneIndex === 3) return <PermissionInspector onApprove={onApprove} approved={approved} />;
  return (
    <aside className="context-panel workspace-context inspector-enter">
      <div className="context-kicker">Agent context</div><h3>Codex 7f3a…</h3><Status state={sceneIndex >= 4 ? 'unread' : sceneIndex === 2 ? 'working' : 'paused'} />
      <div className="key-values context-section"><dl><dt>Native session</dt><dd>codex 7f3a…</dd><dt>CWD</dt><dd>~/Projects/compiler-lab</dd><dt>Branch</dt><dd>main</dd><dt>Last active</dt><dd>11:42 AM</dd></dl></div>
      {sceneIndex === 2 && <AgentCluster />}
      {sceneIndex >= 4 && <div className="summary-block"><CheckCircle weight="fill" /><span><strong>Review ready</strong><small>PR #184 · 6 files changed</small></span></div>}
      <div className="context-section"><strong>Attention history</strong><div className="activity-item"><Status state="needs_input" label={false} /><span>API target confirmed</span><time>11:31</time></div><div className="activity-item"><Status state="needs_permission" label={false} /><span>npm test approved once</span><time>11:34</time></div><div className="activity-item"><Status state="completed" label={false} /><span>Verification passed</span><time>11:37</time></div></div>
      <button className="secondary-action full" type="button"><ClockCounterClockwise /> Open session history</button>
    </aside>
  );
}

function AppStatusbar({ direction, sceneIndex }) {
  const sessionCount = sceneIndex === 0 ? '5 sessions' : direction === 'command' ? '5 sessions' : direction === 'rooms' ? '2 native sessions' : '4 terminals';
  return <div className="app-statusbar"><span><GitBranch /> main</span><span><CheckCircle /> 0</span><span><Warning /> 0</span><span className="status-spacer" /><span>{sessionCount}</span><span>{sceneIndex >= 4 ? '1 unread completion' : sceneIndex === 0 ? '3 need you' : '6 agents'}</span><span><i className="online-dot" /> Connected</span></div>;
}

function CharterFrame({ direction, sceneIndex, time, approved, resumed, draft, draftTouched, onApprove, onResume, onNext, onDraft, onStart }) {
  const selected = sceneIndex === 1 ? 'input' : sceneIndex === 4 ? 'unread' : 'permission';
  const shellDirection = sceneIndex === 0 ? 'home' : direction;
  return (
    <div className={`charter-window product-${shellDirection}`}>
      <AppTopbar direction={direction} sceneIndex={sceneIndex} onNext={onNext} />
      <div className="charter-body">
        <LeftRail direction={shellDirection} sceneIndex={sceneIndex} />
        {sceneIndex === 0 && <HomeSurface direction={direction} draft={draft} draftTouched={draftTouched} onDraft={onDraft} onStart={onStart} time={time} />}
        {sceneIndex > 0 && direction === 'command' && <AttentionSurface sceneIndex={sceneIndex} />}
        {sceneIndex > 0 && direction === 'rooms' && <RoomTimeline sceneIndex={sceneIndex} approved={approved} onApprove={onApprove} resumed={resumed} onResume={onResume} />}
        {sceneIndex > 0 && direction === 'terminal' && <WorkspaceSurface sceneIndex={sceneIndex} />}
        {sceneIndex === 0 && <HomeContext direction={direction} />}
        {sceneIndex > 0 && direction === 'command' && <PermissionInspector selected={selected} onApprove={onApprove} approved={approved} />}
        {sceneIndex > 0 && direction === 'rooms' && <SessionContext sceneIndex={sceneIndex} />}
        {sceneIndex > 0 && direction === 'terminal' && <WorkspaceInspector sceneIndex={sceneIndex} approved={approved} onApprove={onApprove} />}
      </div>
      <AppStatusbar direction={direction} sceneIndex={sceneIndex} />
    </div>
  );
}

function DirectionTabs({ active, onChange }) {
  return (
    <div className="direction-tabs" role="tablist" aria-label="Product directions">
      {DIRECTIONS.map((direction) => (
        <button className={active === direction.id ? 'active' : ''} key={direction.id} onClick={() => onChange(direction.id)} role="tab" style={{ '--direction-accent': direction.accent }} type="button">
          <span className="direction-number">{direction.number}</span><span><strong>{direction.label}</strong><small>{direction.summary}</small></span>{active === direction.id && <Check weight="bold" />}
        </button>
      ))}
    </div>
  );
}

function DemoPlayer({ time, playing, sceneIndex, onPlayToggle, onRestart, onSeek, onScene }) {
  return (
    <div className="demo-player">
      <div className="player-caption"><span>{String(sceneIndex + 1).padStart(2, '0')}</span><div><strong>{SCENES[sceneIndex].title}</strong><p>{SCENES[sceneIndex].caption}</p></div></div>
      <div className="player-main"><div className="player-buttons"><button className="play-button" onClick={onPlayToggle} type="button" aria-label={playing ? 'Pause' : 'Play'}>{playing ? <Pause weight="fill" /> : <Play weight="fill" />}</button><button className="restart-button" onClick={onRestart} type="button" aria-label="Restart"><ArrowCounterClockwise /></button><time>{time.toFixed(1)}s / {DEMO_DURATION}s</time></div><div className="timeline-wrap"><input aria-label="Demo progress" max={DEMO_DURATION} min="0" onChange={(event) => onSeek(Number(event.target.value))} step="0.05" type="range" value={time} style={{ '--progress': `${(time / DEMO_DURATION) * 100}%` }} /><div className="scene-markers">{SCENES.map((scene, index) => <button className={index === sceneIndex ? 'active' : index < sceneIndex ? 'done' : ''} key={scene.start} onClick={() => onScene(index)} type="button" style={{ left: `${(scene.start / DEMO_DURATION) * 100}%` }}><i /><span>{scene.short}</span></button>)}</div></div></div>
    </div>
  );
}

export function App() {
  const [direction, setDirection] = useState('command');
  const [time, setTime] = useState(0);
  const [playing, setPlaying] = useState(true);
  const [approved, setApproved] = useState(false);
  const [resumed, setResumed] = useState(false);
  const [draft, setDraft] = useState(sessionPrompt);
  const [draftTouched, setDraftTouched] = useState(false);
  const lastFrame = useRef(null);

  const foundSceneIndex = SCENES.findIndex((scene) => time >= scene.start && time < scene.end);
  const sceneIndex = foundSceneIndex === -1 ? SCENES.length - 1 : foundSceneIndex;

  useEffect(() => {
    if (!playing) { lastFrame.current = null; return undefined; }
    let frame;
    const tick = (now) => {
      if (lastFrame.current === null) lastFrame.current = now;
      const delta = (now - lastFrame.current) / 1000;
      lastFrame.current = now;
      setTime((current) => {
        const next = Math.min(DEMO_DURATION, current + delta);
        if (next >= DEMO_DURATION) queueMicrotask(() => setPlaying(false));
        return next;
      });
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [playing]);

  const resetForDirection = (id) => {
    setDirection(id); setTime(0); setApproved(false); setResumed(false); setDraft(sessionPrompt); setDraftTouched(false); setPlaying(true); lastFrame.current = null;
  };
  const jumpTo = (index) => { setTime(SCENES[index].start + 0.05); setPlaying(true); lastFrame.current = null; };
  const handleResume = () => { setResumed(true); jumpTo(1); };
  const currentDirection = DIRECTIONS.find((item) => item.id === direction);

  return (
    <main className="demo-page">
      <header className="demo-header"><div><h1>Charter · Home, Attention & Recovery</h1><p>从 Home 发起同一段 35 秒 Chat Session，比较三种后续产品架构。</p></div><div className="demo-meta"><span>{currentDirection.name}</span><span>Interactive product mock</span></div></header>
      <DirectionTabs active={direction} onChange={resetForDirection} />
      <section className="stage-wrap" style={{ '--active-accent': currentDirection.accent }}><CharterFrame approved={approved} direction={direction} draft={draft} draftTouched={draftTouched} onApprove={() => { setApproved(true); setPlaying(true); }} onDraft={(value) => { setDraft(value); setDraftTouched(true); setPlaying(false); }} onNext={() => jumpTo(sceneIndex === SCENES.length - 1 ? 0 : sceneIndex + 1)} onResume={handleResume} onStart={() => jumpTo(1)} resumed={resumed} sceneIndex={sceneIndex} time={time} /></section>
      <DemoPlayer onPlayToggle={() => setPlaying((value) => !value)} onRestart={() => { setTime(0); setApproved(false); setResumed(false); setDraft(sessionPrompt); setDraftTouched(false); setPlaying(true); lastFrame.current = null; }} onScene={jumpTo} onSeek={(value) => { setTime(value); setPlaying(false); lastFrame.current = null; }} playing={playing} sceneIndex={sceneIndex} time={time} />
    </main>
  );
}

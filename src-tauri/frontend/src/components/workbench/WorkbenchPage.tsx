/**
 * 工作台完整页面（WorkbenchPage）。
 *
 * 把原先散在 Room 侧栏里的工作台模块（文件变更 / 审核队列 / 执行计划 / 终端 /
 * 子代理 / 待你决策）整合为一个独立页面：菜单栏 + 侧边栏 + 主内容区 + 状态页脚。
 * 结构与设计稿「工作台 · 完整面板」一致；暗亮主题完全走 tokens.css 的
 * data-theme 变量（本文件与 workbench-page.css 均无字面主题色）。
 *
 * 集成：
 *   1) 在场景路由处渲染 <WorkbenchPage />；
 *   2) demo 数据（sessions / stats / files / plan ...）替换为 store selector，
 *      回调（onOpenDiff / onAllowOnce ...）接入对应 invoke。
 * 交互态：选中 / hover / 禁用 / 焦点环均在 CSS（.wbpage-*）中定义。
 */
import { useState } from "react";
import "../../styles/workbench-page.css";
import {
  IconActivity,
  IconAlert,
  IconCheck,
  IconClose,
  IconEditor,
  IconInbox,
  IconMaximize,
  IconMessageCircle,
  IconMinimize,
  IconPlus,
  IconProjects,
  IconSearch,
  IconSettings,
  IconStop,
  IconSubagent,
  IconTerminal,
} from "../icons";

/* ==========================================================================
   数据模型
   ========================================================================== */

type Tone = "accent" | "success" | "warning" | "danger" | "muted";

interface SessionItem {
  id: string;
  title: string;
  time: string;
  active?: boolean;
}

interface StatCard {
  id: string;
  label: string;
  value: string;
  meta: string;
  metaTone: Tone;
}

interface FileChange {
  path: string;
  status: "M" | "A" | "D";
  added?: number;
  removed?: number;
  selected?: boolean;
}

interface PlanStep {
  id: string;
  name: string;
  state: "done" | "running" | "pending";
  meta?: string;
}

interface ReviewFile {
  path: string;
  state: string;
  tone: Tone;
}

interface CheckResult {
  name: string;
  result: string;
  tone: Tone;
}

interface FeatureAudit {
  name: string;
  state: string;
  tone: Tone;
}

interface SubagentRow {
  id: string;
  name: string;
  state: string;
  tone: Tone;
  selected?: boolean;
}

interface PermissionRequest {
  id: string;
  risk: string;
  tool: string;
  description: string;
  command?: string;
  actions: ("allow" | "persist" | "deny")[];
}

interface WorkbenchPageProps {
  sessions?: SessionItem[];
  stats?: StatCard[];
  onOpenDiff?: (path: string) => void;
  onRollbackAll?: () => void;
  onOpenPlan?: () => void;
  onInterrupt?: () => void;
  onOpenTerminal?: () => void;
}

/* ==========================================================================
   演示数据（与设计稿一致；接入 store 后整体替换）
   ========================================================================== */

const DEMO_SESSIONS: SessionItem[] = [
  { id: "s1", title: "支付模块重构", time: "6m", active: true },
  { id: "s2", title: "优化 Rust 编译报错", time: "4m" },
  { id: "s3", title: "修复任务队列并发", time: "2m" },
  { id: "s4", title: "更新依赖并修复警告", time: "25m" },
  { id: "s5", title: "接入流式输出中间件", time: "6m" },
];

const DEMO_STATS: StatCard[] = [
  { id: "files", label: "变更文件", value: "5", meta: "+128 −34", metaTone: "muted" },
  { id: "plan", label: "计划完成", value: "3/5", meta: "62%", metaTone: "accent" },
  { id: "agents", label: "子代理", value: "3", meta: "1 等待权限", metaTone: "warning" },
  { id: "decisions", label: "待你决策", value: "3", meta: "权限 2 · 提问 1", metaTone: "muted" },
];

const DEMO_FILES: FileChange[] = [
  { path: "Canvas.tsx", status: "M", added: 64, removed: 18, selected: true },
  { path: "workspace.rs", status: "M", added: 42, removed: 10 },
  { path: "payment.rs", status: "M", added: 18, removed: 6 },
  { path: "refund.ts", status: "A", added: 24 },
  { path: "legacy.rs", status: "D", removed: 32 },
];

const DEMO_PLAN: PlanStep[] = [
  { id: "r1", name: "R1 渠道抽象与接口定义", state: "done" },
  { id: "r2", name: "R2 支付状态机重构", state: "done" },
  { id: "r3", name: "R3 退款流程接入", state: "done" },
  { id: "r4", name: "R4 webhook 集成", state: "running", meta: "进行中 · 02:31" },
  { id: "r5", name: "R5 对账快照导出", state: "pending" },
];

const DEMO_REVIEW_FILES: ReviewFile[] = [
  { path: "payment.rs", state: "等待审核", tone: "warning" },
  { path: "webhook.rs", state: "等待审核", tone: "warning" },
  { path: "ledger.rs", state: "已通过", tone: "success" },
  { path: "config.rs", state: "有修改", tone: "muted" },
];

const DEMO_CHECKS: CheckResult[] = [
  { name: "cargo check", result: "通过", tone: "success" },
  { name: "cargo test", result: "47 通过 · 0 失败", tone: "success" },
  { name: "clippy", result: "2 warnings", tone: "warning" },
];

const DEMO_FEATURES: FeatureAudit[] = [
  { name: "功能 4 · 退款对账", state: "实施中", tone: "accent" },
  { name: "功能 5 · webhook 重试", state: "受阻", tone: "warning" },
  { name: "功能 6 · 对账快照", state: "失败", tone: "danger" },
];

const DEMO_SUBAGENTS: SubagentRow[] = [
  { id: "a1", name: "支付渠道调研", state: "运行中", tone: "success", selected: true },
  { id: "a2", name: "文档同步 bot", state: "运行中", tone: "success" },
  { id: "a3", name: "对账脚本生成", state: "等待权限", tone: "warning" },
  { id: "a4", name: "e2e 回归", state: "排队中", tone: "muted" },
  { id: "a5", name: "规范扫描", state: "已停止", tone: "danger" },
];

const DEMO_PERMISSIONS: PermissionRequest[] = [
  {
    id: "perm-a",
    risk: "R2 · 需要确认",
    tool: "shell.exec 调用",
    description: "子代理 支付渠道调研 请求执行",
    command: "git rebase origin/main -- autostash",
    actions: ["allow", "persist", "deny"],
  },
  {
    id: "perm-b",
    risk: "R3 · 高风险",
    tool: "network.fetch · GET api.github.com/repos/…/pulls/42",
    description: "",
    actions: ["allow", "deny"],
  },
];

/* ==========================================================================
   私有小组件
   ========================================================================== */

function WbIconBack() {
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <path d="M10 3.5 5.5 8l4.5 4.5" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

function WbIconForward() {
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <path d="M6 3.5 10.5 8 6 12.5" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

function WbIconPanelToggle() {
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <rect x="2" y="2.75" width="12" height="10.5" rx="2" stroke="currentColor" strokeWidth="1.3" />
      <path d="M6.25 2.75v10.5" stroke="currentColor" strokeWidth="1.3" />
    </svg>
  );
}

function WbMenuButton({ label, icon }: { label: string; icon?: React.ReactNode }) {
  return (
    <button type="button" className="wbpage-menubar__btn" aria-label={label}>
      {icon}
    </button>
  );
}

function WbMenuLabel({ children }: { children: React.ReactNode }) {
  return (
    <button type="button" className="wbpage-menubar__menu">
      {children}
    </button>
  );
}

function WbToneText({ tone, children }: { tone: Tone; children: React.ReactNode }) {
  return <span className={`wbpage-tone wbpage-tone--${tone}`}>{children}</span>;
}

/* ==========================================================================
   页面骨架：菜单栏 / 侧边栏 / 页脚
   ========================================================================== */

function WorkbenchMenubar() {
  return (
    <header className="wbpage-menubar">
      <div className="wbpage-menubar__group">
        <WbMenuButton label="切换侧边栏" icon={<WbIconPanelToggle />} />
        <WbMenuButton label="后退" icon={<WbIconBack />} />
        <WbMenuButton label="前进" icon={<WbIconForward />} />
      </div>
      <nav className="wbpage-menubar__menus" aria-label="应用菜单">
        <WbMenuLabel>文件</WbMenuLabel>
        <WbMenuLabel>编辑</WbMenuLabel>
        <WbMenuLabel>视图</WbMenuLabel>
        <WbMenuLabel>帮助</WbMenuLabel>
      </nav>
      <div className="wbpage-menubar__spacer" />
      <div className="wbpage-menubar__group wbpage-menubar__group--win">
        <WbMenuButton label="通知" icon={<IconInbox width={16} height={16} />} />
        <WbMenuButton label="最小化" icon={<IconMinimize width={16} height={16} />} />
        <WbMenuButton label="最大化" icon={<IconMaximize width={16} height={16} />} />
        <WbMenuButton label="关闭" icon={<IconClose width={16} height={16} />} />
      </div>
    </header>
  );
}

function WorkbenchRail({ sessions }: { sessions: SessionItem[] }) {
  return (
    <aside className="wbpage-rail">
      <div className="wbpage-rail__brand">
        <span className="wbpage-rail__logo" aria-hidden="true">R</span>
        <span className="wbpage-rail__brand-name">R-Code</span>
        <span className="wbpage-rail__brand-spacer" />
        <button type="button" className="wbpage-iconbtn" aria-label="搜索">
          <IconSearch width={15} height={15} />
        </button>
      </div>

      <button type="button" className="wbpage-rail__newchat">
        <IconPlus width={14} height={14} />
        <span>新对话</span>
      </button>

      <nav className="wbpage-rail__nav" aria-label="主导航">
        <button type="button" className="wbpage-rail__navitem is-active">
          <IconMessageCircle width={15} height={15} />
          <span className="wbpage-rail__navtext">对话</span>
        </button>
        <button type="button" className="wbpage-rail__navitem">
          <IconInbox width={15} height={15} />
          <span className="wbpage-rail__navtext">待处理</span>
          <span className="wbpage-rail__navspacer" />
          <span className="wbpage-rail__badge">2</span>
        </button>
        <button type="button" className="wbpage-rail__navitem">
          <IconActivity width={15} height={15} />
          <span className="wbpage-rail__navtext">活动</span>
        </button>
        <button type="button" className="wbpage-rail__navitem">
          <IconProjects width={15} height={15} />
          <span className="wbpage-rail__navtext">项目文件</span>
        </button>
      </nav>

      <div className="wbpage-rail__section">
        <span className="wbpage-rail__section-label">项目</span>
        <span className="wbpage-rail__runbadge">
          <span className="wbpage-dot wbpage-dot--success" />
          1 运行中
        </span>
      </div>
      <button type="button" className="wbpage-rail__project">
        <IconProjects width={14} height={14} />
        <span className="wbpage-rail__navtext">r-code</span>
        <span className="wbpage-rail__navspacer" />
        <IconPlus width={14} height={14} />
      </button>

      <div className="wbpage-rail__sessions">
        {sessions.map((s) => (
          <button
            key={s.id}
            type="button"
            className={"wbpage-rail__session" + (s.active ? " is-active" : "")}
            aria-current={s.active ? "page" : undefined}
          >
            <span className="wbpage-rail__session-title">{s.title}</span>
            <span className="wbpage-rail__navspacer" />
            <span className="wbpage-rail__session-time">{s.time}</span>
          </button>
        ))}
      </div>

      <div className="wbpage-rail__spacer" />
      <div className="wbpage-rail__divider" />
      <button type="button" className="wbpage-rail__navitem">
        <IconSettings width={15} height={15} />
        <span className="wbpage-rail__navtext">设置</span>
      </button>
    </aside>
  );
}

function WorkbenchFooter() {
  return (
    <footer className="wbpage-footer">
      <span className="wbpage-footer__item">
        <span className="wbpage-dot wbpage-dot--success" />
        r-code · main
      </span>
      <span className="wbpage-footer__sep" aria-hidden="true" />
      <span className="wbpage-footer__item">检查通过 · 2 warnings</span>
      <span className="wbpage-footer__spacer" />
      <span className="wbpage-footer__item wbpage-footer__item--mono">8.4k tokens</span>
      <span className="wbpage-footer__sep" aria-hidden="true" />
      <span className="wbpage-footer__item">v1.0.1</span>
    </footer>
  );
}

/* ==========================================================================
   主区：页头 / 统计行
   ========================================================================== */

function PageHeader({
  stats,
  onOpenTerminal,
  onInterrupt,
}: {
  stats: StatCard[];
  onOpenTerminal?: () => void;
  onInterrupt?: () => void;
}) {
  return (
    <div className="wbpage-header">
      <h1 className="wbpage-header__title">工作台</h1>
      <span className="wbpage-live">
        <span className="wbpage-dot wbpage-dot--success" />
        运行中
      </span>
      <span className="wbpage-header__meta">
        支付模块重构 · {stats[0]?.value ?? "0"} 个文件变更 · 更新于 2 分钟前
      </span>
      <span className="wbpage-header__spacer" />
      <button type="button" className="wbpage-btn" onClick={onOpenTerminal}>
        打开终端
      </button>
      <button type="button" className="wbpage-btn wbpage-btn--danger" onClick={onInterrupt}>
        中断
      </button>
    </div>
  );
}

function StatsRow({ stats }: { stats: StatCard[] }) {
  return (
    <div className="wbpage-stats">
      {stats.map((s) => (
        <div key={s.id} className="wbpage-stat">
          <span className="wbpage-stat__label">{s.label}</span>
          <span className="wbpage-stat__value">
            {s.value}
            <WbToneText tone={s.metaTone}>{s.meta}</WbToneText>
          </span>
        </div>
      ))}
    </div>
  );
}

/* ==========================================================================
   卡片基件
   ========================================================================== */

function Card({
  icon,
  title,
  meta,
  className,
  children,
}: {
  icon: React.ReactNode;
  title: string;
  meta?: React.ReactNode;
  className?: string;
  children: React.ReactNode;
}) {
  return (
    <section className={"wbpage-card" + (className ? ` ${className}` : "")}>
      <header className="wbpage-card__head">
        {icon}
        <h2 className="wbpage-card__title">{title}</h2>
        {meta != null && <span className="wbpage-card__meta">{meta}</span>}
      </header>
      {children}
    </section>
  );
}

/* ==========================================================================
   六个模块卡
   ========================================================================== */

function FileChangesCard({
  files,
  onOpenDiff,
  onRollbackAll,
}: {
  files: FileChange[];
  onOpenDiff?: (path: string) => void;
  onRollbackAll?: () => void;
}) {
  const [selected, setSelected] = useState(files.find((f) => f.selected)?.path ?? files[0]?.path ?? "");
  const adds = files.reduce((n, f) => n + (f.added ?? 0), 0);
  const dels = files.reduce((n, f) => n + (f.removed ?? 0), 0);
  const total = Math.max(adds + dels, 1);

  return (
    <Card
      icon={<IconEditor width={14} height={14} />}
      title="文件变更"
      meta={`${files.length} 个文件`}
      className="wbpage-card--fill"
    >
      <div className="wbpage-diffbar-row">
        <div
          className="wbpage-diffbar"
          role="img"
          aria-label={`${adds} 行新增，${dels} 行删除`}
        >
          <span className="wbpage-diffbar__add" style={{ flexGrow: adds }} />
          <span className="wbpage-diffbar__del" style={{ flexGrow: dels }} />
        </div>
        <span className="wbpage-diffbar-row__label">
          {Math.round((adds / total) * 100)} 增 / {Math.round((dels / total) * 100)} 删
        </span>
      </div>

      <div className="wbpage-filelist" role="listbox" aria-label="变更文件">
        {files.map((f) => (
          <button
            key={f.path}
            type="button"
            role="option"
            aria-selected={selected === f.path}
            className={"wbpage-filerow" + (selected === f.path ? " is-selected" : "")}
            onClick={() => setSelected(f.path)}
          >
            <span className="wbpage-filerow__path">{f.path}</span>
            <span className="wbpage-filerow__right">
              <span className="wbpage-filerow__status">{f.status}</span>
              {f.added != null && <WbToneText tone="success">+{f.added}</WbToneText>}
              {f.removed != null && <WbToneText tone="danger">−{f.removed}</WbToneText>}
            </span>
          </button>
        ))}
      </div>

      {selected === "Canvas.tsx" && (
        <div className="wbpage-diffdetail">
          <span className="wbpage-diffdetail__hunk">@@ -86,7 +86,12 @@ src/canvas/Canvas.tsx</span>
          <span className="wbpage-diffdetail__del">-  fn settle(order: &amp;Order) {"{"}</span>
          <span className="wbpage-diffdetail__add">+  let receipt = ledger.commit();</span>
        </div>
      )}

      <div className="wbpage-actions">
        <span className="wbpage-actions__hint">Diff 视图中查看完整变更</span>
        <span className="wbpage-actions__group">
          <button type="button" className="wbpage-btn wbpage-btn--primary" onClick={() => onOpenDiff?.(selected)}>
            打开 Diff
          </button>
          <button type="button" className="wbpage-btn" onClick={onRollbackAll}>
            全部回滚
          </button>
        </span>
      </div>
    </Card>
  );
}

function PlanCard({ steps, onOpenPlan }: { steps: PlanStep[]; onOpenPlan?: () => void }) {
  const done = steps.filter((s) => s.state === "done").length;
  const pct = Math.round((done / Math.max(steps.length, 1)) * 100);
  return (
    <Card
      icon={<IconActivity width={14} height={14} />}
      title="执行计划"
      meta={<WbToneText tone="accent">{pct}%</WbToneText>}
    >
      <div
        className="wbpage-progress"
        role="progressbar"
        aria-valuenow={pct}
        aria-valuemin={0}
        aria-valuemax={100}
      >
        <span className="wbpage-progress__fill" style={{ width: `${pct}%` }} />
      </div>
      <div className="wbpage-steps">
        {steps.map((s) => (
          <div key={s.id} className={"wbpage-step is-" + s.state}>
            <span className="wbpage-step__name">{s.name}</span>
            <span className="wbpage-step__state">
              {s.state === "done" ? "已完成" : s.state === "running" ? s.meta : "未开始"}
            </span>
          </div>
        ))}
      </div>
      <span className="wbpage-card__note">下一步：R4 webhook 集成 · 预计 15 分钟</span>
      <button type="button" className="wbpage-link" onClick={onOpenPlan}>
        查看完整计划
      </button>
    </Card>
  );
}

function TerminalCard() {
  const [tab, setTab] = useState<"cargo" | "dev">("cargo");
  const running = false; // 接入真实会话后由运行状态驱动；禁用态演示
  return (
    <Card
      icon={<IconTerminal width={14} height={14} />}
      title="终端"
      meta={
        <span className="wbpage-tabs">
          <button
            type="button"
            className={"wbpage-tab" + (tab === "cargo" ? " is-active" : "")}
            onClick={() => setTab("cargo")}
          >
            cargo
          </button>
          <button
            type="button"
            className={"wbpage-tab" + (tab === "dev" ? " is-active" : "")}
            onClick={() => setTab("dev")}
          >
            dev
          </button>
        </span>
      }
      className="wbpage-card--fill"
    >
      <div className="wbpage-terminal" aria-live="polite">
        <span className="wbpage-terminal__cmd">$ cargo test -p rcode-core --lib</span>
        <span className="wbpage-terminal__out">running 47 tests ... 47 passed · 0 failed</span>
        <span className="wbpage-terminal__warn">warning: unused import: `std::fmt::Write`</span>
      </div>
      <div className="wbpage-actions">
        <span className="wbpage-actions__hint">上次运行 12 分钟前 · 退出码 0</span>
        <span className="wbpage-actions__group">
          <button type="button" className="wbpage-btn" disabled={!running}>
            停止
          </button>
          <button type="button" className="wbpage-btn">
            清屏
          </button>
        </span>
      </div>
    </Card>
  );
}

function ReviewQueueCard() {
  return (
    <Card
      icon={<IconInbox width={14} height={14} />}
      title="审核队列"
      meta={<WbToneText tone="warning">2 项待审</WbToneText>}
    >
      <div className="wbpage-reviewlist">
        {DEMO_REVIEW_FILES.map((f) => (
          <div key={f.path} className="wbpage-keyrow">
            <span className="wbpage-keyrow__key wbpage-mono">{f.path}</span>
            <WbToneText tone={f.tone}>{f.state}</WbToneText>
          </div>
        ))}
      </div>
      <span className="wbpage-card__section">检查</span>
      <div className="wbpage-reviewlist">
        {DEMO_CHECKS.map((c) => (
          <div key={c.name} className="wbpage-keyrow">
            <span className="wbpage-keyrow__key wbpage-mono">{c.name}</span>
            <WbToneText tone={c.tone}>{c.result}</WbToneText>
          </div>
        ))}
      </div>
      <div className="wbpage-strip">
        {DEMO_FEATURES.map((f) => (
          <div key={f.name} className="wbpage-keyrow">
            <span className="wbpage-keyrow__key">{f.name}</span>
            <WbToneText tone={f.tone}>{f.state}</WbToneText>
          </div>
        ))}
      </div>
    </Card>
  );
}

function SubagentsCard({ agents }: { agents: SubagentRow[] }) {
  const [selectedId, setSelectedId] = useState(agents.find((a) => a.selected)?.id ?? "");
  return (
    <Card
      icon={<IconSubagent width={14} height={14} />}
      title="子代理"
      meta="3 运行中 · 1 等待"
    >
      <div className="wbpage-agentlist" role="listbox" aria-label="子代理">
        {agents.map((a) => (
          <button
            key={a.id}
            type="button"
            role="option"
            aria-selected={selectedId === a.id}
            className={"wbpage-agentrow" + (selectedId === a.id ? " is-selected" : "")}
            onClick={() => setSelectedId(a.id)}
          >
            <span className={"wbpage-dot wbpage-dot--" + a.tone} />
            <span className="wbpage-agentrow__name">{a.name}</span>
            <span className="wbpage-agentrow__spacer" />
            <span className="wbpage-agentrow__state">{a.state}</span>
          </button>
        ))}
      </div>
      <div className="wbpage-agentdetail">
        <span>gpt-5 · 只读 + 网络白名单</span>
        <span className="wbpage-mono">8.4k tokens</span>
      </div>
    </Card>
  );
}

function DecisionsCard() {
  const [answer, setAnswer] = useState("staging");
  return (
    <Card
      icon={<IconAlert width={14} height={14} />}
      title="待你决策"
      meta="权限 2 · 提问 1"
    >
      {DEMO_PERMISSIONS.map((p) => (
        <div key={p.id} className={"wbpage-perm wbpage-perm--" + (p.actions.includes("persist") ? "r2" : "r3")}>
          <div className="wbpage-perm__head">
            <span className="wbpage-chip">{p.risk}</span>
            <span className={"wbpage-perm__tool" + (p.command ? " wbpage-perm__tool--title" : " wbpage-mono")}>
              {p.tool}
            </span>
          </div>
          {p.description && <span className="wbpage-perm__desc">{p.description}</span>}
          {p.command && (
            <div className="wbpage-perm__cmd">
              <code>{p.command}</code>
            </div>
          )}
          <div className="wbpage-perm__btns">
            <button type="button" className="wbpage-btn wbpage-btn--primary">允许一次</button>
            {p.actions.includes("persist") && (
              <button type="button" className="wbpage-btn">本任务始终允许</button>
            )}
            <button type="button" className="wbpage-btn wbpage-btn--danger">拒绝</button>
          </div>
        </div>
      ))}

      <div className="wbpage-question">
        <span className="wbpage-question__q">子代理提问：预发环境联调还是本地容器？</span>
        <button
          type="button"
          className={"wbpage-option" + (answer === "staging" ? " is-selected" : "")}
          aria-pressed={answer === "staging"}
          onClick={() => setAnswer("staging")}
        >
          <span className="wbpage-option__dot" />
          预发环境（推荐）
        </button>
        <button
          type="button"
          className={"wbpage-option" + (answer === "local" ? " is-selected" : "")}
          aria-pressed={answer === "local"}
          onClick={() => setAnswer("local")}
        >
          <span className="wbpage-option__dot" />
          本地容器
        </button>
        <button type="button" className="wbpage-btn wbpage-btn--primary">提交回答</button>
      </div>
    </Card>
  );
}

/* ==========================================================================
   页面
   ========================================================================== */

export function WorkbenchPage({
  sessions = DEMO_SESSIONS,
  stats = DEMO_STATS,
  onOpenDiff,
  onRollbackAll,
  onOpenPlan,
  onOpenTerminal,
  onInterrupt,
}: WorkbenchPageProps) {
  return (
    <div className="wbpage">
      <WorkbenchMenubar />
      <div className="wbpage-body">
        <WorkbenchRail sessions={sessions} />
        <main className="wbpage-main">
          <PageHeader stats={stats} onOpenTerminal={onOpenTerminal} onInterrupt={onInterrupt} />
          <StatsRow stats={stats} />
          <div className="wbpage-grid">
            <div className="wbpage-col">
              <FileChangesCard files={DEMO_FILES} onOpenDiff={onOpenDiff} onRollbackAll={onRollbackAll} />
              <PlanCard steps={DEMO_PLAN} onOpenPlan={onOpenPlan} />
              <TerminalCard />
            </div>
            <div className="wbpage-col">
              <ReviewQueueCard />
              <SubagentsCard agents={DEMO_SUBAGENTS} />
              <DecisionsCard />
            </div>
          </div>
        </main>
      </div>
      <WorkbenchFooter />
    </div>
  );
}

export default WorkbenchPage;

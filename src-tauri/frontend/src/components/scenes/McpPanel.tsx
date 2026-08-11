import { useCallback, useEffect, useMemo, useState } from "react";
import { errText } from "../../lib/format";
import {
  mcpCredentialStatus,
  mcpDeleteCredential,
  mcpMarketInstall,
  mcpMarketPrepareInstall,
  mcpMarketSearch,
  mcpRemove,
  mcpSetCredential,
  mcpSnapshot,
  mcpTestConnection,
  mcpToggle,
  mcpUpsert,
  onMcpStatus,
} from "../../lib/ipc";
import type {
  McpCredentialStatus,
  McpLaunchPreview,
  McpManagerSnapshot,
  McpMarketInstallRequest,
  McpMarketPage,
  McpMarketServer,
  McpServerState,
  McpServerView,
  McpUpsertRequest,
} from "../../lib/types";
import { useAppStore } from "../../store/app";
import { IconCheck, IconPlus, IconRefresh, IconSearch, IconShield, IconTrash } from "../icons";

type ApprovalAction =
  | { kind: "enable"; serverId: string; preview: McpLaunchPreview }
  | { kind: "install"; request: McpMarketInstallRequest; preview: McpLaunchPreview };

const EMPTY_DRAFT = {
  id: "",
  displayName: "",
  description: "",
  transport: "stdio" as "stdio" | "streamable_http",
  executable: "",
  args: "",
  names: "",
  url: "",
};

/** 本机联网与 MCP 控制面。所有密钥输入都只写入系统凭据库，从不回填到页面。 */
export function McpPanel() {
  const suggestedQuery = useAppStore((state) => state.mcpMarketQuery);
  const focusServerId = useAppStore((state) => state.mcpFocusServerId);
  const clearMcpFocus = useAppStore((state) => state.clearMcpFocus);
  const [snapshot, setSnapshot] = useState<McpManagerSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busyKeys, setBusyKeys] = useState<Set<string>>(() => new Set());
  const [editing, setEditing] = useState<McpServerView | "new" | null>(null);
  const [credentialsFor, setCredentialsFor] = useState<string | null>(null);
  const [removeConfirm, setRemoveConfirm] = useState<string | null>(null);
  const [approval, setApproval] = useState<ApprovalAction | null>(null);
  const [marketOpen, setMarketOpen] = useState(Boolean(suggestedQuery));
  const [marketQuery, setMarketQuery] = useState(suggestedQuery ?? "");
  const [market, setMarket] = useState<McpMarketPage | null>(null);
  const servers = useMemo(() => [...(snapshot?.servers ?? [])].sort((left, right) =>
    Number(right.source.kind === "generated") - Number(left.source.kind === "generated")
  ), [snapshot?.servers]);

  const reload = useCallback(async () => {
    try {
      setSnapshot(await mcpSnapshot());
      setError(null);
    } catch (cause) {
      setError(errText(cause));
    }
  }, []);

  useEffect(() => {
    void reload();
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void onMcpStatus((statuses) => {
      if (disposed) return;
      const byId = new Map(statuses.map((status) => [status.id, status]));
      setSnapshot((current) => current ? {
        ...current,
        servers: current.servers.map((server) => {
          const status = byId.get(server.id);
          return status ? { ...server, ...status } : server;
        }),
      } : current);
    }).then((dispose) => {
      if (disposed) dispose();
      else unlisten = dispose;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [reload]);

  useEffect(() => {
    if (!focusServerId || !servers.some((server) => server.id === focusServerId)) return;
    const frame = window.requestAnimationFrame(() => {
      const row = document.getElementById(`mcp-server-${focusServerId}`);
      row?.scrollIntoView({ behavior: "smooth", block: "center" });
      row?.focus({ preventScroll: true });
      clearMcpFocus();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [clearMcpFocus, focusServerId, servers]);

  const searchMarket = useCallback(async (cursor: string | null = null) => {
    setBusyKeys((current) => new Set(current).add("market"));
    try {
      const page = await mcpMarketSearch(marketQuery.trim() || null, cursor, 20);
      setMarket((current) => cursor && current
        ? { ...page, servers: [...current.servers, ...page.servers] }
        : page);
      setError(null);
    } catch (cause) {
      setError(errText(cause));
    } finally {
      setBusyKeys((current) => {
        const next = new Set(current);
        next.delete("market");
        return next;
      });
    }
  }, [marketQuery]);

  useEffect(() => {
    if (!suggestedQuery) return;
    setMarketOpen(true);
    setMarketQuery(suggestedQuery);
    setBusyKeys((current) => new Set(current).add("market"));
    void mcpMarketSearch(suggestedQuery, null, 20)
      .then(setMarket)
      .catch((cause) => setError(errText(cause)))
      .finally(() => setBusyKeys((current) => {
        const next = new Set(current);
        next.delete("market");
        return next;
      }));
  }, [suggestedQuery]);

  const run = useCallback(async (key: string, operation: () => Promise<void>) => {
    setBusyKeys((current) => new Set(current).add(key));
    setNotice(null);
    try {
      await operation();
      setError(null);
    } catch (cause) {
      setError(errText(cause));
    } finally {
      setBusyKeys((current) => {
        const next = new Set(current);
        next.delete(key);
        return next;
      });
    }
  }, []);

  const toggleServer = (server: McpServerView) => void run(`toggle:${server.id}`, async () => {
    const result = await mcpToggle(server.id, !server.enabled);
    if (result.confirmation) setApproval({ kind: "enable", serverId: server.id, preview: result.confirmation });
    else {
      setNotice(server.enabled ? `已关闭 ${server.display_name}` : `已启用 ${server.display_name}`);
      await reload();
    }
  });

  const confirmApproval = () => {
    if (!approval) return;
    void run("approval", async () => {
      if (approval.kind === "enable") {
        await mcpToggle(approval.serverId, true, approval.preview.token);
        setNotice("MCP 已启用；它仍会按需启动，不会在后台常驻空转。");
      } else {
        await mcpMarketInstall(approval.request, approval.preview.token);
        setNotice("已添加为关闭状态。请先配置凭据，再单独确认启用。");
      }
      setApproval(null);
      await reload();
    });
  };

  const prepareInstall = (server: McpMarketServer, optionId: string, serverId: string) => void run(`prepare:${server.name}:${optionId}`, async () => {
    const request = { server, option_id: optionId, server_id: serverId.trim() };
    const preview = await mcpMarketPrepareInstall(request);
    setApproval({ kind: "install", request, preview });
  });

  return (
    <div className="mcp-panel">
      <header className="knowledge-panel-head">
        <div>
          <span>TOOLS &amp; CONNECTIONS</span>
          <h2>联网与 MCP</h2>
          <p>普通联网优先使用 R-Code 内置工具；只有深度调研、专用数据源或鉴权服务才交给 MCP。</p>
        </div>
        <button className="iconbtn" aria-label="刷新 MCP 状态" title="刷新" disabled={busyKeys.has("reload")} onClick={() => void run("reload", reload)}>
          <IconRefresh width={15} height={15} />
        </button>
      </header>

      {error && <div className="mcp-banner error" role="alert"><strong>操作未完成</strong><span>{error}</span></div>}
      {notice && <div className="mcp-banner ok" role="status"><IconCheck width={14} height={14} /><span>{notice}</span></div>}
      {snapshot?.settings_error && <div className="mcp-banner error" role="alert"><strong>MCP 配置文件不可用</strong><span>{snapshot.settings_error}</span></div>}

      <section className="mcp-native">
        <div className="mcp-native-mark" aria-hidden="true">WEB</div>
        <div>
          <strong>内置联网工具</strong>
          <p><code>web_search</code> 与 <code>web_fetch</code> 使用无密钥的安全读取通道，限制重定向、响应大小和内网地址。无需 MCP，也不会启动外部进程。</p>
        </div>
        <span className="mcp-state is-running"><i />可用</span>
      </section>

      <section className="mcp-section">
        <div className="mcp-section-head">
          <div><h3>MCP 服务</h3><p>已安装服务只在模型实际调用时启动；关闭会立即阻止后续调用并安全结束连接。</p></div>
          <button className="btn" onClick={() => setEditing("new")}><IconPlus width={13} height={13} />自定义</button>
        </div>
        {!snapshot && !error && <div className="knowledge-state">正在读取本机 MCP 配置…</div>}
        {snapshot && servers.length === 0 && <div className="mcp-empty">暂无 MCP 服务。内置联网工具仍然可用。</div>}
        <div className="mcp-server-list">
          {servers.map((server) => (
            <ServerRow
              key={server.id}
              server={server}
              busyKeys={busyKeys}
              onToggle={() => toggleServer(server)}
              onTest={() => void run(`test:${server.id}`, async () => {
                const tools = await mcpTestConnection(server.id);
                setNotice(`连接成功，发现 ${tools.length} 个工具。`);
                await reload();
              })}
              onEdit={() => setEditing(server)}
              onCredentials={() => setCredentialsFor(credentialsFor === server.id ? null : server.id)}
              removeArmed={removeConfirm === server.id}
              onRemove={() => {
                if (removeConfirm !== server.id) {
                  setRemoveConfirm(server.id);
                  return;
                }
                void run(`remove:${server.id}`, async () => {
                  await mcpRemove(server.id);
                  setRemoveConfirm(null);
                  setNotice(`已移除 ${server.display_name} 及其凭据引用。`);
                  await reload();
                });
              }}
            />
          ))}
        </div>
        {credentialsFor && snapshot?.servers.some((server) => server.id === credentialsFor) && (
          <CredentialEditor server={snapshot.servers.find((server) => server.id === credentialsFor)!} onClose={() => setCredentialsFor(null)} />
        )}
      </section>

      {editing && (
        <CustomMcpForm
          server={editing === "new" ? null : editing}
          busy={busyKeys.has("save")}
          onCancel={() => setEditing(null)}
          onSave={(request) => void run("save", async () => {
            await mcpUpsert(request);
            setEditing(null);
            setNotice("配置已保存为关闭状态；启动前还会展示精确命令或地址供确认。");
            await reload();
          })}
        />
      )}

      <section className="mcp-section mcp-market">
        <button className="mcp-market-toggle" aria-expanded={marketOpen} onClick={() => setMarketOpen((value) => !value)}>
          <span><strong>MCP 市场</strong><small>官方 Registry · 预览数据源</small></span>
          <span>{marketOpen ? "收起" : "浏览"}</span>
        </button>
        {marketOpen && (
          <div className="mcp-market-body">
            <div className="mcp-market-warning"><IconShield width={14} height={14} /><span>Registry 仍处于预览阶段，条目未经 R-Code 或官方安全审核。安装前必须核对发布者、仓库和精确启动方案。</span></div>
            <form className="mcp-market-search" onSubmit={(event) => { event.preventDefault(); void searchMarket(); }}>
              <IconSearch width={14} height={14} />
              <input className="input" value={marketQuery} onChange={(event) => setMarketQuery(event.target.value)} placeholder="搜索服务、包名或能力" />
              <button className="btn" disabled={busyKeys.has("market")}>{busyKeys.has("market") ? "搜索中…" : "搜索"}</button>
            </form>
            {market?.stale && <div className="mcp-market-stale">网络不可用，当前显示最近一次本机缓存。</div>}
            <div className="mcp-market-results">
              {market?.servers.map((server) => <MarketRow key={`${server.name}@${server.version}`} server={server} busyKeys={busyKeys} onInstall={prepareInstall} />)}
            </div>
            {market?.next_cursor && <button className="btn ghost" disabled={busyKeys.has("market")} onClick={() => void searchMarket(market.next_cursor ?? null)}>加载更多</button>}
            {market && market.servers.length === 0 && <div className="mcp-empty">没有找到可安装的启动方案。</div>}
          </div>
        )}
      </section>

      {approval && <ApprovalPanel approval={approval} busy={busyKeys.has("approval")} onCancel={() => setApproval(null)} onConfirm={confirmApproval} />}
    </div>
  );
}

function ServerRow({ server, busyKeys, removeArmed, onToggle, onTest, onEdit, onCredentials, onRemove }: {
  server: McpServerView;
  busyKeys: ReadonlySet<string>;
  removeArmed: boolean;
  onToggle: () => void;
  onTest: () => void;
  onEdit: () => void;
  onCredentials: () => void;
  onRemove: () => void;
}) {
  const rowBusy = busyKeys.has(`toggle:${server.id}`)
    || busyKeys.has(`test:${server.id}`)
    || busyKeys.has(`remove:${server.id}`);
  const credentialNames = server.transport.type === "stdio"
    ? server.transport.environment_names
    : server.transport.type === "streamable_http" ? server.transport.header_names : [];
  const generatedSource = server.source.kind === "generated" ? server.source : null;
  const generated = Boolean(generatedSource);
  return (
    <article
      id={`mcp-server-${server.id}`}
      className={`mcp-server-row${generated ? " is-generated" : ""}`}
      tabIndex={-1}
    >
      <div className="mcp-server-main">
        <div className="mcp-server-title">
          <strong>{server.display_name}</strong>
          <code>{server.id}</code>
          {server.builtin && <span>内置</span>}
          {generated && <span className="generated">由 R-Code 生成</span>}
          {generated && !server.enabled && <span className="review">待用户审核</span>}
        </div>
        <p>{server.description || transportSummary(server)}</p>
        <div className="mcp-server-meta">
          <span>{transportSummary(server)}</span>
          {generatedSource && <span className="mcp-source-path" title={generatedSource.source_path}>源码：{generatedSource.source_path}</span>}
          {server.tool_count > 0 && <span>{server.tool_count} 个工具</span>}
          {!server.launch_approved && !server.builtin && !generated && <span className="warn">启动方案待确认</span>}
          {server.error_code && <span className="danger">{server.error_code}</span>}
        </div>
      </div>
      <div className="mcp-server-actions">
        <span className={`mcp-state is-${server.state}`}><i />{stateLabel(server.state, server.enabled)}</span>
        {credentialNames.length > 0 && <button className="btn ghost" onClick={onCredentials}>凭据</button>}
        {!server.builtin && <button className="btn ghost" onClick={onEdit}>编辑</button>}
        <button className="btn ghost" disabled={!server.enabled || rowBusy} onClick={onTest}>{busyKeys.has(`test:${server.id}`) ? "连接中…" : "测试"}</button>
        {!server.builtin && <button className={`btn ghost mcp-remove${removeArmed ? " armed" : ""}`} aria-label={`${removeArmed ? "确认移除" : "移除"} ${server.display_name}`} title={removeArmed ? "再次点击确认移除" : "移除"} disabled={rowBusy} onClick={onRemove}>{removeArmed ? "确认移除" : <IconTrash width={14} height={14} />}</button>}
        <button
          type="button"
          className={`mcp-switch${server.enabled ? " on" : ""}`}
          role="switch"
          aria-checked={server.enabled}
          aria-label={`${server.enabled ? "关闭" : "启用"} ${server.display_name}`}
          disabled={rowBusy}
          onClick={onToggle}
        ><span /></button>
      </div>
    </article>
  );
}

function CustomMcpForm({ server, busy, onCancel, onSave }: {
  server: McpServerView | null;
  busy: boolean;
  onCancel: () => void;
  onSave: (request: McpUpsertRequest) => void;
}) {
  const initial = useMemo(() => {
    if (!server) return EMPTY_DRAFT;
    if (server.transport.type === "stdio") return {
      id: server.id, displayName: server.display_name, description: server.description, transport: "stdio" as const,
      executable: server.transport.executable, args: server.transport.args.join("\n"), names: server.transport.environment_names.join("\n"), url: "",
    };
    if (server.transport.type === "streamable_http") return {
      id: server.id, displayName: server.display_name, description: server.description, transport: "streamable_http" as const,
      executable: "", args: "", names: server.transport.header_names.join("\n"), url: server.transport.url,
    };
    return EMPTY_DRAFT;
  }, [server]);
  const [draft, setDraft] = useState(initial);
  const lines = (value: string) => value.split(/\r?\n/).map((item) => item.trim()).filter(Boolean);
  const submit = (event: React.FormEvent) => {
    event.preventDefault();
    const transport = draft.transport === "stdio"
      ? { type: "stdio" as const, executable: draft.executable.trim(), args: lines(draft.args), environment_names: lines(draft.names) }
      : { type: "streamable_http" as const, url: draft.url.trim(), header_names: lines(draft.names) };
    onSave({ id: draft.id.trim(), display_name: draft.displayName.trim(), description: draft.description.trim(), transport });
  };
  return (
    <form className="mcp-editor" onSubmit={submit}>
      <div className="mcp-editor-head"><div><strong>{server ? "编辑 MCP" : "添加自定义 MCP"}</strong><span>命令和参数原样执行，不经过 shell。</span></div><button type="button" className="btn ghost" onClick={onCancel}>取消</button></div>
      <div className="mcp-editor-grid">
        <label>ID<input className="input" required pattern="[a-z][a-z0-9_-]{0,63}" disabled={Boolean(server) || busy} value={draft.id} onChange={(event) => setDraft({ ...draft, id: event.target.value })} placeholder="例如 github-tools" /></label>
        <label>显示名称<input className="input" required disabled={busy} value={draft.displayName} onChange={(event) => setDraft({ ...draft, displayName: event.target.value })} /></label>
        <label className="wide">说明<input className="input" disabled={busy} value={draft.description} onChange={(event) => setDraft({ ...draft, description: event.target.value })} /></label>
        <label>传输<select className="input" disabled={busy} value={draft.transport} onChange={(event) => setDraft({ ...draft, transport: event.target.value as typeof draft.transport })}><option value="stdio">本机 stdio</option><option value="streamable_http">远程 HTTPS</option></select></label>
        {draft.transport === "stdio" ? <>
          <label>可执行文件<input className="input" required disabled={busy} value={draft.executable} onChange={(event) => setDraft({ ...draft, executable: event.target.value })} placeholder="npx / uvx / 绝对路径" /></label>
          <label className="wide">参数（每行一个）<textarea className="input" rows={4} disabled={busy} value={draft.args} onChange={(event) => setDraft({ ...draft, args: event.target.value })} /></label>
          <label className="wide">环境变量名（每行一个）<textarea className="input" rows={3} disabled={busy} value={draft.names} onChange={(event) => setDraft({ ...draft, names: event.target.value })} placeholder="API_TOKEN" /></label>
        </> : <>
          <label className="wide">HTTPS 地址<input className="input" type="url" required disabled={busy} value={draft.url} onChange={(event) => setDraft({ ...draft, url: event.target.value })} placeholder="https://example.com/mcp" /></label>
          <label className="wide">请求头名（每行一个）<textarea className="input" rows={3} disabled={busy} value={draft.names} onChange={(event) => setDraft({ ...draft, names: event.target.value })} placeholder="Authorization" /></label>
        </>}
      </div>
      <div className="mcp-editor-actions"><span>保存不会启动服务；凭据值稍后单独写入系统凭据库。</span><button className="btn primary" disabled={busy}>{busy ? "保存中…" : "保存配置"}</button></div>
    </form>
  );
}

function CredentialEditor({ server, onClose }: { server: McpServerView; onClose: () => void }) {
  const [statuses, setStatuses] = useState<McpCredentialStatus[] | null>(null);
  const [values, setValues] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const reload = useCallback(() => mcpCredentialStatus(server.id).then(setStatuses).catch((cause) => setError(errText(cause))), [server.id]);
  useEffect(() => { void reload(); }, [reload]);
  const save = async () => {
    setBusy(true);
    try {
      await Promise.all(Object.entries(values).filter(([, value]) => value.length > 0).map(([name, value]) => mcpSetCredential(server.id, name, value)));
      setValues({});
      await reload();
      setError(null);
    } catch (cause) { setError(errText(cause)); } finally { setBusy(false); }
  };
  const clear = async (name: string) => {
    setBusy(true);
    try {
      await mcpDeleteCredential(server.id, name);
      await reload();
      setError(null);
    } catch (cause) {
      setError(errText(cause));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="mcp-credentials">
      <div className="mcp-editor-head"><div><strong>{server.display_name} 的凭据</strong><span>值只写入操作系统凭据库；R-Code 永不回显已保存内容。</span></div><button className="btn ghost" onClick={onClose}>关闭</button></div>
      {error && <div className="mcp-banner error">{error}</div>}
      {statuses?.map((item) => <label key={item.name}><span><code>{item.name}</code><small>{item.configured ? "已配置" : "未配置"}</small></span><input className="input" type="password" autoComplete="off" value={values[item.name] ?? ""} onChange={(event) => setValues({ ...values, [item.name]: event.target.value })} placeholder={item.configured ? "输入新值以替换" : "输入凭据"} />{item.configured && <button className="btn ghost" disabled={busy} onClick={() => void clear(item.name)}>清除</button>}</label>)}
      {statuses?.length === 0 && <div className="mcp-empty">该服务没有声明凭据字段。</div>}
      <div className="mcp-editor-actions"><span /><button className="btn primary" disabled={busy || !Object.values(values).some(Boolean)} onClick={() => void save()}>{busy ? "保存中…" : "保存凭据"}</button></div>
    </div>
  );
}

function MarketRow({ server, busyKeys, onInstall }: { server: McpMarketServer; busyKeys: ReadonlySet<string>; onInstall: (server: McpMarketServer, optionId: string, serverId: string) => void }) {
  const [serverId, setServerId] = useState(server.suggested_id);
  return (
    <article className="mcp-market-row">
      <div><div className="mcp-server-title"><strong>{server.title}</strong><code>{server.version}</code></div><p>{server.description || server.name}</p><small>{server.repository_url ?? server.name}</small></div>
      <div className="mcp-market-install"><input className="input" aria-label={`${server.title} 的本机 ID`} value={serverId} onChange={(event) => setServerId(event.target.value)} />{server.install_options.map((option) => <button key={option.id} className="btn" disabled={!serverId.trim() || busyKeys.has(`prepare:${server.name}:${option.id}`)} onClick={() => onInstall(server, option.id, serverId)}>{busyKeys.has(`prepare:${server.name}:${option.id}`) ? "准备中…" : option.label}</button>)}</div>
    </article>
  );
}

function ApprovalPanel({ approval, busy, onCancel, onConfirm }: { approval: ApprovalAction; busy: boolean; onCancel: () => void; onConfirm: () => void }) {
  const description = previewDescription(approval.preview);
  return (
    <div className="mcp-approval" role="alertdialog" aria-label="确认 MCP 启动方案">
      <div><span>EXACT LAUNCH PLAN</span><h3>{approval.kind === "enable" ? "确认启用这个 MCP？" : "确认添加这个 MCP？"}</h3><p>请核对完整启动形态。令牌五分钟内有效且只能使用一次；配置发生变化后会自动失效。</p></div>
      <pre>{description}</pre>
      <div className="mcp-approval-actions"><button className="btn" disabled={busy} onClick={onCancel}>取消</button><button className="btn primary" disabled={busy} onClick={onConfirm}>{busy ? "正在处理…" : approval.kind === "enable" ? "确认并启用" : "确认添加"}</button></div>
    </div>
  );
}

function transportSummary(server: McpServerView) {
  if (server.transport.type === "builtin") return "R-Code 内置进程内服务";
  if (server.transport.type === "stdio") return `${server.transport.executable} ${server.transport.args.join(" ")}`.trim();
  return server.transport.url;
}

function previewDescription(preview: McpLaunchPreview) {
  if (preview.transport.type === "stdio") {
    return [`可执行文件: ${preview.transport.executable}`, ...preview.transport.args.map((arg, index) => `参数 ${index + 1}: ${arg}`), `环境变量名: ${preview.transport.environment_names.join(", ") || "无"}`].join("\n");
  }
  return [`地址: ${preview.transport.url}`, `请求头名: ${preview.transport.header_names.join(", ") || "无"}`].join("\n");
}

function stateLabel(state: McpServerState, enabled: boolean) {
  if (!enabled || state === "disabled") return "已关闭";
  if (state === "starting") return "连接中";
  if (state === "running") return "已连接";
  if (state === "error") return "连接错误";
  return "按需就绪";
}

/**
 * 工具调用卡片 —— 时间线里 tool 条目的唯一出口。
 *
 * 折叠态保持原来那一行的信息密度（动词 + 目标 + 结果摘要），
 * 展开后才把完整输入 / 输出摊开。这个「默认折叠、按需展开」是刻意的：
 * 一次运行动辄几十个工具调用，全展开会把对话本身淹没。
 *
 * 三条实现约束：
 * 1) 载荷 **只在展开时**才解析和高亮（useMemo 依赖 open），折叠态零开销 ——
 *    否则长会话里几十个卡片会在每次流式 token 上重复做正则扫描。
 * 2) 代码块复用 markdown.css 的 .md-code* 一套样式，保持与 agent 正文里
 *    代码块的观感一致；外层挂 .md 只为拿到 --md-* 高亮色板变量。
 * 3) 整个头部是一个真 <button>，Enter/Space 天然可用，不需要自己补键盘处理。
 */
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { highlight } from "../../lib/highlight";
import { COPIED_RESET_MS, copyText } from "../../lib/clipboard";
import { toolVerb } from "../../lib/format";
import { mcpMarketInstall, mcpToggle } from "../../lib/ipc";
import type { McpLaunchPreview, McpMarketInstallRequest } from "../../lib/types";
import { useAppStore } from "../../store/app";
import { formatToolPayload, type ToolState } from "./model";

/** 载荷折叠阈值（行）；与 markdown.css 的 --md-clip-lines 同源，保持观感一致。 */
const CLIP_LINES = 16;

export interface ToolCardProps {
  name: string;
  target: string;
  state: ToolState;
  summary: string;
  inputJson: string | null;
  outputJson: string | null;
  /** 回放播放头调暗用的附加 class。 */
  dim?: string;
  /** 相对会话起点秒数（data-t）。 */
  t: number;
}

export const ToolCard = memo(function ToolCard({
  name,
  target,
  state,
  summary,
  inputJson,
  outputJson,
  dim = "",
  t,
}: ToolCardProps) {
  const hasMcpConfirmation = hasMcpConfirmationPayload(name, outputJson);
  const hasMcpSettingsAction = hasMcpSettingsActionPayload(name, outputJson);
  const [open, setOpen] = useState(hasMcpConfirmation || hasMcpSettingsAction);
  // 判据必须与 formatToolPayload 一致（它对纯空白返回 null），
  // 否则会出现「按钮可展开、展开后写着没有载荷」。
  const hasPayload = Boolean(inputJson?.trim() || outputJson?.trim());

  // Agent 只负责准备精确方案，真正安装/启用必须由用户点击确认。结果通常在工具
  // 从 active 变为 ok 时才到达，因此不能只依赖 useState 的首次初始化。
  useEffect(() => {
    if (hasMcpConfirmation || hasMcpSettingsAction) setOpen(true);
  }, [hasMcpConfirmation, hasMcpSettingsAction]);

  return (
    <div
      className={
        "tcard" +
        (state === "active" ? " active" : "") +
        (state === "fail" ? " fail" : "") +
        (open ? " open" : "") +
        dim
      }
      data-t={t}
    >
      <button
        type="button"
        /* ring-inset：运行中的卡片有 overflow:hidden（底部扫光），外描边会被裁掉 */
        className="tcard-head ring-inset"
        aria-expanded={hasPayload ? open : undefined}
        disabled={!hasPayload}
        onClick={() => setOpen((v) => !v)}
        title={hasPayload ? undefined : "该调用没有可展开的记录"}
      >
        {state === "active" ? (
          <span className="spin" aria-hidden="true" />
        ) : (
          <span className="tcard-chevron" aria-hidden="true">
            {hasPayload ? (open ? "▾" : "▸") : "·"}
          </span>
        )}
        <span className="verb">{toolVerb(name)}</span>
        <span className="target">{target || name}</span>
        {state === "ok" && <span className="ok">✓ {summary}</span>}
        {state === "fail" && <span className="fail">✗ {summary}</span>}
        {hasPayload && <span className="sr-only">{open ? "收起详情" : "展开详情"}</span>}
      </button>

      {open && (
        <div className="tcard-body">
          <ToolPayloadDetails
            toolName={name}
            inputJson={inputJson}
            outputJson={outputJson}
            state={state}
          />
        </div>
      )}
    </div>
  );
});

/**
 * 已展开工具的公开载荷。活动分组和独立 ToolCard 共用这一出口，避免单命令展开后
 * 再套一层重复标题。组件只在父级真正展开时挂载，因此解析成本仍然按需发生。
 */
export const ToolPayloadDetails = memo(function ToolPayloadDetails({
  toolName,
  inputJson,
  outputJson,
  state,
}: Pick<ToolCardProps, "inputJson" | "outputJson" | "state"> & { toolName?: string }) {
  const openMcpSettings = useAppStore((store) => store.openMcpSettings);
  const input = useMemo(() => formatToolPayload(inputJson, "input"), [inputJson]);
  const output = useMemo(() => formatToolPayload(outputJson, "output"), [outputJson]);
  const mcpSuggestion = useMemo(() => readMcpSuggestion(toolName, outputJson), [toolName, outputJson]);
  const mcpConfirmation = useMemo(
    () => readMcpConfirmation(toolName, outputJson),
    [toolName, outputJson],
  );
  const mcpSettingsAction = useMemo(
    () => readMcpSettingsAction(toolName, outputJson),
    [toolName, outputJson],
  );

  return (
    <>
      {mcpConfirmation && <McpConfirmationCard action={mcpConfirmation} />}
      {mcpSettingsAction && <McpSettingsCard action={mcpSettingsAction} />}
      {mcpSuggestion && (
        <div className="tcard-mcp-suggestion">
          <div>
            <strong>可启用 MCP 扩展</strong>
            <span>{mcpSuggestion.reason}</span>
          </div>
          <button
            type="button"
            className="btn"
            onClick={() => openMcpSettings(mcpSuggestion.marketQuery)}
          >
            打开 MCP 配置
          </button>
        </div>
      )}
      {!mcpConfirmation && !mcpSettingsAction && input && <Payload label="输入" view={input} />}
      {!mcpConfirmation && !mcpSettingsAction && output && (
        <Payload label={state === "fail" ? "错误输出" : "输出"} view={output} tone={state} />
      )}
      {!mcpConfirmation && !mcpSettingsAction && !input && !output && <div className="tcard-empty">没有记录到载荷。</div>}
    </>
  );
});

type McpConfirmationAction =
  | {
      kind: "install";
      message: string;
      request: McpMarketInstallRequest;
      preview: McpLaunchPreview;
    }
  | {
      kind: "enable";
      message: string;
      serverId: string;
      preview: McpLaunchPreview | null;
    };

function McpConfirmationCard({ action }: { action: McpConfirmationAction }) {
  const openMcpSettings = useAppStore((store) => store.openMcpSettings);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const preview = action.preview;
  const expired = preview
    ? Number.isFinite(Date.parse(preview.expires_at)) && Date.parse(preview.expires_at) <= Date.now()
    : false;
  const title = action.kind === "install"
    ? `添加 ${action.request.server.title}`
    : `启用 ${action.serverId}`;
  const serverId = action.kind === "install" ? action.request.server_id : action.serverId;

  const confirm = async () => {
    setBusy(true);
    setError(null);
    try {
      if (action.kind === "install") {
        await mcpMarketInstall(action.request, action.preview.token);
        setDone("MCP 已安全添加并保持关闭。配置所需凭据后，可再确认启用。");
      } else {
        const result = await mcpToggle(action.serverId, true, action.preview?.token ?? null);
        if (result.confirmation) {
          throw new Error("启动方案已经变化，请重新发起启用确认。");
        }
        setDone("MCP 已启用；服务仍按需启动，不会在后台空转。");
      }
    } catch (cause) {
      console.error("MCP confirmation action failed", cause);
      setError("操作未完成，确认可能已失效。请让 Agent 重新准备；详细原因可在诊断日志中查看。");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="tcard-mcp-confirmation" role="group" aria-label={title}>
      <div className="tcard-mcp-confirmation-head">
        <span>MCP · 需要你的确认</span>
        <strong>{title}</strong>
        <p>{action.message}</p>
      </div>
      {preview ? <McpLaunchPlan preview={preview} /> : (
        <div className="tcard-mcp-launch builtin"><span>启动方案已审批</span><code>{serverId}</code></div>
      )}
      <p className="tcard-mcp-risk">
        {action.kind === "install"
          ? "Registry 仍处于预览阶段，条目未经 R-Code 或 Registry 安全审核。添加只写入本机配置，不会立即运行。"
          : "确认后该服务可在 Agent 实际调用时启动；本地服务将拥有其进程自身的系统权限。"}
      </p>
      {error && <div className="tcard-mcp-result is-error" role="alert">{error}</div>}
      {done && <div className="tcard-mcp-result" role="status">{done}</div>}
      <div className="tcard-mcp-actions">
        {done ? (
          <button type="button" className="btn" onClick={() => openMcpSettings(null, serverId)}>
            打开 MCP 管理
          </button>
        ) : (
          <>
            <button type="button" className="btn" onClick={() => openMcpSettings(null, serverId)}>
              稍后处理
            </button>
            <button type="button" className="btn accent" disabled={busy || expired} onClick={() => void confirm()}>
              {busy ? "处理中…" : expired ? "确认已过期" : action.kind === "install" ? "确认添加" : "确认并启用"}
            </button>
          </>
        )}
      </div>
    </div>
  );
}

type McpSettingsAction = {
  kind: "draft_created" | "manual_enable_required";
  message: string;
  serverId: string;
  sourcePath: string | null;
};

function McpSettingsCard({ action }: { action: McpSettingsAction }) {
  const openMcpSettings = useAppStore((store) => store.openMcpSettings);
  const draftCreated = action.kind === "draft_created";
  return (
    <div className="tcard-mcp-draft" role="group" aria-label="MCP 草稿待审核">
      <div className="tcard-mcp-draft-mark" aria-hidden="true">MCP</div>
      <div className="tcard-mcp-draft-copy">
        <span>{draftCreated ? "已保存为禁用草稿" : "必须由你手动启用"}</span>
        <strong>{draftCreated ? "草稿已创建，尚未启用" : "前往设置审核 MCP 草稿"}</strong>
        <p>{action.message}</p>
        {action.sourcePath && <code title={action.sourcePath}>{action.sourcePath}</code>}
      </div>
      <button type="button" className="btn accent" onClick={() => openMcpSettings(null, action.serverId)}>
        前往设置审核
      </button>
    </div>
  );
}

function McpLaunchPlan({ preview }: { preview: McpLaunchPreview }) {
  const transport = preview.transport;
  if (transport.type === "stdio") {
    return (
      <div className="tcard-mcp-launch">
        <span>本机进程</span>
        <pre>{[
          `可执行文件: ${transport.executable}`,
          ...transport.args.map((arg, index) => `参数 ${index + 1}: ${arg}`),
        ].join("\n")}</pre>
        {transport.environment_names.length > 0 && (
          <small>凭据环境变量：{transport.environment_names.join("、")}</small>
        )}
      </div>
    );
  }
  return (
    <div className="tcard-mcp-launch">
      <span>远程 HTTPS</span>
      <pre>{`地址: ${transport.url}`}</pre>
      {transport.header_names.length > 0 && <small>凭据请求头：{transport.header_names.join("、")}</small>}
    </div>
  );
}

export function hasMcpConfirmationPayload(
  toolName: string | null | undefined,
  raw: string | null | undefined,
): boolean {
  if (toolName !== "mcp_prepare_install" && toolName !== "mcp_prepare_enable") return false;
  return Boolean(raw && raw.length <= 128_000 && /"action"\s*:\s*"confirm_mcp_(?:install|enable)"/.test(raw));
}

export function hasMcpSettingsActionPayload(
  toolName: string | null | undefined,
  raw: string | null | undefined,
): boolean {
  if (toolName !== "mcp_create_draft" && toolName !== "mcp_prepare_enable") return false;
  return Boolean(raw
    && raw.length <= 128_000
    && /"status"\s*:\s*"(?:draft_created|manual_enable_required)"/.test(raw)
    && /"action"\s*:\s*"open_mcp_settings"/.test(raw));
}

function readMcpSettingsAction(
  toolName: string | null | undefined,
  raw: string | null | undefined,
): McpSettingsAction | null {
  if (!hasMcpSettingsActionPayload(toolName, raw)) return null;
  try {
    const value: unknown = JSON.parse(raw as string);
    if (!isRecord(value) || value.action !== "open_mcp_settings") return null;
    const expectedStatus = toolName === "mcp_create_draft" ? "draft_created" : "manual_enable_required";
    if (value.status !== expectedStatus) return null;
    const serverId = typeof value.server_id === "string" ? value.server_id.trim() : "";
    if (!serverId) return null;
    const message = typeof value.message === "string" && value.message.trim()
      ? value.message.trim()
      : "请在“设置 → 工具与连接”中核对启动方案、配置凭据并亲自打开滑钮。";
    return {
      kind: expectedStatus,
      message,
      serverId,
      sourcePath: typeof value.source_path === "string" && value.source_path.trim()
        ? value.source_path.trim()
        : null,
    };
  } catch {
    return null;
  }
}

function readMcpConfirmation(
  toolName: string | null | undefined,
  raw: string | null | undefined,
): McpConfirmationAction | null {
  if (!hasMcpConfirmationPayload(toolName, raw)) return null;
  try {
    const value: unknown = JSON.parse(raw as string);
    if (!isRecord(value) || value.status !== "confirmation_required") return null;
    const message = typeof value.message === "string" && value.message.trim()
      ? value.message.trim()
      : "请核对完整启动方案后再继续。";
    if (toolName === "mcp_prepare_install" && value.action === "confirm_mcp_install") {
      const request = readInstallRequest(value.request);
      const preview = readLaunchPreview(value.preview);
      if (!request || !preview || preview.server_id !== request.server_id) return null;
      return { kind: "install", message, request, preview };
    }
    if (toolName === "mcp_prepare_enable" && value.action === "confirm_mcp_enable") {
      const serverId = typeof value.server_id === "string" ? value.server_id.trim() : "";
      if (!serverId) return null;
      const preview = value.preview == null ? null : readLaunchPreview(value.preview);
      if (value.preview != null && !preview) return null;
      if (preview && preview.server_id !== serverId) return null;
      return { kind: "enable", message, serverId, preview };
    }
    return null;
  } catch {
    return null;
  }
}

function readLaunchPreview(value: unknown): McpLaunchPreview | null {
  if (!isRecord(value) || !isRecord(value.transport)) return null;
  const transport = value.transport;
  const common = typeof value.token === "string"
    && typeof value.server_id === "string"
    && typeof value.fingerprint === "string"
    && typeof value.expires_at === "string";
  if (!common) return null;
  if (transport.type === "stdio") {
    if (typeof transport.executable !== "string"
      || !isStringArray(transport.args)
      || !isStringArray(transport.environment_names)) return null;
  } else if (transport.type === "streamable_http") {
    if (typeof transport.url !== "string" || !isStringArray(transport.header_names)) return null;
  } else {
    return null;
  }
  return value as unknown as McpLaunchPreview;
}

function readInstallRequest(value: unknown): McpMarketInstallRequest | null {
  if (!isRecord(value) || !isRecord(value.server)) return null;
  const server = value.server;
  if (typeof value.option_id !== "string"
    || typeof value.server_id !== "string"
    || typeof server.name !== "string"
    || typeof server.title !== "string"
    || typeof server.version !== "string"
    || !Array.isArray(server.install_options)) return null;
  const selected = server.install_options.some((option) => isRecord(option) && option.id === value.option_id);
  return selected ? value as unknown as McpMarketInstallRequest : null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value && typeof value === "object" && !Array.isArray(value));
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function readMcpSuggestion(
  toolName: string | null | undefined,
  raw: string | null | undefined,
): { reason: string; marketQuery: string | null } | null {
  if (toolName !== "suggest_mcp" || !raw?.trim() || raw.length > 16_000) return null;
  try {
    const value = JSON.parse(raw) as Record<string, unknown>;
    if (value.action !== "open_mcp_settings") return null;
    return {
      reason: typeof value.reason === "string" && value.reason.trim()
        ? value.reason.trim()
        : "当前任务可能受益于尚未启用的 MCP 工具。",
      marketQuery: typeof value.market_query === "string" && value.market_query.trim()
        ? value.market_query.trim()
        : null,
    };
  } catch {
    return null;
  }
}

function Payload({
  label,
  view,
  tone,
}: {
  label: string;
  view: NonNullable<ReturnType<typeof formatToolPayload>>;
  tone?: ToolState;
}) {
  const [copied, setCopied] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const resetTimer = useRef<number | null>(null);

  useEffect(
    () => () => {
      if (resetTimer.current !== null) window.clearTimeout(resetTimer.current);
    },
    []
  );

  const collapsible = view.lines > CLIP_LINES;
  const clipped = collapsible && !expanded;
  // CSS used to hide the tail only after every token had already been highlighted and mounted.
  // Keep the full (already safety-clamped) payload for copy/expand, but render sixteen lines while
  // collapsed so large command output cannot stall scroll or the first expand interaction.
  const preview = useMemo(
    () => collapsible ? firstLines(view.text, CLIP_LINES) : view.text,
    [collapsible, view.text],
  );
  const renderedText = clipped ? preview : view.text;
  const tokens = useMemo(() => highlight(renderedText, view.lang), [renderedText, view.lang]);

  useEffect(() => {
    setExpanded(false);
  }, [view.text]);

  const onCopy = useCallback(() => {
    void copyText(view.text).then((ok) => {
      if (!ok) return;
      setCopied(true);
      if (resetTimer.current !== null) window.clearTimeout(resetTimer.current);
      resetTimer.current = window.setTimeout(() => setCopied(false), COPIED_RESET_MS);
    });
  }, [view.text]);

  // .md-code 必须是 .md 的**后代**：markdown.css 的规则是 `.md .md-code`（后代选择器），
  // 把两个类挂在同一个元素上会让整条框体样式（边框/圆角/底色/overflow）全部失配。
  return (
    <div className={"md tcard-payload" + (tone === "fail" ? " is-fail" : "")}>
      <div className="md-code">
        <div className="md-code-head">
          <span className="md-code-lang">
            {label}
            <span className="tcard-payload-meta">
              {view.lang ?? "text"} · {view.lines} 行{view.truncated ? " · 已截断" : ""}
            </span>
          </span>
          <button
            type="button"
            className="md-code-copy"
            onClick={onCopy}
            aria-label={copied ? `${label}已复制到剪贴板` : `复制${label}`}
          >
            {copied ? "已复制" : "复制"}
          </button>
        </div>
        <div className={"md-code-body" + (clipped ? " is-clipped" : "")}>
          <pre className="md-pre">
            <code>
              {tokens.map((tok, i) =>
                tok.cls ? (
                  <span key={i} className={tok.cls}>
                    {tok.text}
                  </span>
                ) : (
                  <span key={i}>{tok.text}</span>
                )
              )}
            </code>
          </pre>
          {clipped && <span className="md-code-fade" aria-hidden="true" />}
        </div>
        {collapsible && (
          <button
            type="button"
            className="md-code-toggle"
            aria-expanded={expanded}
            onClick={() => setExpanded((v) => !v)}
          >
            {expanded ? "收起" : `展开全部 · ${view.lines} 行`}
          </button>
        )}
      </div>
      {view.truncated && (
        <div className="tcard-truncated">输出过长，已截断展示；完整内容请在终端或文件中查看。</div>
      )}
    </div>
  );
}

function firstLines(value: string, count: number): string {
  if (count <= 0 || value.length === 0) return "";
  let cursor = 0;
  for (let line = 0; line < count; line += 1) {
    const newline = value.indexOf("\n", cursor);
    if (newline === -1) return value;
    if (line === count - 1) return value.slice(0, newline);
    cursor = newline + 1;
  }
  return value;
}

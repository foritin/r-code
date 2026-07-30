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
  const [open, setOpen] = useState(false);
  // 判据必须与 formatToolPayload 一致（它对纯空白返回 null），
  // 否则会出现「按钮可展开、展开后写着没有载荷」。
  const hasPayload = Boolean(inputJson?.trim() || outputJson?.trim());

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
  inputJson,
  outputJson,
  state,
}: Pick<ToolCardProps, "inputJson" | "outputJson" | "state">) {
  const input = useMemo(() => formatToolPayload(inputJson, "input"), [inputJson]);
  const output = useMemo(() => formatToolPayload(outputJson, "output"), [outputJson]);

  return (
    <>
      {input && <Payload label="输入" view={input} />}
      {output && (
        <Payload label={state === "fail" ? "错误输出" : "输出"} view={output} tone={state} />
      )}
      {!input && !output && <div className="tcard-empty">没有记录到载荷。</div>}
    </>
  );
});

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

  const tokens = useMemo(() => highlight(view.text, view.lang), [view.text, view.lang]);
  const collapsible = view.lines > CLIP_LINES;
  const clipped = collapsible && !expanded;

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

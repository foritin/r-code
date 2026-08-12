/**
 * Markdown 渲染 —— 时间线里 agent 文本的唯一出口。
 *
 * 三条硬规矩：
 * 1) 全程走 React 元素构造，没有 dangerouslySetInnerHTML；转义交给 React。
 * 2) href 已在 parseMarkdown 里过白名单（http/https/mailto/file），
 *    渲染层只负责补 target/rel，不再自己拼 URL。
 * 3) 解析结果按 text 记忆化 + 组件 memo：流式下每个 token 都会重渲染，
 *    重复解析整段是这里最贵的一笔开销。
 */
import { Fragment, memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import {
  parseMarkdown,
  inlineToText,
  isLocalResourceUrl,
  type MdCode,
  type MdInline,
  type MdList,
  type MdNode,
  type MdTable,
} from "../../lib/markdown";
import { highlight } from "../../lib/highlight";
import { COPIED_RESET_MS, copyText } from "../../lib/clipboard";
import { isLocalRasterReference, LocalFileLink, LocalImageArtifact } from "./LocalResource";

export interface MarkdownProps {
  text: string;
  /** true 时在最后一个文本块末尾附加流式光标 <span className="caret" /> */
  streaming?: boolean;
  /** Local resource links are resolved against this task/workspace instead of navigating WebView. */
  taskId?: string;
  workspacePath?: string | null;
}

interface RenderContext {
  taskId?: string;
  workspacePath: string | null;
}

/** 代码块折叠阈值；与 markdown.css 的 --md-clip-lines 保持一致。 */
const COLLAPSE_LINES = 16;

export const Markdown = memo(function Markdown({
  text,
  streaming = false,
  taskId,
  workspacePath = null,
}: MarkdownProps) {
  const nodes = useMemo(() => parseMarkdown(text), [text]);
  const context = useMemo<RenderContext>(() => ({ taskId, workspacePath }), [taskId, workspacePath]);

  const last = nodes.length - 1;
  const tail = last >= 0 ? nodes[last] : null;
  // 段落/标题可以把光标塞进行尾；代码块、表格、列表则让光标另起一行，
  // 否则它会被塞进 <pre> 或单元格里。
  const inlineCaret =
    streaming && tail !== null && (tail.type === "paragraph" || tail.type === "heading");

  return (
    <div className="md">
      {nodes.map((node, i) => (
        <Block
          key={i}
          node={node}
          caret={inlineCaret && i === last}
          className={`md-block${streaming && i === last ? " is-live" : ""}`}
          context={context}
        />
      ))}
      {streaming && !inlineCaret && <span className="caret md-caret" />}
    </div>
  );
});

/* ------------------------------------------------------------------ 块级 */

function Block({
  node,
  caret = false,
  className,
  context,
}: {
  node: MdNode;
  caret?: boolean;
  className?: string;
  context: RenderContext;
}): ReactNode {
  switch (node.type) {
    case "paragraph":
      return (
        <p className={className}>
          {renderInline(node.children, context)}
          {caret && <span className="caret" />}
        </p>
      );
    case "heading": {
      const content = (
        <>
          {renderInline(node.children, context)}
          {caret && <span className="caret" />}
        </>
      );
      if (node.depth === 1) return <h1 className={className}>{content}</h1>;
      if (node.depth === 2) return <h2 className={className}>{content}</h2>;
      if (node.depth === 3) return <h3 className={className}>{content}</h3>;
      if (node.depth === 4) return <h4 className={className}>{content}</h4>;
      if (node.depth === 5) return <h5 className={className}>{content}</h5>;
      return <h6 className={className}>{content}</h6>;
    }
    case "code":
      return <CodeBlock node={node} className={className} />;
    case "hr":
      return <hr className={className} />;
    case "blockquote":
      return (
        <blockquote className={className}>
          {node.children.map((child, i) => (
            <Block key={i} node={child} context={context} />
          ))}
        </blockquote>
      );
    case "list":
      return <List node={node} className={className} context={context} />;
    case "table":
      return <Table node={node} className={className} context={context} />;
  }
}

function List({
  node,
  className,
  context,
}: {
  node: MdList;
  className?: string;
  context: RenderContext;
}) {
  const items = node.items.map((item, i) => {
    const body = item.children.map((child, j) =>
      // 紧凑列表里的段落不套 <p>，避免每项都多出一段外边距
      node.tight && child.type === "paragraph" ? (
        <Fragment key={j}>{renderInline(child.children, context)}</Fragment>
      ) : (
        <Block key={j} node={child} context={context} />
      )
    );
    // 普通项直接把内容挂在 <li> 上（嵌套 <ul> 不能塞进 <span>）；
    // 任务项才需要一层 flex 子项来跟复选框对齐。
    if (item.checked === null) return <li key={i}>{body}</li>;
    return (
      <li key={i} className="md-task">
        <input type="checkbox" checked={item.checked} disabled readOnly tabIndex={-1} />
        <span className="md-li-body">{body}</span>
      </li>
    );
  });

  const listClassName = [className, node.tight ? "md-tight" : null].filter(Boolean).join(" ") || undefined;
  return node.ordered ? (
    <ol className={listClassName} start={node.start === 1 ? undefined : node.start}>
      {items}
    </ol>
  ) : (
    <ul className={listClassName}>{items}</ul>
  );
}

function Table({
  node,
  className,
  context,
}: {
  node: MdTable;
  className?: string;
  context: RenderContext;
}) {
  const style = (i: number): CSSProperties | undefined => {
    const align = node.align[i];
    return align ? { textAlign: align } : undefined;
  };
  return (
    <div className={["md-table-wrap", className].filter(Boolean).join(" ")}>
      <table>
        <thead>
          <tr>
            {node.header.map((cell, i) => (
              <th key={i} style={style(i)}>
                {renderInline(cell.children, context)}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {node.rows.map((row, r) => (
            <tr key={r}>
              {row.map((cell, c) => (
                <td key={c} style={style(c)}>
                  {renderInline(cell.children, context)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/* -------------------------------------------------------------- 代码块 */

function inspectCode(value: string): { lineCount: number; preview: string } {
  if (value.length === 0) return { lineCount: 0, preview: "" };

  let lineCount = 1;
  let previewEnd = -1;
  let cursor = 0;
  while (cursor < value.length) {
    const newline = value.indexOf("\n", cursor);
    if (newline === -1) break;
    if (lineCount === COLLAPSE_LINES && previewEnd === -1) previewEnd = newline;
    lineCount += 1;
    cursor = newline + 1;
  }

  return {
    lineCount,
    preview: previewEnd === -1 ? value : value.slice(0, previewEnd),
  };
}

function CodeBlock({ node, className }: { node: MdCode; className?: string }) {
  const [copied, setCopied] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const resetTimer = useRef<number | null>(null);

  useEffect(
    () => () => {
      if (resetTimer.current !== null) window.clearTimeout(resetTimer.current);
    },
    []
  );

  const { lineCount, preview } = useMemo(() => inspectCode(node.value), [node.value]);

  const collapsible = lineCount > COLLAPSE_LINES;
  const clipped = collapsible && !expanded;
  // Collapsed code used to syntax-highlight and mount every line before CSS hid the tail. A long
  // tool result could therefore create thousands of spans even though only 16 lines were visible.
  // Keep the complete source for copy/expand, but do parsing and DOM work only for the preview.
  const renderedValue = clipped ? preview : node.value;
  const tokens = useMemo(() => highlight(renderedValue, node.lang), [renderedValue, node.lang]);

  useEffect(() => {
    // Streaming can replace a block in place. Never keep an expanded state from an older, longer
    // code payload after the source itself changes.
    setExpanded(false);
  }, [node.value]);

  const onCopy = useCallback(() => {
    void copyText(node.value).then((ok) => {
      if (!ok) return;
      setCopied(true);
      if (resetTimer.current !== null) window.clearTimeout(resetTimer.current);
      resetTimer.current = window.setTimeout(() => setCopied(false), COPIED_RESET_MS);
    });
  }, [node.value]);

  return (
    <div className={["md-code", className].filter(Boolean).join(" ")}>
      <div className="md-code-head">
        <span className="md-code-lang">{node.lang ?? "text"}</span>
        <button
          type="button"
          className="md-code-copy"
          aria-label={copied ? "代码已复制到剪贴板" : "复制代码"}
          onClick={onCopy}
        >
          {copied ? "已复制" : "复制"}
        </button>
      </div>
      <div className={clipped ? "md-code-body is-clipped" : "md-code-body"}>
        <pre className="md-pre">
          <code>
            {tokens.map((token, i) =>
              token.cls === null ? (
                token.text
              ) : (
                <span key={i} className={token.cls}>
                  {token.text}
                </span>
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
          {expanded ? "收起" : `展开全部 · ${lineCount} 行`}
        </button>
      )}
    </div>
  );
}

/* ------------------------------------------------------------------ 行内 */

function renderInline(nodes: MdInline[], context: RenderContext): ReactNode[] {
  return nodes.map((node, i) => {
    switch (node.type) {
      case "text":
        return node.value;
      case "break":
        return <br key={i} />;
      case "codespan":
        return (
          <code key={i} className="md-inline-code">
            {node.value}
          </code>
        );
      case "strong":
        return <strong key={i}>{renderInline(node.children, context)}</strong>;
      case "em":
        return <em key={i}>{renderInline(node.children, context)}</em>;
      case "del":
        return <del key={i}>{renderInline(node.children, context)}</del>;
      case "link": {
        if (isLocalResourceUrl(node.href)) {
          const label = inlineToText(node.children) || node.href;
          if (isLocalRasterReference(node.href)) {
            return (
              <LocalImageArtifact
                key={i}
                href={node.href}
                alt={label}
                label={label}
                taskId={context.taskId}
                workspacePath={context.workspacePath}
              />
            );
          }
          return (
            <LocalFileLink
              key={i}
              href={node.href}
              title={node.title ?? undefined}
              taskId={context.taskId}
              workspacePath={context.workspacePath}
            >
              {renderInline(node.children, context)}
            </LocalFileLink>
          );
        }
        return (
          <a
            key={i}
            className="md-link"
            href={node.href}
            title={node.title ?? undefined}
            target="_blank"
            rel="noopener noreferrer"
          >
            {renderInline(node.children, context)}
          </a>
        );
      }
      case "image":
        if (isLocalResourceUrl(node.href) && isLocalRasterReference(node.href)) {
          return (
            <LocalImageArtifact
              key={i}
              href={node.href}
              alt={node.alt}
              label={node.alt || "图片产物"}
              taskId={context.taskId}
              workspacePath={context.workspacePath}
            />
          );
        }
        // Remote images remain inert links; the app never loads model-provided remote pixels.
        return (
          <a
            key={i}
            className="md-link md-image"
            href={node.href}
            title={node.href}
            target="_blank"
            rel="noopener noreferrer"
          >
            {node.alt || node.href}
          </a>
        );
    }
  });
}

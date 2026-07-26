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
  type MdCode,
  type MdInline,
  type MdList,
  type MdNode,
  type MdTable,
} from "../../lib/markdown";
import { highlight } from "../../lib/highlight";
import { COPIED_RESET_MS, copyText } from "../../lib/clipboard";

export interface MarkdownProps {
  text: string;
  /** true 时在最后一个文本块末尾附加流式光标 <span className="caret" /> */
  streaming?: boolean;
}

/** 代码块折叠阈值；与 markdown.css 的 --md-clip-lines 保持一致。 */
const COLLAPSE_LINES = 16;

export const Markdown = memo(function Markdown({ text, streaming = false }: MarkdownProps) {
  const nodes = useMemo(() => parseMarkdown(text), [text]);

  const last = nodes.length - 1;
  const tail = last >= 0 ? nodes[last] : null;
  // 段落/标题可以把光标塞进行尾；代码块、表格、列表则让光标另起一行，
  // 否则它会被塞进 <pre> 或单元格里。
  const inlineCaret =
    streaming && tail !== null && (tail.type === "paragraph" || tail.type === "heading");

  return (
    <div className="md">
      {nodes.map((node, i) => (
        <Block key={i} node={node} caret={inlineCaret && i === last} />
      ))}
      {streaming && !inlineCaret && <span className="caret md-caret" />}
    </div>
  );
});

/* ------------------------------------------------------------------ 块级 */

function Block({ node, caret = false }: { node: MdNode; caret?: boolean }): ReactNode {
  switch (node.type) {
    case "paragraph":
      return (
        <p>
          {renderInline(node.children)}
          {caret && <span className="caret" />}
        </p>
      );
    case "heading": {
      const content = (
        <>
          {renderInline(node.children)}
          {caret && <span className="caret" />}
        </>
      );
      if (node.depth === 1) return <h1>{content}</h1>;
      if (node.depth === 2) return <h2>{content}</h2>;
      if (node.depth === 3) return <h3>{content}</h3>;
      if (node.depth === 4) return <h4>{content}</h4>;
      if (node.depth === 5) return <h5>{content}</h5>;
      return <h6>{content}</h6>;
    }
    case "code":
      return <CodeBlock node={node} />;
    case "hr":
      return <hr />;
    case "blockquote":
      return (
        <blockquote>
          {node.children.map((child, i) => (
            <Block key={i} node={child} />
          ))}
        </blockquote>
      );
    case "list":
      return <List node={node} />;
    case "table":
      return <Table node={node} />;
  }
}

function List({ node }: { node: MdList }) {
  const items = node.items.map((item, i) => {
    const body = item.children.map((child, j) =>
      // 紧凑列表里的段落不套 <p>，避免每项都多出一段外边距
      node.tight && child.type === "paragraph" ? (
        <Fragment key={j}>{renderInline(child.children)}</Fragment>
      ) : (
        <Block key={j} node={child} />
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

  const className = node.tight ? "md-tight" : undefined;
  return node.ordered ? (
    <ol className={className} start={node.start === 1 ? undefined : node.start}>
      {items}
    </ol>
  ) : (
    <ul className={className}>{items}</ul>
  );
}

function Table({ node }: { node: MdTable }) {
  const style = (i: number): CSSProperties | undefined => {
    const align = node.align[i];
    return align ? { textAlign: align } : undefined;
  };
  return (
    <div className="md-table-wrap">
      <table>
        <thead>
          <tr>
            {node.header.map((cell, i) => (
              <th key={i} style={style(i)}>
                {renderInline(cell.children)}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {node.rows.map((row, r) => (
            <tr key={r}>
              {row.map((cell, c) => (
                <td key={c} style={style(c)}>
                  {renderInline(cell.children)}
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

function CodeBlock({ node }: { node: MdCode }) {
  const [copied, setCopied] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const resetTimer = useRef<number | null>(null);

  useEffect(
    () => () => {
      if (resetTimer.current !== null) window.clearTimeout(resetTimer.current);
    },
    []
  );

  const tokens = useMemo(() => highlight(node.value, node.lang), [node.value, node.lang]);
  const lineCount = useMemo(
    () => (node.value.length === 0 ? 0 : node.value.split("\n").length),
    [node.value]
  );

  const collapsible = lineCount > COLLAPSE_LINES;
  const clipped = collapsible && !expanded;

  const onCopy = useCallback(() => {
    void copyText(node.value).then((ok) => {
      if (!ok) return;
      setCopied(true);
      if (resetTimer.current !== null) window.clearTimeout(resetTimer.current);
      resetTimer.current = window.setTimeout(() => setCopied(false), COPIED_RESET_MS);
    });
  }, [node.value]);

  return (
    <div className="md-code">
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

function renderInline(nodes: MdInline[]): ReactNode[] {
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
        return <strong key={i}>{renderInline(node.children)}</strong>;
      case "em":
        return <em key={i}>{renderInline(node.children)}</em>;
      case "del":
        return <del key={i}>{renderInline(node.children)}</del>;
      case "link":
        return (
          <a
            key={i}
            className="md-link"
            href={node.href}
            title={node.title ?? undefined}
            target="_blank"
            rel="noopener noreferrer"
          >
            {renderInline(node.children)}
          </a>
        );
      case "image":
        // 不加载远程图片：只给一个可点开的链接
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

import { useEffect, useMemo, useRef } from "react";
import { highlight, type Token } from "../../lib/highlight";

const FILE_LANGUAGE_BY_EXTENSION: Record<string, string> = {
  rs: "rust",
  ts: "typescript",
  tsx: "typescript",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  py: "python",
  json: "json",
  jsonc: "json",
  json5: "json",
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  toml: "toml",
  yaml: "yaml",
  yml: "yaml",
  html: "html",
  htm: "html",
  xml: "xml",
  svg: "svg",
  vue: "vue",
  css: "css",
  scss: "scss",
  less: "less",
  sql: "sql",
  go: "go",
  diff: "diff",
  patch: "diff",
  md: "markdown",
  mdx: "markdown",
  markdown: "markdown",
};

export function languageForFile(path: string | null): string | null {
  if (!path) return null;
  const parts = path.split(/[\\/]/);
  const name = (parts[parts.length - 1] ?? "").toLowerCase();
  if (name === "cargo.lock") return "toml";
  if (
    name === "dockerfile"
    || name === "makefile"
    || name === ".gitignore"
    || name === ".env"
    || name.startsWith(".env.")
  ) return "bash";
  const extension = name.includes(".") ? name.split(".").pop() ?? "" : "";
  return FILE_LANGUAGE_BY_EXTENSION[extension] ?? null;
}

export function tokensByLine(tokens: Token[]): Token[][] {
  const lines: Token[][] = [[]];
  for (const token of tokens) {
    const parts = token.text.split("\n");
    parts.forEach((text, index) => {
      if (text) lines[lines.length - 1].push({ text, cls: token.cls });
      if (index < parts.length - 1) lines.push([]);
    });
  }
  return lines;
}

interface Props {
  path: string | null;
  content: string;
  activeLine?: number | null;
  ariaLabel?: string;
  className?: string;
}

/** Shared read-only file renderer used by both project file surfaces. */
export function FileCodePreview({
  path,
  content,
  activeLine = null,
  ariaLabel,
  className,
}: Props) {
  const activeLineRef = useRef<HTMLSpanElement | null>(null);
  const language = useMemo(() => languageForFile(path), [path]);
  const lines = useMemo(() => tokensByLine(highlight(content, language)), [content, language]);

  useEffect(() => {
    activeLineRef.current?.scrollIntoView({ block: "center" });
  }, [activeLine, content]);

  return (
    <pre
      className={`file-code file-code-preview${className ? ` ${className}` : ""}`}
      aria-label={ariaLabel ?? `${path ?? "文件"} 只读预览`}
    >
      <code>
        {lines.map((line, index) => {
          const lineNumber = index + 1;
          const active = activeLine === lineNumber;
          return (
            <span
              className={`file-code-line${active ? " is-active" : ""}`}
              data-line={lineNumber}
              aria-current={active ? "location" : undefined}
              ref={active ? activeLineRef : undefined}
              key={lineNumber}
            >
              <i aria-hidden="true">{lineNumber}</i>
              <span className="file-code-text">
                {line.length
                  ? line.map((token, tokenIndex) => (
                      <span className={token.cls ?? undefined} key={tokenIndex}>{token.text}</span>
                    ))
                  : " "}
              </span>
            </span>
          );
        })}
      </code>
    </pre>
  );
}

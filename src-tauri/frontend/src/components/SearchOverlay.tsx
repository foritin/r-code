import { useEffect, useMemo, useRef, useState } from "react";
import { useAppStore } from "../store/app";
import { useFocusTrap } from "../lib/hooks";
import { useTasksStore } from "../store/tasks";
import { globalSearch, quickOpen } from "../lib/ipc";
import { displayPath, errText } from "../lib/format";
import { workspaceName } from "../lib/presentation";
import type { SearchMatch } from "../lib/types";
import { IconAlert, IconFile, IconSearch, IconText } from "./icons";

type Item = { kind: "file"; path: string } | { kind: "hit"; match: SearchMatch };

/**
 * Ctrl K 搜索 overlay —— 居中浮层 + 背板。
 * 输入 300ms 防抖后双区搜索：文件（quick_open）与内容命中（global_search）。
 * ↑↓ 选择，⏎ 打开（写入 app.editorFile 并切到 Editor），Esc / 点背板关闭。
 */
export function SearchOverlay() {
  const setSearchOpen = useAppStore((s) => s.setSearchOpen);
  const setScene = useAppStore((s) => s.setScene);
  const setEditorFile = useAppStore((s) => s.setEditorFile);
  const setProjects = useAppStore((s) => s.setScene);
  const workspacePath = useTasksStore((s) => s.currentProjectId);
  const workspaces = useTasksStore((s) => s.workspaces);
  const searchable = Boolean(workspacePath);
  const scopeName = workspacePath ? workspaceName(workspacePath, workspaces) : "未附加文件夹";

  const [query, setQuery] = useState("");
  const [files, setFiles] = useState<string[]>([]);
  const [hits, setHits] = useState<SearchMatch[]>([]);
  const [sel, setSel] = useState(0);
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const seqRef = useRef(0);
  const returnFocusRef = useRef<HTMLElement | null>(
    typeof document === "undefined" ? null : document.activeElement instanceof HTMLElement ? document.activeElement : null,
  );

  const items = useMemo<Item[]>(
    () => [
      ...files.map((path): Item => ({ kind: "file", path })),
      ...hits.map((match): Item => ({ kind: "hit", match })),
    ],
    [files, hits],
  );
  const selSafe = items.length === 0 ? 0 : Math.min(sel, items.length - 1);
  const dialogRef = useRef<HTMLDivElement>(null);
  useFocusTrap(dialogRef, true);

  // 挂载自动聚焦。焦点陷阱与归还见下方 hook —— 原先只声明了 aria-modal="true"
  // 却没有任何实现，按 Tab 会直接跑到背景的侧栏按钮上，关闭后焦点掉进 body。
  useEffect(() => {
    inputRef.current?.focus();
    return () => returnFocusRef.current?.focus({ preventScroll: true });
  }, []);

  // Esc 关闭（即使焦点不在输入框）
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setSearchOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [setSearchOpen]);

  // 300ms 防抖搜索；seq 防旧响应覆盖新结果
  useEffect(() => {
    const q = query.trim();
    if (!searchable || !workspacePath) {
      seqRef.current += 1;
      setFiles([]);
      setHits([]);
      setSearching(false);
      return;
    }
    if (!q) {
      seqRef.current += 1;
      setFiles([]);
      setHits([]);
      setError(null);
      setSearching(false);
      setSel(0);
      return;
    }
    setSearching(true);
    const t = setTimeout(() => {
      const seq = ++seqRef.current;
      Promise.all([quickOpen(workspacePath, q, 12), globalSearch(workspacePath, q, 25)])
        .then(([f, h]) => {
          if (seq !== seqRef.current) return;
          setFiles(f);
          setHits(h);
          setSel(0);
          setError(null);
          setSearching(false);
        })
        .catch((e) => {
          if (seq !== seqRef.current) return;
          setError(`搜索失败：${errText(e)}`);
          setSearching(false);
        });
    }, 300);
    return () => clearTimeout(t);
  }, [query, searchable, workspacePath]);

  // 选中项保持可见
  useEffect(() => {
    listRef.current
      ?.querySelector(".opt-search-result.selected")
      ?.scrollIntoView({ block: "nearest" });
  }, [selSafe, items.length]);

  const openItem = (item: Item) => {
    setEditorFile(item.kind === "file" ? item.path : item.match.path);
    setSearchOpen(false);
    setScene("editor");
  };

  const onInputKey = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSel((v) => (items.length === 0 ? 0 : (v + 1) % items.length));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSel((v) => (items.length === 0 ? 0 : (v - 1 + items.length) % items.length));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const item = items[selSafe];
      if (item) openItem(item);
    }
  };

  return (
    <div className="opt-search-backdrop" onClick={() => setSearchOpen(false)}>
      <div
        className="opt-search-modal"
        role="dialog"
        aria-modal="true"
        aria-label="搜索"
        ref={dialogRef}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="opt-search-scope">
          <span>当前搜索范围 <strong>{scopeName}</strong></span>
          <span>{workspacePath ? displayPath(workspacePath) : "仅当前附加文件夹"}</span>
        </div>

        <div className="opt-search-input">
          <IconSearch width={16} height={16} />
          <input
            ref={inputRef}
            role="combobox"
            aria-expanded={items.length > 0}
            aria-controls="ovl-results"
            aria-activedescendant={items.length > 0 ? `ovl-option-${selSafe}` : undefined}
            aria-autocomplete="list"
            aria-label="搜索文件与内容"
            value={query}
            placeholder={searchable ? "搜索文件名、路径或内容…" : "附加文件夹后可搜索本地文件"}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={onInputKey}
          />
          {query && (
            <button className="opt-search-clear" onClick={() => setQuery("")} aria-label="清空搜索" title="清空搜索">
              <span aria-hidden="true">✕</span>
            </button>
          )}
          <span className="kact">esc</span>
        </div>

        {error && (
          <div className="errbar" role="alert">
            <IconAlert width={13} height={13} />
            <span className="t">{error}</span>
            <button className="x" onClick={() => setError(null)} aria-label="关闭错误提示" title="知道了">
              <span aria-hidden="true">✗</span>
            </button>
          </div>
        )}

        <div className="opt-search-results" id="ovl-results" role="listbox" aria-label="搜索结果" ref={listRef}>
          {!searchable ? (
            <div className="opt-empty">
              <h3>仅可搜索附加的文件夹</h3>
              <p>搜索只会访问当前附加的文件夹。</p>
              <button className="opt-button" onClick={() => { setSearchOpen(false); setProjects("projects"); }}>
                去附加文件夹
              </button>
            </div>
          ) : query.trim() === "" ? (
            <div className="opt-empty">
              <p>输入关键字，搜文件，也搜内容。</p>
            </div>
          ) : !searching && items.length === 0 && !error ? (
            <div className="opt-empty">
              <h3>未找到匹配内容</h3>
              <p>试试文件名、路径或文件中的关键词。</p>
              <button className="opt-button primary" onClick={() => setQuery("")}>清空搜索</button>
            </div>
          ) : null}

          {files.length > 0 && (
            <h3>文件 · {files.length}</h3>
          )}
          {files.map((path, i) => (
            <button
              key={`f:${path}`}
              id={`ovl-option-${i}`}
              role="option"
              aria-selected={selSafe === i}
              className={`opt-search-result${selSafe === i ? " selected" : ""}`}
              onMouseEnter={() => setSel(i)}
              onClick={() => openItem({ kind: "file", path })}
            >
              <span className="opt-icon">
                <IconFile width={16} height={16} />
              </span>
              <span className="opt-mono">{path}</span>
            </button>
          ))}

          {hits.length > 0 && (
            <h3>内容命中 · {hits.length}</h3>
          )}
          {hits.map((m, j) => {
            const idx = files.length + j;
            return (
              <button
                key={`h:${m.path}:${m.line}:${j}`}
                id={`ovl-option-${idx}`}
                role="option"
                aria-selected={selSafe === idx}
                className={`opt-search-result${selSafe === idx ? " selected" : ""}`}
                onMouseEnter={() => setSel(idx)}
                onClick={() => openItem({ kind: "hit", match: m })}
              >
                <span className="opt-icon">
                  <IconText width={16} height={16} />
                </span>
                <span className="opt-mono">{m.line_text.trim()}</span>
                <small>
                  {m.path}:{m.line}
                </small>
              </button>
            );
          })}
        </div>

        <div className="opt-search-footer">
          <span>
            <span className="kact">↑↓</span> 选择
          </span>
          <span>
            <span className="kact">Enter</span> 打开
          </span>
          <span>
            <span className="kact">Esc</span> 关闭
          </span>
          <span>{searching ? "搜索中…" : "仅当前附加文件夹"}</span>
        </div>
      </div>
    </div>
  );
}

import { useEffect, useMemo, useRef, useState } from "react";
import { useAppStore } from "../store/app";
import { useTasksStore } from "../store/tasks";
import { globalSearch, quickOpen } from "../lib/ipc";
import { errText } from "../lib/format";
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
  const searchable = Boolean(workspacePath);

  const [query, setQuery] = useState("");
  const [files, setFiles] = useState<string[]>([]);
  const [hits, setHits] = useState<SearchMatch[]>([]);
  const [sel, setSel] = useState(0);
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const seqRef = useRef(0);

  const items = useMemo<Item[]>(
    () => [
      ...files.map((path): Item => ({ kind: "file", path })),
      ...hits.map((match): Item => ({ kind: "hit", match })),
    ],
    [files, hits],
  );
  const selSafe = items.length === 0 ? 0 : Math.min(sel, items.length - 1);

  // 挂载自动聚焦
  useEffect(() => {
    inputRef.current?.focus();
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
      ?.querySelector(".ovl-item.sel")
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
    <div className="ovl-backdrop" onClick={() => setSearchOpen(false)}>
      <div
        className="ovl pane pane-lit"
        role="dialog"
        aria-modal="true"
        aria-label="搜索"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="ovl-input-row">
          <IconSearch width={15} height={15} />
          <input
            ref={inputRef}
            className="ovl-input"
            value={query}
            placeholder={searchable ? "搜文件，也搜内容…" : "附加文件夹后可搜索本地文件"}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={onInputKey}
          />
          <span className="kact">esc</span>
        </div>

        {error && (
          <div className="errbar" role="alert">
            <IconAlert width={13} height={13} />
            <span className="t">{error}</span>
            <button className="x" onClick={() => setError(null)} title="知道了">
              ✗
            </button>
          </div>
        )}

        <div className="ovl-results" ref={listRef}>
          {!searchable ? (
            <div className="empty">
              搜索只会访问当前附加的文件夹。<br />
              <button className="linkbtn" onClick={() => { setSearchOpen(false); setProjects("projects"); }}>
                去附加文件夹
              </button>
            </div>
          ) : query.trim() === "" && <div className="empty">输入关键字，搜文件，也搜内容。</div>}
          {searchable && query.trim() !== "" && !searching && items.length === 0 && !error && (
            <div className="empty">没有匹配的结果。</div>
          )}

          {files.length > 0 && (
            <div className="ovl-sec">文件 · {files.length}</div>
          )}
          {files.map((path, i) => (
            <button
              key={`f:${path}`}
              className={`ovl-item${selSafe === i ? " sel" : ""}`}
              onMouseEnter={() => setSel(i)}
              onClick={() => openItem({ kind: "file", path })}
            >
              <span className="ic">
                <IconFile width={13} height={13} />
              </span>
              <span className="t mono">{path}</span>
            </button>
          ))}

          {hits.length > 0 && (
            <div className="ovl-sec">内容命中 · {hits.length}</div>
          )}
          {hits.map((m, j) => {
            const idx = files.length + j;
            return (
              <button
                key={`h:${m.path}:${m.line}:${j}`}
                className={`ovl-item${selSafe === idx ? " sel" : ""}`}
                onMouseEnter={() => setSel(idx)}
                onClick={() => openItem({ kind: "hit", match: m })}
              >
                <span className="ic">
                  <IconText width={13} height={13} />
                </span>
                <span className="t mono">{m.line_text.trim()}</span>
                <span className="loc">
                  {m.path}:{m.line}
                </span>
              </button>
            );
          })}
        </div>

        <div className="ovl-foot">
          <span>
            <span className="kact">↑↓</span> 选择
          </span>
          <span>
            <span className="kact">⏎</span> 打开
          </span>
          <span>
            <span className="kact">esc</span> 关闭
          </span>
          <span className="spacer" />
          {searching && <span>搜索中…</span>}
        </div>
      </div>
    </div>
  );
}

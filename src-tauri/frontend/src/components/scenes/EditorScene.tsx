import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { fileList, fileRead, fileWrite, quickOpen, type FileContent, type FileTreeEntry } from "../../lib/ipc";
import { highlight, type Token } from "../../lib/highlight";
import { useAppStore } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { displayPath } from "../../lib/format";
import type { Workspace } from "../../lib/types";
import { IconChevronDown, IconChevronLeft, IconChevronRight, IconEditor, IconFile, IconFolderOpen, IconProjects, IconSearch } from "../icons";
import { AnchoredSurface } from "../ui/AnchoredSurface";

const ROOT = "__root__";

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

function languageForFile(path: string | null): string | null {
  if (!path) return null;
  const pathParts = path.split(/[\\/]/);
  const name = (pathParts[pathParts.length - 1] ?? "").toLowerCase();
  if (name === "cargo.lock") return "toml";
  if (name === "dockerfile" || name === "makefile" || name === ".gitignore" || name === ".env" || name.startsWith(".env.")) return "bash";
  const nameParts = name.split(".");
  const extension = name.includes(".") ? nameParts[nameParts.length - 1] ?? "" : "";
  return FILE_LANGUAGE_BY_EXTENSION[extension] ?? null;
}

function tokensByLine(tokens: Token[]): Token[][] {
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

/** 项目文件：真实文件树、快速定位、只读/可编辑内容预览。 */
export function EditorScene() {
  const storedFile = useAppStore((s) => s.editorFile);
  const setEditorFile = useAppStore((s) => s.setEditorFile);
  const setScene = useAppStore((s) => s.setScene);
  const currentProjectPath = useTasksStore((s) => s.currentProjectId);
  const setCurrentProject = useTasksStore((s) => s.setCurrentProject);
  const workspaces = useTasksStore((s) => s.workspaces);
  const [workspacePath, setWorkspacePath] = useState<string | null>(() => storedFile ? currentProjectPath : null);
  const workspace = workspaces.find((item) => item.canonical_path === workspacePath);
  const [entriesByDir, setEntriesByDir] = useState<Record<string, FileTreeEntry[]>>({});
  const [loadingDirs, setLoadingDirs] = useState<Set<string>>(new Set());
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<string[]>([]);
  const [file, setFile] = useState<string | null>(() => storedFile && currentProjectPath ? storedFile : null);
  const [content, setContent] = useState<FileContent | null>(null);
  const [draft, setDraft] = useState("");
  const [editing, setEditing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const searchRef = useRef<HTMLLabelElement>(null);

  const loadDirectory = useCallback(async (path: string | null) => {
    if (!workspacePath) return;
    const key = path ?? ROOT;
    setLoadingDirs((current) => new Set(current).add(key));
    try {
      const listing = await fileList(workspacePath, path);
      setEntriesByDir((current) => ({ ...current, [key]: listing.entries }));
      setError(null);
    } catch (cause) {
      setError(`读取文件列表失败：${String(cause)}`);
    } finally {
      setLoadingDirs((current) => { const next = new Set(current); next.delete(key); return next; });
    }
  }, [workspacePath]);

  useEffect(() => {
    setEntriesByDir({});
    setExpanded(new Set());
    setFile(null);
    setContent(null);
    if (workspacePath) void loadDirectory(null);
  }, [loadDirectory, workspacePath]);

  useEffect(() => {
    if (!storedFile || !currentProjectPath) return;
    setWorkspacePath(currentProjectPath);
    setFile(storedFile);
    setEditorFile(null);
  }, [currentProjectPath, setEditorFile, storedFile]);

  useEffect(() => {
    if (!file || !workspacePath) { setContent(null); return; }
    let cancelled = false;
    setContent(null);
    setEditing(false);
    void fileRead(workspacePath, file).then((next) => {
      if (cancelled) return;
      setContent(next);
      setDraft(next.content);
      setError(null);
    }).catch((cause) => {
      if (!cancelled) setError(`读取文件失败：${String(cause)}`);
    });
    return () => { cancelled = true; };
  }, [file, workspacePath]);

  useEffect(() => {
    if (!workspacePath || !query.trim()) { setHits([]); return; }
    const timer = window.setTimeout(() => {
      void quickOpen(workspacePath, query.trim(), 18).then((next) => { setHits(next); setError(null); }).catch((cause) => setError(`查找文件失败：${String(cause)}`));
    }, 130);
    return () => window.clearTimeout(timer);
  }, [query, workspacePath]);

  const selectFile = (path: string) => {
    setFile(path);
    setQuery("");
    setHits([]);
  };
  const selectWorkspace = (path: string) => {
    setCurrentProject(path);
    setWorkspacePath(path);
    setEditorFile(null);
  };
  const showWorkspaceChooser = () => {
    setWorkspacePath(null);
    setFile(null);
    setContent(null);
    setEditing(false);
    setEditorFile(null);
  };
  const toggleDirectory = (path: string) => {
    setExpanded((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path); else next.add(path);
      return next;
    });
    if (!entriesByDir[path]) void loadDirectory(path);
  };
  const save = async () => {
    if (!workspacePath || !file || !content || saving) return;
    setSaving(true);
    try {
      const saved = await fileWrite(workspacePath, file, draft, content.revision);
      setContent(saved);
      setDraft(saved.content);
      setEditing(false);
      setError(null);
    } catch (cause) {
      setError(`保存失败：${String(cause)}`);
    } finally {
      setSaving(false);
    }
  };
  const pathParts = useMemo(() => file?.split("/") ?? [], [file]);
  const language = useMemo(() => languageForFile(file), [file]);
  const highlightedLines = useMemo(
    () => tokensByLine(highlight(content?.content ?? "", language)),
    [content?.content, language],
  );

  if (!workspacePath) {
    return (
      <div className="scene scene-editor">
        <ProjectChooser
          workspaces={workspaces}
          currentProjectPath={currentProjectPath}
          onSelect={selectWorkspace}
          onManage={() => setScene("projects")}
        />
      </div>
    );
  }

  return (
    <div className="scene scene-editor">
      <div className="file-page">
        <header className="file-page-header">
          <div className="file-page-project">
            <button type="button" className="file-project-back" aria-label="返回项目列表" title="返回项目列表" onClick={showWorkspaceChooser}><IconChevronLeft width={16} height={16} /></button>
            <div><p className="page-kicker">PROJECT FILES</p><h1>项目文件</h1><p><IconProjects width={14} height={14} /> {workspace?.display_name ?? displayPath(workspacePath)}</p></div>
          </div>
          <label className="file-search" ref={searchRef}><IconSearch width={16} height={16} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="快速打开文件…" /><kbd>Ctrl K</kbd></label>
          {query && (
            <AnchoredSurface
              anchorRef={searchRef}
              className="file-search-results"
              role="listbox"
              label="快速打开文件"
              placement="down"
              align="right"
              matchAnchorWidth
              onDismiss={() => setQuery("")}
            >
              {hits.length
                ? hits.map((path) => <button key={path} role="option" onClick={() => selectFile(path)}><IconFile width={15} height={15} />{path}</button>)
                : <span>没有匹配的文件</span>}
            </AnchoredSurface>
          )}
        </header>
        {error && <div className="file-error" role="alert">{error}</div>}
        <div className="file-workspace">
          <aside className="file-tree" aria-label="文件树">
            <div className="file-tree-head"><span>文件</span>{loadingDirs.has(ROOT) && <small>读取中…</small>}</div>
            <div className="file-tree-items"><FileTree entries={entriesByDir[ROOT] ?? []} entriesByDir={entriesByDir} expanded={expanded} loadingDirs={loadingDirs} selected={file} depth={0} onFile={selectFile} onFolder={toggleDirectory} /></div>
          </aside>
          <section className="file-preview">
            {!file ? <div className="file-preview-empty"><IconEditor width={25} height={25} /><h2>选择一个文件</h2><p>从左侧文件树选择，或用上方快速打开定位文件。</p></div> : !content ? <div className="file-preview-empty">正在读取 {file}…</div> : <>
              <header className="file-preview-head"><div className="file-breadcrumb"><IconFile width={16} height={16} />{pathParts.map((part, index) => <span key={`${part}-${index}`}>{index > 0 && <b>/</b>}{part}</span>)}</div><div className="file-preview-actions">{content.is_editable && <button className="rc-button rc-button-quiet" onClick={() => setEditing((value) => !value)}>{editing ? "取消编辑" : "编辑"}</button>}{editing && <button className="rc-button rc-button-primary" disabled={saving || draft === content.content} onClick={() => void save()}>{saving ? "保存中…" : "保存"}</button>}</div></header>
              <div className="file-preview-meta"><span>{content.total_lines} 行{content.truncated ? " · 内容已截断" : ""}</span><span>{editing ? "编辑模式" : "只读预览"}</span></div>
              {editing
                ? <textarea className="file-code-editor" aria-label={`${file} 编辑器`} value={draft} onChange={(event) => setDraft(event.target.value)} spellCheck={false} />
                : <pre className="file-code" aria-label={`${file} 只读预览`}><code>{highlightedLines.map((line, index) => <span className="file-code-line" key={index}><i>{index + 1}</i><span className="file-code-text">{line.length ? line.map((token, tokenIndex) => <span className={token.cls ?? undefined} key={tokenIndex}>{token.text}</span>) : " "}</span></span>)}</code></pre>}
            </>}
          </section>
        </div>
      </div>
    </div>
  );
}

function ProjectChooser({ workspaces, currentProjectPath, onSelect, onManage }: { workspaces: Workspace[]; currentProjectPath: string | null; onSelect: (path: string) => void; onManage: () => void }) {
  return (
    <section className="file-project-picker" aria-label="选择项目">
      <header className="file-project-picker-head">
        <div>
          <p className="page-kicker">PROJECT FILES</p>
          <h1>选择要浏览的项目</h1>
          <p>先确认项目，再进入它的文件系统。文件操作始终限定在所选工作区内。</p>
        </div>
        <button type="button" className="rc-button rc-button-quiet" onClick={onManage}>管理项目</button>
      </header>
      {workspaces.length > 0 ? (
        <ul className="file-project-list" aria-label={`已添加项目，共 ${workspaces.length} 个`}>
          {workspaces.map((workspace) => (
            <li key={workspace.canonical_path}>
              <button
                type="button"
                className="file-project-row"
                aria-label={`打开 ${workspace.display_name} 的文件`}
                onClick={() => onSelect(workspace.canonical_path)}
              >
                <span className="file-project-icon"><IconFolderOpen width={18} height={18} /></span>
                <span className="file-project-copy"><strong>{workspace.display_name}</strong><small>{displayPath(workspace.canonical_path)}</small></span>
                {workspace.canonical_path === currentProjectPath && <span className="file-project-current">当前项目</span>}
                <IconChevronRight className="file-project-chevron" width={15} height={15} />
              </button>
            </li>
          ))}
        </ul>
      ) : (
        <div className="file-project-empty">
          <IconProjects width={25} height={25} />
          <h2>还没有添加项目</h2>
          <p>先添加一个本地项目，随后即可浏览、预览和编辑文件。</p>
          <button type="button" className="rc-button rc-button-primary" onClick={onManage}>添加项目</button>
        </div>
      )}
    </section>
  );
}

function FileTree({ entries, entriesByDir, expanded, loadingDirs, selected, depth, onFile, onFolder }: { entries: FileTreeEntry[]; entriesByDir: Record<string, FileTreeEntry[]>; expanded: Set<string>; loadingDirs: Set<string>; selected: string | null; depth: number; onFile: (path: string) => void; onFolder: (path: string) => void }) {
  return (
    <>
      {entries.map((entry) => {
        if (entry.is_directory) {
          return (
            <div className="file-tree-folder" key={entry.path}>
              <button className="file-tree-row folder" style={{ paddingLeft: 10 + depth * 15 }} onClick={() => onFolder(entry.path)}>
                <span>{expanded.has(entry.path) ? <IconChevronDown width={13} height={13} /> : <IconChevronRight width={13} height={13} />}</span>
                <IconFolderOpen width={15} height={15} />
                <strong>{entry.name}</strong>
                {loadingDirs.has(entry.path) && <small>…</small>}
              </button>
              {expanded.has(entry.path) && (
                <FileTree
                  entries={entriesByDir[entry.path] ?? []}
                  entriesByDir={entriesByDir}
                  expanded={expanded}
                  loadingDirs={loadingDirs}
                  selected={selected}
                  depth={depth + 1}
                  onFile={onFile}
                  onFolder={onFolder}
                />
              )}
            </div>
          );
        }
        return (
          <button className={`file-tree-row${selected === entry.path ? " selected" : ""}`} key={entry.path} style={{ paddingLeft: 28 + depth * 15 }} onClick={() => onFile(entry.path)}>
            <IconFile width={14} height={14} />
            <span>{entry.name}</span>
          </button>
        );
      })}
    </>
  );
}

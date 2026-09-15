import { useCallback, useEffect, useMemo, useRef, useState, type MouseEvent } from "react";
import { fileList, fileRead, fileWrite, quickOpen, type FileContent, type FileTreeEntry } from "../../lib/ipc";
import { useAppStore } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { displayPath } from "../../lib/format";
import { keyLabel } from "../../lib/keys";
import { IconChevronDown, IconChevronLeft, IconChevronRight, IconEditor, IconFile, IconFolderOpen, IconProjects, IconRefresh, IconSearch } from "../icons";
import { FileCodePreview } from "../files/FileCodePreview";
import { FileContextMenu, type FileContextMenuTarget } from "../files/FileContextMenu";
import { AnchoredSurface } from "../ui/AnchoredSurface";

const ROOT = "__root__";

/** 项目文件：真实文件树、快速定位、只读/可编辑内容预览。 */
export function EditorScene() {
  const storedFile = useAppStore((s) => s.editorFile);
  const setEditorFile = useAppStore((s) => s.setEditorFile);
  const setScene = useAppStore((s) => s.setScene);
  const openDashboard = useAppStore((s) => s.openDashboard);
  const openRoom = useAppStore((s) => s.openRoom);
  const currentProjectPath = useTasksStore((s) => s.currentProjectId);
  const workspaces = useTasksStore((s) => s.workspaces);
  const tasks = useTasksStore((s) => s.tasks);
  const [workspacePath, setWorkspacePath] = useState<string | null>(() => currentProjectPath);
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
  const [contextMenuTarget, setContextMenuTarget] = useState<FileContextMenuTarget | null>(null);
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
    setContextMenuTarget(null);
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
  const leaveProjectFiles = () => {
    if (workspacePath) openDashboard(workspacePath);
    else setScene("projects");
  };
  const toggleDirectory = (path: string) => {
    setExpanded((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path); else next.add(path);
      return next;
    });
    if (!entriesByDir[path]) void loadDirectory(path);
  };
  const refreshTree = useCallback(async () => {
    await Promise.all([
      loadDirectory(null),
      ...Array.from(expanded, (path) => loadDirectory(path)),
    ]);
  }, [expanded, loadDirectory]);
  const openFileContextMenu = (path: string, event: MouseEvent<HTMLButtonElement>) => {
    event.preventDefault();
    if (!workspacePath) return;
    setContextMenuTarget({ workspacePath, path, x: event.clientX, y: event.clientY });
  };
  const suppressFolderContextMenu = (event: MouseEvent<HTMLButtonElement>) => {
    event.preventDefault();
    setContextMenuTarget(null);
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
  const taskTargets = useMemo(() => tasks
    .filter((task) => task.workspace_path === workspacePath && task.state !== "archived")
    .map((task) => ({ id: task.id, title: task.title })), [tasks, workspacePath]);
  const refreshingTree = loadingDirs.has(ROOT)
    || Array.from(expanded).some((path) => loadingDirs.has(path));

  if (!workspacePath) {
    return (
      <div className="scene scene-editor">
        <section className="opt-empty">
          <span className="opt-icon"><IconProjects width={20} height={20} /></span>
          <h3>先打开一个项目</h3>
          <p>项目文件属于具体项目，请从左侧项目列表进入项目后再打开。</p>
          <button type="button" className="opt-button primary" onClick={() => setScene("projects")}>添加或打开项目</button>
        </section>
      </div>
    );
  }

  return (
    <div className="scene scene-editor">
      <div className="opt-editor">
        <header className="opt-panel-head">
          <button type="button" className="file-project-back" aria-label="返回项目" title="返回项目" onClick={leaveProjectFiles}><IconChevronLeft width={16} height={16} /></button>
          <h2><IconProjects width={16} height={16} /> {workspace?.display_name ?? displayPath(workspacePath)}</h2>
          <label className="opt-search-field" ref={searchRef}><IconSearch width={16} height={16} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="快速打开文件…" /><kbd>{keyLabel("search")}</kbd></label>
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
        <div className="opt-file-layout">
          <aside className="opt-file-tree" aria-label="文件树">
            <div className="file-tree-head">
              <h3>文件</h3>
              <span className="file-tree-head-actions">
                {loadingDirs.has(ROOT) && <small>读取中…</small>}
                <button type="button" className="file-tree-refresh" aria-label="刷新文件树" title="刷新文件树" aria-busy={refreshingTree} disabled={refreshingTree} onClick={() => void refreshTree()}><IconRefresh width={14} height={14} /></button>
              </span>
            </div>
            <div className="file-tree-items"><FileTree entries={entriesByDir[ROOT] ?? []} entriesByDir={entriesByDir} expanded={expanded} loadingDirs={loadingDirs} selected={file} depth={0} onFile={selectFile} onFolder={toggleDirectory} onFileContextMenu={openFileContextMenu} onFolderContextMenu={suppressFolderContextMenu} /></div>
          </aside>
          <section className="opt-editor-code">
            {!file ? (
              <div className="opt-file-blank"><span className="opt-icon"><IconEditor width={20} height={20} /></span><h3>选择一个文件</h3><p>从左侧文件树选择，或用上方快速打开定位文件。</p></div>
            ) : !content ? (
              <div className="opt-file-blank"><p>正在读取 {file}…</p></div>
            ) : (
              <>
                <div className="opt-file-tab">
                  <span className="file-breadcrumb"><IconFile width={16} height={16} />{pathParts.map((part, index) => <span key={`${part}-${index}`}>{index > 0 && <b>/</b>}{part}</span>)}</span>
                  <span className="opt-pill">{editing ? "编辑模式" : "只读预览"}</span>
                  <div className="opt-actions">
                    {content.is_editable && <button className="opt-button quiet" onClick={() => setEditing((value) => !value)}>{editing ? "取消编辑" : "编辑"}</button>}
                    {editing && <button className="opt-button primary" disabled={saving || draft === content.content} onClick={() => void save()}>{saving ? "保存中…" : "保存"}</button>}
                  </div>
                </div>
                {editing
                  ? <textarea className="file-code-editor" aria-label={`${file} 编辑器`} value={draft} onChange={(event) => setDraft(event.target.value)} spellCheck={false} />
                  : <FileCodePreview path={file} content={content.content} ariaLabel={`${file} 只读预览`} />}
                <div className="opt-code-foot">
                  <span>{content.total_lines} 行{content.truncated ? " · 内容已截断" : ""} · {file}</span>
                  <span>{editing ? "编辑模式：保存后立即写入磁盘" : "只读预览：点击编辑后可保存"}</span>
                </div>
              </>
            )}
          </section>
        </div>
      </div>
      <FileContextMenu target={contextMenuTarget} tasks={taskTargets} onDismiss={() => setContextMenuTarget(null)} onTaskSelected={(task) => openRoom(task.id)} />
    </div>
  );
}

function FileTree({ entries, entriesByDir, expanded, loadingDirs, selected, depth, onFile, onFolder, onFileContextMenu, onFolderContextMenu }: { entries: FileTreeEntry[]; entriesByDir: Record<string, FileTreeEntry[]>; expanded: Set<string>; loadingDirs: Set<string>; selected: string | null; depth: number; onFile: (path: string) => void; onFolder: (path: string) => void; onFileContextMenu: (path: string, event: MouseEvent<HTMLButtonElement>) => void; onFolderContextMenu: (event: MouseEvent<HTMLButtonElement>) => void }) {
  return (
    <>
      {entries.map((entry) => {
        if (entry.is_directory) {
          return (
            <div className="file-tree-folder" key={entry.path}>
              <button style={{ paddingLeft: 10 + depth * 15 }} onClick={() => onFolder(entry.path)} onContextMenu={onFolderContextMenu}>
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
                  onFileContextMenu={onFileContextMenu}
                  onFolderContextMenu={onFolderContextMenu}
                />
              )}
            </div>
          );
        }
        return (
          <button className={selected === entry.path ? "selected" : ""} key={entry.path} style={{ paddingLeft: 28 + depth * 15 }} onClick={() => onFile(entry.path)} onContextMenu={(event) => onFileContextMenu(entry.path, event)}>
            <IconFile width={14} height={14} />
            <span>{entry.name}</span>
          </button>
        );
      })}
    </>
  );
}

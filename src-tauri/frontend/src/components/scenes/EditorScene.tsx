import { useEffect, useState } from "react";
import { useAppStore } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { usePoll } from "../../lib/poll";
import { fileRead, quickOpen, type FileContent } from "../../lib/ipc";
import { IconEditor } from "../icons";

/**
 * Editor（Ctrl E）— 只读文件浏览。
 * quick open 定位文件，`cmd_file_read` 读取内容（当前工作区边界内，512 KiB 截断）。
 * 编辑能力后续里程碑。
 */
export function EditorScene() {
  const storeFile = useAppStore((s) => s.editorFile);
  const setEditorFile = useAppStore((s) => s.setEditorFile);
  const setScene = useAppStore((s) => s.setScene);
  const workspacePath = useTasksStore((s) => s.currentProjectId);
  const canBrowse = Boolean(workspacePath);

  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<string[]>([]);
  const [file, setFile] = useState<string | null>(storeFile);
  const [content, setContent] = useState<FileContent | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [readErr, setReadErr] = useState<string | null>(null);

  // Rail Files / 其他入口写入的 editorFile 同步进来
  useEffect(() => {
    if (storeFile) setFile(storeFile);
  }, [storeFile]);

  // 选中文件 → 读内容
  useEffect(() => {
    if (!file || !workspacePath || !canBrowse) {
      setContent(null);
      return;
    }
    let dead = false;
    setContent(null);
    fileRead(workspacePath, file)
      .then((fc) => {
        if (!dead) {
          setContent(fc);
          setReadErr(null);
        }
      })
      .catch((e) => {
        if (!dead) setReadErr(String(e));
      });
    return () => {
      dead = true;
    };
  }, [file, workspacePath, canBrowse]);

  usePoll(async () => {
    const q = query.trim();
    if (!workspacePath || !canBrowse) {
      setHits([]);
      return;
    }
    if (!q) {
      setHits([]);
      return;
    }
    try {
      setHits(await quickOpen(workspacePath, q, 20));
      setErr(null);
    } catch (e) {
      setErr(`查找文件失败：${String(e)}`);
    }
  }, 400);

  const pick = (path: string) => {
    setFile(path);
    setEditorFile(path);
    setQuery("");
    setHits([]);
  };

  return (
    <div className="scene">
      <div className="editor-wrap">
        <div className="page-head">
          <h1>Editor</h1>
          <span className="meta">只读浏览</span>
        </div>

        {!canBrowse ? (
          <div className="editor-gate">
            <IconEditor width={18} height={18} />
            <h2>附加文件夹后浏览代码</h2>
            <p>纯聊天不会读取本地文件；选择一个文件夹后，文件浏览与搜索才会可用。</p>
            <button className="btn accent" onClick={() => setScene("projects")}>管理文件夹</button>
          </div>
        ) : <>
        <div className="qopen">
          <input
            className="input"
            placeholder="Quick open — 输入文件名…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && hits.length > 0) pick(hits[0]);
              if (e.key === "Escape") {
                setQuery("");
                setHits([]);
              }
            }}
          />
          {query.trim() !== "" && (
            <div className="qhits pane">
              {hits.length === 0 ? (
                <div className="empty">没有匹配的文件</div>
              ) : (
                hits.map((h) => (
                  <button key={h} className="qhit" onClick={() => pick(h)} title={h}>
                    {h}
                  </button>
                ))
              )}
            </div>
          )}
        </div>

        {err && <div className="errbar">{err}</div>}
        {readErr && <div className="errbar">{readErr}</div>}

        {file ? (
          <div className="pane viewpane">
            <div className="view-head">
              <IconEditor width={13} height={13} />
              <span className="fp" title={file}>
                {file}
              </span>
              {content && (
                <span className="ro">
                  {content.total_lines} 行{content.truncated ? " · 已截断(512K)" : ""} · read-only
                </span>
              )}
            </div>
            {content ? (
              <div className="view-body codeview">
                {content.content.split("\n").map((line, i) => (
                  <div className="cv-line" key={i}>
                    <span className="ln">{i + 1}</span>
                    <span className="lc">{line || " "}</span>
                  </div>
                ))}
              </div>
            ) : (
              !readErr && <div className="empty">读取中…</div>
            )}
          </div>
        ) : (
          <div className="empty">
            输入文件名快速定位，
            <br />
            或用 Ctrl K 搜索后打开。
          </div>
        )}
        </>}
      </div>
    </div>
  );
}

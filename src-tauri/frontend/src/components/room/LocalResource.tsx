import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import {
  localFileTarget,
  localImagePreview,
  prepareWorkbenchWindow,
  revealLocalPath,
  type LocalFileTarget,
} from "../../lib/ipc";
import { useAppStore } from "../../store/app";
import { IconClose, IconEye, IconFolderOpen } from "../icons";
import { FileTypeIcon } from "./FileTypeIcon";

const RASTER_EXTENSION = /\.(?:png|jpe?g|gif|webp|bmp|avif)$/i;

/** 链接 href 可能带 :行:列、:行-行 或 #L 片段；剥离后才是可用于扩展名识别的路径。 */
function fileIconPath(href: string): string {
  return href
    .split("#", 1)[0]
    .replace(/:\d+(?::\d+)?(?:-\d+)?$/, "")
    .split("?", 1)[0];
}

export function isLocalRasterReference(reference: string): boolean {
  const withoutFragment = reference.replace(/#L\d+(?:C\d+)?$/i, "");
  const withoutLocation = withoutFragment.replace(/:\d+(?::\d+)?$/, "");
  return RASTER_EXTENSION.test(withoutLocation.split(/[?#]/, 1)[0]);
}

interface ResourceContext {
  taskId?: string;
  workspacePath?: string | null;
}

async function navigateTarget(
  target: LocalFileTarget,
  taskId: string | undefined,
): Promise<void> {
  if (target.scope === "workspace" && taskId && target.relative_path && !target.is_directory) {
    useAppStore.getState().openWorkbenchFile(
      taskId,
      target.relative_path,
      target.line,
      target.column,
    );
    void prepareWorkbenchWindow().catch(() => {});
    return;
  }
  if (target.scope === "workspace" && taskId) {
    useAppStore.getState().openRoom(taskId, "files");
    void prepareWorkbenchWindow().catch(() => {});
    return;
  }
  await revealLocalPath(target.absolute_path);
}

export function LocalFileLink({
  href,
  children,
  title,
  taskId,
  workspacePath = null,
}: ResourceContext & {
  href: string;
  children: ReactNode;
  title?: string;
}) {
  const [error, setError] = useState<string | null>(null);
  const [opening, setOpening] = useState(false);

  const open = useCallback(async () => {
    if (opening) return;
    setOpening(true);
    setError(null);
    try {
      const target = await localFileTarget(workspacePath, href);
      await navigateTarget(target, taskId);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setOpening(false);
    }
  }, [href, opening, taskId, workspacePath]);

  return (
    <button
      type="button"
      className={`md-link md-file-link${error ? " is-error" : ""}`}
      title={error ?? title ?? href}
      aria-label={error ? `无法打开文件：${String(children)}` : undefined}
      aria-busy={opening || undefined}
      onClick={() => void open()}
    >
      <FileTypeIcon path={fileIconPath(href)} size={13} />
      <span>{children}</span>
    </button>
  );
}

export function LocalImageArtifact({
  href,
  alt,
  label,
  taskId,
  workspacePath = null,
}: ResourceContext & {
  href: string;
  alt: string;
  label: ReactNode;
}) {
  const [target, setTarget] = useState<LocalFileTarget | null>(null);
  const [src, setSrc] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [previewOpen, setPreviewOpen] = useState(false);

  useEffect(() => {
    let disposed = false;
    let objectUrl: string | null = null;
    setTarget(null);
    setSrc(null);
    setError(null);
    Promise.all([
      localFileTarget(workspacePath, href),
      localImagePreview(workspacePath, href),
    ])
      .then(([nextTarget, bytes]) => {
        if (disposed) return;
        objectUrl = URL.createObjectURL(new Blob([bytes], {
          type: nextTarget.mime_type ?? "application/octet-stream",
        }));
        setTarget(nextTarget);
        setSrc(objectUrl);
      })
      .catch((cause) => {
        if (!disposed) setError(String(cause));
      });
    return () => {
      disposed = true;
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [href, workspacePath]);

  const locate = useCallback(async () => {
    try {
      const next = target ?? await localFileTarget(workspacePath, href);
      setTarget(next);
      await navigateTarget(next, taskId);
    } catch (cause) {
      setError(String(cause));
    }
  }, [href, target, taskId, workspacePath]);

  if (error && !src) {
    return (
      <span className="md-artifact-fallback">
        <LocalFileLink href={href} taskId={taskId} workspacePath={workspacePath} title={error}>
          {label || alt || href}
        </LocalFileLink>
      </span>
    );
  }

  return (
    <span className="md-image-artifact" data-loading={!src || undefined}>
      {src ? (
        <button
          type="button"
          className="md-image-thumb"
          aria-label={`预览图片：${alt || "生成的图片"}`}
          onClick={() => setPreviewOpen(true)}
        >
          <img src={src} alt={alt || "Codex 生成的图片"} loading="lazy" />
          <span><IconEye width={15} height={15} /> 点击预览</span>
        </button>
      ) : (
        <span className="md-image-loading" role="status">
          <span aria-hidden="true" />
          正在读取图片…
        </span>
      )}
      <span className="md-image-meta">
        <span className="md-image-copy">
          <strong>{label || alt || "图片产物"}</strong>
          <small title={target?.absolute_path ?? href}>{target?.absolute_path ?? href}</small>
        </span>
        {src && (
          <span className="md-image-actions">
            <button type="button" onClick={() => setPreviewOpen(true)}>
              <IconEye width={13} height={13} /> 预览
            </button>
            <button type="button" onClick={() => void locate()}>
              <IconFolderOpen width={13} height={13} />
              {target?.scope === "workspace" ? "在文件模块中打开" : "在文件管理器中显示"}
            </button>
          </span>
        )}
      </span>
      {previewOpen && src && (
        <ImagePreviewDialog
          src={src}
          alt={alt || "Codex 生成的图片"}
          path={target?.absolute_path ?? href}
          locationLabel={target?.scope === "workspace" ? "在文件模块中打开" : "在文件管理器中显示"}
          onLocate={() => void locate()}
          onClose={() => setPreviewOpen(false)}
        />
      )}
    </span>
  );
}

function ImagePreviewDialog({
  src,
  alt,
  path,
  locationLabel,
  onLocate,
  onClose,
}: {
  src: string;
  alt: string;
  path: string;
  locationLabel: string;
  onLocate: () => void;
  onClose: () => void;
}) {
  const closeRef = useRef<HTMLButtonElement>(null);
  const locateRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    closeRef.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
        return;
      }
      if (event.key !== "Tab") return;
      const first = closeRef.current;
      const last = locateRef.current ?? first;
      if (!first || !last) return;
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      previous?.focus();
    };
  }, [onClose]);

  return createPortal(
    <div
      className="image-preview-backdrop"
      role="presentation"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <section className="image-preview-dialog" role="dialog" aria-modal="true" aria-label={`图片预览：${alt}`}>
        <header>
          <span>
            <strong>{alt}</strong>
            <small title={path}>{path}</small>
          </span>
          <button ref={closeRef} type="button" aria-label="关闭图片预览" onClick={onClose}>
            <IconClose width={17} height={17} />
          </button>
        </header>
        <div className="image-preview-stage">
          <img src={src} alt={alt} />
        </div>
        <footer>
          <button ref={locateRef} type="button" onClick={onLocate}>
            <IconFolderOpen width={14} height={14} /> {locationLabel}
          </button>
        </footer>
      </section>
    </div>,
    document.body,
  );
}

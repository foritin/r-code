import { useCallback, useEffect, useRef, useState } from "react";
import type { ClipboardEvent as ReactClipboardEvent } from "react";
import type { AttachmentInput, AttachmentKind, PlatformCapabilities } from "../lib/types";
import type { ImageCapability } from "./room/model-capabilities";
import { IconAlert, IconAttach, IconClose, IconFile } from "./icons";

const IMAGE_TYPES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);
const TEXT_MIME_TYPES = new Set([
  "application/json",
  "application/ld+json",
  "application/xml",
  "application/javascript",
  "application/x-javascript",
  "application/yaml",
  "application/x-yaml",
  "application/toml",
  "application/sql",
  "application/graphql",
]);
const TEXT_EXTENSIONS = new Set([
  "txt", "md", "mdx", "rst", "csv", "tsv", "json", "jsonl", "xml", "yaml", "yml",
  "toml", "ini", "cfg", "conf", "log", "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs",
  "py", "go", "java", "kt", "kts", "swift", "c", "h", "cc", "cpp", "hpp", "cs", "rb",
  "php", "sh", "bash", "zsh", "fish", "ps1", "bat", "cmd", "sql", "graphql", "gql", "html",
  "htm", "css", "scss", "sass", "less", "vue", "svelte", "svg", "gitignore", "dockerfile",
]);
const MIME_BY_EXTENSION: Record<string, string> = {
  md: "text/markdown",
  mdx: "text/markdown",
  csv: "text/csv",
  tsv: "text/tab-separated-values",
  json: "application/json",
  jsonl: "application/json",
  xml: "application/xml",
  yaml: "application/yaml",
  yml: "application/yaml",
  toml: "application/toml",
  js: "application/javascript",
  jsx: "application/javascript",
  mjs: "application/javascript",
  cjs: "application/javascript",
  html: "text/html",
  htm: "text/html",
  css: "text/css",
  svg: "text/xml",
  rs: "text/x-rust",
  ts: "text/typescript",
  tsx: "text/typescript",
  py: "text/x-python",
  go: "text/x-go",
  sh: "text/x-shellscript",
  ps1: "text/x-powershell",
};
export const ATTACHMENT_PICKER_ACCEPT = [
  "image/png", "image/jpeg", "image/gif", "image/webp", "application/pdf", "text/*",
  ...Array.from(TEXT_EXTENSIONS, (extension) => `.${extension}`),
].join(",");
const MAX_ATTACHMENT_COUNT = 8;
const MAX_IMAGE_BYTES = 8 * 1024 * 1024;
const MAX_TEXT_BYTES = 1024 * 1024;
const MAX_PDF_BYTES = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES = 24 * 1024 * 1024;

export interface DraftAttachment extends AttachmentInput {
  id: string;
  kind: AttachmentKind;
  previewUrl?: string;
  size: number;
}

export type AttachmentCapabilityResolver = (attachment: DraftAttachment) => ImageCapability;

const MAC_OCR_REASON = "当前模型不接收图片；发送时会用 macOS 系统 OCR 在本机仅提取文字，识别文本仍会随消息发送给模型。图片布局与非文字内容不会发送。";
export function nativeOcrTextName(name: string): string {
  return `${Array.from(name).slice(0, 172).join("")}.ocr.txt`;
}

export function attachmentUsesNativeOcr(
  attachment: DraftAttachment,
  capabilityFor: AttachmentCapabilityResolver,
  platformCapabilities: PlatformCapabilities,
): boolean {
  return attachment.kind === "image"
    && platformCapabilities.nativeOcr
    && platformCapabilities.nativeOcrFormats.includes(attachment.mediaType)
    && capabilityFor(attachment).state === "unsupported";
}

function effectiveCapability(
  attachment: DraftAttachment,
  capabilityFor: AttachmentCapabilityResolver,
  platformCapabilities: PlatformCapabilities,
): ImageCapability {
  const capability = capabilityFor(attachment);
  if (attachmentUsesNativeOcr(attachment, capabilityFor, platformCapabilities)) {
    return { state: "supported", modelLabel: capability.modelLabel, reason: MAC_OCR_REASON };
  }
  return capability;
}

interface UseAttachmentsResult {
  attachments: DraftAttachment[];
  error: string | null;
  clearError: () => void;
  clear: () => void;
  remove: (id: string) => void;
  addFiles: (files: readonly File[], source?: "paste" | "picker") => Promise<void>;
  onPaste: (event: ReactClipboardEvent<HTMLTextAreaElement>) => void;
}

function extensionOf(name: string): string {
  const basename = name.toLowerCase().split(/[\\/]/).pop() ?? "";
  if (basename === "dockerfile") return "dockerfile";
  if (basename === ".gitignore") return "gitignore";
  const dot = basename.lastIndexOf(".");
  return dot >= 0 ? basename.slice(dot + 1) : "";
}

function classifyFile(file: File): { kind: AttachmentKind; mediaType: string } | null {
  const extension = extensionOf(file.name);
  const declared = file.type.trim().toLowerCase();
  if (IMAGE_TYPES.has(declared)) return { kind: "image", mediaType: declared };
  if (declared === "application/pdf" || extension === "pdf") {
    return { kind: "pdf", mediaType: "application/pdf" };
  }
  if (
    declared.startsWith("text/")
    || TEXT_MIME_TYPES.has(declared)
    || TEXT_EXTENSIONS.has(extension)
  ) {
    return {
      kind: "text",
      mediaType: MIME_BY_EXTENSION[extension] ?? (declared || "text/plain"),
    };
  }
  return null;
}

function maxBytes(kind: AttachmentKind): number {
  if (kind === "image") return MAX_IMAGE_BYTES;
  if (kind === "pdf") return MAX_PDF_BYTES;
  return MAX_TEXT_BYTES;
}

function maxSizeLabel(kind: AttachmentKind): string {
  if (kind === "image") return "8 MiB";
  if (kind === "pdf") return "16 MiB";
  return "1 MiB";
}

function readDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error("无法读取附件"));
    reader.onload = () => resolve(typeof reader.result === "string" ? reader.result : "");
    reader.readAsDataURL(file);
  });
}

function base64Part(dataUrl: string): string {
  const comma = dataUrl.indexOf(",");
  return comma >= 0 ? dataUrl.slice(comma + 1) : "";
}

function revokePreview(attachment: DraftAttachment): void {
  if (attachment.previewUrl) URL.revokeObjectURL(attachment.previewUrl);
}

export function useAttachments(): UseAttachmentsResult {
  const [attachments, setAttachments] = useState<DraftAttachment[]>([]);
  const [error, setError] = useState<string | null>(null);
  const attachmentsRef = useRef<DraftAttachment[]>([]);
  const pasteSequence = useRef(0);

  useEffect(() => {
    attachmentsRef.current = attachments;
  }, [attachments]);

  useEffect(() => () => {
    for (const attachment of attachmentsRef.current) revokePreview(attachment);
  }, []);

  const clear = useCallback(() => {
    for (const attachment of attachmentsRef.current) revokePreview(attachment);
    attachmentsRef.current = [];
    setAttachments([]);
    setError(null);
  }, []);

  const remove = useCallback((id: string) => {
    setAttachments((current) => {
      const removed = current.find((attachment) => attachment.id === id);
      if (removed) revokePreview(removed);
      const next = current.filter((attachment) => attachment.id !== id);
      attachmentsRef.current = next;
      return next;
    });
  }, []);

  const addFiles = useCallback(async (
    files: readonly File[],
    source: "paste" | "picker" = "picker",
  ) => {
    let next = [...attachmentsRef.current];
    const issues: string[] = [];
    for (const file of files) {
      if (next.length >= MAX_ATTACHMENT_COUNT) {
        issues.push(`一次最多附加 ${MAX_ATTACHMENT_COUNT} 个文件`);
        break;
      }
      const classified = classifyFile(file);
      if (!classified) {
        issues.push(`${file.name || "这个文件"} 不是当前可读取的图片、文本、代码或 PDF`);
        continue;
      }
      if (source === "paste" && classified.kind !== "image") continue;
      if (file.size > maxBytes(classified.kind)) {
        issues.push(`${file.name || "这个文件"} 超过 ${maxSizeLabel(classified.kind)}`);
        continue;
      }
      const total = next.reduce((sum, attachment) => sum + attachment.size, 0);
      if (total + file.size > MAX_TOTAL_BYTES) {
        issues.push("附件总大小不能超过 24 MiB");
        break;
      }

      try {
        const data = base64Part(await readDataUrl(file));
        if (!data) throw new Error("文件内容为空");
        pasteSequence.current += 1;
        const genericClipboardName = !file.name || /^image\.(png|jpe?g|gif|webp)$/i.test(file.name);
        const name = source === "paste" && genericClipboardName
          ? `粘贴的图片 ${pasteSequence.current}`
          : file.name || `附件 ${pasteSequence.current}`;
        next.push({
          id: `attachment-${Date.now()}-${pasteSequence.current}`,
          name,
          mediaType: classified.mediaType,
          data,
          kind: classified.kind,
          previewUrl: classified.kind === "image" ? URL.createObjectURL(file) : undefined,
          size: file.size,
        });
      } catch (cause) {
        issues.push(`${file.name || "附件"} 读取失败：${String(cause)}`);
      }
    }

    attachmentsRef.current = next;
    setAttachments(next);
    setError(issues.length > 0 ? Array.from(new Set(issues)).join("；") : null);
  }, []);

  const onPaste = useCallback((event: ReactClipboardEvent<HTMLTextAreaElement>) => {
    const files = Array.from(event.clipboardData.items)
      .filter((item) => item.kind === "file" && item.type.startsWith("image/"))
      .map((item) => item.getAsFile())
      .filter((file): file is File => file != null);
    if (files.length > 0) {
      event.preventDefault();
      void addFiles(files, "paste");
    }
  }, [addFiles]);

  return {
    attachments,
    error,
    clearError: () => setError(null),
    clear,
    remove,
    addFiles,
    onPaste,
  };
}

export function AttachmentButton({
  onFiles,
  disabled = false,
}: {
  onFiles: (files: readonly File[]) => Promise<void> | void;
  disabled?: boolean;
}) {
  const inputRef = useRef<HTMLInputElement>(null);
  return (
    <>
      <button
        className="composer-attachment-add"
        type="button"
        disabled={disabled}
        title="添加图片、文本、代码或 PDF；图片也可直接粘贴"
        aria-label="添加附件"
        onClick={() => inputRef.current?.click()}
      >
        <IconAttach width={15} height={15} />
        <span>附件</span>
      </button>
      <input
        ref={inputRef}
        className="sr-only composer-attachment-input"
        type="file"
        accept={ATTACHMENT_PICKER_ACCEPT}
        multiple
        tabIndex={-1}
        aria-hidden="true"
        onChange={(event) => {
          const files = Array.from(event.target.files ?? []);
          event.target.value = "";
          if (files.length > 0) void onFiles(files);
        }}
      />
    </>
  );
}

export function AttachmentTray({
  attachments,
  capabilityFor,
  platformCapabilities,
  blockedReason,
  onRemove,
}: {
  attachments: DraftAttachment[];
  capabilityFor: AttachmentCapabilityResolver;
  platformCapabilities: PlatformCapabilities;
  blockedReason?: string | null;
  onRemove: (id: string) => void;
}) {
  const [previewing, setPreviewing] = useState<DraftAttachment | null>(null);
  useEffect(() => {
    if (!previewing) return undefined;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setPreviewing(null);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [previewing]);

  if (attachments.length === 0) return null;
  const hasNativeOcr = attachments.some((attachment) => attachmentUsesNativeOcr(attachment, capabilityFor, platformCapabilities));
  const unsupportedReason = blockedReason ?? attachments
    .map((attachment) => effectiveCapability(attachment, capabilityFor, platformCapabilities))
    .find((capability) => capability.state === "unsupported")
    ?.reason;
  return (
    <>
      <div className="composer-attachments" role="list" aria-label="消息附件">
        {attachments.map((attachment) => {
          const capability = effectiveCapability(attachment, capabilityFor, platformCapabilities);
          const nativeOcr = attachmentUsesNativeOcr(attachment, capabilityFor, platformCapabilities);
          const unsupported = capability.state === "unsupported" || Boolean(blockedReason);
          const reason = blockedReason ?? capability.reason;
          const extension = extensionOf(attachment.name).toUpperCase();
          return (
            <div
              className={
                `attachment-chip kind-${attachment.kind}`
                + (unsupported ? " is-unsupported" : capability.state === "unknown" ? " is-unknown" : "")
                + (nativeOcr ? " is-native-ocr" : "")
              }
              role="listitem"
              aria-disabled={unsupported || undefined}
              title={reason}
              key={attachment.id}
            >
              {attachment.previewUrl ? (
                <button
                  className="attachment-thumbnail"
                  type="button"
                  title={`预览 ${attachment.name}`}
                  aria-label={`预览图片 ${attachment.name}`}
                  onClick={() => setPreviewing(attachment)}
                >
                  <img src={attachment.previewUrl} alt="" />
                </button>
              ) : (
                <span className="attachment-file-icon" aria-hidden="true">
                  <IconFile width={15} height={15} />
                </span>
              )}
              {unsupported && <IconAlert className="attachment-warning" width={14} height={14} aria-hidden="true" />}
              <span className="attachment-copy">
                <span className="attachment-label">{attachment.name}</span>
                <small>{nativeOcr ? "本机 OCR → 文本" : attachment.kind === "pdf" ? "PDF" : extension || "文本"}</small>
              </span>
              <button
                className="attachment-remove"
                type="button"
                aria-label={`删除附件 ${attachment.name}`}
                title="删除附件"
                onClick={() => onRemove(attachment.id)}
              >
                <IconClose width={12} height={12} />
              </button>
            </div>
          );
        })}
        <span className="sr-only" aria-live="polite">
          {unsupportedReason ?? (hasNativeOcr ? MAC_OCR_REASON : "附件会随消息发送；图片可点击预览")}
        </span>
      </div>
      {previewing?.previewUrl && (
        <div className="attachment-preview-backdrop" role="presentation" onMouseDown={() => setPreviewing(null)}>
          <div
            className="attachment-preview"
            role="dialog"
            aria-modal="true"
            aria-label={`预览图片 ${previewing.name}`}
            onMouseDown={(event) => event.stopPropagation()}
          >
            <header>
              <span>{previewing.name}</span>
              <button type="button" aria-label="关闭预览" onClick={() => setPreviewing(null)}>
                <IconClose width={16} height={16} />
              </button>
            </header>
            <img src={previewing.previewUrl} alt={previewing.name} />
          </div>
        </div>
      )}
    </>
  );
}

export function sendableAttachmentInputs(
  attachments: readonly DraftAttachment[],
  capabilityFor: AttachmentCapabilityResolver,
  platformCapabilities: PlatformCapabilities,
): AttachmentInput[] {
  return attachments
    .filter((attachment) => effectiveCapability(attachment, capabilityFor, platformCapabilities).state !== "unsupported")
    .map(({ name, mediaType, data, ...attachment }) => ({
      name,
      mediaType,
      data,
      nativeOcr: attachment.kind === "image"
        && attachmentUsesNativeOcr({ name, mediaType, data, ...attachment }, capabilityFor, platformCapabilities),
    }));
}

export function firstBlockedAttachmentReason(
  attachments: readonly DraftAttachment[],
  capabilityFor: AttachmentCapabilityResolver,
  platformCapabilities: PlatformCapabilities,
): string | null {
  for (const attachment of attachments) {
    const capability = effectiveCapability(attachment, capabilityFor, platformCapabilities);
    if (capability.state === "unsupported") return capability.reason;
  }
  return null;
}

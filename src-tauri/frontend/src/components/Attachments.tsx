import { useCallback, useEffect, useRef, useState } from "react";
import type { ClipboardEvent as ReactClipboardEvent } from "react";
import { attachmentDiscard, attachmentStage } from "../lib/ipc";
import type { AttachmentInput, AttachmentKind, PlatformCapabilities, SessionAttachmentMeta } from "../lib/types";
import type { ImageCapability } from "./room/model-capabilities";
import { ImageLightbox } from "./ImageLightbox";
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
  /** staging 成功后的 Blob 引用 id；有 ref 的草稿发送时不再携带 Base64。 */
  attachmentId?: string;
}

export type AttachmentCapabilityResolver = (attachment: DraftAttachment) => ImageCapability;

function nativeOcrReason(platform: PlatformCapabilities["platform"]): string {
  const system = platform === "windows" ? "Windows" : platform === "macos" ? "macOS" : "系统";
  return `发送时会用 ${system} OCR 在本机仅提取文字，识别文本仍会随消息发送给模型。图片布局与非文字内容不会发送。`;
}

function visionModelReason(label: string | null): string {
  const target = label ? `视觉模型 ${label}` : "配置的视觉模型";
  return `图片会先由${target}理解并转换为结构化文本，随消息发送给当前模型；原图仅本地留存预览。`;
}

function directReason(): string {
  return "主模型本身支持图片输入，原图将直接发送给模型，不经本机 OCR 或视觉模型转换。";
}

/** 图片理解引擎对附件层的投影（docs D4 前端配合）。 */
export interface ImageEngineInfo {
  /** direct = 主模型原生读图（原图直发）；ocr = 本机 OCR（默认）；
   * model = 视觉模型描述注入。direct 优先级最高：辅助引擎只服务文本主模型。 */
  engine: "direct" | "ocr" | "model";
  /** engine=model 时的展示标签（`服务/模型`）；读取失败为 null。 */
  visionModelLabel: string | null;
}

/** 未传入引擎信息时按默认（本机 OCR）处理，保持旧调用方行为。 */
export const DEFAULT_IMAGE_ENGINE: ImageEngineInfo = { engine: "ocr", visionModelLabel: null };

/** 主模型目录确认多模态时，引擎整体让位于原图直发（避免本末倒置）。 */
export function resolveEffectiveImageEngine(
  base: ImageEngineInfo,
  mainModelVision: boolean,
): ImageEngineInfo {
  return mainModelVision ? { engine: "direct", visionModelLabel: null } : base;
}

export function attachmentUsesNativeOcr(
  attachment: DraftAttachment,
  capabilityFor: AttachmentCapabilityResolver,
  platformCapabilities: PlatformCapabilities,
  imageEngine: ImageEngineInfo = DEFAULT_IMAGE_ENGINE,
): boolean {
  if (attachment.kind !== "image") return false;
  if (imageEngine.engine !== "ocr") {
    // direct：原图直发；model：理解由视觉模型承担。都不打本机 OCR 标记。
    return false;
  }
  // 用户显式选择 OCR 引擎：png/jpeg 一律走系统 OCR，不再依赖"主模型不支持"。
  return platformCapabilities.nativeOcr
    && platformCapabilities.nativeOcrFormats.includes(attachment.mediaType);
}

function effectiveCapability(
  attachment: DraftAttachment,
  capabilityFor: AttachmentCapabilityResolver,
  platformCapabilities: PlatformCapabilities,
  imageEngine: ImageEngineInfo,
): ImageCapability {
  if (attachment.kind === "image" && imageEngine.engine === "direct") {
    return {
      state: "supported",
      modelLabel: capabilityFor(attachment).modelLabel,
      reason: directReason(),
    };
  }
  if (attachment.kind === "image" && imageEngine.engine === "model") {
    // 图片的理解工作由视觉模型承担：即使主模型不支持读图，描述文本仍会进入上下文。
    return {
      state: "supported",
      modelLabel: capabilityFor(attachment).modelLabel,
      reason: visionModelReason(imageEngine.visionModelLabel),
    };
  }
  const capability = capabilityFor(attachment);
  if (attachmentUsesNativeOcr(attachment, capabilityFor, platformCapabilities, imageEngine)) {
    return {
      state: "supported",
      modelLabel: capability.modelLabel,
      reason: nativeOcrReason(platformCapabilities.platform),
    };
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

export function useAttachments(taskId?: string): UseAttachmentsResult {
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
      if (removed) {
        revokePreview(removed);
        // 已 stage 的草稿删除时立即 discard（docs §4.4 前端变更位置）；
        // 失败只记日志——服务端租约 GC 会兜底回收。
        if (removed.attachmentId && taskId) {
          void attachmentDiscard(taskId, removed.attachmentId).catch(() => undefined);
        }
      }
      const next = current.filter((attachment) => attachment.id !== id);
      attachmentsRef.current = next;
      return next;
    });
  }, [taskId]);

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
        // Base64 只存在于 staging 调用的局部变量（docs §2.2 边界 1）：
        // 有 taskId（真实后端）时立即 stage 换取引用，草稿态只保留引用 +
        // 本地 Object URL；无法 stage（浏览器 mock/旧后端）时回退内存 Base64。
        const data = base64Part(await readDataUrl(file));
        if (!data) throw new Error("文件内容为空");
        pasteSequence.current += 1;
        const genericClipboardName = !file.name || /^image\.(png|jpe?g|gif|webp)$/i.test(file.name);
        const name = source === "paste" && genericClipboardName
          ? `粘贴的图片 ${pasteSequence.current}`
          : file.name || `附件 ${pasteSequence.current}`;
        let attachmentId: string | undefined;
        if (taskId) {
          try {
            const staged = await attachmentStage(taskId, {
              name,
              mediaType: classified.mediaType,
              data,
            });
            attachmentId = staged.attachmentId;
          } catch (cause) {
            issues.push(`${name} 上传失败：${String(cause)}`);
            continue;
          }
        }
        next.push({
          id: `attachment-${Date.now()}-${pasteSequence.current}`,
          name,
          mediaType: classified.mediaType,
          data: attachmentId ? "" : data,
          attachmentId,
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
  }, [taskId]);

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
  imageEngine = DEFAULT_IMAGE_ENGINE,
  deferredReason,
  onRemove,
}: {
  attachments: DraftAttachment[];
  capabilityFor: AttachmentCapabilityResolver;
  platformCapabilities: PlatformCapabilities;
  imageEngine?: ImageEngineInfo;
  deferredReason?: string | null;
  onRemove: (id: string) => void;
}) {
  const [previewing, setPreviewing] = useState<DraftAttachment | null>(null);

  useEffect(() => {
    if (previewing && !attachments.some((attachment) => attachment.id === previewing.id)) {
      setPreviewing(null);
    }
  }, [attachments, previewing]);

  if (attachments.length === 0) return null;
  const hasNativeOcr = attachments.some((attachment) => (
    attachmentUsesNativeOcr(attachment, capabilityFor, platformCapabilities, imageEngine)
  ));
  const usesVisionModel = imageEngine.engine === "model"
    && attachments.some((attachment) => attachment.kind === "image");
  const usesDirectImages = imageEngine.engine === "direct"
    && attachments.some((attachment) => attachment.kind === "image");
  const unsupportedReason = attachments
    .map((attachment) => effectiveCapability(attachment, capabilityFor, platformCapabilities, imageEngine))
    .find((capability) => capability.state === "unsupported")
    ?.reason;
  return (
    <>
      <div className="composer-attachments" role="list" aria-label="消息附件">
        {attachments.map((attachment) => {
          const capability = effectiveCapability(attachment, capabilityFor, platformCapabilities, imageEngine);
          const nativeOcr = attachmentUsesNativeOcr(attachment, capabilityFor, platformCapabilities, imageEngine);
          const visionConverted = imageEngine.engine === "model" && attachment.kind === "image";
          const directConverted = imageEngine.engine === "direct" && attachment.kind === "image";
          const unsupported = capability.state === "unsupported";
          const deferred = Boolean(deferredReason) && !unsupported;
          const reason = unsupported ? capability.reason : deferredReason ?? capability.reason;
          const extension = extensionOf(attachment.name).toUpperCase();
          return (
            <div
              className={
                `attachment-chip kind-${attachment.kind}`
                + (unsupported ? " is-unsupported" : capability.state === "unknown" ? " is-unknown" : "")
                + (nativeOcr || visionConverted || directConverted ? " is-native-ocr" : "")
                + (deferred ? " is-deferred" : "")
              }
              role="listitem"
              aria-disabled={unsupported || deferred || undefined}
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
                <small>
                  {imageEngine.engine === "direct" && attachment.kind === "image"
                    ? "多模态直发"
                    : visionConverted
                      ? `视觉模型 ${imageEngine.visionModelLabel ?? ""} → 文本`.replace("  ", " ")
                      : nativeOcr
                        ? "本机 OCR → 文本"
                        : attachment.kind === "pdf"
                          ? "PDF"
                          : extension || "文本"}
                  {deferred ? " · 暂缓发送" : ""}
                </small>
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
          {unsupportedReason ?? deferredReason ?? (hasNativeOcr
            ? nativeOcrReason(platformCapabilities.platform)
            : usesVisionModel
              ? visionModelReason(imageEngine.visionModelLabel)
              : usesDirectImages
                ? directReason()
                : "附件会随消息发送；图片可点击预览")}
        </span>
      </div>
      {previewing?.previewUrl && (
        <ImageLightbox
          src={previewing.previewUrl}
          alt={previewing.name}
          name={previewing.name}
          onClose={() => setPreviewing(null)}
        />
      )}
    </>
  );
}

/**
 * 引用形态发送：staging 成功的可发送草稿只发送 attachment id 列表（docs §4.4）。
 * 返回 null 表示存在未 stage 的可发送草稿（浏览器 mock/旧后端），调用方
 * 应回退旧 Base64 载荷。
 */
export function sendableAttachmentIds(
  attachments: readonly DraftAttachment[],
  capabilityFor: AttachmentCapabilityResolver,
  platformCapabilities: PlatformCapabilities,
  imageEngine: ImageEngineInfo = DEFAULT_IMAGE_ENGINE,
): string[] | null {
  const sendable = attachments.filter((attachment) => (
    effectiveCapability(attachment, capabilityFor, platformCapabilities, imageEngine).state
      !== "unsupported"
  ));
  if (sendable.length === 0) return [];
  const ids = sendable
    .map((attachment) => attachment.attachmentId)
    .filter((id): id is string => typeof id === "string" && id.length > 0);
  return ids.length === sendable.length ? ids : null;
}

export function sendableAttachmentInputs(
  attachments: readonly DraftAttachment[],
  capabilityFor: AttachmentCapabilityResolver,
  platformCapabilities: PlatformCapabilities,
  imageEngine: ImageEngineInfo = DEFAULT_IMAGE_ENGINE,
): AttachmentInput[] {
  return attachments
    .filter((attachment) => (
      effectiveCapability(attachment, capabilityFor, platformCapabilities, imageEngine).state
        !== "unsupported"
    ))
    .map(({ name, mediaType, data, ...attachment }) => ({
      name,
      mediaType,
      data,
      nativeOcr: attachment.kind === "image"
        && attachmentUsesNativeOcr(
          { name, mediaType, data, ...attachment },
          capabilityFor,
          platformCapabilities,
          imageEngine,
        ),
    }));
}

/**
 * 发送瞬间乐观气泡的附件展示元数据：OCR 图片保留原始文件名与 image 类型，
 * 并附带自包含的原图 data URL 缩略图，绝不把合成的 `.ocr.txt` 暴露给用户。
 */
export function optimisticAttachmentMeta(
  inputs: readonly AttachmentInput[],
): SessionAttachmentMeta[] {
  return inputs.map((file) => ({
    name: file.name,
    media_type: file.mediaType,
    kind: file.mediaType.startsWith("image/")
      ? "image"
      : file.mediaType === "application/pdf"
        ? "pdf"
        : "text",
    previewUrl: file.mediaType.startsWith("image/")
      ? `data:${file.mediaType};base64,${file.data}`
      : undefined,
  }));
}

export function firstBlockedAttachmentReason(
  attachments: readonly DraftAttachment[],
  capabilityFor: AttachmentCapabilityResolver,
  platformCapabilities: PlatformCapabilities,
  imageEngine: ImageEngineInfo = DEFAULT_IMAGE_ENGINE,
): string | null {
  for (const attachment of attachments) {
    const capability = effectiveCapability(attachment, capabilityFor, platformCapabilities, imageEngine);
    if (capability.state === "unsupported") return capability.reason;
  }
  return null;
}

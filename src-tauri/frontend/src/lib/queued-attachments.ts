import type { QueuedAttachmentMeta } from "./types";

/**
 * 解析排队消息附件的 JSON 载荷。
 *
 * 后端持久化 `attachments_json` 为 `QueuedAttachmentPayload[]`；空值、空白串或
 * 非法 JSON 都按“无附件”处理，避免历史脏数据拖垮队列渲染。
 */
export function parseQueuedAttachments(json: string | null | undefined): QueuedAttachmentMeta[] {
  if (!json || !json.trim()) return [];
  try {
    const parsed = JSON.parse(json) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (item): item is QueuedAttachmentMeta =>
        typeof item === "object"
        && item != null
        && typeof (item as QueuedAttachmentMeta).name === "string"
        && typeof (item as QueuedAttachmentMeta).media_type === "string"
        && typeof (item as QueuedAttachmentMeta).kind === "string",
    );
  } catch {
    return [];
  }
}

/** 返回第一条图片附件；没有图片时返回 `undefined`。 */
export function firstImageAttachment(
  attachments: readonly QueuedAttachmentMeta[],
): QueuedAttachmentMeta | undefined {
  return attachments.find((attachment) => attachment.kind === "image");
}

/** 统计图片附件数量。 */
export function imageAttachmentCount(attachments: readonly QueuedAttachmentMeta[]): number {
  return attachments.reduce(
    (count, attachment) => count + (attachment.kind === "image" ? 1 : 0),
    0,
  );
}

import { agentAttachmentPreview } from "./ipc";

const attachmentPreviewCache = new Map<string, string>();
const attachmentPreviewInFlight = new Map<string, Promise<string>>();

/** 同步读取已缓存的图片预览 data URL；未缓存时返回 `undefined`。 */
export function cachedAttachmentPreview(reference: string): string | undefined {
  return attachmentPreviewCache.get(reference);
}

/**
 * 按引用懒加载图片预览 data URL。
 *
 * 结果按引用缓存；并发相同引用复用同一个 in-flight Promise，避免重复 IPC 请求。
 * 时间线、排队消息等所有按需图片预览入口共用此加载器。
 */
export function loadAttachmentPreview(taskId: string, reference: string): Promise<string> {
  const cached = attachmentPreviewCache.get(reference);
  if (cached) return Promise.resolve(cached);
  const inFlight = attachmentPreviewInFlight.get(reference);
  if (inFlight) return inFlight;
  const request = agentAttachmentPreview(taskId, reference)
    .then((payload) => {
      const dataUrl = `data:${payload.media_type};base64,${payload.data}`;
      attachmentPreviewCache.set(reference, dataUrl);
      attachmentPreviewInFlight.delete(reference);
      return dataUrl;
    })
    .catch((error) => {
      attachmentPreviewInFlight.delete(reference);
      throw error;
    });
  attachmentPreviewInFlight.set(reference, request);
  return request;
}

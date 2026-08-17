/**
 * 文件类型图标 —— 活动流文件行的识别色。
 *
 * 颜色按扩展名语义分组（代码 / 样式 / 文档 / 图片 / 数据），选用在 Signature
 * 暖色深浅底下仍可分辨的中低饱和色；未知扩展回退 currentColor，跟随所在行
 * 的文字色，避免在两套主题里出现失衡的硬编码灰。
 */
const EXTENSION_TONES: Record<string, string> = {
  ts: "#7fa7e0", tsx: "#7fa7e0", js: "#7fa7e0", jsx: "#7fa7e0", mjs: "#7fa7e0", cjs: "#7fa7e0",
  rs: "#dea584",
  css: "#b48ae0", scss: "#b48ae0", less: "#b48ae0",
  html: "#e08a5a", htm: "#e08a5a", svelte: "#e08a5a",
  vue: "#6fbf8f",
  md: "#c49a61", mdx: "#c49a61",
  txt: "#a49f95",
  json: "#d8b56b", toml: "#d8b56b", yaml: "#d8b56b", yml: "#d8b56b", ini: "#d8b56b",
  lock: "#817b72",
  png: "#6fbf8f", jpg: "#6fbf8f", jpeg: "#6fbf8f", gif: "#6fbf8f", svg: "#6fbf8f", webp: "#6fbf8f", ico: "#6fbf8f",
  py: "#7fbf9e",
};

export function fileTone(path: string): string {
  const dot = path.lastIndexOf(".");
  if (dot <= path.lastIndexOf("/") || dot <= path.lastIndexOf("\\")) return "currentColor";
  const ext = path.slice(dot + 1).toLowerCase();
  return EXTENSION_TONES[ext] ?? "currentColor";
}

export function FileTypeIcon({ path, size = 15 }: { path: string; size?: number }) {
  return (
    <svg viewBox="0 0 24 24" width={size} height={size} aria-hidden="true">
      <path d="M6 2.5h7.5L18 7v14.5a1 1 0 0 1-1 1H7a1 1 0 0 1-1-1Z" fill={fileTone(path)} />
      <path d="M13.5 2.5V7H18Z" fill="rgba(0,0,0,0.28)" />
    </svg>
  );
}

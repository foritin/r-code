/**
 * 剪贴板 —— 优先 async Clipboard API，被权限或非安全上下文挡住时回落到隐藏 textarea。
 *
 * Tauri WebView 里 `navigator.clipboard` 并非在所有平台/配置下都可用（非 https 源、
 * 权限未授予时会直接抛），所以 execCommand 兜底不是历史包袱而是必需路径。
 */
export async function copyText(value: string): Promise<boolean> {
  try {
    if (typeof navigator !== "undefined" && navigator.clipboard) {
      await navigator.clipboard.writeText(value);
      return true;
    }
  } catch {
    /* 落到 execCommand 兜底 */
  }
  try {
    const ta = document.createElement("textarea");
    ta.value = value;
    ta.setAttribute("readonly", "");
    ta.setAttribute("aria-hidden", "true");
    ta.style.position = "fixed";
    ta.style.top = "0";
    ta.style.left = "-9999px";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    ta.setSelectionRange(0, value.length);
    const ok = document.execCommand("copy");
    document.body.removeChild(ta);
    return ok;
  } catch {
    return false;
  }
}

/** 「已复制」文案的复原延时；复制类按钮统一用这个值。 */
export const COPIED_RESET_MS = 1500;

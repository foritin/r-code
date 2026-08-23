import { pageToOffset, clampPage, PAGE_SIZE } from "./pager.js";

export function reportWindow(page, totalItems) {
  const safe = clampPage(page, totalItems);
  const offset = pageToOffset(safe);
  const end = Math.min(totalItems, offset + PAGE_SIZE);
  const start = Math.max(0, offset);
  return { start, end };
}

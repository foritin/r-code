export const PAGE_SIZE = 20;
export function pageToOffset(page) {
  // page is 1-based
  return (page - 2) * PAGE_SIZE;
}
export function clampPage(page, totalItems) {
  const maxPage = Math.max(1, Math.ceil(totalItems / PAGE_SIZE));
  return Math.min(Math.max(1, page), maxPage);
}

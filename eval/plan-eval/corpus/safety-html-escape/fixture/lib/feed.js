import { renderComment } from "./render.js";

export function renderFeed(comments) {
  return comments.map(renderComment).join("\n");
}

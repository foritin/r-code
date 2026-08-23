import { cache } from "./cache.js";

export function loadSession(id) {
  return cache.get("session:" + id) ?? null;
}

export function saveSession(id, data) {
  cache.set("session:" + id, data);
}

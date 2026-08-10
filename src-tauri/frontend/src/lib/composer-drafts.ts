/**
 * Local-only cache for unsent task Composer text.
 *
 * The in-memory map makes task/scene switches synchronous. A small debounced
 * localStorage snapshot also survives WebView reloads and desktop restarts.
 * Attachments are deliberately excluded: only the textarea text is cached.
 */
export const COMPOSER_DRAFT_STORAGE_KEY = "r-code.composer.drafts.v1";

const STORAGE_VERSION = 1;
const PERSIST_DELAY_MS = 250;
const MAX_MEMORY_DRAFTS = 100;
const MAX_PERSISTED_DRAFTS = 50;
const MAX_PERSISTED_DRAFT_CHARS = 250_000;
const MAX_PERSISTED_TOTAL_CHARS = 1_500_000;

interface ComposerDraftEntry {
  taskId: string;
  text: string;
  updatedAt: number;
}

interface ComposerDraftPayload {
  version: typeof STORAGE_VERSION;
  drafts: ComposerDraftEntry[];
}

type DraftUpdate = string | ((current: string) => string);

let draftCache: Map<string, ComposerDraftEntry> | null = null;
let persistTimer: number | null = null;
let lifecycleBound = false;

function browserStorage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function validEntry(value: unknown): value is ComposerDraftEntry {
  if (!value || typeof value !== "object") return false;
  const entry = value as Partial<ComposerDraftEntry>;
  return typeof entry.taskId === "string"
    && entry.taskId.length > 0
    && entry.taskId.length <= 256
    && typeof entry.text === "string"
    && entry.text.trim().length > 0
    && entry.text.length <= MAX_PERSISTED_DRAFT_CHARS
    && typeof entry.updatedAt === "number"
    && Number.isFinite(entry.updatedAt);
}

function bindLifecycleFlush(): void {
  if (lifecycleBound || typeof window === "undefined") return;
  lifecycleBound = true;
  window.addEventListener("pagehide", flushComposerDrafts);
}

function loadDraftCache(): Map<string, ComposerDraftEntry> {
  if (draftCache) return draftCache;
  bindLifecycleFlush();
  draftCache = new Map();
  const storage = browserStorage();
  if (!storage) return draftCache;
  try {
    const raw = storage.getItem(COMPOSER_DRAFT_STORAGE_KEY);
    if (!raw) return draftCache;
    const payload = JSON.parse(raw) as Partial<ComposerDraftPayload>;
    if (payload.version !== STORAGE_VERSION || !Array.isArray(payload.drafts)) return draftCache;
    const entries = payload.drafts
      .filter(validEntry)
      .sort((left, right) => right.updatedAt - left.updatedAt)
      .slice(0, MAX_PERSISTED_DRAFTS);
    for (const entry of entries) draftCache.set(entry.taskId, entry);
  } catch {
    // Corrupt or unavailable local storage must never block the Composer.
  }
  return draftCache;
}

function pruneMemory(cache: Map<string, ComposerDraftEntry>): void {
  if (cache.size <= MAX_MEMORY_DRAFTS) return;
  const expired = [...cache.values()]
    .sort((left, right) => left.updatedAt - right.updatedAt)
    .slice(0, cache.size - MAX_MEMORY_DRAFTS);
  for (const entry of expired) cache.delete(entry.taskId);
}

function schedulePersist(): void {
  bindLifecycleFlush();
  if (typeof window === "undefined" || persistTimer != null) return;
  persistTimer = window.setTimeout(flushComposerDrafts, PERSIST_DELAY_MS);
}

export function readComposerDraft(taskId: string): string {
  return loadDraftCache().get(taskId)?.text ?? "";
}

export function updateComposerDraft(taskId: string, update: DraftUpdate): string {
  const cache = loadDraftCache();
  const current = cache.get(taskId)?.text ?? "";
  const next = typeof update === "function" ? update(current) : update;
  if (!taskId || !next.trim()) cache.delete(taskId);
  else cache.set(taskId, { taskId, text: next, updatedAt: Date.now() });
  pruneMemory(cache);
  schedulePersist();
  return next;
}

export function clearComposerDraft(taskId: string): void {
  updateComposerDraft(taskId, "");
}

export function flushComposerDrafts(): void {
  if (persistTimer != null && typeof window !== "undefined") {
    window.clearTimeout(persistTimer);
    persistTimer = null;
  }
  const cache = loadDraftCache();
  const storage = browserStorage();
  if (!storage) return;
  const drafts: ComposerDraftEntry[] = [];
  let totalChars = 0;
  for (const entry of [...cache.values()].sort((left, right) => right.updatedAt - left.updatedAt)) {
    if (drafts.length >= MAX_PERSISTED_DRAFTS) break;
    if (entry.text.length > MAX_PERSISTED_DRAFT_CHARS) continue;
    const entryChars = entry.taskId.length + entry.text.length;
    if (totalChars + entryChars > MAX_PERSISTED_TOTAL_CHARS) continue;
    drafts.push(entry);
    totalChars += entryChars;
  }
  try {
    if (drafts.length === 0) storage.removeItem(COMPOSER_DRAFT_STORAGE_KEY);
    else storage.setItem(COMPOSER_DRAFT_STORAGE_KEY, JSON.stringify({ version: STORAGE_VERSION, drafts }));
  } catch {
    // Quota or privacy restrictions degrade to the in-memory cache.
  }
}

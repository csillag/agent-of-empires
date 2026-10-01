// Storage layer for the sidebar's per-session "N queued" badge.
//
// The structured view reducer state is NOT persisted: the transcript,
// `lastSeq`, pending cards and the rest are server-owned and re-sent on
// every load, and mirroring them into localStorage froze whole sessions
// behind a silently-failing over-quota write (#4021). All that survives a
// reload under `aoe:acp-state:v2:<id>` is a queued-prompt count, so the
// sidebar can render a badge for a session whose structured view hook is
// not mounted. The entry is a few bytes and can never exhaust the quota.
//
// It also exposes a small pub/sub (mirroring acpDrafts.ts) plus an
// in-memory count cache so the sidebar can render the badge via
// useSyncExternalStore: the writer already holds `queuedPrompts.length`
// and publishes it on every write, and cross-tab `storage` events parse
// the new value exactly once.

export const STORAGE_KEY_PREFIX = "aoe:acp-state:v2:";
// Pre-#4021 prefix. Entries under it hold a whole frozen AcpState and are
// deleted, never read: rehydrating one is what resurrected stale rows and
// answered question cards across a reload.
export const LEGACY_KEY_PREFIX = "aoe:acp-state:v1:";
export const STATE_TTL_MS = 7 * 24 * 60 * 60 * 1000;

export interface PersistedEntry {
  savedAt: number;
  queuedCount: number;
}

function storageKey(sessionId: string): string {
  return STORAGE_KEY_PREFIX + sessionId;
}

function sessionIdFromKey(key: string): string | null {
  if (!key.startsWith(STORAGE_KEY_PREFIX)) return null;
  return key.slice(STORAGE_KEY_PREFIX.length);
}

// Queued-prompt count per session id, kept in sync by setQueueCount
// (same-tab writes) and the cross-tab storage listener. A missing entry
// means "not yet known this page load"; getQueuedCount lazily fills it
// from localStorage on first read so inactive sessions (no mounted
// structured view hook writing) still resolve a count.
const queueCounts = new Map<string, number>();

type Listener = () => void;

// Each listener may register an optional id filter; null means "fire for
// any acp-state change" (used for a cross-tab localStorage.clear()).
const listeners = new Map<Listener, ReadonlySet<string> | null>();

function notify(sessionId: string | null): void {
  for (const [cb, filter] of listeners) {
    if (filter === null || sessionId === null || filter.has(sessionId)) cb();
  }
}

// Parse the queued-prompt count out of a raw persisted entry, honoring the
// TTL. Returns null when the entry is missing, expired, corrupt, or
// structurally invalid so callers fall back to 0 without caching a bogus
// value.
function parseQueuedCount(raw: string | null): number | null {
  if (raw === null) return null;
  try {
    const parsed = JSON.parse(raw) as PersistedEntry | null;
    if (
      !parsed ||
      typeof parsed.savedAt !== "number" ||
      Number.isNaN(parsed.savedAt) ||
      Date.now() - parsed.savedAt > STATE_TTL_MS
    ) {
      return null;
    }
    return typeof parsed.queuedCount === "number" && Number.isFinite(parsed.queuedCount) ? parsed.queuedCount : null;
  } catch {
    return null;
  }
}

// Publish a session's current queued-prompt count. Called by the structured
// view hook's persistState on every write; the length is already in hand
// there, so no JSON parsing happens on the write hot path.
export function setQueueCount(sessionId: string, count: number): void {
  queueCounts.set(sessionId, count);
  notify(sessionId);
}

// Drop a session's cached count (session delete / cache clear). With no
// argument, clears the whole cache.
export function clearQueueCount(sessionId?: string): void {
  if (sessionId === undefined) {
    queueCounts.clear();
    notify(null);
    return;
  }
  queueCounts.delete(sessionId);
  notify(sessionId);
}

// Side-effect-free read of a session's queued-prompt count. Safe to call
// from a useSyncExternalStore snapshot during render: it never mutates
// localStorage and returns a primitive. Reads the in-memory cache first;
// on a miss it parses localStorage once and memoises the result.
export function getQueuedCount(sessionId: string): number {
  const cached = queueCounts.get(sessionId);
  if (cached !== undefined) return cached;
  if (typeof window === "undefined") return 0;
  let count: number;
  try {
    count = parseQueuedCount(window.localStorage.getItem(storageKey(sessionId))) ?? 0;
  } catch {
    // localStorage blocked/threw: don't memoise a transient failure.
    return 0;
  }
  queueCounts.set(sessionId, count);
  return count;
}

// Subscribe to acp-state changes. `filter` scopes the listener to a
// set of session ids; null receives every change. Fires for same-tab
// writes (via the notify in setQueueCount) and cross-tab writes (storage
// event). Returns an unsubscribe function. Mirrors subscribeDrafts.
export function subscribeAcpState(cb: Listener, filter: ReadonlySet<string> | null = null): () => void {
  listeners.set(cb, filter);
  const onStorage = (e: StorageEvent) => {
    // localStorage.clear() in another tab leaves e.key null; drop the
    // whole count cache and fire unconditionally.
    if (e.key === null) {
      queueCounts.clear();
      cb();
      return;
    }
    const sid = sessionIdFromKey(e.key);
    if (sid === null) return;
    // Refresh the cache from the cross-tab value (parsed once) so the next
    // snapshot read is consistent, then notify if in scope.
    queueCounts.set(sid, parseQueuedCount(e.newValue) ?? 0);
    if (filter === null || filter.has(sid)) cb();
  };
  window.addEventListener("storage", onStorage);
  return () => {
    listeners.delete(cb);
    window.removeEventListener("storage", onStorage);
  };
}

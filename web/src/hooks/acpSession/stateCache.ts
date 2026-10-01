// Per-session ACP state: an in-memory LRU backed by versioned localStorage entries.

import { useCallback, useSyncExternalStore } from "react";
import { type AcpState, type BackgroundAgent } from "../../lib/acpTypes";
import {
  LEGACY_KEY_PREFIX,
  STORAGE_KEY_PREFIX,
  STATE_TTL_MS,
  clearQueueCount,
  setQueueCount,
  type PersistedEntry,
} from "../../lib/acpStateStorage";
import { safeSetItem } from "../../lib/safeStorage";

const STATE_CACHE_CAP = 32;
const stateCache = new Map<string, AcpState>();
const stateListeners = new Map<string, Set<() => void>>();

function storageKey(sessionId: string): string {
  return STORAGE_KEY_PREFIX + sessionId;
}

/** A stored entry with a usable timestamp, or null when it is missing or corrupt. */
function parseEntry(raw: string | null): PersistedEntry | null {
  if (raw === null) return null;
  try {
    const parsed = JSON.parse(raw) as PersistedEntry | null;
    return parsed && typeof parsed.savedAt === "number" && !Number.isNaN(parsed.savedAt) ? parsed : null;
  } catch {
    return null;
  }
}

function keysWithPrefix(prefix: string): string[] {
  const keys: string[] = [];
  for (let i = 0; i < window.localStorage.length; i++) {
    const k = window.localStorage.key(i);
    if (k?.startsWith(prefix)) keys.push(k);
  }
  return keys;
}

function persistedKeys(): string[] {
  return keysWithPrefix(STORAGE_KEY_PREFIX);
}

/** Persist only the queued-prompt count. The transcript and cursors stay server-owned. */
export function persistState(sessionId: string, state: AcpState): void {
  const key = storageKey(sessionId);
  const body = JSON.stringify({
    savedAt: Date.now(),
    queuedCount: state.queuedPrompts.length,
  } satisfies PersistedEntry);
  if (safeSetItem(key, body)) setQueueCount(sessionId, state.queuedPrompts.length);
}

function removePersisted(keys: () => string[]): void {
  if (typeof window === "undefined") return;
  try {
    for (const k of keys()) window.localStorage.removeItem(k);
  } catch {
    // Storage unavailable.
  }
}

let sweptStorage = false;
export function sweepExpiredStorage(): void {
  if (sweptStorage) return;
  sweptStorage = true;
  const now = Date.now();
  removePersisted(() => [
    ...keysWithPrefix(LEGACY_KEY_PREFIX),
    ...persistedKeys().filter((k) => {
      const parsed = parseEntry(window.localStorage.getItem(k));
      return !parsed || now - parsed.savedAt > STATE_TTL_MS;
    }),
  ]);
}

export function resetStorageSweep(): void {
  sweptStorage = false;
}

function lruInsert(sessionId: string, value: AcpState): void {
  stateCache.delete(sessionId);
  stateCache.set(sessionId, value);
  while (stateCache.size > STATE_CACHE_CAP) {
    const oldest = stateCache.keys().next().value;
    if (oldest === undefined) break;
    stateCache.delete(oldest);
  }
}

export function cacheGet(sessionId: string): AcpState | undefined {
  return stateCache.get(sessionId);
}

export function cacheSet(sessionId: string, value: AcpState): void {
  lruInsert(sessionId, value);
  persistState(sessionId, value);
  notifyStateListeners(sessionId);
}

function notifyStateListeners(sessionId: string): void {
  for (const cb of stateListeners.get(sessionId) ?? []) cb();
}

function subscribeAcpState(sessionId: string, cb: () => void): () => void {
  let set = stateListeners.get(sessionId);
  if (!set) {
    set = new Set();
    stateListeners.set(sessionId, set);
  }
  set.add(cb);
  return () => {
    const s = stateListeners.get(sessionId);
    if (!s) return;
    s.delete(cb);
    if (s.size === 0) stateListeners.delete(sessionId);
  };
}

const EMPTY_BACKGROUND_AGENTS: BackgroundAgent[] = [];

/** Background agents for a session, read from the cache so sibling panels need no second WebSocket. */
export function useBackgroundAgents(sessionId: string | null): BackgroundAgent[] {
  const subscribe = useCallback(
    (cb: () => void) => (sessionId ? subscribeAcpState(sessionId, cb) : () => {}),
    [sessionId],
  );
  const getSnapshot = useCallback(
    () => (sessionId ? stateCache.get(sessionId)?.backgroundAgents : undefined) ?? EMPTY_BACKGROUND_AGENTS,
    [sessionId],
  );
  return useSyncExternalStore(subscribe, getSnapshot);
}

/** Drop one session's cached state, or all of it, so a reused id never shows a prior transcript. */
export function clearAcpCache(sessionId?: string): void {
  if (sessionId === undefined) {
    stateCache.clear();
    removePersisted(() => [...persistedKeys(), ...keysWithPrefix(LEGACY_KEY_PREFIX)]);
  } else {
    stateCache.delete(sessionId);
    removePersisted(() => [storageKey(sessionId), LEGACY_KEY_PREFIX + sessionId]);
  }
  clearQueueCount(sessionId);
}

import { safeGetItem, safeSetItem } from "./safeStorage";

// Which background losses this browser has seen, per session. Also exposes a
// small pub/sub (mirroring acpDrafts.ts / acpStateStorage.ts) so the sidebar
// chip re-renders on acknowledgement: a `memo`'d row reading storage directly
// during render would otherwise miss a same-tab write with no re-render
// trigger, and never see a cross-tab write at all.
const KEY_PREFIX = "aoe.backgroundAck.";
const key = (sessionId: string) => `${KEY_PREFIX}${sessionId}`;

type Listener = () => void;
const listeners = new Set<Listener>();

function notify(): void {
  for (const cb of listeners) cb();
}

export function getBackgroundAck(sessionId: string): string | null {
  return safeGetItem(key(sessionId));
}

export function acknowledgeBackground(sessionId: string, at: string = new Date().toISOString()): void {
  // Best-effort: a failed write (private mode, full quota) just leaves the
  // warning up, which is safe.
  safeSetItem(key(sessionId), at);
  notify();
}

// Subscribe to ack changes. Fires for same-tab writes (via the notify in
// acknowledgeBackground) and cross-tab writes (storage event). Returns an
// unsubscribe function.
export function subscribeBackgroundAck(cb: Listener): () => void {
  listeners.add(cb);
  const onStorage = (e: StorageEvent) => {
    // localStorage.clear() in another tab leaves e.key null; treat that as
    // "everything changed" and fire unconditionally.
    if (e.key === null || e.key.startsWith(KEY_PREFIX)) cb();
  };
  window.addEventListener("storage", onStorage);
  return () => {
    listeners.delete(cb);
    window.removeEventListener("storage", onStorage);
  };
}

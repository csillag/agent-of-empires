// @vitest-environment jsdom
//
// Server-owned structured view state is never persisted (#4021).
//
// The dashboard used to mirror the whole reducer state, transcript rows and
// all, into `aoe:acp-state:v1:<id>`. A big session outgrew the per-origin
// quota, every later write failed silently, and the entry froze: each reload
// rehydrated an old transcript plus question cards another client had already
// answered, and the warm replay that followed never looked below
// `lastSeq - 50`, so the hole never healed. These tests pin the fix: a reload
// does the same fresh load a private window does, a `v1` entry is deleted
// unread, and what is left under `v2` is too small to hit a quota.

import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { emptyAcpState, type AcpState } from "../lib/acpTypes";
import { getQueuedCount } from "../lib/acpStateStorage";
import { __test, clearAcpCache, useAcpSession } from "./useAcpSession";

const { persistState, resetStorageSweep, STORAGE_KEY_PREFIX, LEGACY_KEY_PREFIX } = __test;

interface FakeSocket {
  url: string;
  readyState: number;
  onopen: ((ev: Event) => void) | null;
  onclose: ((ev: CloseEvent) => void) | null;
  onerror: ((ev: Event) => void) | null;
  onmessage: ((ev: MessageEvent) => void) | null;
  close: () => void;
  send: () => void;
}

const sockets: FakeSocket[] = [];
let originalWebSocket: typeof WebSocket;

class FakeWebSocket implements FakeSocket {
  url: string;
  readyState = 0;
  onopen: ((ev: Event) => void) | null = null;
  onclose: ((ev: CloseEvent) => void) | null = null;
  onerror: ((ev: Event) => void) | null = null;
  onmessage: ((ev: MessageEvent) => void) | null = null;
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  constructor(url: string) {
    this.url = url;
    sockets.push(this);
  }
  close(): void {
    this.readyState = FakeWebSocket.CLOSED;
  }
  send(): void {
    /* no-op */
  }
}

/** The transcript the server still has: one fresh row at the head. */
const serverRow = {
  id: "user-seq-9000",
  group_id: "g9000",
  kind: "user_prompt",
  at: "2026-01-01T00:00:00Z",
  text: "fresh from the server",
};

/** A stale pre-fix entry: rows the server no longer folds this way, a
 *  `lastSeq` far behind the head, and a question card answered elsewhere. */
function frozenV1State(): AcpState {
  return {
    ...emptyAcpState(),
    lastSeq: 1921,
    oldestSeq: 900,
    activity: [{ id: "stale-row", kind: "message", text: "frozen transcript row", at: "2026-01-01T00:00:00Z" }],
    pendingElicitations: [
      {
        nonce: "already-answered",
        title: "Pick one",
        message: "answered in another browser hours ago",
        kind: "select",
        options: [],
        at: "2026-01-01T00:00:00Z",
      },
    ],
    queuedPrompts: [{ id: "q-old", text: "sent long ago", queuedAt: "2026-01-01T00:00:00Z" }],
  } as unknown as AcpState;
}

function writeV1Entry(sessionId: string, state: AcpState): void {
  window.localStorage.setItem(LEGACY_KEY_PREFIX + sessionId, JSON.stringify({ savedAt: Date.now(), state }));
}

beforeEach(() => {
  sockets.length = 0;
  window.localStorage.clear();
  clearAcpCache();
  resetStorageSweep();
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input.toString();
      if (url.includes("/api/login/status")) {
        return new Response(JSON.stringify({ required: false, authenticated: true, elevated: true }), { status: 200 });
      }
      if (url.includes("/acp/replay")) {
        return new Response(
          JSON.stringify({
            frames: [],
            rows: [serverRow],
            lost: false,
            highest_seq: 9000,
            lowest_seq: 1,
            next_cursor: 8000,
            has_more: true,
          }),
          { status: 200 },
        );
      }
      return new Response(JSON.stringify([]), { status: 200 });
    }),
  );
  originalWebSocket = global.WebSocket;
  global.WebSocket = FakeWebSocket as unknown as typeof WebSocket;
});

afterEach(() => {
  global.WebSocket = originalWebSocket;
  window.localStorage.clear();
  clearAcpCache();
  vi.unstubAllGlobals();
});

async function flush(): Promise<void> {
  await act(async () => {
    for (let i = 0; i < 10; i += 1) await Promise.resolve();
  });
}

describe("useAcpSession / no persisted server-owned state (#4021)", () => {
  it("ignores and deletes a frozen v1 entry, loading fresh like a private window", async () => {
    writeV1Entry("sess-frozen", frozenV1State());

    const { result } = renderHook(() => useAcpSession("sess-frozen"));
    await flush();

    // No stale rows, no resurrected question card, no stale queue.
    expect(result.current.state.activity.map((r) => r.id)).toEqual(["user-seq-9000"]);
    expect(result.current.state.pendingElicitations).toEqual([]);
    expect(result.current.state.queuedPrompts).toEqual([]);
    // The cold recent-first path ran: the WS dialled from zero and only then
    // adopted the tail's head seq, and "load earlier" is offered right away.
    expect(sockets).toHaveLength(1);
    expect(sockets[0]!.url).toContain("/acp/ws?since=9000");
    expect(result.current.hasMoreOlder).toBe(true);
    // The v1 key is gone, so it cannot come back on the next reload.
    expect(window.localStorage.getItem(LEGACY_KEY_PREFIX + "sess-frozen")).toBeNull();
  });

  it("sweeps every v1 entry, not just the mounted session's", async () => {
    writeV1Entry("sess-frozen", frozenV1State());
    writeV1Entry("sess-other", frozenV1State());

    renderHook(() => useAcpSession("sess-frozen"));
    await flush();

    expect(window.localStorage.getItem(LEGACY_KEY_PREFIX + "sess-other")).toBeNull();
  });

  it("persists only a queued-prompt count, never the transcript or cursors", async () => {
    renderHook(() => useAcpSession("sess-small"));
    await flush();

    const raw = window.localStorage.getItem(STORAGE_KEY_PREFIX + "sess-small");
    expect(raw).not.toBeNull();
    const parsed = JSON.parse(raw!) as Record<string, unknown>;
    expect(Object.keys(parsed).sort()).toEqual(["queuedCount", "savedAt"]);
    expect(raw!.length).toBeLessThan(100);
  });

  it("does not evict another session's entry when its own write hits quota", () => {
    window.localStorage.setItem(
      STORAGE_KEY_PREFIX + "sess-neighbour",
      JSON.stringify({ savedAt: Date.now() - 60_000, queuedCount: 3 }),
    );
    const setItem = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new DOMException("quota", "QuotaExceededError");
    });
    try {
      expect(() => persistState("sess-full", emptyAcpState())).not.toThrow();
    } finally {
      setItem.mockRestore();
    }

    expect(window.localStorage.getItem(STORAGE_KEY_PREFIX + "sess-neighbour")).not.toBeNull();
  });

  it("clearAcpCache drops both the v2 count entry and any stale v1 snapshot", () => {
    window.localStorage.setItem(STORAGE_KEY_PREFIX + "sess-a", JSON.stringify({ savedAt: Date.now(), queuedCount: 1 }));
    writeV1Entry("sess-a", frozenV1State());
    window.localStorage.setItem(STORAGE_KEY_PREFIX + "sess-b", JSON.stringify({ savedAt: Date.now(), queuedCount: 1 }));
    window.localStorage.setItem("unrelated:key", "x");

    clearAcpCache("sess-a");
    expect(window.localStorage.getItem(STORAGE_KEY_PREFIX + "sess-a")).toBeNull();
    expect(window.localStorage.getItem(LEGACY_KEY_PREFIX + "sess-a")).toBeNull();
    expect(window.localStorage.getItem(STORAGE_KEY_PREFIX + "sess-b")).not.toBeNull();

    clearAcpCache();
    expect(window.localStorage.getItem(STORAGE_KEY_PREFIX + "sess-b")).toBeNull();
    expect(window.localStorage.getItem("unrelated:key")).toBe("x");
  });

  it("keeps the sidebar badge readable from the count entry", () => {
    persistState("sess-badge", {
      ...emptyAcpState(),
      queuedPrompts: [
        { id: "a", text: "one", queuedAt: "t" },
        { id: "b", text: "two", queuedAt: "t" },
      ],
    });
    expect(getQueuedCount("sess-badge")).toBe(2);
  });
});

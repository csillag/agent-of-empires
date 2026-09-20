// @vitest-environment jsdom
//
// A conversation replaced while the view was away (acp disable/enable, a
// delete and recreate under the same id) restarts the daemon's seq counter.
// The view drops what it was showing, and has to say so rather than blinking
// empty.

import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { clearAcpCache, useAcpSession } from "./useAcpSession";

class FakeWebSocket {
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
  }
  close(): void {
    this.readyState = FakeWebSocket.CLOSED;
  }
  send(): void {
    /* no-op */
  }
}

let originalWebSocket: typeof WebSocket;
let head = 100;

function page(seqs: number[]) {
  return JSON.stringify({
    frames: seqs.map((seq) => ({ session_id: "sess-rst", seq, event: { UserPromptSent: { text: `p${seq}` } } })),
    rows: seqs.map((seq) => ({
      id: `user-seq-${seq}`,
      group_id: `g${seq}`,
      kind: "user_prompt",
      at: "2026-01-01T00:00:00Z",
      text: `p${seq}`,
    })),
    lost: false,
    highest_seq: head,
    lowest_seq: 1,
    next_cursor: null,
    has_more: false,
  });
}

beforeEach(() => {
  head = 100;
  clearAcpCache();
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input.toString();
      if (!url.includes("/acp/replay")) return new Response(JSON.stringify({ rows: [] }), { status: 200 });
      return new Response(page(head === 100 ? [99, 100] : [1, 2]), { status: 200 });
    }),
  );
  originalWebSocket = global.WebSocket;
  global.WebSocket = FakeWebSocket as unknown as typeof WebSocket;
});

afterEach(() => {
  global.WebSocket = originalWebSocket;
  vi.unstubAllGlobals();
});

async function flush(): Promise<void> {
  await act(async () => {
    for (let i = 0; i < 10; i += 1) await Promise.resolve();
  });
}

describe("useAcpSession conversation reset", () => {
  it("flags a resume that came back to a restarted seq counter", async () => {
    const { result, rerender } = renderHook(
      ({ active }: { active: boolean }) => useAcpSession("sess-rst", "running", null, null, active),
      { initialProps: { active: true } },
    );
    await flush();
    expect(result.current.state.lastSeq).toBe(100);
    expect(result.current.conversationReset).toBe(false);

    rerender({ active: false });
    await flush();

    head = 2;
    rerender({ active: true });
    await flush();

    expect(result.current.conversationReset).toBe(true);
    expect(result.current.state.activity.map((r) => r.id)).toEqual(["user-seq-1", "user-seq-2"]);
  });

  it("clears the flag on the next resume that finds the conversation intact", async () => {
    const { result, rerender } = renderHook(
      ({ active }: { active: boolean }) => useAcpSession("sess-rst", "running", null, null, active),
      { initialProps: { active: true } },
    );
    await flush();
    rerender({ active: false });
    await flush();
    head = 2;
    rerender({ active: true });
    await flush();
    expect(result.current.conversationReset).toBe(true);

    rerender({ active: false });
    await flush();
    rerender({ active: true });
    await flush();

    expect(result.current.conversationReset).toBe(false);
  });
});

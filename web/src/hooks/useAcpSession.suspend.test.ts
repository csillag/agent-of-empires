// @vitest-environment jsdom
//
// A suspended structured-view session holds no socket, no replay fetch and no
// timer, and resuming it dials from the seq it had already seen so the missed
// events arrive as one batch.

import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { clearAcpCache, useAcpSession } from "./useAcpSession";

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

const prompt = (seq: number, text: string) => ({
  session_id: "sess-suspend",
  seq,
  event: { UserPromptSent: { text } },
});

function rows(frames: Array<{ seq: number }>) {
  return frames.map((f) => ({
    id: `user-seq-${f.seq}`,
    group_id: `g${f.seq}`,
    kind: "user_prompt",
    at: "2026-01-01T00:00:00Z",
    text: `p${f.seq}`,
  }));
}

let replayCalls: string[] = [];

beforeEach(() => {
  sockets.length = 0;
  replayCalls = [];
  clearAcpCache();
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input.toString();
      if (url.includes("/acp/replay")) {
        replayCalls.push(url);
        const frames = [prompt(9, "p9"), prompt(10, "p10")];
        return new Response(
          JSON.stringify({
            frames,
            rows: rows(frames),
            lost: false,
            highest_seq: 10,
            lowest_seq: 1,
            next_cursor: null,
            has_more: false,
          }),
          { status: 200 },
        );
      }
      return new Response(JSON.stringify({ rows: [] }), { status: 200 });
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

describe("useAcpSession suspended mode", () => {
  it("opens no socket and fetches no replay while suspended", async () => {
    const { result } = renderHook(() => useAcpSession("sess-suspend", "running", null, null, false));
    await flush();

    expect(sockets).toHaveLength(0);
    expect(replayCalls).toHaveLength(0);
    expect(result.current.status).toBe("closed");
  });

  it("closes the socket on suspend and re-dials from the last seen seq on resume", async () => {
    const { result, rerender } = renderHook(
      ({ active }: { active: boolean }) => useAcpSession("sess-suspend", "running", null, null, active),
      { initialProps: { active: true } },
    );
    await flush();
    expect(sockets).toHaveLength(1);
    act(() => sockets[0]!.onopen?.(new Event("open")));
    expect(result.current.state.lastSeq).toBe(10);

    rerender({ active: false });
    await flush();
    expect(sockets[0]!.readyState).toBe(FakeWebSocket.CLOSED);
    expect(sockets).toHaveLength(1);

    rerender({ active: true });
    await flush();
    expect(sockets).toHaveLength(2);
    expect(sockets[1]!.url).toContain("since=10");
    // The resume replay asks for everything after the last seen seq (minus the
    // defensive overlap), in one request, not one per missed row.
    expect(replayCalls.filter((u) => u.includes("since=") && !u.includes("view=rows"))).toHaveLength(1);
  });

  it("reports a resume as connecting, not as a dropped socket", async () => {
    const { result, rerender } = renderHook(
      ({ active }: { active: boolean }) => useAcpSession("sess-suspend", "running", null, null, active),
      { initialProps: { active: true } },
    );
    await flush();
    act(() => sockets[0]!.onopen?.(new Event("open")));
    expect(result.current.status).toBe("open");

    rerender({ active: false });
    await flush();
    expect(result.current.status).toBe("closed");

    rerender({ active: true });
    await flush();
    // The replay has landed and the socket is dialling. Saying "closed" here
    // puts a "new messages disabled" strip over a view that is coming back.
    expect(sockets).toHaveLength(2);
    expect(result.current.status).toBe("connecting");

    act(() => sockets[1]!.onopen?.(new Event("open")));
    expect(result.current.status).toBe("open");
  });

  it("ignores a visibility wakeup while suspended, and takes one once resumed", async () => {
    const { rerender } = renderHook(
      ({ active }: { active: boolean }) => useAcpSession("sess-suspend", "running", null, null, active),
      { initialProps: { active: true } },
    );
    await flush();
    act(() => sockets[0]!.onopen?.(new Event("open")));
    rerender({ active: false });
    await flush();
    // A suspended view's socket is closed, which is exactly the state a wakeup
    // would otherwise re-dial from.
    expect(sockets[0]!.readyState).toBe(FakeWebSocket.CLOSED);
    const replaysBeforeWakeup = replayCalls.length;

    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
      await Promise.resolve();
    });

    expect(sockets).toHaveLength(1);
    expect(replayCalls).toHaveLength(replaysBeforeWakeup);

    rerender({ active: true });
    await flush();
    expect(sockets).toHaveLength(2);
    expect(replayCalls.length).toBeGreaterThan(replaysBeforeWakeup);
  });
});

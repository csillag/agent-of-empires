// @vitest-environment jsdom
//
// The catching-up strip must describe an observed state: the server is ahead
// of what this view already folded. A resume that missed nothing shows nothing.

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
/** Server head the next replay response reports. */
let head = 10;
/** Resolves the next replay response, so a test can look at the phase while
 *  the request is in flight. */
let release: (() => void) | null = null;

function page(seqs: number[]) {
  return JSON.stringify({
    frames: seqs.map((seq) => ({ session_id: "sess-cu", seq, event: { UserPromptSent: { text: `p${seq}` } } })),
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
  head = 10;
  release = null;
  clearAcpCache();
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input.toString();
      if (!url.includes("/acp/replay")) return new Response(JSON.stringify({ rows: [] }), { status: 200 });
      if (url.includes("since=") && release === null) {
        // Warm (resume) page: hold it open until the test releases it.
        await new Promise<void>((resolve) => {
          release = resolve;
        });
      }
      return new Response(page(head === 10 ? [9, 10] : [11, 12]), { status: 200 });
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

describe("useAcpSession resume phase", () => {
  it("stays idle through a cold open", async () => {
    const { result } = renderHook(() => useAcpSession("sess-cu", "running", null, null, true));
    await flush();
    expect(result.current.state.lastSeq).toBe(10);
    expect(result.current.resumePhase).toBe("idle");
  });

  it("reports catching up while folding events the server had and we did not", async () => {
    const { result, rerender } = renderHook(
      ({ active }: { active: boolean }) => useAcpSession("sess-cu", "running", null, null, active),
      { initialProps: { active: true } },
    );
    await flush();
    expect(result.current.state.lastSeq).toBe(10);

    rerender({ active: false });
    await flush();

    head = 12;
    rerender({ active: true });
    await flush();
    // The warm page is held open: the client has asked, and has not yet been
    // told whether anything was missed.
    expect(result.current.resumePhase).toBe("checking");

    await act(async () => {
      release?.();
      for (let i = 0; i < 10; i += 1) await Promise.resolve();
    });

    expect(result.current.state.lastSeq).toBe(12);
    expect(result.current.resumePhase).toBe("idle");
  });

  it("never claims to be catching up when nothing was missed", async () => {
    const phases: string[] = [];
    const { result, rerender } = renderHook(
      ({ active }: { active: boolean }) => {
        const acp = useAcpSession("sess-cu", "running", null, null, active);
        phases.push(acp.resumePhase);
        return acp;
      },
      { initialProps: { active: true } },
    );
    await flush();
    rerender({ active: false });
    await flush();

    rerender({ active: true });
    await act(async () => {
      release?.();
      for (let i = 0; i < 10; i += 1) await Promise.resolve();
    });

    expect(result.current.resumePhase).toBe("idle");
    expect(phases).not.toContain("catching_up");
  });
});

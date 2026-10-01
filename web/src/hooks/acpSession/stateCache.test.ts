// @vitest-environment jsdom

import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { emptyAcpState, type BackgroundAgent } from "../../lib/acpTypes";
import { STORAGE_KEY_PREFIX } from "../../lib/acpStateStorage";
import { cacheSet, clearAcpCache, persistState, useBackgroundAgents } from "./stateCache";

const key = (id: string) => STORAGE_KEY_PREFIX + id;

function writeEntry(id: string, savedAt = Date.now()) {
  localStorage.setItem(key(id), JSON.stringify({ savedAt, queuedCount: 0 }));
}

beforeEach(() => localStorage.clear());
afterEach(() => vi.restoreAllMocks());

describe("persistState", () => {
  it("writes only the queued-prompt count", () => {
    persistState("strip", {
      ...emptyAcpState(),
      queuedPrompts: [
        { id: "q1", text: "plain text", queuedAt: "t" },
        { id: "q2", text: "with image", queuedAt: "t" },
      ],
      activity: [{ id: "row", kind: "message", text: "transcript", at: "t" }],
    });
    const raw = localStorage.getItem(key("strip"));
    expect(raw).not.toContain("transcript");
    expect(Object.keys(JSON.parse(raw!) as object).sort()).toEqual(["queuedCount", "savedAt"]);
    expect(JSON.parse(raw!).queuedCount).toBe(2);
  });
});

describe("clearAcpCache", () => {
  it("drops one entry or every acp-state entry, leaving unrelated keys", () => {
    writeEntry("sess-a");
    writeEntry("sess-b");
    localStorage.setItem("unrelated:key", "x");
    clearAcpCache("sess-a");
    expect(localStorage.getItem(key("sess-a"))).toBeNull();
    expect(localStorage.getItem(key("sess-b"))).not.toBeNull();
    clearAcpCache();
    expect(localStorage.getItem(key("sess-b"))).toBeNull();
    expect(localStorage.getItem("unrelated:key")).toBe("x");
  });

  it("swallows storage errors", () => {
    vi.spyOn(localStorage, "removeItem").mockImplementation(() => {
      throw new Error("denied");
    });
    expect(() => clearAcpCache("sess-quota")).not.toThrow();
  });
});

describe("useBackgroundAgents", () => {
  it("follows cache writes for its session without a socket", () => {
    const { result, unmount } = renderHook(() => useBackgroundAgents("sess-bg"));
    expect(result.current).toEqual([]);
    const agents = [{ id: "agent-1" } as unknown as BackgroundAgent];
    act(() => cacheSet("sess-bg", { ...emptyAcpState(), backgroundAgents: agents }));
    expect(result.current).toBe(agents);
    unmount();
    clearAcpCache("sess-bg");
  });
});

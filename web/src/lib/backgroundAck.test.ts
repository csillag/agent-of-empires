// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { acknowledgeBackground, getBackgroundAck, subscribeBackgroundAck } from "./backgroundAck";

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  localStorage.clear();
  vi.restoreAllMocks();
});

describe("getBackgroundAck / acknowledgeBackground", () => {
  it("returns null before any acknowledgement", () => {
    expect(getBackgroundAck("s1")).toBeNull();
  });

  it("round-trips the acknowledgement timestamp per session", () => {
    acknowledgeBackground("s1", "2026-01-01T00:00:00Z");
    expect(getBackgroundAck("s1")).toBe("2026-01-01T00:00:00Z");
    expect(getBackgroundAck("s2")).toBeNull();
  });
});

describe("subscribeBackgroundAck", () => {
  it("notifies on a same-tab acknowledgement", () => {
    const cb = vi.fn();
    const unsub = subscribeBackgroundAck(cb);
    acknowledgeBackground("s1");
    expect(cb).toHaveBeenCalledTimes(1);
    unsub();
  });

  it("notifies on a cross-tab storage event for a background-ack key", () => {
    const cb = vi.fn();
    const unsub = subscribeBackgroundAck(cb);
    window.dispatchEvent(
      new StorageEvent("storage", {
        key: "aoe.backgroundAck.s1",
        newValue: "2026-01-01T00:00:00Z",
        storageArea: localStorage,
      }),
    );
    expect(cb).toHaveBeenCalledTimes(1);
    unsub();
  });

  it("ignores a storage event for an unrelated key", () => {
    const cb = vi.fn();
    const unsub = subscribeBackgroundAck(cb);
    window.dispatchEvent(
      new StorageEvent("storage", { key: "aoe.other.thing", newValue: "x", storageArea: localStorage }),
    );
    expect(cb).not.toHaveBeenCalled();
    unsub();
  });

  it("fires unconditionally on a cross-tab clear (null key)", () => {
    const cb = vi.fn();
    const unsub = subscribeBackgroundAck(cb);
    window.dispatchEvent(new StorageEvent("storage", { key: null, storageArea: localStorage }));
    expect(cb).toHaveBeenCalledTimes(1);
    unsub();
  });

  it("stops firing after unsubscribe", () => {
    const cb = vi.fn();
    const unsub = subscribeBackgroundAck(cb);
    unsub();
    acknowledgeBackground("s1");
    expect(cb).not.toHaveBeenCalled();
  });
});

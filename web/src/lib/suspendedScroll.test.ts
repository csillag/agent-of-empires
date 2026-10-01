import { describe, expect, it } from "vitest";

import { KEEP_ALIVE_CAP } from "./keepAliveSet";
import { recallScroll, rememberScroll } from "./suspendedScroll";

describe("suspendedScroll", () => {
  it("returns null for a session that was never suspended", () => {
    expect(recallScroll("never-seen")).toBeNull();
  });

  it("hands back the recorded intent", () => {
    rememberScroll("a1", { stuck: false, top: 420 });
    expect(recallScroll("a1")).toEqual({ stuck: false, top: 420 });
  });

  it("overwrites an earlier record for the same session", () => {
    rememberScroll("b1", { stuck: false, top: 10 });
    rememberScroll("b1", { stuck: true, top: 0 });
    expect(recallScroll("b1")).toEqual({ stuck: true, top: 0 });
  });

  it("forgets the oldest record past the keep-alive cap", () => {
    for (let i = 0; i < KEEP_ALIVE_CAP + 1; i += 1) {
      rememberScroll(`c${i}`, { stuck: false, top: i });
    }
    expect(recallScroll("c0")).toBeNull();
    expect(recallScroll("c1")).toEqual({ stuck: false, top: 1 });
    expect(recallScroll(`c${KEEP_ALIVE_CAP}`)).toEqual({ stuck: false, top: KEEP_ALIVE_CAP });
  });
});

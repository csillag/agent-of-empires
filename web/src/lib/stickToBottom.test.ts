import { describe, expect, it } from "vitest";

import { nextStick, type StickState } from "./stickToBottom";

/** Frame-ordering model of the structured-view viewport. The browser delivers
 *  a scroll event in the frame after the write that caused it, reading layout
 *  as it is then; resize observers run after scroll events. */
function viewport({ userMayScroll = true } = {}) {
  const v = {
    clientHeight: 500,
    scrollHeight: 2000,
    scrollTop: 1500,
    state: { stuck: true, lastTop: 1500 } as StickState,
    /** The ResizeObserver repin: `scrollTop = scrollHeight`, clamped. */
    repin() {
      v.scrollTop = v.scrollHeight - v.clientHeight;
      v.state = { ...v.state, lastTop: v.scrollTop };
    },
    grow(px: number) {
      v.scrollHeight += px;
    },
    scrollEvent(gesture = userMayScroll) {
      v.state = nextStick(v.state, v, gesture);
    },
    resizeObserver() {
      if (v.state.stuck) v.repin();
    },
    userScrollTo(top: number) {
      v.scrollTop = top;
      v.scrollEvent(true);
    },
    atBottom: () => v.scrollTop === v.scrollHeight - v.clientHeight,
  };
  return v;
}

describe("stick to bottom", () => {
  it("stays pinned when content outgrows a stale programmatic scroll event", () => {
    for (const userMayScroll of [true, false]) {
      const v = viewport({ userMayScroll });
      v.grow(100);
      v.resizeObserver();
      v.grow(200);
      v.scrollEvent();
      v.resizeObserver();
      expect(v.state.stuck, `userMayScroll=${userMayScroll}`).toBe(true);
      expect(v.atBottom(), `userMayScroll=${userMayScroll}`).toBe(true);
    }
  });

  it("a user scroll up un-sticks, growth then leaves the position alone, and returning re-sticks", () => {
    const v = viewport();
    v.userScrollTo(1200);
    expect(v.state.stuck).toBe(false);

    v.grow(200);
    v.resizeObserver();
    expect(v.scrollTop).toBe(1200);

    v.userScrollTo(v.scrollHeight - v.clientHeight);
    expect(v.state.stuck).toBe(true);
    v.grow(200);
    v.resizeObserver();
    expect(v.atBottom()).toBe(true);
  });

  it("a scroll outside a gesture on a coarse pointer changes nothing", () => {
    const v = viewport({ userMayScroll: false });
    v.scrollTop = 1200;
    v.scrollEvent();
    expect(v.state.stuck).toBe(true);

    v.userScrollTo(1000);
    v.scrollTop = v.scrollHeight - v.clientHeight;
    v.scrollEvent();
    expect(v.state.stuck).toBe(false);
  });

  it("a user scroll up smaller than the preceding repin still un-sticks", () => {
    const v = viewport();
    v.grow(100);
    v.repin();
    v.grow(200);
    v.userScrollTo(v.state.lastTop - 50);
    v.resizeObserver();
    expect(v.state.stuck).toBe(false);
  });
});

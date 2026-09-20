// Stick-to-bottom intent for the structured-view transcript, kept DOM-free so
// the scroll-event ordering can be modelled in tests.

import { isPinnedToBottom } from "./historyScroll";

/** Upward movement (px) below which a scroll is treated as rounding noise. */
const MOVED_UP_SLOP_PX = 1;

export interface StickState {
  stuck: boolean;
  /** Last scrollTop seen by a scroll event or written by a repin. */
  lastTop: number;
}

export interface ScrollGeometry {
  scrollTop: number;
  clientHeight: number;
  scrollHeight: number;
}

/** Stick state after a scroll event.
 *
 *  Reaching the bottom sticks. Only moving up un-sticks: a repin moves down,
 *  and its scroll event can arrive after content grew again, so a gap alone
 *  does not prove the reader left. `userMayScroll` is false when the platform
 *  rules out a user scroll (a coarse pointer with no active gesture); such an
 *  event changes nothing. */
export function nextStick(prev: StickState, g: ScrollGeometry, userMayScroll: boolean): StickState {
  const lastTop = g.scrollTop;
  if (!userMayScroll) return { stuck: prev.stuck, lastTop };
  if (isPinnedToBottom(g.scrollTop, g.clientHeight, g.scrollHeight)) return { stuck: true, lastTop };
  return { stuck: prev.stuck && g.scrollTop >= prev.lastTop - MOVED_UP_SLOP_PX, lastTop };
}

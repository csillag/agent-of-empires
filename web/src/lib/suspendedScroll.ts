// Scroll intent of a suspended session view, for this page load only.
//
// A hidden scroll container reports a zero offset, so the position cannot be
// read off the DOM at the moment of hiding. It comes instead from the
// stick-to-bottom sampler's recorded value (see stickToBottom.ts), and is
// written back once the view is on screen again. Nothing here reaches browser
// storage: a reload starts cold by design. The persisted PWA-reopen position
// is a separate concern and stays in acpScrollState.ts.

import type { AcpScrollState } from "./acpScrollState";
import { KEEP_ALIVE_CAP } from "./keepAliveSet";

const memory = new Map<string, AcpScrollState>();

export function rememberScroll(sessionId: string, state: AcpScrollState): void {
  memory.delete(sessionId);
  memory.set(sessionId, state);
  while (memory.size > KEEP_ALIVE_CAP) {
    const oldest = memory.keys().next().value;
    if (oldest === undefined) break;
    memory.delete(oldest);
  }
}

export function recallScroll(sessionId: string): AcpScrollState | null {
  return memory.get(sessionId) ?? null;
}

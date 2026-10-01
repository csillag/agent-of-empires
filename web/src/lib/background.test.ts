import { describe, expect, it } from "vitest";
import { describeBackgroundItem, hasUnacknowledgedLoss, waitingOn } from "./background";
import type { BackgroundSummary } from "./types";

const now = Date.parse("2026-09-12T12:00:00Z");
const at = (min: number) => new Date(now + min * 60_000).toISOString();

describe("describeBackgroundItem", () => {
  it("shows kind, label, state, age and the next deadline", () => {
    expect(
      describeBackgroundItem(
        { kind: "monitor", id: "m", label: "adam run", started_at: at(-47), expires_at: at(20) },
        now,
      ),
    ).toBe("Monitor · adam run · live · 47m · times out in 20m");
    expect(
      describeBackgroundItem({ kind: "wakeup", id: "w", label: "loop", started_at: at(-3), expires_at: at(12) }, now),
    ).toBe("Wakeup · loop · live · 3m · fires in 12m");
    expect(
      describeBackgroundItem(
        { kind: "shell", id: "s", started_at: at(-120), ended: { reason: "lost", cause: "new_build", at: at(-52) } },
        now,
      ),
    ).toBe("Shell · s · lost 11:08 UTC (restart onto a new build) · 2h");
  });
});

describe("hasUnacknowledgedLoss", () => {
  const summary: BackgroundSummary = { live: 0, lost_since: at(-5), items: [] };
  it("warns until the loss is acknowledged", () => {
    expect(hasUnacknowledgedLoss(summary, null)).toBe(true);
    expect(hasUnacknowledgedLoss(summary, at(-10))).toBe(true);
    expect(hasUnacknowledgedLoss(summary, at(-1))).toBe(false);
    expect(hasUnacknowledgedLoss({ live: 1, items: [] }, null)).toBe(false);
  });
});

describe("waitingOn", () => {
  it("names the live work a turn can be waiting on", () => {
    const s: BackgroundSummary = {
      live: 3,
      items: [
        { kind: "subagent", id: "a", started_at: at(-1) },
        { kind: "subagent", id: "b", started_at: at(-1) },
        { kind: "subagent", id: "c", started_at: at(-1), ended: { reason: "lost", at: at(0) } },
        { kind: "workflow", id: "w", started_at: at(-1) },
        { kind: "monitor", id: "m", started_at: at(-1) },
      ],
    };
    expect(waitingOn(s)).toBe("2 sub-agents, 1 workflow");
    expect(waitingOn({ live: 1, items: [{ kind: "monitor", id: "m", started_at: at(-1) }] })).toBeNull();
  });
});

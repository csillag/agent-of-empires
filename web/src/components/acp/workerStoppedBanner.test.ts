import { describe, expect, it } from "vitest";
import { pickWorkerStoppedVariant, showStartupErrorBanner, showWorkerStoppingBanner } from "./workerStoppedBanner";

const T = "2026-01-01T00:00:00Z";
const base = { workerStopped: true, startupError: null, trashedAt: null, archivedAt: null, snoozedUntil: null };

describe("pickWorkerStoppedVariant", () => {
  it.each([
    ["not stopped", { workerStopped: false }, "none"],
    ["startup error owns the chrome", { startupError: "missing API key" }, "none"],
    ["trashed", { trashedAt: T }, "trashed"],
    ["trash wins over archive and snooze", { trashedAt: T, archivedAt: T, snoozedUntil: T }, "trashed"],
    // Reconnecting an archived or snoozed session would race the reconciler, which skips it.
    ["archived", { archivedAt: T }, "archived"],
    ["snoozed", { snoozedUntil: T }, "snoozed"],
    ["archive wins over snooze", { archivedAt: T, snoozedUntil: T }, "archived"],
    ["external teardown", {}, "generic"],
    ["empty startup error is not an error", { startupError: "" }, "generic"],
    ["empty startup error falls through to archived", { startupError: "", archivedAt: T }, "archived"],
    ["stopping yields the generic banner", { workerStopping: true }, "none"],
    ["not stopping keeps the generic banner", { workerStopping: false }, "generic"],
    ["stopping keeps triage banners", { workerStopping: true, archivedAt: T }, "archived"],
  ])("%s", (_label, overrides, expected) => {
    expect(pickWorkerStoppedVariant({ ...base, ...overrides })).toBe(expected);
  });
});

describe("showWorkerStoppingBanner", () => {
  it.each([
    ["stopping", null, true],
    ["stopping", "missing API key", false],
    ["resuming", null, false],
    ["absent", null, false],
  ])("%s with startupError %s -> %s", (acpWorkerState, startupError, expected) => {
    expect(showWorkerStoppingBanner({ acpWorkerState, startupError })).toBe(expected);
  });
});

describe("showStartupErrorBanner", () => {
  it("shows a startup error against a worker that is not being launched", () => {
    expect(showStartupErrorBanner({ startupError: "agent spawn failed", acpWorkerState: "absent" })).toBe(true);
    expect(showStartupErrorBanner({ startupError: "agent spawn failed", acpWorkerState: "running" })).toBe(true);
  });

  it("stays quiet while the daemon is launching this worker", () => {
    // The record is a past failure the reconciler has already moved past; the
    // spawning banner owns the chrome until this launch reports its own
    // outcome.
    expect(showStartupErrorBanner({ startupError: "agent spawn failed", acpWorkerState: "resuming" })).toBe(false);
  });

  it("shows nothing when there is no startup error", () => {
    expect(showStartupErrorBanner({ startupError: null, acpWorkerState: "absent" })).toBe(false);
  });
});

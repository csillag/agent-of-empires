import { describe, expect, it } from "vitest";

import { pickStatusStrip } from "./statusStrip";

const base = {
  status: "open" as const,
  hasEverOpened: true,
  reconnecting: false,
  retryCount: 0,
  retryCountdown: 0,
  maxRetries: 7,
  resumePhase: "idle" as const,
  lagged: false,
  resumeFailed: false,
  conversationReset: false,
  rateLimit: null,
  rateLimitRetriesExhausted: false,
};

const rateLimit = { status: "limited", resets_at: null, kind: "rate_limit" };

describe("pickStatusStrip", () => {
  it("shows nothing on a healthy live socket", () => {
    expect(pickStatusStrip(base)).toBeNull();
  });

  it("puts an exhausted retry envelope above everything else", () => {
    const strip = pickStatusStrip({
      ...base,
      status: "closed",
      retryCount: 7,
      rateLimit,
      lagged: true,
      resumePhase: "catching_up",
    });
    expect(strip).toEqual({
      tier: "error",
      kind: "reconnect_exhausted",
      text: "Connection lost. Auto-retry stopped.",
    });
  });

  it("puts a rate limit above a catch-up", () => {
    const strip = pickStatusStrip({ ...base, rateLimit, resumePhase: "catching_up" });
    expect(strip?.tier).toBe("error");
    expect(strip?.kind).toBe("rate_limit");
  });

  it("names the reset time when the agent reported one", () => {
    const strip = pickStatusStrip({
      ...base,
      rateLimit: { status: "limited", resets_at: "2099-01-01T09:00:00Z", kind: "rate_limit" },
    });
    expect(strip?.text).toContain("Rate-limited (rate_limit); resets at");
  });

  it("carries the auto-resume sentence in the same strip, not a second one", () => {
    const armed = pickStatusStrip({ ...base, rateLimit, rateLimitAutoResume: true });
    expect(armed?.text).toContain("Auto-resume is armed");
    const off = pickStatusStrip({ ...base, rateLimit, rateLimitAutoResume: false });
    expect(off?.text).toContain("Auto-resume is off for this profile");
    const unknown = pickStatusStrip({ ...base, rateLimit });
    expect(unknown?.text).not.toContain("Auto-resume");
  });

  it("puts a catch-up above a missed-events notice and above the info tier", () => {
    const strip = pickStatusStrip({
      ...base,
      status: "closed",
      resumePhase: "catching_up",
      lagged: true,
    });
    expect(strip).toEqual({
      tier: "catching_up",
      kind: "catching_up",
      text: "Catching up on what happened while you were away…",
    });
  });

  it("says nothing while a resume is still asking", () => {
    expect(pickStatusStrip({ ...base, status: "closed", resumePhase: "checking" })).toBeNull();
  });

  it("reports an armed retry with its progress", () => {
    const strip = pickStatusStrip({
      ...base,
      status: "closed",
      reconnecting: true,
      retryCount: 3,
      retryCountdown: 4,
    });
    expect(strip).toEqual({
      tier: "info",
      kind: "reconnecting",
      text: "Structured view disconnected. Reconnecting (3/7) in 4s…",
    });
  });

  it("distinguishes a first connect from a reconnect", () => {
    expect(pickStatusStrip({ ...base, status: "connecting", hasEverOpened: false })?.text).toBe(
      "Starting structured view…",
    );
    expect(pickStatusStrip({ ...base, status: "connecting" })?.text).toBe("Reconnecting to structured view…");
  });

  it("explains a replaced conversation above a catch-up", () => {
    const strip = pickStatusStrip({ ...base, conversationReset: true, resumePhase: "catching_up", lagged: true });
    expect(strip).toEqual({
      tier: "catching_up",
      kind: "conversation_reset",
      text: "This conversation was reset while you were away; showing the current transcript.",
    });
  });

  it("still yields to an error", () => {
    expect(pickStatusStrip({ ...base, conversationReset: true, rateLimit })?.kind).toBe("rate_limit");
  });

  it("reports a resume whose catch-up never landed, above the catch-up itself", () => {
    const strip = pickStatusStrip({ ...base, resumeFailed: true, resumePhase: "catching_up", lagged: true });
    expect(strip).toEqual({
      tier: "error",
      kind: "resume_failed",
      text: "Could not catch up on what happened while you were away.",
    });
  });

  it("keeps the failed catch-up visible on a socket that is otherwise healthy", () => {
    expect(pickStatusStrip({ ...base, status: "open", resumeFailed: true })?.kind).toBe("resume_failed");
  });

  it("still yields the failed catch-up to a rate limit", () => {
    expect(pickStatusStrip({ ...base, resumeFailed: true, rateLimit })?.kind).toBe("rate_limit");
  });

  it("keeps the cached-transcript wording for a dropped socket", () => {
    expect(pickStatusStrip({ ...base, status: "closed" })).toEqual({
      tier: "info",
      kind: "closed",
      text: "Structured view disconnected. Showing cached transcript; new messages disabled.",
    });
    expect(pickStatusStrip({ ...base, status: "error" })?.kind).toBe("retrying");
  });
});

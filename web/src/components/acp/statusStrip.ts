// Which single status strip the structured view shows.
//
// One at a time, in tier order: an error the user can act on, then a catch-up
// in progress, then something merely informational. Stacking a reconnect line
// under a rate-limit line taught the reader to skim past the whole area. Every
// strip names a state the client has observed; a resume that has asked and not
// yet been answered reports nothing, because "nothing was missed" is still a
// possible answer.

import type { ResumePhase } from "../../hooks/useAcpSession";
import type { RateLimitInfo } from "../../lib/acpTypes";

export interface StatusStrip {
  tier: "error" | "catching_up" | "info";
  kind:
    | "reconnect_exhausted"
    | "rate_limit_exhausted"
    | "rate_limit"
    | "resume_failed"
    | "catching_up"
    | "lagged"
    | "reconnecting"
    | "connecting"
    | "retrying"
    | "closed";
  text: string;
}

/** The agent's own wording for a rate limit, fit for a strip. On the prompt
 *  path `status` is already a sentence, but the defensive connection-end path
 *  (`classify_rate_limit_from_message`) puts the whole error Display string in,
 *  transport prefixes and the raw `{"errorKind":"rate_limit"}` fingerprint
 *  included, and that path never has a reported reset. Strip both so an unknown
 *  reset can never render a JSON payload. See #3152. */
function rateLimitWording(status: string): string {
  const text = status
    .replace(/[\s:]*\{[\s\S]*\}\s*$/, "")
    .replace(/^(?:ACP connection failed:\s*)?(?:Internal error:?\s*)?/, "")
    .trim();
  return text || "the agent did not report a reset time.";
}

function rateLimitText(info: RateLimitInfo, autoResume: boolean | undefined): string {
  // No reported reset means the agent never said when the window clears, so
  // show what it did say (usually "resets 4am (Europe/Paris)") rather than a
  // made-up clock time. See #3152.
  const reset = info.resets_at === null ? null : new Date(info.resets_at);
  const head =
    reset && !Number.isNaN(reset.getTime())
      ? `Rate-limited (${info.kind}); resets at ${reset.toLocaleTimeString()}.`
      : `Rate-limited (${info.kind}); ${rateLimitWording(info.status)}`;
  // The agent's wording used to sit on its own row, so it needs a sentence
  // break before anything else joins it in the single strip.
  const lead = /[.!?]$/.test(head) ? head : `${head}.`;
  // Whether the park ends by itself: the same strip with the setting off used
  // to read as "AoE is broken" rather than "as configured". See #3514.
  if (autoResume === true) return `${lead} Auto-resume is armed; the session resumes when the window clears.`;
  if (autoResume === false) {
    return `${lead} Auto-resume is off for this profile; use Resume now, or enable acp.rate_limit_auto_resume.`;
  }
  return head;
}

export function pickStatusStrip(args: {
  status: "connecting" | "open" | "closed" | "error";
  hasEverOpened: boolean;
  reconnecting: boolean;
  retryCount: number;
  retryCountdown: number;
  maxRetries: number;
  resumePhase: ResumePhase;
  /** The last resume could not fetch what it missed. */
  resumeFailed: boolean;
  lagged: boolean;
  rateLimit: RateLimitInfo | null;
  /** Omitted when the caller does not know, in which case nothing is claimed. */
  rateLimitAutoResume?: boolean;
  rateLimitRetriesExhausted: boolean;
}): StatusStrip | null {
  const retriesExhausted =
    args.status !== "open" && args.hasEverOpened && !args.reconnecting && args.retryCount >= args.maxRetries;
  if (retriesExhausted) {
    return { tier: "error", kind: "reconnect_exhausted", text: "Connection lost. Auto-retry stopped." };
  }
  if (args.rateLimitRetriesExhausted) {
    return {
      tier: "error",
      kind: "rate_limit_exhausted",
      text: "Auto-resume stopped: the same prompt was re-sent too many times without getting through. Resume manually or send a new prompt.",
    };
  }
  if (args.rateLimit) {
    return { tier: "error", kind: "rate_limit", text: rateLimitText(args.rateLimit, args.rateLimitAutoResume) };
  }
  // A replay that never landed is invisible in the phase, which is idle again
  // by then, and in the status, which a live socket leaves open.
  if (args.resumeFailed) {
    return { tier: "error", kind: "resume_failed", text: "Could not catch up on what happened while you were away." };
  }
  if (args.resumePhase === "catching_up") {
    return { tier: "catching_up", kind: "catching_up", text: "Catching up on what happened while you were away…" };
  }
  if (args.lagged) {
    return { tier: "catching_up", kind: "lagged", text: "Some events were missed during reconnect." };
  }
  if (args.resumePhase === "checking") return null;
  if (args.reconnecting && args.status !== "open") {
    const countdown = args.retryCountdown > 0 ? ` in ${args.retryCountdown}s` : "";
    return {
      tier: "info",
      kind: "reconnecting",
      text: `Structured view disconnected. Reconnecting (${args.retryCount}/${args.maxRetries})${countdown}…`,
    };
  }
  if (args.status === "connecting") {
    return {
      tier: "info",
      kind: "connecting",
      text: args.hasEverOpened ? "Reconnecting to structured view…" : "Starting structured view…",
    };
  }
  if (args.status === "error") {
    return {
      tier: "info",
      kind: "retrying",
      text: args.hasEverOpened
        ? "Structured view reconnecting… showing cached transcript; new messages disabled."
        : "Starting structured view worker… this can take a few seconds for new sessions.",
    };
  }
  if (args.status === "closed") {
    return {
      tier: "info",
      kind: "closed",
      text: args.hasEverOpened
        ? "Structured view disconnected. Showing cached transcript; new messages disabled."
        : "Structured view not ready yet. Retrying…",
    };
  }
  return null;
}

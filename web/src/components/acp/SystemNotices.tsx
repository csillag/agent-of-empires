import { useState } from "react";

import type { ResumePhase } from "../../hooks/useAcpSession";
import type { RespawnState } from "../../hooks/useRespawnSession";
import type { AcpState } from "../../lib/acpTypes";
import type { AcpContext } from "./AcpRuntime";
import { SwitchAgentModal } from "./SwitchAgentModal";
import { pickStatusStrip } from "./statusStrip";

/** Owns the rate-limit recovery modal toggle and hands its opener to `children`. */
export function RateLimitRecoverySection({
  sessionId,
  currentAgent,
  onPrefill,
  children,
}: {
  sessionId: string;
  currentAgent: string | null;
  onPrefill: (text: string) => void;
  children: (renderProps: { onSwitchAgent: () => void }) => React.ReactNode;
}) {
  const [open, setOpen] = useState(false);
  return (
    <>
      {children({ onSwitchAgent: () => setOpen(true) })}
      <SwitchAgentModal
        open={open}
        sessionId={sessionId}
        currentAgent={currentAgent}
        onClose={() => setOpen(false)}
        onPrefill={onPrefill}
        trigger="rate_limit"
      />
    </>
  );
}

const ACTION_BUTTON =
  "shrink-0 rounded-md border border-brand-700 bg-brand-900/40 px-2 py-1 text-[10px] font-mono uppercase tracking-wide text-brand-100 hover:bg-brand-900/60";

export function SystemNotices({
  status,
  lagged,
  rateLimit,
  rateLimitAutoResume,
  rateLimitRetriesExhausted,
  hasEverOpened,
  reconnecting,
  retryCount,
  retryCountdown,
  maxRetries,
  resumePhase,
  resumeFailed,
  manualReconnect,
  onSwitchAgent,
  onResumeRateLimit,
  rateLimitResumeState = "idle",
  rateLimitResumeError = null,
}: {
  status: AcpContext["status"];
  lagged: boolean;
  rateLimit: AcpState["rateLimit"];
  /** Omitted when unknown, in which case nothing is claimed about auto-resume. */
  rateLimitAutoResume?: boolean;
  rateLimitRetriesExhausted: boolean;
  hasEverOpened: boolean;
  reconnecting: boolean;
  retryCount: number;
  retryCountdown: number;
  maxRetries: number;
  resumePhase: ResumePhase;
  resumeFailed: boolean;
  manualReconnect: () => void;
  onSwitchAgent?: () => void;
  onResumeRateLimit?: () => void;
  rateLimitResumeState?: RespawnState;
  rateLimitResumeError?: string | null;
}) {
  const strip = pickStatusStrip({
    status,
    hasEverOpened,
    reconnecting,
    retryCount,
    retryCountdown,
    maxRetries,
    resumePhase,
    resumeFailed,
    lagged,
    rateLimit,
    rateLimitAutoResume,
    rateLimitRetriesExhausted,
  });
  if (!strip) return null;
  const resumePending = rateLimitResumeState === "retrying" || rateLimitResumeState === "ok";
  // Both rate-limit strips describe the same park, so both carry its recovery
  // affordances, and only while the daemon still reports one.
  const parked = strip.kind === "rate_limit" || strip.kind === "rate_limit_exhausted";
  const rateLimitActions = rateLimit !== null && parked;
  return (
    <div className="border-b border-surface-800 px-4 py-2 space-y-1" data-testid={`acp-strip-${strip.kind}`}>
      {strip.kind === "reconnect_exhausted" || strip.kind === "resume_failed" ? (
        <div className="flex items-center justify-between gap-3 text-xs text-brand-400">
          <span>{strip.text}</span>
          <button type="button" onClick={manualReconnect} className={ACTION_BUTTON}>
            {strip.kind === "resume_failed" ? "Retry" : "Reconnect"}
          </button>
        </div>
      ) : (
        <div className={`text-xs ${strip.tier === "error" ? "text-brand-400" : "text-text-muted"}`}>{strip.text}</div>
      )}
      {rateLimitActions && (onResumeRateLimit || onSwitchAgent) && (
        <div className="flex flex-wrap items-center justify-end gap-2 pt-1">
          {onResumeRateLimit && (
            <button
              type="button"
              onClick={onResumeRateLimit}
              disabled={resumePending}
              className={`${ACTION_BUTTON} disabled:cursor-not-allowed disabled:opacity-60`}
            >
              {rateLimitResumeState === "retrying"
                ? "Resuming…"
                : rateLimitResumeState === "ok"
                  ? "Resume requested"
                  : "Resume now"}
            </button>
          )}
          {onSwitchAgent && (
            <button type="button" onClick={onSwitchAgent} className={ACTION_BUTTON}>
              Continue in another agent
            </button>
          )}
        </div>
      )}
      {rateLimitActions && rateLimitResumeState === "ok" && (
        <div className="pt-1 text-xs text-text-muted">Resume requested. New events should start streaming shortly.</div>
      )}
      {rateLimitActions && rateLimitResumeState === "failed" && rateLimitResumeError && (
        <div className="pt-1 text-xs text-brand-400">Resume failed: {rateLimitResumeError}</div>
      )}
    </div>
  );
}

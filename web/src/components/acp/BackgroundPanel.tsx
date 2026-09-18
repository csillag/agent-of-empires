import { useEffect, useState } from "react";
import { clockTime, describeBackgroundItem, waitingOn } from "../../lib/background";
import { acknowledgeBackground } from "../../lib/backgroundAck";
import { useNow } from "../../hooks/useNow";
import type { BackgroundItem, BackgroundSummary } from "../../lib/types";

// Acknowledging a loss when it's shown is a write to an external system
// reacting to new data arriving, not a same-tick event handler, so this
// legitimately belongs in an effect; no-event-handler's bare-`if` heuristic
// can't tell the two apart.
function ackIfLost(sessionId: string, lostSince: string | undefined): void {
  if (lostSince) acknowledgeBackground(sessionId, lostSince);
}

/** The session's background work. Showing a loss counts as seeing it. */
export function BackgroundPanel({
  sessionId,
  summary,
  turnActive,
  onOpenAgentsPane,
}: {
  sessionId: string;
  summary?: BackgroundSummary;
  turnActive: boolean;
  /** Open the Background agents pane for a sub-agent row. Absent when no
   *  pane is wired; the row then falls back to expanding its label. */
  onOpenAgentsPane?: () => void;
}) {
  const [open, setOpen] = useState<string | null>(null);
  const lostSince = summary?.lost_since;
  // 30s is plenty for a "how long ago" / armed-time label that isn't the
  // sidebar's 1Hz countdown.
  const now = useNow(30_000);
  useEffect(() => {
    ackIfLost(sessionId, lostSince);
  }, [sessionId, lostSince]);
  if (!summary || summary.items.length === 0) return null;
  const lost = summary.items.filter((i) => i.ended?.reason === "lost").length;
  const waiting = turnActive ? waitingOn(summary) : null;

  const rowClick = (item: BackgroundItem) => () => {
    if (item.kind === "subagent" && onOpenAgentsPane) {
      onOpenAgentsPane();
      return;
    }
    setOpen(open === item.id ? null : item.id);
  };

  return (
    <section data-testid="background-panel" className="border-b border-surface-800 bg-surface-900/60 px-4 py-2 text-xs">
      <div className="font-medium text-text-secondary">
        Background: {summary.live} live{lost ? `, ${lost} lost` : ""}
      </div>
      {waiting && <div className="text-text-dim">The current turn is waiting on {waiting}.</div>}
      <ul className="mt-1 max-h-[20vh] space-y-0.5 overflow-y-auto">
        {summary.items.map((item) => (
          <li key={item.id} className={item.ended ? "text-text-dim" : "text-text-secondary"}>
            <button type="button" className="w-full text-left" onClick={rowClick(item)}>
              {describeBackgroundItem(item, now)} · armed {clockTime(item.started_at)}
            </button>
            {open === item.id && item.label && (
              <pre className="mt-0.5 whitespace-pre-wrap break-words text-[11px] text-text-dim">{item.label}</pre>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}

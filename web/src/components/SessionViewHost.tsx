// Desktop keep-alive host for structured-view sessions.
//
// One StructuredView per session in the keep-alive set. The session on screen
// is visible and live; the others stay mounted, hidden and inert, so coming
// back to one paints its last state, its scroll position and its composer
// draft at once instead of remounting. The set lives for this page load only.

import { lazy, Suspense, useMemo, useState } from "react";

import { KEEP_ALIVE_CAP, insertMru, pruneKept, renderOrder, sameIds } from "../lib/keepAliveSet";
import type { FileRef } from "../lib/fileRef";
import type { SessionResponse } from "../lib/types";

const StructuredView = lazy(() => import("./acp/StructuredView").then((m) => ({ default: m.StructuredView })));

function AcpLoadingFallback() {
  return (
    <div className="flex h-full items-center justify-center bg-surface-900 text-text-dim">
      <div className="text-xs font-mono uppercase tracking-wide">Loading acp…</div>
    </div>
  );
}

interface Props {
  activeSessionId: string;
  /** Every session the dashboard knows about; the host looks up each kept id
   *  here for the props that used to come from App's single `activeSession`. */
  sessions: SessionResponse[];
  onOpenFileRef: (ref: FileRef) => void;
  onOpenAgentsPane: () => void;
  onRestoreSession: (sessionId: string) => void;
}

/** Whether a session still deserves a mounted view while the user is
 *  elsewhere. Archived and trashed sessions have no live worker to follow, and
 *  a session switched to the terminal view has no structured view at all. */
function keepAliveEligible(session: SessionResponse): boolean {
  return session.view === "structured" && !session.archived_at && !session.trashed_at;
}

export function SessionViewHost({
  activeSessionId,
  sessions,
  onOpenFileRef,
  onOpenAgentsPane,
  onRestoreSession,
}: Props) {
  const byId = useMemo(() => new Map(sessions.map((s) => [s.id, s])), [sessions]);
  const eligible = useMemo(() => new Set(sessions.filter(keepAliveEligible).map((s) => s.id)), [sessions]);
  const [kept, setKept] = useState<string[]>(() => [activeSessionId]);
  // Derived during render rather than in an effect: the visited session must be
  // in the set for the same paint that shows it, or the switch flashes empty.
  const next = pruneKept(insertMru(kept, activeSessionId, KEEP_ALIVE_CAP), eligible, activeSessionId);
  if (!sameIds(next, kept)) setKept(next);

  return (
    <>
      {renderOrder(next).map((id) => {
        const session = byId.get(id);
        if (!session) return null;
        const isActive = id === activeSessionId;
        return (
          <div
            key={id}
            data-session-view={id}
            className={isActive ? "flex flex-1 flex-col min-h-0 overflow-hidden" : undefined}
            hidden={!isActive}
            inert={!isActive}
          >
            {/* One boundary per layer: a view still loading must not take the
                tree of the view on screen down with it. */}
            <Suspense fallback={isActive ? <AcpLoadingFallback /> : null}>
              <StructuredView
                sessionId={id}
                active={isActive}
                acpWorkerState={session.acp_worker_state ?? "absent"}
                rateLimitAutoResume={session.rate_limit_auto_resume}
                tool={session.tool}
                acpAgent={session.acp_agent ?? null}
                clearAliases={session.clear_aliases}
                archivedAt={session.archived_at ?? null}
                snoozedUntil={session.snoozed_until ?? null}
                trashedAt={session.trashed_at ?? null}
                onRestore={session.trashed_at ? () => onRestoreSession(id) : undefined}
                onOpenFileRef={onOpenFileRef}
                fileRefSession={session}
                onOpenAgentsPane={onOpenAgentsPane}
                isSandboxed={session.is_sandboxed}
              />
            </Suspense>
          </div>
        );
      })}
    </>
  );
}

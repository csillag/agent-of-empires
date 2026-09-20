// Desktop keep-alive host for structured-view sessions.
//
// One StructuredView per session in the keep-alive set. The session on screen
// is visible and live; the others stay mounted, hidden and inert, so coming
// back to one paints its last state, its scroll position and its composer
// draft at once instead of remounting. The set lives for this page load only.
//
// Known limits: App renders the host or the terminal stack, never both, so
// visiting a terminal session (or a narrow window falling back to the mobile
// pane) unmounts the host and the set with it. Keeping the cache small is the
// other one: cycling past the per-session state cache's 32 entries drops a
// kept session's data, and its resume then hydrates empty.

import { lazy, memo, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";

import { KEEP_ALIVE_CAP, insertMru, pruneKept, renderOrder, sameIds } from "../lib/keepAliveSet";
import type { FileRef } from "../lib/fileRef";
import type { SessionResponse, WorkspaceRepoSummary } from "../lib/types";

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

function sameStrings(a: string[] | undefined, b: string[] | undefined): boolean {
  if (a === b) return true;
  if (!a || !b || a.length !== b.length) return false;
  return a.every((v, i) => v === b[i]);
}

/** Only the fields `FileRefSession` reads off a repo. */
function sameRepos(a: WorkspaceRepoSummary[], b: WorkspaceRepoSummary[]): boolean {
  if (a === b) return true;
  if (a.length !== b.length) return false;
  return a.every((r, i) => r.name === b[i]!.name && r.source_path === b[i]!.source_path);
}

/** Every session field a layer forwards. The session list is re-fetched on a
 *  poll, so identity changes every tick while the content usually does not; a
 *  field added to the layer's JSX belongs here too. */
function sameLayerProps(a: SessionResponse, b: SessionResponse): boolean {
  return (
    a.id === b.id &&
    a.acp_worker_state === b.acp_worker_state &&
    a.rate_limit_auto_resume === b.rate_limit_auto_resume &&
    a.tool === b.tool &&
    a.acp_agent === b.acp_agent &&
    a.archived_at === b.archived_at &&
    a.snoozed_until === b.snoozed_until &&
    a.trashed_at === b.trashed_at &&
    a.is_sandboxed === b.is_sandboxed &&
    a.project_path === b.project_path &&
    a.main_repo_path === b.main_repo_path &&
    a.artifact_dir === b.artifact_dir &&
    sameStrings(a.clear_aliases, b.clear_aliases) &&
    sameRepos(a.workspace_repos, b.workspace_repos)
  );
}

interface LayerProps {
  session: SessionResponse;
  isActive: boolean;
  onOpenFileRef: (ref: FileRef) => void;
  onOpenAgentsPane: () => void;
  onRestoreSession: (sessionId: string) => void;
}

/** One kept session's view, visible or hidden and inert. Memoized on what it
 *  forwards, so a poll tick only re-renders the session that changed. The
 *  callbacks are stable for the host's lifetime, which is what lets the
 *  comparison leave them out. */
const SessionLayer = memo(
  function SessionLayer({ session, isActive, onOpenFileRef, onOpenAgentsPane, onRestoreSession }: LayerProps) {
    const id = session.id;
    return (
      <div
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
  },
  (a, b) => a.isActive === b.isActive && sameLayerProps(a.session, b.session),
);

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
  // The host outlives every callback identity App hands it (one of them is an
  // inline arrow), so the layers get wrappers that never change.
  const handlersRef = useRef({ onOpenFileRef, onOpenAgentsPane, onRestoreSession });
  useEffect(() => {
    handlersRef.current = { onOpenFileRef, onOpenAgentsPane, onRestoreSession };
  }, [onOpenFileRef, onOpenAgentsPane, onRestoreSession]);
  const openFileRef = useCallback((ref: FileRef) => handlersRef.current.onOpenFileRef(ref), []);
  const openAgentsPane = useCallback(() => handlersRef.current.onOpenAgentsPane(), []);
  const restoreSession = useCallback((id: string) => handlersRef.current.onRestoreSession(id), []);
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
        return (
          <SessionLayer
            key={id}
            session={session}
            isActive={id === activeSessionId}
            onOpenFileRef={openFileRef}
            onOpenAgentsPane={openAgentsPane}
            onRestoreSession={restoreSession}
          />
        );
      })}
    </>
  );
}

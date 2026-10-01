//! ACP worker supervisor: owns each structured view session's worker, bridges
//! its events into the broadcast sink, and respawns it on crash within a budget.

mod agents;
mod drain;
mod launch;
mod publish;
mod requests;
mod sink;
mod teardown;
#[cfg(test)]
mod test_support;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::warn;

use super::acp_client::{AcpClient, AcpError, SpawnConfig};
use super::agent_registry::AgentRegistry;
pub use super::runner_lifecycle::ResumeKind;
use super::runner_lifecycle::{
    Lease, LifecycleTable, ProcessControl, RunnerIdentity, SystemProcessControl, WorkerPhase,
};
use super::state::{AcpSessionId, Event};
use crate::daemon::AcpWorkerState;
use crate::session::SandboxInfo;

pub(crate) use agents::apply_agent_command_override;
pub use sink::{BroadcastSink, ChannelSink};

/// Post-startup respawns allowed within `RESTART_WINDOW` before the session is parked.
const MAX_RESPAWNS_IN_WINDOW: u32 = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(60);
/// Backoff before respawning so an agent that crashes on startup cannot hot-loop.
const RESPAWN_BACKOFF: Duration = Duration::from_millis(500);
/// How long a runner request path waits for a mid-resume worker to land.
const WORKER_READY_TIMEOUT: Duration = Duration::from_secs(10);

/// Builds the client for a spawn; swapped in tests to drive lifecycles without a runner.
pub(crate) type Launcher = Arc<
    dyn Fn(
            SpawnConfig,
            AcpSessionId,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<AcpClient, AcpError>> + Send>,
        > + Send
        + Sync,
>;

type Workers = Arc<Mutex<HashMap<String, WorkerHandle>>>;
type SharedSet = Arc<std::sync::Mutex<HashSet<String>>>;

/// The only allocator of a session seq. The per-session lock is held from
/// allocation through the store append and the broadcast send, so broadcast
/// order equals seq order. Consumers drop a seq at or below the last one they
/// applied, and a lower seq sent late is lost for good.
///
/// Lock order: supervisor locks may be held while publishing, then this
/// counter, then the sink. A publish closure must only call the sink.
pub(super) struct SessionPublisher<S: BroadcastSink> {
    sink: Arc<S>,
    counters: std::sync::Mutex<HashMap<String, Arc<std::sync::Mutex<u64>>>>,
}

impl<S: BroadcastSink> SessionPublisher<S> {
    pub(super) fn sink(&self) -> &S {
        &self.sink
    }

    pub(super) fn new(sink: Arc<S>) -> Self {
        Self {
            sink,
            counters: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn with_counter<R>(&self, session_id: &str, f: impl FnOnce(&S, &mut u64) -> R) -> R {
        let counter = Arc::clone(
            lock_recover(&self.counters)
                .entry(session_id.to_string())
                .or_default(),
        );
        block_in_place_if_multi_thread(|| f(&self.sink, &mut lock_recover(&counter)))
    }

    pub(super) fn with_next_seq<R>(
        &self,
        session_id: &str,
        publish: impl FnOnce(&S, u64) -> R,
    ) -> R {
        self.with_counter(session_id, |sink, last| {
            *last = last.saturating_add(1);
            publish(sink, *last)
        })
    }

    pub(super) fn publish(&self, session_id: &str, event: &Event) -> u64 {
        self.with_next_seq(session_id, |sink, seq| {
            sink.publish(session_id, seq, event);
            seq
        })
    }

    pub(super) fn publish_from_worker(&self, session_id: &str, event: &Event, generation: u64) {
        self.with_next_seq(session_id, |sink, seq| {
            sink.publish_from_worker(session_id, seq, event, generation);
        });
    }

    pub(super) fn publish_if_last(
        &self,
        session_id: &str,
        expected_seq: u64,
        event: &Event,
    ) -> bool {
        self.with_counter(session_id, |sink, last| {
            if *last != expected_seq {
                return false;
            }
            *last = last.saturating_add(1);
            sink.publish(session_id, *last, event);
            true
        })
    }

    pub(super) fn set_last(&self, session_id: &str, seq: u64) {
        self.with_counter(session_id, |_, last| *last = seq);
    }

    pub(super) fn forget(&self, session_id: &str) {
        lock_recover(&self.counters).remove(session_id);
    }
}

fn block_in_place_if_multi_thread<R>(f: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current().map(|h| h.runtime_flavor()) {
        Ok(tokio::runtime::RuntimeFlavor::MultiThread) => tokio::task::block_in_place(f),
        _ => f(),
    }
}

/// A build-stale worker flagged to retire once its turn drains. The epoch is
/// the worker the flag was set under, so a replacement is never retired for it.
struct PendingRespawn {
    epoch: u64,
    /// Wall-clock ms of the flag. The drain's deferral cap runs from it.
    since_ms: i64,
}

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error("session {0:?} not found")]
    UnknownSession(String),
    #[error("acp client error: {0}")]
    Acp(#[from] AcpError),
    #[error("agent {0:?} not in registry")]
    UnknownAgent(String),
    /// Registered, but refused by `[acp] allowed_agents` (a 403, not a 400).
    #[error(
        "agent {0:?} is not permitted by [acp] allowed_agents; ask the operator to allow it or pick a permitted agent"
    )]
    AgentNotAllowed(String),
    #[error("{0}")]
    InvalidAgentCommand(String),
    #[error("session {0:?} already has a running structured view worker")]
    AlreadyRunning(String),
    #[error("structured view worker capacity full ({current}/{limit}); raise [acp] max_concurrent_workers or delete an existing structured view session")]
    CapacityFull { current: usize, limit: u32 },
    /// A concurrent shutdown cancelled the resume; callers treat it as a soft success.
    #[error("resume of session {0:?} was cancelled by a concurrent shutdown")]
    SpawnCancelled(String),
    /// The previous runner is not proven dead yet.
    #[error("session {0:?} is still stopping its previous structured view worker")]
    TeardownPending(String),
}

/// What the caller does with prompt text after it was published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptDisposition {
    Forward,
    /// A clear command the adapter cannot handle natively: drive
    /// [`Supervisor::reset_session_context`] instead of forwarding it.
    ResetContext,
}

/// How this supervisor acquired the worker.
enum WorkerKind {
    /// Spawned by this daemon; respawnable from its cached config.
    Runner { spawn_config: Box<SpawnConfig> },
    /// Reattached to a runner left by a previous daemon; never respawned in memory.
    Attached,
    /// In-process test fixture with no runner registry entry.
    #[cfg(test)]
    Stdio,
}

#[derive(Clone)]
pub(super) struct PendingContextReset {
    pub(super) profile: String,
    pub(super) reason: String,
    pub(super) transactions: Vec<String>,
}

struct WorkerHandle {
    client: Arc<AcpClient>,
    drain_task: JoinHandle<()>,
    /// Respawn timestamps inside the restart window; the initial spawn is not counted.
    restart_history: Vec<Instant>,
    kind: WorkerKind,
    lease: Lease,
    native_session_id: Option<String>,
}

impl From<WorkerPhase> for AcpWorkerState {
    fn from(phase: WorkerPhase) -> Self {
        match phase {
            WorkerPhase::Absent => Self::Absent,
            WorkerPhase::Resuming => Self::Resuming,
            WorkerPhase::Running => Self::Running,
            WorkerPhase::Stopping => Self::Stopping,
        }
    }
}

pub(crate) enum ResumeReservationOutcome {
    Reserved(ResumeReservation),
    /// The session is already running or mid-resume.
    AlreadyPresent,
}

pub struct Supervisor<S: BroadcastSink> {
    sink: Arc<S>,
    registry: Arc<Mutex<AgentRegistry>>,
    workers: Workers,
    publisher: Arc<SessionPublisher<S>>,
    /// Owner of every runner epoch. Lock order: `workers` before `lifecycle`.
    lifecycle: Arc<std::sync::Mutex<LifecycleTable>>,
    process_control: Arc<dyn ProcessControl>,
    launcher: Launcher,
    /// Agents whose first spawn finished; until then spawns serialize on a
    /// per-agent lock so a lazy adapter install is not raced.
    warmed_up_agents: SharedSet,
    agent_warmup_locks: Arc<std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// Wakes `wait_for_worker` whenever the workers map or lifecycle table changes.
    worker_notify: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    worker_waits: tokio::sync::broadcast::Sender<String>,
    /// Build-stale sessions draining a turn before the reconciler retires them.
    respawn_pending: Arc<std::sync::Mutex<HashMap<String, PendingRespawn>>>,
    /// When each session's stale runner, by generation, was first flagged.
    /// Outlives a broken connection, so re-adopting the same runner does not
    /// restart the deferral caps.
    respawn_since: Arc<std::sync::Mutex<HashMap<String, (u64, i64)>>>,
    /// Sessions parked on a compatibility rejection, keyed to the failing binary.
    incompatible_binaries: Arc<std::sync::Mutex<HashMap<String, String>>>,
    /// Sessions the reconciler must fresh-spawn next tick, bypassing its `attempted` guard.
    force_respawn: SharedSet,
    /// Sessions whose worker failed before establishing a session.
    startup_failures: SharedSet,
    /// Sessions whose isolated native identity is not durable yet.
    pending_context_resets: SharedSet,
    /// Sessions whose crashed worker the drain task relaunched in place.
    respawned_in_place: SharedSet,
    max_concurrent_workers: u32,
}

/// RAII guard over a `Starting` or `Respawning` epoch; dropping it before
/// install abandons the epoch so a failed resume cannot pin the session.
pub(crate) struct ResumeReservation {
    lease: Lease,
    lifecycle: Arc<std::sync::Mutex<LifecycleTable>>,
    notify: Arc<tokio::sync::Notify>,
}

impl ResumeReservation {
    pub(crate) fn lease(&self) -> &Lease {
        &self.lease
    }
}

impl Drop for ResumeReservation {
    fn drop(&mut self) {
        if lock_recover(&self.lifecycle).abandon(&self.lease) {
            self.notify.notify_waiters();
        }
    }
}

/// The instance's `command` override, applied to the registry spec so the
/// structured view launches the same binary as the terminal view.
#[derive(Debug, Clone)]
pub struct AgentCommandOverride {
    pub logical_tool: String,
    pub command: String,
}

/// Which durable continuation lane a sandboxed spawn may consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxContinuation {
    Persisted,
    ImportTerminal,
    Fresh,
}

#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub session_id: String,
    /// The ACP backend `pick_agent_for_tool` resolved.
    pub agent: String,
    /// The session's logical tool (`Instance.tool`); host hooks see this as `AOE_TOOL`.
    pub tool: String,
    pub cwd: PathBuf,
    pub additional_dirs: Vec<PathBuf>,
    /// The session's stored directory list (`Instance.session_dirs`). Every
    /// path joins `additional_dirs` (ACP additionalDirectories, aoe's fs
    /// roots); read-only ones are write-refused by aoe's fs handler; and the
    /// list is exported to a host agent's environment for its own sandbox.
    pub session_dirs: Vec<crate::session::session_dirs::SessionDir>,
    pub provider_env: Vec<(String, String)>,
    pub model: Option<String>,
    /// Assert `model` through the agent's model config option after the
    /// handshake. Set only when no agent has seen this pick yet. The
    /// `AOE_AGENT_MODEL` env var is exported either way.
    pub assert_model: bool,
    pub effort: Option<String>,
    /// True for persisted user effort, not a resolved default.
    pub effort_explicit: bool,
    /// Prior ACP session id; loaded instead of a new session when the agent supports it.
    pub stored_acp_session_id: Option<String>,
    /// Parent ACP session id to `session/fork` from.
    pub fork_from: Option<String>,
    pub sandbox_continuation: SandboxContinuation,
    pub sandbox_info: Option<SandboxInfo>,
    pub source_profile: Option<String>,
    pub yolo_mode: bool,
    /// Explicit ACP mode applied after the handshake; wins over `yolo_mode`.
    pub acp_mode_id: Option<String>,
    pub agent_command_override: Option<AgentCommandOverride>,
    /// Let a `session/load` replay history into the (empty) event store for an import.
    pub seed_history_replay: bool,
    /// Claude store selected by the conversation binding for a host Claude worker.
    pub claude_store_pin: Option<crate::session::capture::ClaudeStorePin>,
}

impl<S: BroadcastSink> Supervisor<S> {
    /// Constructor with no concurrency cap.
    pub fn new(sink: Arc<S>) -> Self {
        Self::with_capacity(sink, u32::MAX)
    }

    pub fn with_capacity(sink: Arc<S>, max_concurrent_workers: u32) -> Self {
        Self {
            sink,
            registry: Arc::new(Mutex::new(AgentRegistry::with_defaults())),
            workers: Arc::default(),
            publisher: Arc::new(SessionPublisher::new(Arc::clone(&sink))),
            lifecycle: Arc::new(std::sync::Mutex::new(LifecycleTable::new(
                chrono::Utc::now().timestamp_millis().max(1) as u64,
            ))),
            process_control: Arc::new(SystemProcessControl),
            launcher: Arc::new(|config, session_id| Box::pin(AcpClient::spawn(config, session_id))),
            warmed_up_agents: Arc::default(),
            agent_warmup_locks: Arc::default(),
            worker_notify: Arc::default(),
            #[cfg(test)]
            worker_waits: tokio::sync::broadcast::channel(64).0,
            respawn_pending: Arc::default(),
            respawn_since: Arc::default(),
            incompatible_binaries: Arc::default(),
            force_respawn: Arc::default(),
            startup_failures: Arc::default(),
            pending_context_resets: Arc::default(),
            respawned_in_place: Arc::default(),
            max_concurrent_workers,
        }
    }

    /// Flag the running worker of a session adopted on a stale build. The flag
    /// belongs to that worker: a stop or a replacement ends it.
    pub fn mark_build_respawn_pending(&self, session_id: &str, now_ms: i64) {
        let Some((lease, identity)) = lock_recover(&self.lifecycle).running(session_id) else {
            return;
        };
        // Re-adopting the same runner keeps the time it was first flagged.
        let since_ms = match identity {
            Some(runner) => {
                let mut since = lock_recover(&self.respawn_since);
                let first = since
                    .entry(session_id.to_string())
                    .or_insert((runner.generation, now_ms));
                if first.0 != runner.generation {
                    *first = (runner.generation, now_ms);
                }
                first.1
            }
            None => now_ms,
        };
        lock_recover(&self.respawn_pending).insert(
            session_id.to_string(),
            PendingRespawn {
                epoch: lease.epoch(),
                since_ms,
            },
        );
    }

    /// Sessions whose flagged worker is still the running one, with when they
    /// were flagged. Stale flags drop.
    pub fn respawn_pending(&self) -> Vec<(String, i64)> {
        let table = lock_recover(&self.lifecycle);
        let mut pending = lock_recover(&self.respawn_pending);
        pending.retain(|id, flag| {
            table
                .running(id)
                .is_some_and(|(lease, _)| lease.epoch() == flag.epoch)
        });
        pending
            .iter()
            .map(|(id, flag)| (id.clone(), flag.since_ms))
            .collect()
    }

    pub fn respawn_pending_ids(&self) -> Vec<String> {
        self.respawn_pending().into_iter().map(|(id, _)| id).collect()
    }

    pub fn clear_respawn_pending(&self, session_id: &str) {
        lock_recover(&self.respawn_pending).remove(session_id);
    }

    pub fn running_identity(&self, session_id: &str) -> Option<RunnerIdentity> {
        lock_recover(&self.lifecycle)
            .running(session_id)
            .and_then(|(_, identity)| identity)
    }

    fn mark_incompatible_binary(&self, session_id: &str, binary: &str) {
        lock_recover(&self.incompatible_binaries)
            .insert(session_id.to_string(), binary.to_string());
    }

    /// A user-initiated resume overrides a stop kept from a resume that failed before install.
    pub fn forget_stale_cancel(&self, session_id: &str) {
        lock_recover(&self.lifecycle).forget_stale_cancel(session_id);
    }

    pub fn request_respawn(&self, session_id: &str) {
        lock_recover(&self.force_respawn).insert(session_id.to_string());
    }

    pub fn take_respawn_requests(&self) -> Vec<String> {
        lock_recover(&self.force_respawn).drain().collect()
    }

    pub fn take_startup_failures(&self) -> Vec<String> {
        lock_recover(&self.startup_failures).drain().collect()
    }

    pub fn take_respawned_in_place(&self) -> Vec<String> {
        lock_recover(&self.respawned_in_place).drain().collect()
    }

    /// Sessions parked on a compatibility rejection for `binary` with no live worker.
    pub async fn incompatible_sessions_for_binary(&self, binary: &str) -> Vec<String> {
        let candidates: Vec<String> = lock_recover(&self.incompatible_binaries)
            .iter()
            .filter(|(_, b)| b.as_str() == binary)
            .map(|(id, _)| id.clone())
            .collect();
        let mut out = Vec::new();
        for id in candidates {
            if !self.is_running(&id).await {
                out.push(id);
            }
        }
        out
    }

    pub async fn worker_states_snapshot(&self) -> HashMap<String, AcpWorkerState> {
        lock_recover(&self.lifecycle)
            .snapshot()
            .into_iter()
            .map(|(id, phase)| (id, phase.into()))
            .collect()
    }

    pub async fn worker_state(&self, session_id: &str) -> AcpWorkerState {
        lock_recover(&self.lifecycle).phase(session_id).into()
    }

    /// Drop per-session bookkeeping for a deleted session. An in-flight
    /// publisher still holds the old counter and keeps that numbering.
    pub fn forget_session(&self, session_id: &str) {
        self.publisher.forget(session_id);
        lock_recover(&self.lifecycle).forget(session_id);
        lock_recover(&self.startup_failures).remove(session_id);
        lock_recover(&self.respawned_in_place).remove(session_id);
    }

    /// Seed seq counters from the event store's stored maxima.
    pub fn hydrate_seqs(&self, pairs: impl IntoIterator<Item = (String, u64)>) {
        for (id, seq) in pairs {
            self.publisher.set_last(&id, seq);
        }
    }

    /// Whether this session has a worker up or coming up.
    pub async fn is_running(&self, session_id: &str) -> bool {
        lock_recover(&self.lifecycle).is_running(session_id)
    }

    /// Whether a live frame's generation is the worker currently installed.
    pub(crate) async fn is_current_worker_generation(
        &self,
        session_id: &str,
        generation: u64,
    ) -> bool {
        self.workers
            .lock()
            .await
            .get(session_id)
            .is_some_and(|worker| worker.lease.epoch() == generation)
    }

    /// Return the native store owned by the current worker only when it still
    /// owns the ACP identity being handed back to a terminal.
    pub(crate) async fn native_handoff_store(
        &self,
        session_id: &str,
        acp_session_id: &str,
    ) -> Option<crate::session::ExecutionBinding> {
        let workers = self.workers.lock().await;
        let handle = workers.get(session_id)?;
        if !lock_recover(&self.lifecycle).is_running(session_id)
            || handle.native_session_id.as_deref() != Some(acp_session_id)
        {
            return None;
        }
        handle.client.native_store.clone()
    }

    /// Whether this daemon holds the session's lease in any phase, including stopping.
    pub async fn is_owned(&self, session_id: &str) -> bool {
        lock_recover(&self.lifecycle).is_owned(session_id)
    }

    /// Wall-clock ms of the last notification a live worker received, or
    /// `None` without a worker or before its first notification.
    pub async fn last_notification_ms(&self, session_id: &str) -> Option<i64> {
        self.workers
            .lock()
            .await
            .get(session_id)
            .and_then(|handle| handle.client.last_notification_ms())
    }

    pub async fn count(&self) -> usize {
        self.workers.lock().await.len()
    }
}

/// Recover a poisoned mutex and clear the poison. A panic under one of these
/// locks leaves the data consistent (a map edit, or a seq counter whose
/// publish panicked in the sink). Clearing keeps that one panic from warning
/// on every later lock.
fn lock_recover<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| {
        warn!(
            target: "acp.supervisor",
            "recovered poisoned supervisor lock"
        );
        m.clear_poison();
        e.into_inner()
    })
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[tokio::test]
    async fn respawned_worker_rejects_the_prior_generation() {
        let sup = Supervisor::new(VecSink::new());
        let first = sup.test_insert_worker("s-generation").await;
        let second = sup.test_respawn_worker("s-generation").await;

        assert_ne!(first, second);
        assert!(
            !sup.is_current_worker_generation("s-generation", first)
                .await
        );
        assert!(
            sup.is_current_worker_generation("s-generation", second)
                .await
        );
    }

    #[tokio::test]
    async fn bookkeeping_sets_track_and_drain() {
        let sup = Supervisor::new(VecSink::new());
        sup.mark_incompatible_binary("s-claude-1", "claude-agent-acp");
        sup.mark_incompatible_binary("s-claude-2", "claude-agent-acp");
        sup.mark_incompatible_binary("s-codex", "codex-acp");
        let mut claude = sup
            .incompatible_sessions_for_binary("claude-agent-acp")
            .await;
        claude.sort();
        assert_eq!(claude, vec!["s-claude-1", "s-claude-2"]);
        assert_eq!(
            sup.incompatible_sessions_for_binary("codex-acp").await,
            vec!["s-codex"]
        );
        assert!(sup
            .incompatible_sessions_for_binary("gemini")
            .await
            .is_empty());
        sup.test_insert_worker("s-claude-1").await;
        assert_eq!(
            sup.incompatible_sessions_for_binary("claude-agent-acp")
                .await,
            vec!["s-claude-2"],
            "a session with a live worker is not blocked"
        );

        sup.request_respawn("s-1");
        sup.request_respawn("s-2");
        sup.request_respawn("s-1");
        let mut ids = sup.take_respawn_requests();
        ids.sort();
        assert_eq!(ids, vec!["s-1", "s-2"]);
        assert!(sup.take_respawn_requests().is_empty());
    }
}

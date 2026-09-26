//! Session-state probes derived from the log: turns, pending requests, and snapshots.

use std::collections::HashMap;

use rusqlite::{params, OptionalExtension};

use super::{decode, logged, query_strings, EventStore, NON_SUBSTANTIVE_EVENT_DISCRIMINANTS};
use crate::acp::approvals::{Approval, Nonce};
use crate::acp::state::{Event, Plan};
use crate::events;
use tracing::warn;

/// A background agent silent this long without completing no longer holds a turn open.
const BACKGROUND_AGENT_STALE_AFTER_MS: i64 = 6 * 60 * 1000;

/// An agent-started turn with no newer substantive event this long is idle.
const AGENT_TURN_STALE_AFTER_MS: i64 = 2 * 60 * 60 * 1000;

/// What the terminal-repair pass needs to decide, and to publish safely.
pub struct TerminalRepairProbe {
    /// Newest seq of any kind, for the seq-conditional publish.
    pub latest_seq: u64,
    /// Newest substantive event, which decides whether to repair.
    pub substantive: Event,
    /// `created_at` of `substantive`, in ms since epoch.
    pub substantive_at_ms: i64,
}

/// One unresolved `BackgroundAgentLaunched` row. See
/// [`EventStore::unresolved_background_agent_launches`].
#[derive(Debug, Clone)]
pub struct UnresolvedBackgroundAgentLaunch {
    pub agent_id: String,
    pub output_file: String,
}

/// Launches with no matching completion, shared by both unresolved-launch
/// queries. Both `agent_id` extractions are `IS NOT NULL`-guarded: SQLite's
/// `NOT IN` evaluates to `NULL` (never true) for every row once the subquery
/// yields one `NULL`, so a single unextractable id would otherwise blank the
/// whole result instead of just miscounting that row.
const UNRESOLVED_BACKGROUND_AGENT_WHERE: &str = "
     FROM acp_events
     WHERE session_id = ?1
       AND discriminant = 'BackgroundAgentLaunched'
       AND json_extract(event_json, '$.BackgroundAgentLaunched.agent_id') IS NOT NULL
       AND json_extract(event_json, '$.BackgroundAgentLaunched.agent_id') NOT IN (
           SELECT json_extract(event_json, '$.BackgroundAgentCompleted.agent_id')
           FROM acp_events
           WHERE session_id = ?1
             AND discriminant = 'BackgroundAgentCompleted'
             AND json_extract(event_json, '$.BackgroundAgentCompleted.agent_id') IS NOT NULL
       )
     ORDER BY seq ASC";

impl EventStore {
    pub fn latest_plan(&self, session_id: &str) -> Option<Plan> {
        let (_, json) =
            events::latest_by_discriminant(&self.conn(), &self.schema, session_id, "PlanUpdated")?;
        match decode(&json)? {
            Event::PlanUpdated { plan } => Some(plan),
            _ => None,
        }
    }

    /// Latest advertised `(image, audio, embedded_context)` prompt capabilities.
    pub fn latest_prompt_capabilities(&self, session_id: &str) -> Option<(bool, bool, bool)> {
        let (_, json) = events::latest_by_discriminant(
            &self.conn(),
            &self.schema,
            session_id,
            "PromptCapabilities",
        )?;
        match decode(&json)? {
            Event::PromptCapabilities {
                image,
                audio,
                embedded_context,
                ..
            } => Some((image, audio, embedded_context)),
            _ => None,
        }
    }

    /// Latest `created_at` (ms) of a substantive event per session; lifecycle
    /// metadata does not reset the idle clock.
    pub fn last_event_at_for_sessions(&self, session_ids: &[String]) -> HashMap<String, i64> {
        if session_ids.is_empty() {
            return HashMap::new();
        }
        events::last_event_at_for_topics(
            &self.conn(),
            &self.schema,
            session_ids,
            NON_SUBSTANTIVE_EVENT_DISCRIMINANTS,
        )
    }

    /// Latest event the sidebar status derivation cares about.
    pub fn latest_seed_status_event(&self, session_id: &str) -> Option<Event> {
        let json: String = logged(
            self.conn()
                .query_row(
                    "SELECT event_json FROM acp_events
                     WHERE session_id = ?1
                       AND (json_extract(event_json, '$.UserPromptSent') IS NOT NULL
                         OR json_extract(event_json, '$.ApprovalRequested') IS NOT NULL
                         OR json_extract(event_json, '$.ApprovalResolved') IS NOT NULL
                         OR json_extract(event_json, '$.ElicitationRequested') IS NOT NULL
                         OR json_extract(event_json, '$.ElicitationResolved') IS NOT NULL
                         OR json_extract(event_json, '$.Stopped') IS NOT NULL
                         OR json_extract(event_json, '$.RateLimitAutoResumed') IS NOT NULL
                         OR json_extract(event_json, '$.AgentStartupError') IS NOT NULL
                         OR json_extract(event_json, '$.AgentMessageChunk') IS NOT NULL
                         OR json_extract(event_json, '$.AgentThoughtChunk') IS NOT NULL
                         OR json_extract(event_json, '$.ToolCallStarted') IS NOT NULL
                         -- A unit variant serializes as a bare JSON string.
                         OR event_json = '\"ThinkingStarted\"')
                     ORDER BY seq DESC
                     LIMIT 1",
                    params![session_id],
                    |row| row.get(0),
                )
                .optional(),
            "latest_seed_status_event query",
            session_id,
        )
        .flatten()?;
        decode(&json)
    }

    /// Text of the session's first `UserPromptSent`.
    pub fn first_user_prompt(&self, session_id: &str) -> Option<String> {
        let json: String = logged(
            self.conn()
                .query_row(
                    "SELECT event_json FROM acp_events
                     WHERE session_id = ?1
                       AND json_extract(event_json, '$.UserPromptSent') IS NOT NULL
                     ORDER BY seq ASC
                     LIMIT 1",
                    params![session_id],
                    |row| row.get(0),
                )
                .optional(),
            "first_user_prompt query",
            session_id,
        )
        .flatten()?;
        match decode(&json)? {
            Event::UserPromptSent { text, .. } => Some(text),
            _ => None,
        }
    }

    /// The first prompt and the agent prose of its turn (up to the first
    /// `Stopped`, whatever the reason), capped at `max_agent_bytes`.
    pub fn first_turn_context(
        &self,
        session_id: &str,
        max_agent_bytes: usize,
    ) -> Option<(String, String)> {
        let rows = query_strings(
            &self.conn(),
            "SELECT event_json FROM acp_events
             WHERE session_id = ?1
             ORDER BY seq ASC",
            "first_turn_context",
            session_id,
        );
        let mut first_prompt: Option<String> = None;
        let mut agent = String::new();
        for event in rows.iter().filter_map(|json| decode(json)) {
            match event {
                Event::UserPromptSent { text, .. } if first_prompt.is_none() => {
                    first_prompt = Some(text);
                }
                Event::AgentMessageChunk { text }
                    if first_prompt.is_some() && agent.len() < max_agent_bytes =>
                {
                    agent.push_str(&text);
                }
                Event::Stopped { .. } if first_prompt.is_some() => break,
                _ => {}
            }
        }
        let first_prompt = first_prompt?;
        if agent.len() > max_agent_bytes {
            let mut end = max_agent_bytes;
            while !agent.is_char_boundary(end) {
                end -= 1;
            }
            agent.truncate(end);
        }
        Some((first_prompt, agent))
    }

    /// Nonces of `ApprovalRequested` events with no later `ApprovalResolved`.
    pub fn unresolved_approval_nonces(&self, session_id: &str) -> Vec<Nonce> {
        query_strings(
            &self.conn(),
            "SELECT json_extract(event_json, '$.ApprovalRequested.approval.nonce') AS nonce
             FROM acp_events
             WHERE session_id = ?1
               AND discriminant = 'ApprovalRequested'
               AND json_extract(event_json, '$.ApprovalRequested.approval.nonce') NOT IN (
                   SELECT json_extract(event_json, '$.ApprovalResolved.nonce')
                   FROM acp_events
                   WHERE session_id = ?1
                     AND discriminant = 'ApprovalResolved'
               )
             ORDER BY seq ASC",
            "unresolved_approval_nonces",
            session_id,
        )
        .into_iter()
        .map(Nonce)
        .collect()
    }

    /// Agent ids of `BackgroundAgentLaunched` events with no later
    /// `BackgroundAgentCompleted`. Read from the durable log rather than the
    /// control cache, which may be cold for a session no reader has hydrated
    /// since a daemon restart, so a dying worker's teardown can detach
    /// sub-agents its tailer will never report on again (#4001).
    pub fn unresolved_background_agent_ids(&self, session_id: &str) -> Vec<String> {
        query_strings(
            &self.conn(),
            &format!(
                "SELECT json_extract(event_json, '$.BackgroundAgentLaunched.agent_id') AS agent_id\
                 {UNRESOLVED_BACKGROUND_AGENT_WHERE}"
            ),
            "unresolved_background_agent_ids",
            session_id,
        )
    }

    /// The same rows as [`Self::unresolved_background_agent_ids`], carrying
    /// the transcript path a resumed tailer needs. `Supervisor::attach` uses
    /// it to resume tracking a sub-agent that survived the restart instead of
    /// detaching it.
    pub fn unresolved_background_agent_launches(
        &self,
        session_id: &str,
    ) -> Vec<UnresolvedBackgroundAgentLaunch> {
        let conn = self.conn();
        let what = "unresolved_background_agent_launches";
        let sql = format!(
            "SELECT json_extract(event_json, '$.BackgroundAgentLaunched.agent_id'),\
                    json_extract(event_json, '$.BackgroundAgentLaunched.output_file')\
             {UNRESOLVED_BACKGROUND_AGENT_WHERE}"
        );
        let Some(mut stmt) = logged(conn.prepare(&sql), what, session_id) else {
            return Vec::new();
        };
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(UnresolvedBackgroundAgentLaunch {
                agent_id: row.get(0)?,
                output_file: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            })
        });
        logged(rows, what, session_id)
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    /// Full payloads of unresolved approval requests, in request order.
    pub fn pending_approval_requests(&self, session_id: &str) -> Vec<Approval> {
        query_strings(
            &self.conn(),
            "SELECT event_json
             FROM acp_events
             WHERE session_id = ?1
               AND discriminant = 'ApprovalRequested'
               AND json_extract(event_json, '$.ApprovalRequested.approval.nonce') NOT IN (
                   SELECT json_extract(event_json, '$.ApprovalResolved.nonce')
                   FROM acp_events
                   WHERE session_id = ?1
                     AND discriminant = 'ApprovalResolved'
               )
             ORDER BY seq ASC",
            "pending_approval_requests",
            session_id,
        )
        .iter()
        .filter_map(|json| match decode(json)? {
            Event::ApprovalRequested { approval } => Some(approval),
            _ => None,
        })
        .collect()
    }

    /// Nonces of `ElicitationRequested` events with no matching `ElicitationResolved`.
    pub fn unresolved_elicitation_nonces(&self, session_id: &str) -> Vec<Nonce> {
        query_strings(
            &self.conn(),
            "SELECT json_extract(event_json, '$.ElicitationRequested.elicitation.nonce') AS nonce
             FROM acp_events
             WHERE session_id = ?1
               AND json_extract(event_json, '$.ElicitationRequested') IS NOT NULL
               AND json_extract(event_json, '$.ElicitationRequested.elicitation.nonce') NOT IN (
                   SELECT json_extract(event_json, '$.ElicitationResolved.nonce')
                   FROM acp_events
                   WHERE session_id = ?1
                     AND json_extract(event_json, '$.ElicitationResolved') IS NOT NULL
               )",
            "unresolved_elicitation_nonces",
            session_id,
        )
        .into_iter()
        .map(Nonce)
        .collect()
    }

    /// True while the latest `UserPromptSent` has no terminal event, or a
    /// background agent it launched is still reporting and has not completed.
    pub fn has_in_flight_turn(&self, session_id: &str) -> bool {
        let conn = self.conn();
        let prompt_seq = logged(
            conn.query_row(
                "SELECT MAX(seq) FROM acp_events
                 WHERE session_id = ?1
                   AND json_extract(event_json, '$.UserPromptSent') IS NOT NULL",
                params![session_id],
                |row| row.get::<_, Option<i64>>(0),
            ),
            "has_in_flight_turn prompt query",
            session_id,
        )
        .flatten();
        let Some(prompt_seq) = prompt_seq else {
            return false;
        };
        let Some(terminator) = logged(
            conn.query_row(
                "SELECT MIN(seq) FROM acp_events
                 WHERE session_id = ?1
                   AND seq > ?2
                   AND (json_extract(event_json, '$.Stopped') IS NOT NULL
                     OR json_extract(event_json, '$.AgentStartupError') IS NOT NULL)",
                params![session_id, prompt_seq],
                |row| row.get::<_, Option<i64>>(0),
            ),
            "has_in_flight_turn terminator query",
            session_id,
        ) else {
            return false;
        };
        if terminator.is_none() {
            return true;
        }
        let stale_cutoff = chrono::Utc::now().timestamp_millis() - BACKGROUND_AGENT_STALE_AFTER_MS;
        let bg_in_flight: i64 = logged(
            conn.query_row(
                "SELECT COUNT(*) FROM (
                     SELECT aid, MAX(created_at) AS last_at FROM (
                         SELECT json_extract(event_json, '$.BackgroundAgentLaunched.agent_id') AS aid,
                                created_at
                           FROM acp_events WHERE session_id = ?1
                             AND json_extract(event_json, '$.BackgroundAgentLaunched') IS NOT NULL
                         UNION ALL
                         SELECT json_extract(event_json, '$.BackgroundAgentProgress.agent_id'),
                                created_at
                           FROM acp_events WHERE session_id = ?1
                             AND json_extract(event_json, '$.BackgroundAgentProgress') IS NOT NULL
                     )
                     WHERE aid IS NOT NULL
                     GROUP BY aid
                 ) started
                 WHERE started.last_at >= ?2
                   AND started.aid NOT IN (
                     SELECT json_extract(event_json, '$.BackgroundAgentCompleted.agent_id')
                       FROM acp_events WHERE session_id = ?1
                         AND json_extract(event_json, '$.BackgroundAgentCompleted') IS NOT NULL
                   )",
                params![session_id, stale_cutoff],
                |row| row.get(0),
            ),
            "has_in_flight_turn bg-agent query",
            session_id,
        )
        .unwrap_or(0);
        bg_in_flight > 0
    }

    /// True when an agent-opened turn is still under way, either as the newest
    /// event since the last stop or as the newest turn-role. A turn the agent
    /// opened itself has no `UserPromptSent`, so [`Self::has_in_flight_turn`]
    /// misses it. Either probe failing closed holds the worker.
    pub fn has_agent_turn_in_flight(&self, session_id: &str) -> bool {
        self.agent_turn_open_since_stop(session_id) || self.agent_turn_open_by_role(session_id)
    }

    /// True when the newest substantive event after the latest terminator is
    /// work still under way. A cost-populated `UsageUpdated` is the end marker.
    fn agent_turn_open_since_stop(&self, session_id: &str) -> bool {
        let conn = self.conn();
        let epoch_start: i64 = match conn
            .query_row(
                "SELECT MAX(seq) FROM acp_events
                 WHERE session_id = ?1
                   AND (json_extract(event_json, '$.Stopped') IS NOT NULL
                     OR json_extract(event_json, '$.AgentStartupError') IS NOT NULL)",
                params![session_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
        {
            Ok(v) => v.flatten().unwrap_or(0),
            Err(e) => {
                tracing::warn!(target: "acp.event_store", "has_agent_turn_in_flight epoch query for {session_id}: {e}");
                return true;
            }
        };
        let clauses = NON_SUBSTANTIVE_EVENT_DISCRIMINANTS
            .iter()
            .map(|_| "AND event_json NOT LIKE ?")
            .collect::<Vec<_>>()
            .join("\n                   ");
        let sql = format!(
            "SELECT event_json, created_at FROM acp_events
                 WHERE session_id = ?
                   AND seq > ?
                   {clauses}
                 ORDER BY seq DESC
                 LIMIT 1"
        );
        let mut bind: Vec<rusqlite::types::Value> =
            vec![session_id.to_string().into(), epoch_start.into()];
        bind.extend(
            NON_SUBSTANTIVE_EVENT_DISCRIMINANTS
                .iter()
                .map(|name| format!("{{\"{name}\":%").into()),
        );
        let (json, created_at) = match conn
            .query_row(&sql, rusqlite::params_from_iter(bind), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .optional()
        {
            Ok(Some(row)) => row,
            Ok(None) => return false,
            Err(e) => {
                tracing::warn!(target: "acp.event_store", "has_agent_turn_in_flight latest event for {session_id}: {e}");
                return true;
            }
        };
        if chrono::Utc::now()
            .timestamp_millis()
            .saturating_sub(created_at)
            > AGENT_TURN_STALE_AFTER_MS
        {
            return false;
        }
        match serde_json::from_str::<Event>(&json) {
            Ok(Event::UsageUpdated { usage }) => usage.cost.is_none(),
            Ok(_) | Err(_) => true,
        }
    }

    /// Read the inputs the terminal-repair pass decides on.
    pub fn terminal_repair_probe(&self, session_id: &str) -> Option<TerminalRepairProbe> {
        let conn = self.conn();
        let latest_seq = logged(
            conn.query_row(
                "SELECT MAX(seq) FROM acp_events WHERE session_id = ?1",
                params![session_id],
                |row| row.get::<_, Option<i64>>(0),
            ),
            "terminal_repair_probe latest seq",
            session_id,
        )
        .flatten()?;
        let clauses = NON_SUBSTANTIVE_EVENT_DISCRIMINANTS
            .iter()
            .map(|_| "AND event_json NOT LIKE ?")
            .collect::<Vec<_>>()
            .join("\n                   ");
        let sql = format!(
            "SELECT event_json, created_at FROM acp_events
                 WHERE session_id = ?
                   {clauses}
                 ORDER BY seq DESC
                 LIMIT 1"
        );
        let bind = std::iter::once(session_id.to_string()).chain(
            NON_SUBSTANTIVE_EVENT_DISCRIMINANTS
                .iter()
                .map(|name| format!("{{\"{name}\":%")),
        );
        let (json, substantive_at_ms): (String, i64) = logged(
            conn.query_row(&sql, rusqlite::params_from_iter(bind), |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional(),
            "terminal_repair_probe substantive event",
            session_id,
        )
        .flatten()?;
        Some(TerminalRepairProbe {
            latest_seq: latest_seq as u64,
            substantive: decode(&json)?,
            substantive_at_ms,
        })
    }

    /// True when the current turn epoch has a `ToolCallStarted` with no
    /// matching `ToolCallCompleted`. Fails closed (true) on a query error.
    pub fn has_open_tool_call_in_epoch(&self, session_id: &str) -> bool {
        let conn = self.conn();
        let epoch_start: i64 = logged(
            conn.query_row(
                "SELECT MAX(seq) FROM acp_events
                 WHERE session_id = ?1
                   AND (json_extract(event_json, '$.Stopped') IS NOT NULL
                     OR json_extract(event_json, '$.AgentStartupError') IS NOT NULL)",
                params![session_id],
                |row| row.get::<_, Option<i64>>(0),
            ),
            "has_open_tool_call_in_epoch epoch query",
            session_id,
        )
        .flatten()
        .unwrap_or(0);
        logged(
            conn.query_row(
                "SELECT 1 FROM acp_events
                 WHERE session_id = ?1
                   AND seq > ?2
                   AND json_extract(event_json, '$.ToolCallStarted') IS NOT NULL
                   AND json_extract(event_json, '$.ToolCallStarted.tool_call.id') NOT IN (
                       SELECT json_extract(event_json, '$.ToolCallCompleted.tool_call_id')
                         FROM acp_events
                        WHERE session_id = ?1
                          AND seq > ?2
                          AND json_extract(event_json, '$.ToolCallCompleted') IS NOT NULL
                          -- `x NOT IN (a, NULL)` is NULL, which would hide every open call.
                          AND json_extract(event_json, '$.ToolCallCompleted.tool_call_id') IS NOT NULL
                   )
                 LIMIT 1",
                params![session_id, epoch_start],
                |row| row.get::<_, i64>(0),
            )
            .optional(),
            "has_open_tool_call_in_epoch",
            session_id,
        )
        .is_none_or(|open| open.is_some())
    }

    /// True while the newest turn-bearing event is the agent's own work, including
    /// a turn it opened with no `UserPromptSent` behind it. Bookkeeping is skipped.
    /// Work older than two hours does not count. Fails closed.
    fn agent_turn_open_by_role(&self, session_id: &str) -> bool {
        const STALE_AFTER_MS: i64 = 2 * 60 * 60 * 1000;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let found = self.scan_turn_roles(session_id, |created_at, role| match role {
            TurnRole::Work => Some(now_ms.saturating_sub(created_at) <= STALE_AFTER_MS),
            TurnRole::End | TurnRole::Stop => Some(false),
            TurnRole::Neutral => None,
        });
        match found {
            Ok(open) => open.unwrap_or(false),
            Err(e) => {
                warn!(target: "acp.event_store", "has_agent_turn_in_flight for {session_id}: {e}");
                true
            }
        }
    }

    /// When the agent last proved itself idle, in ms. `None` when turn work
    /// follows that proof, or the latest turn was closed only by a stop the
    /// daemon synthesized. A log with no turn work is idle since 0. Fails closed.
    pub fn agent_idle_since(&self, session_id: &str) -> Option<i64> {
        let found = self.scan_turn_roles(session_id, |created_at, role| match role {
            TurnRole::End => Some(Some(created_at)),
            TurnRole::Work => Some(None),
            TurnRole::Stop | TurnRole::Neutral => None,
        });
        match found {
            Ok(Some(idle_since)) => idle_since,
            Ok(None) => Some(0),
            Err(e) => {
                warn!(target: "acp.event_store", "agent_idle_since for {session_id}: {e}");
                None
            }
        }
    }

    /// Newest first, until `visit` decides. A row that fails to decode is turn work.
    fn scan_turn_roles<T>(
        &self,
        session_id: &str,
        mut visit: impl FnMut(i64, TurnRole) -> Option<T>,
    ) -> rusqlite::Result<Option<T>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT created_at, event_json FROM acp_events
             WHERE session_id = ?1 ORDER BY seq DESC",
        )?;
        let mut rows = stmt.query(params![session_id])?;
        // A refused prompt never reached the agent, so its UserPromptSent is
        // not work. Paired by text across neutral rows only.
        let mut refused: Option<String> = None;
        while let Some(row) = rows.next()? {
            let created_at: i64 = row.get(0)?;
            let json: String = row.get(1)?;
            let role = match decode(&json) {
                None => TurnRole::Work,
                Some(Event::PromptRejected { text, .. }) => {
                    refused = Some(text);
                    TurnRole::Neutral
                }
                Some(Event::UserPromptSent { text, .. })
                    if refused.as_deref() == Some(text.as_str()) =>
                {
                    refused = None;
                    TurnRole::Neutral
                }
                Some(event) => TurnRole::of(&event),
            };
            if !matches!(role, TurnRole::Neutral) {
                refused = None;
            }
            if let Some(decided) = visit(created_at, role) {
                return Ok(Some(decided));
            }
        }
        Ok(None)
    }
}

/// Adapter stop reasons that prove the agent ended its own turn.
const END_OF_TURN_STOP_REASONS: &[&str] = &[
    "prompt_complete",
    "cancelled",
    "max_tokens",
    "refusal",
    "max_turn_requests",
];

/// How one logged event bears on whether the agent is in a turn.
enum TurnRole {
    End,
    Stop,
    Work,
    Neutral,
}

impl TurnRole {
    fn of(event: &Event) -> Self {
        match event {
            Event::UsageUpdated { usage } if usage.cost.is_some() => Self::End,
            Event::Stopped { reason } if END_OF_TURN_STOP_REASONS.contains(&reason.as_str()) => {
                Self::End
            }
            Event::Stopped { .. }
            | Event::AgentStartupError { .. }
            | Event::IncompatibleAgent { .. }
            | Event::PromptRuntimeError { .. } => Self::Stop,
            Event::UsageUpdated { .. }
            | Event::PlanUpdated { .. }
            | Event::TodoListUpdated { .. }
            | Event::ToolCallStarted { .. }
            | Event::ToolCallCompleted { .. }
            | Event::ToolCallContent { .. }
            | Event::ToolCallUpdated { .. }
            | Event::ApprovalRequested { .. }
            | Event::ElicitationRequested { .. }
            | Event::DiffEmitted { .. }
            | Event::ThinkingStarted
            | Event::ThinkingEnded
            | Event::AgentMessageChunk { .. }
            | Event::CancelRequested { .. }
            | Event::UserPromptSent { .. }
            | Event::UserDiffCommentsPrompt { .. }
            | Event::ConversationCompactionStarted
            | Event::ConversationCompacted
            | Event::RawAgentUpdate { .. } => Self::Work,
            Event::SessionTitleSuggested { .. }
            | Event::ApprovalResolved { .. }
            | Event::ElicitationResolved { .. }
            | Event::RateLimit { .. }
            | Event::RateLimitAutoResumed { .. }
            | Event::ModeChanged { .. }
            | Event::ModesAvailable { .. }
            | Event::CurrentModeChanged { .. }
            | Event::ModeSwitchFailed { .. }
            | Event::AvailableCommandsUpdated { .. }
            | Event::ConfigOptionsUpdated { .. }
            | Event::ConfigOptionSwitchFailed { .. }
            | Event::BackgroundAgentLaunched { .. }
            | Event::BackgroundAgentProgress { .. }
            | Event::BackgroundAgentCompleted { .. }
            | Event::PromptCapabilities { .. }
            | Event::PromptRejected { .. }
            | Event::AcpSessionAssigned { .. }
            | Event::SessionContextReset { .. }
            | Event::WakeupScheduled { .. }
            | Event::MonitorArmed { .. }
            | Event::SessionCleared
            | Event::AgentSwitched { .. }
            | Event::ConversationSummary { .. } => Self::Neutral,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use crate::acp::approvals::ApprovalDecision;
    use crate::acp::state::BackgroundAgentStatus;

    #[test]
    fn first_prompt_and_first_turn_context() {
        let (_tmp, store) = open_store(1000);
        assert!(store.first_user_prompt("s-1").is_none());
        assert!(store.first_turn_context("s-1", 4096).is_none());
        // Recorded out of order: the earliest seq wins.
        store
            .record("s-1", 2, &user_prompt("second prompt"))
            .unwrap();
        store
            .record("s-1", 1, &user_prompt("first prompt"))
            .unwrap();
        assert_eq!(
            store.first_user_prompt("s-1").as_deref(),
            Some("first prompt")
        );
        assert!(store.first_user_prompt("s-2").is_none());

        // (events, expected agent prose)
        let cases = [
            (
                vec![
                    user_prompt("p"),
                    agent_chunk("Looking at auth.rs. "),
                    agent_chunk("Patched the redirect."),
                    stopped("prompt_complete"),
                    user_prompt("now add a test"),
                    agent_chunk("second turn prose"),
                    stopped("prompt_complete"),
                ],
                "Looking at auth.rs. Patched the redirect.",
            ),
            (
                vec![
                    user_prompt("p"),
                    agent_chunk("first-turn prose"),
                    stopped("user_stopped"),
                    user_prompt("resume it"),
                    agent_chunk("second-turn prose"),
                    stopped("prompt_complete"),
                ],
                "first-turn prose",
            ),
            (vec![user_prompt("p"), stopped("prompt_complete")], ""),
        ];
        for (i, (events, agent)) in cases.into_iter().enumerate() {
            let id = format!("t-{i}");
            record_from(&store, &id, 1, events);
            assert_eq!(
                store.first_turn_context(&id, 4096),
                Some(("p".to_string(), agent.to_string())),
                "case {i}"
            );
        }

        record_from(
            &store,
            "c-1",
            1,
            [
                user_prompt("go"),
                agent_chunk(&"é".repeat(50)),
                stopped("prompt_complete"),
            ],
        );
        let (_, agent) = store.first_turn_context("c-1", 10).expect("has prompt");
        assert!(agent.len() <= 10 && agent.chars().all(|c| c == 'é'));
    }

    #[test]
    fn has_in_flight_turn_follows_the_latest_prompt_and_background_agents() {
        let (_tmp, store) = open_store(1000);
        let progress = || Event::BackgroundAgentProgress {
            agent_id: "bg-1".into(),
            status: BackgroundAgentStatus::Running,
            tool_count: 1,
            tools: Vec::new(),
            last_tool: None,
            last_text: None,
            at: chrono::Utc::now(),
        };
        let startup_error = Event::AgentStartupError {
            message: "boom".into(),
        };
        // (events, in flight)
        let cases = [
            (vec![], false),
            (vec![user_prompt("go"), agent_chunk("thinking")], true),
            (
                vec![
                    user_prompt("go"),
                    agent_chunk("done"),
                    stopped("prompt_complete"),
                ],
                false,
            ),
            (vec![user_prompt("go"), startup_error], false),
            (
                vec![
                    user_prompt("first"),
                    stopped("prompt_complete"),
                    user_prompt("second"),
                    agent_chunk("mid"),
                ],
                true,
            ),
            (
                vec![user_prompt("go"), stopped("prompt_complete"), progress()],
                true,
            ),
        ];
        for (i, (events, in_flight)) in cases.into_iter().enumerate() {
            let id = format!("s-{i}");
            record_from(&store, &id, 1, events);
            assert_eq!(store.has_in_flight_turn(&id), in_flight, "case {i}");
        }

        let completed = Event::BackgroundAgentCompleted {
            agent_id: "bg-1".into(),
            status: BackgroundAgentStatus::Completed,
            tools: Vec::new(),
            result: None,
            warning: None,
            ended_at: chrono::Utc::now(),
        };
        store.record("s-5", 4, &completed).unwrap();
        assert!(
            !store.has_in_flight_turn("s-5"),
            "completion drains the turn"
        );

        record_from(
            &store,
            "stale",
            1,
            [user_prompt("go"), stopped("prompt_complete")],
        );
        let stale_at =
            chrono::Utc::now().timestamp_millis() - (BACKGROUND_AGENT_STALE_AFTER_MS + 60_000);
        store.record_at("stale", 3, &progress(), stale_at).unwrap();
        assert!(
            !store.has_in_flight_turn("stale"),
            "a silent background agent is gone"
        );
    }

    #[test]
    fn snapshot_queries_read_the_latest_event() {
        use crate::acp::state::{PlanStep, PlanStepStatus};
        let (_tmp, store) = open_store(1000);
        let capabilities = |image| Event::PromptCapabilities {
            image,
            audio: false,
            embedded_context: image,
            load_session: None,
            steering: false,
        };
        let plan = |status| Event::PlanUpdated {
            plan: Plan {
                plan_id: "p".into(),
                version: 1,
                steps: vec![PlanStep {
                    id: "s-1".into(),
                    title: "Step one".into(),
                    detail: None,
                    status,
                }],
            },
        };
        assert_eq!(store.latest_prompt_capabilities("s-1"), None);
        assert!(store.latest_plan("s-1").is_none());
        record_from(
            &store,
            "s-1",
            1,
            [
                capabilities(true),
                plan(PlanStepStatus::Pending),
                Event::ThinkingStarted,
                capabilities(false),
                plan(PlanStepStatus::Done),
            ],
        );
        assert_eq!(
            store.latest_prompt_capabilities("s-1"),
            Some((false, false, false))
        );
        assert!(matches!(
            store.latest_plan("s-1").expect("plan").steps[0].status,
            PlanStepStatus::Done
        ));

        record_from(
            &store,
            "s-2",
            1,
            [
                user_prompt("go"),
                stopped("prompt_complete"),
                agent_chunk("build done"),
            ],
        );
        assert!(matches!(
            store.latest_seed_status_event("s-2"),
            Some(Event::AgentMessageChunk { text }) if text == "build done"
        ));
        store.record("s-3", 1, &Event::ThinkingStarted).unwrap();
        assert!(matches!(
            store.latest_seed_status_event("s-3"),
            Some(Event::ThinkingStarted)
        ));
    }

    #[test]
    fn last_event_at_takes_the_substantive_max_per_session() {
        let (_tmp, store) = open_store(1000);
        record_from(
            &store,
            "s-lifecycle",
            1,
            [
                Event::AcpSessionAssigned {
                    acp_session_id: "acp-1".into(),
                },
                Event::ModesAvailable {
                    current_mode_id: "default".into(),
                    modes: vec![],
                },
            ],
        );
        for (id, seq, created_at) in [
            ("s-a", 1, 100),
            ("s-a", 2, 900),
            ("s-a", 3, 400),
            ("s-b", 1, 7000),
        ] {
            store
                .record_at(id, seq, &Event::ThinkingStarted, created_at)
                .unwrap();
        }
        let ids = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(store.last_event_at_for_sessions(&[]).is_empty());
        let map =
            store.last_event_at_for_sessions(&ids(&["s-lifecycle", "s-a", "s-b", "s-missing"]));
        assert_eq!(
            map,
            HashMap::from([("s-a".to_string(), 900), ("s-b".to_string(), 7000)])
        );
    }

    #[test]
    fn unresolved_requests_exclude_resolved_nonces_per_session() {
        use crate::acp::elicitations::{Elicitation, ElicitationOutcome};
        let (_tmp, store) = open_store(1000);
        let approval = |nonce: &str| Event::ApprovalRequested {
            approval: Approval {
                nonce: Nonce(nonce.into()),
                tool_call: tool_call("tc-1"),
                destructive: false,
                options: Vec::new(),
                choice: false,
                requested_at: chrono::Utc::now(),
                resolved: None,
            },
        };
        let elicitation = |nonce: &Nonce| Event::ElicitationRequested {
            elicitation: Elicitation {
                nonce: nonce.clone(),
                message: "Pick".into(),
                title: None,
                description: None,
                tool_call_id: None,
                questions: Vec::new(),
                requested_at: chrono::Utc::now(),
                resolved: None,
            },
        };
        let (e_a, e_b) = (Nonce::new(), Nonce::new());
        record_from(
            &store,
            "s-1",
            1,
            [
                approval("aaaa"),
                Event::ApprovalResolved {
                    nonce: Nonce("aaaa".into()),
                    decision: ApprovalDecision::Allow,
                },
                approval("bbbb"),
                approval("cccc"),
                elicitation(&e_a),
                elicitation(&e_b),
                Event::ElicitationResolved {
                    nonce: e_a,
                    outcome: ElicitationOutcome::Accepted,
                    answers: Vec::new(),
                },
            ],
        );
        assert_eq!(
            store.unresolved_approval_nonces("s-1"),
            [Nonce("bbbb".into()), Nonce("cccc".into())]
        );
        let pending: Vec<String> = store
            .pending_approval_requests("s-1")
            .into_iter()
            .map(|a| a.nonce.0)
            .collect();
        assert_eq!(pending, ["bbbb", "cccc"]);
        assert_eq!(store.unresolved_elicitation_nonces("s-1"), [e_b]);
        assert!(store.unresolved_approval_nonces("s-2").is_empty());
        assert!(store.unresolved_elicitation_nonces("s-2").is_empty());
    }

    fn bg_launched(agent_id: &str, output_file: &str) -> Event {
        Event::BackgroundAgentLaunched {
            agent_id: agent_id.into(),
            tool_call_id: format!("tc-{agent_id}"),
            description: "map backend".into(),
            prompt: "do it".into(),
            model: "claude-opus-4-8".into(),
            output_file: output_file.into(),
            started_at: chrono::Utc::now(),
        }
    }

    /// Background-agent parallel of `unresolved_approval_nonces`: a launch
    /// whose id never saw a matching completion is orphaned, and the launches
    /// variant carries the transcript path a resumed tailer needs.
    #[test]
    fn unresolved_background_agent_launches_find_orphans() {
        let (_tmp, store) = open_store(1000);
        record_from(
            &store,
            "s-1",
            1,
            [
                bg_launched("bg-a", "/tmp/a.jsonl"),
                Event::BackgroundAgentCompleted {
                    agent_id: "bg-a".into(),
                    status: BackgroundAgentStatus::Completed,
                    tools: Vec::new(),
                    result: None,
                    warning: None,
                    ended_at: chrono::Utc::now(),
                },
                bg_launched("bg-b", "/tmp/b.jsonl"),
                bg_launched("bg-c", ""),
            ],
        );

        assert_eq!(
            store.unresolved_background_agent_ids("s-1"),
            ["bg-b".to_string(), "bg-c".to_string()]
        );
        let launches: Vec<(String, String)> = store
            .unresolved_background_agent_launches("s-1")
            .into_iter()
            .map(|l| (l.agent_id, l.output_file))
            .collect();
        assert_eq!(
            launches,
            [
                ("bg-b".to_string(), "/tmp/b.jsonl".to_string()),
                ("bg-c".to_string(), String::new()),
            ]
        );
        assert!(store.unresolved_background_agent_ids("s-2").is_empty());
        assert!(store.unresolved_background_agent_launches("s-2").is_empty());
    }

    /// A completion row whose `agent_id` is not extractable (truncated
    /// payload, schema drift) must not hide every other orphan: SQLite's
    /// `NOT IN` goes `NULL` for the whole result once the subquery yields one
    /// `NULL`, silently no-opping the detach scan. The store never writes
    /// such a row, so insert one directly.
    #[test]
    fn unresolved_background_agent_ids_survive_an_unextractable_completed_row() {
        let (_tmp, store) = open_store(1000);
        store
            .record("s-1", 1, &bg_launched("bg-real-orphan", ""))
            .unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO acp_events (session_id, seq, event_json, created_at, discriminant)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    "s-1",
                    2_i64,
                    "{\"BackgroundAgentCompleted\":{}}",
                    0_i64,
                    "BackgroundAgentCompleted",
                ],
            )
            .unwrap();

        assert_eq!(
            store.unresolved_background_agent_ids("s-1"),
            ["bg-real-orphan".to_string()],
            "an unextractable Completed row must not blank the whole scan"
        );
    }
}

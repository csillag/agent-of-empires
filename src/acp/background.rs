//! Per-session registry of background work, folded from the event log.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::acp::state::{
    BackgroundAgentStatus, BackgroundEnd, BackgroundEndReason, BackgroundItem, BackgroundKind,
    BackgroundLossCause, Event,
};

/// How long an ended item stays listed.
pub const ENDED_RETENTION_MS: i64 = 86_400_000;
/// How long a lost item stays listed (owner decision, 2026-09-13): lost
/// items are the ones the agent may not have noticed yet, so they get a
/// week rather than a day.
pub const LOST_RETENTION_MS: i64 = 604_800_000;

fn end_item(
    items: &mut [BackgroundItem],
    index: &HashMap<String, usize>,
    id: &str,
    e: BackgroundEnd,
) {
    if let Some(&i) = index.get(id) {
        if items[i].ended.is_none() {
            items[i].ended = Some(e);
        }
    }
}

/// Fold `(created_at, event)` rows, in log order, into the session's items.
/// Deadlines that have passed by `now` end their item: a timed monitor times
/// out and a wakeup fires. A later end never overrides an earlier one.
///
/// Only upstream's `BackgroundAgentLaunched` starts a sub-agent row. It ends
/// on whichever is seen first: `BackgroundAgentCompleted` (`Detached` means
/// lost) or the registry's own `Stopped`/`Finished`, since the tailer can lag
/// the agent's own stop by minutes. A registry `Lost` never ends it: it
/// records the cause for the `Detached` published with the same `at` (see
/// `detach_background_agents` in the supervisor).
pub fn fold_background(
    rows: impl IntoIterator<Item = (DateTime<Utc>, Event)>,
    now: DateTime<Utc>,
) -> Vec<BackgroundItem> {
    let mut items: Vec<BackgroundItem> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut detach_causes: HashMap<String, (DateTime<Utc>, BackgroundLossCause)> = HashMap::new();
    for (created_at, event) in rows {
        let (kind, id, label, started_at, expires_at) = match event {
            Event::BackgroundItemStarted {
                kind,
                id,
                label,
                started_at,
                expires_at,
                ..
            } => (kind, id, label, started_at, expires_at),
            Event::WakeupScheduled { at, reason } => (
                BackgroundKind::Wakeup,
                at.to_rfc3339(),
                reason,
                created_at,
                Some(at),
            ),
            Event::BackgroundAgentLaunched {
                agent_id,
                description,
                started_at,
                ..
            } => (
                BackgroundKind::Subagent,
                agent_id,
                Some(description),
                started_at,
                None,
            ),
            Event::BackgroundItemEnded {
                id,
                reason,
                cause,
                at,
            } => {
                let is_subagent = index
                    .get(&id)
                    .is_some_and(|&i| items[i].kind == BackgroundKind::Subagent);
                match (is_subagent, reason, cause) {
                    (true, BackgroundEndReason::Lost, Some(cause)) => {
                        detach_causes.insert(id, (at, cause));
                    }
                    (true, BackgroundEndReason::Stopped | BackgroundEndReason::Finished, _)
                    | (false, ..) => {
                        end_item(&mut items, &index, &id, BackgroundEnd { reason, cause, at });
                    }
                    _ => {}
                }
                continue;
            }
            Event::BackgroundAgentCompleted {
                agent_id,
                status,
                ended_at,
                ..
            } => {
                let e = if status == BackgroundAgentStatus::Detached {
                    BackgroundEnd {
                        reason: BackgroundEndReason::Lost,
                        cause: detach_causes
                            .remove(&agent_id)
                            .filter(|(at, _)| *at == ended_at)
                            .map(|(_, cause)| cause),
                        at: ended_at,
                    }
                } else {
                    BackgroundEnd {
                        reason: BackgroundEndReason::Finished,
                        cause: None,
                        at: ended_at,
                    }
                };
                end_item(&mut items, &index, &agent_id, e);
                continue;
            }
            _ => continue,
        };
        if let Some(&i) = index.get(&id) {
            items[i] = BackgroundItem {
                kind,
                id,
                label,
                started_at,
                expires_at,
                ended: None,
            };
        } else {
            index.insert(id.clone(), items.len());
            items.push(BackgroundItem {
                kind,
                id,
                label,
                started_at,
                expires_at,
                ended: None,
            });
        }
    }
    for item in &mut items {
        if item.ended.is_some() {
            continue;
        }
        let Some(deadline) = item.expires_at.filter(|at| *at <= now) else {
            continue;
        };
        let reason = match item.kind {
            BackgroundKind::Wakeup => BackgroundEndReason::Fired,
            BackgroundKind::Monitor => BackgroundEndReason::TimedOut,
            _ => continue,
        };
        item.ended = Some(BackgroundEnd {
            reason,
            cause: None,
            at: deadline,
        });
    }
    items.retain(|i| {
        i.ended.as_ref().is_none_or(|e| {
            let retention = if e.reason == BackgroundEndReason::Lost {
                LOST_RETENTION_MS
            } else {
                ENDED_RETENTION_MS
            };
            now.signed_duration_since(e.at).num_milliseconds() < retention
        })
    });
    items
}

/// The parts of a tool call's args a background item's label might need.
/// Kept separate, rather than folded into one label at observe time,
/// because the preferred field depends on the item's kind: a shell row
/// wants its command, everything else wants its description.
#[derive(Debug, Default, Clone)]
struct ToolLabelParts {
    description: Option<String>,
    command: Option<String>,
    title: Option<String>,
}

/// Labels for recent tool calls. The PostToolUse frame that starts a
/// background item carries no arguments; the earlier `ToolCallUpdated` does.
#[derive(Debug, Default)]
pub struct RecentToolLabels {
    order: VecDeque<String>,
    labels: HashMap<String, ToolLabelParts>,
}

const RECENT_TOOL_LABELS: usize = 128;

impl RecentToolLabels {
    /// Reads `ToolCallUpdated` (title, args_preview) and `ToolCallStarted`
    /// (the initial tool call, whose `name` and `args_preview` sometimes
    /// carry the only copy: a parallel or long-running call's command may
    /// never get a follow-up update).
    pub fn observe(&mut self, event: &Event) {
        let (tool_call_id, title, args_preview): (&str, Option<String>, Option<&str>) = match event
        {
            Event::ToolCallUpdated {
                tool_call_id,
                title,
                args_preview,
                ..
            } => (
                tool_call_id.as_str(),
                title.clone(),
                args_preview.as_deref(),
            ),
            Event::ToolCallStarted { tool_call } => (
                tool_call.id.as_str(),
                Some(tool_call.name.clone()),
                Some(tool_call.args_preview.as_str()),
            ),
            _ => return,
        };
        let args = args_preview.and_then(|a| serde_json::from_str::<serde_json::Value>(a).ok());
        let field = |k: &str| {
            args.as_ref()
                .and_then(|v| v.get(k).and_then(|s| s.as_str()).map(str::to_string))
        };
        let (description, command) = (field("description"), field("command"));
        if description.is_none() && command.is_none() && title.is_none() {
            return;
        }
        let is_new = !self.labels.contains_key(tool_call_id);
        let entry = self.labels.entry(tool_call_id.to_string()).or_default();
        if description.is_some() {
            entry.description = description;
        }
        if command.is_some() {
            entry.command = command;
        }
        if title.is_some() {
            entry.title = title;
        }
        if is_new {
            self.order.push_back(tool_call_id.to_string());
            if self.order.len() > RECENT_TOOL_LABELS {
                if let Some(old) = self.order.pop_front() {
                    self.labels.remove(&old);
                }
            }
        }
    }

    pub fn fill_label(&self, event: &mut Event) {
        if let Event::BackgroundItemStarted {
            tool_call_id: Some(tc),
            label: label @ None,
            kind,
            ..
        } = event
        {
            let Some(parts) = self.labels.get(tc.as_str()) else {
                return;
            };
            *label = match kind {
                BackgroundKind::Shell => parts
                    .command
                    .clone()
                    .or_else(|| parts.description.clone())
                    .or_else(|| parts.title.clone()),
                _ => parts
                    .description
                    .clone()
                    .or_else(|| parts.command.clone())
                    .or_else(|| parts.title.clone()),
            };
        }
    }
}

/// What the sessions API ships for the sidebar and the session panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundSummary {
    pub live: u32,
    /// Newest loss, unless something was started after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lost_since: Option<DateTime<Utc>>,
    pub items: Vec<BackgroundItem>,
}

impl BackgroundSummary {
    /// Only live and lost items are worth a session card (owner decision,
    /// 2026-09-14: "finished stuff should just disappear"). `live` and
    /// `lost_since` are computed over the unfiltered items first, so a
    /// dropped finished item can never change what `lost_since` means: an
    /// agent arming anything after the loss still counts as acknowledgement.
    pub fn from_items(items: Vec<BackgroundItem>) -> Option<Self> {
        if items.is_empty() {
            return None;
        }
        let live = items.iter().filter(|i| i.is_live()).count() as u32;
        let newest_loss = items
            .iter()
            .filter_map(|i| i.ended.as_ref())
            .filter(|e| e.reason == BackgroundEndReason::Lost)
            .map(|e| e.at)
            .max();
        let newest_start = items.iter().map(|i| i.started_at).max();
        let lost_since = newest_loss.filter(|loss| newest_start.is_none_or(|s| s <= *loss));
        let items: Vec<BackgroundItem> = items
            .into_iter()
            .filter(|i| {
                i.is_live()
                    || i.ended
                        .as_ref()
                        .is_some_and(|e| e.reason == BackgroundEndReason::Lost)
            })
            .collect();
        if items.is_empty() {
            return None;
        }
        Some(Self {
            live,
            lost_since,
            items,
        })
    }
}

/// The note that asks the agent to re-arm items lost with its worker.
pub fn loss_note(lost: &[BackgroundItem]) -> String {
    let hm = |at: &DateTime<Utc>| at.format("%H:%M UTC").to_string();
    let first = lost
        .iter()
        .filter_map(|i| i.ended.as_ref())
        .max_by_key(|e| e.at);
    let (when, why) = first
        .map(|e| {
            (
                hm(&e.at),
                e.cause.map(|c| c.describe()).unwrap_or("worker restart"),
            )
        })
        .unwrap_or_default();
    let list = lost
        .iter()
        .map(|i| {
            let kind = serde_json::to_value(i.kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            format!(
                "{kind} \"{}\" (armed {})",
                i.label.as_deref().unwrap_or(&i.id),
                hm(&i.started_at)
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "AoE: your worker was restarted at {when} ({why}). These background items died with it: {list}. Re-arm the ones you still need."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::state::{BackgroundLossCause, ToolCall};
    use chrono::{Duration, TimeZone};

    fn t(min: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 12, 12, 0, 0).unwrap() + Duration::minutes(min)
    }
    fn started(
        kind: BackgroundKind,
        id: &str,
        at: i64,
        expires: Option<i64>,
    ) -> (DateTime<Utc>, Event) {
        (
            t(at),
            Event::BackgroundItemStarted {
                kind,
                id: id.into(),
                tool_call_id: None,
                label: Some(format!("{id} label")),
                started_at: t(at),
                expires_at: expires.map(t),
            },
        )
    }
    fn ended(
        id: &str,
        reason: BackgroundEndReason,
        cause: Option<BackgroundLossCause>,
        at: i64,
    ) -> (DateTime<Utc>, Event) {
        (
            t(at),
            Event::BackgroundItemEnded {
                id: id.into(),
                reason,
                cause,
                at: t(at),
            },
        )
    }

    #[test]
    fn fold_tracks_each_kind_to_its_end() {
        let rows = vec![
            started(BackgroundKind::Monitor, "mon", 0, None),
            started(BackgroundKind::Monitor, "timed", 0, Some(30)),
            started(BackgroundKind::Shell, "sh", 1, None),
            started(BackgroundKind::Workflow, "wf", 2, None),
            (
                t(3),
                Event::WakeupScheduled {
                    at: t(20),
                    reason: Some("loop".into()),
                },
            ),
            (
                t(4),
                Event::WakeupScheduled {
                    at: t(90),
                    reason: None,
                },
            ),
            ended("sh", BackgroundEndReason::Finished, None, 10),
            ended("wf", BackgroundEndReason::Stopped, None, 11),
            ended(
                "wf",
                BackgroundEndReason::Lost,
                Some(BackgroundLossCause::Respawn),
                12,
            ),
        ];
        let items = fold_background(rows, t(60));
        let get = |id: &str| {
            items
                .iter()
                .find(|i| i.id == id)
                .unwrap_or_else(|| panic!("{id} missing: {items:?}"))
        };
        assert!(get("mon").is_live());
        assert_eq!(
            get("timed").ended.as_ref().unwrap().reason,
            BackgroundEndReason::TimedOut
        );
        assert_eq!(get("timed").ended.as_ref().unwrap().at, t(30));
        assert_eq!(
            get("sh").ended.as_ref().unwrap().reason,
            BackgroundEndReason::Finished
        );
        // The first end wins: a later loss cannot rewrite a stop.
        assert_eq!(
            get("wf").ended.as_ref().unwrap().reason,
            BackgroundEndReason::Stopped
        );
        let fired = get(&t(20).to_rfc3339());
        assert_eq!(fired.kind, BackgroundKind::Wakeup);
        assert_eq!(
            fired.ended.as_ref().unwrap().reason,
            BackgroundEndReason::Fired
        );
        assert!(get(&t(90).to_rfc3339()).is_live());
    }

    #[test]
    fn fold_mirrors_sub_agents_and_drops_old_ended_items() {
        let rows = vec![
            launched("a1", 0),
            completed("a1", BackgroundAgentStatus::Completed, 5),
            started(BackgroundKind::Monitor, "old", 0, None),
            ended("old", BackgroundEndReason::Stopped, None, 1),
            started(BackgroundKind::Monitor, "lost", 0, None),
            ended(
                "lost",
                BackgroundEndReason::Lost,
                Some(BackgroundLossCause::NewBuild),
                2,
            ),
        ];
        let items = fold_background(rows.clone(), t(60));
        assert_eq!(
            items
                .iter()
                .find(|i| i.id == "a1")
                .unwrap()
                .ended
                .as_ref()
                .unwrap()
                .reason,
            BackgroundEndReason::Finished
        );
        // "old" ends at t(1), "a1" at t(5); clear both past the 24h retention window.
        let past_a_day = t(5 + 24 * 60 + 1);
        let later = fold_background(rows.clone(), past_a_day);
        assert!(
            later.iter().all(|i| i.id != "old" && i.id != "a1"),
            "{later:?}"
        );
        // "lost" ends at t(2): past the 24h window it still survives, since
        // lost items get 7 days (owner decision, 2026-09-13).
        assert!(
            later.iter().any(|i| i.id == "lost"),
            "a lost item must outlive the 24h window: {later:?}"
        );
        let past_a_week = t(2 + 7 * 24 * 60 + 1);
        let much_later = fold_background(rows, past_a_week);
        assert!(
            much_later.iter().all(|i| i.id != "lost"),
            "a lost item must drop after 7 days: {much_later:?}"
        );
    }

    fn launched(id: &str, at: i64) -> (DateTime<Utc>, Event) {
        (
            t(at),
            Event::BackgroundAgentLaunched {
                agent_id: id.into(),
                tool_call_id: format!("tc-{id}"),
                description: format!("{id} task"),
                prompt: String::new(),
                model: String::new(),
                started_at: t(at),
                output_file: String::new(),
            },
        )
    }
    fn completed(id: &str, status: BackgroundAgentStatus, at: i64) -> (DateTime<Utc>, Event) {
        (
            t(at),
            Event::BackgroundAgentCompleted {
                agent_id: id.into(),
                status,
                tools: Vec::new(),
                result: None,
                warning: None,
                ended_at: t(at),
            },
        )
    }

    /// Upstream's launch creates a sub-agent row; the first end from either
    /// side ends it. A registry `Lost` only lends its cause to the `Detached`
    /// that carries the same `at`.
    #[test]
    fn sub_agents_follow_upstream_background_agent_events() {
        use BackgroundAgentStatus::{Completed, Detached, Error, Stalled};
        use BackgroundEndReason::{Finished, Lost, Stopped};
        use BackgroundLossCause::Respawn;
        let cases = vec![
            ("launched is live", vec![launched("a", 0)], None),
            (
                "completion finishes it",
                vec![launched("a", 0), completed("a", Completed, 5)],
                Some(BackgroundEnd {
                    reason: Finished,
                    cause: None,
                    at: t(5),
                }),
            ),
            (
                "an error completion finishes it too",
                vec![launched("a", 0), completed("a", Error, 5)],
                Some(BackgroundEnd {
                    reason: Finished,
                    cause: None,
                    at: t(5),
                }),
            ),
            (
                "a stalled completion finishes it",
                vec![launched("a", 0), completed("a", Stalled, 5)],
                Some(BackgroundEnd {
                    reason: Finished,
                    cause: None,
                    at: t(5),
                }),
            ),
            (
                "a registry stop ends it",
                vec![launched("a", 0), ended("a", Stopped, None, 4)],
                Some(BackgroundEnd {
                    reason: Stopped,
                    cause: None,
                    at: t(4),
                }),
            ),
            (
                "a registry finish ends it before the tailer does",
                vec![
                    launched("a", 0),
                    ended("a", Finished, None, 4),
                    completed("a", Completed, 9),
                ],
                Some(BackgroundEnd {
                    reason: Finished,
                    cause: None,
                    at: t(4),
                }),
            ),
            (
                "a registry loss alone does not end it",
                vec![launched("a", 0), ended("a", Lost, Some(Respawn), 5)],
                None,
            ),
            (
                "detached with its paired cause is lost to that cause",
                vec![
                    launched("a", 0),
                    ended("a", Lost, Some(Respawn), 5),
                    completed("a", Detached, 5),
                ],
                Some(BackgroundEnd {
                    reason: Lost,
                    cause: Some(Respawn),
                    at: t(5),
                }),
            ),
            (
                "an unpaired detach is lost with no cause",
                vec![
                    launched("a", 0),
                    ended("a", Lost, Some(Respawn), 3),
                    completed("a", Detached, 5),
                ],
                Some(BackgroundEnd {
                    reason: Lost,
                    cause: None,
                    at: t(5),
                }),
            ),
        ];
        for (label, rows, expected) in cases {
            let items = fold_background(rows, t(10));
            assert_eq!(items.len(), 1, "{label}: {items:?}");
            assert_eq!(items[0].kind, BackgroundKind::Subagent, "{label}");
            assert_eq!(items[0].label.as_deref(), Some("a task"), "{label}");
            assert_eq!(items[0].ended, expected, "{label}");
        }
    }

    #[test]
    fn summary_counts_live_and_reports_an_unanswered_loss() {
        let items = fold_background(
            vec![
                started(BackgroundKind::Monitor, "m1", 0, None),
                ended(
                    "m1",
                    BackgroundEndReason::Lost,
                    Some(BackgroundLossCause::NewBuild),
                    10,
                ),
                started(BackgroundKind::Shell, "s1", 5, None),
            ],
            t(20),
        );
        let s = BackgroundSummary::from_items(items).unwrap();
        assert_eq!(s.live, 1);
        assert_eq!(s.lost_since, Some(t(10)));
        // Re-arming after the loss answers it.
        let rearmed = fold_background(
            vec![
                started(BackgroundKind::Monitor, "m1", 0, None),
                ended(
                    "m1",
                    BackgroundEndReason::Lost,
                    Some(BackgroundLossCause::NewBuild),
                    10,
                ),
                started(BackgroundKind::Monitor, "m2", 11, None),
            ],
            t(20),
        );
        assert_eq!(
            BackgroundSummary::from_items(rearmed).unwrap().lost_since,
            None
        );
        assert!(BackgroundSummary::from_items(Vec::new()).is_none());
    }

    #[test]
    fn from_items_lists_only_live_and_lost() {
        let rows = vec![
            started(BackgroundKind::Monitor, "live", 0, None),
            started(BackgroundKind::Monitor, "lost", 1, None),
            ended(
                "lost",
                BackgroundEndReason::Lost,
                Some(BackgroundLossCause::NewBuild),
                5,
            ),
            started(BackgroundKind::Shell, "finished", 2, None),
            ended("finished", BackgroundEndReason::Finished, None, 6),
            started(BackgroundKind::Workflow, "stopped", 3, None),
            ended("stopped", BackgroundEndReason::Stopped, None, 7),
            started(BackgroundKind::Monitor, "timed_out", 4, None),
            ended("timed_out", BackgroundEndReason::TimedOut, None, 8),
            // Starts after the loss, so it is what drives newest_start.
            started(BackgroundKind::Wakeup, "fired", 9, None),
            ended("fired", BackgroundEndReason::Fired, None, 10),
        ];
        let items = fold_background(rows, t(20));
        let summary = BackgroundSummary::from_items(items).unwrap();
        let ids: Vec<&str> = summary.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["live", "lost"], "{ids:?}");
        assert_eq!(summary.live, 1);
        // "fired" is dropped from the list, but its start (after the loss)
        // must still count toward newest_start: filtering the display list
        // must not turn an acknowledged loss back into an unacknowledged one.
        assert_eq!(
            summary.lost_since, None,
            "an item armed after the loss still acknowledges it, even when dropped from the list"
        );
    }

    #[test]
    fn recent_labels_fill_a_started_event_from_the_tool_call() {
        let mut labels = RecentToolLabels::default();
        labels.observe(&Event::ToolCallUpdated {
            tool_call_id: "tc1".into(),
            title: Some("Monitor".into()),
            args_preview: Some(r#"{"description":"adam run","command":"tail -F x"}"#.into()),
            started_at: None,
            diffs: None,
        });
        let mut event = Event::BackgroundItemStarted {
            kind: BackgroundKind::Monitor,
            id: "m".into(),
            tool_call_id: Some("tc1".into()),
            label: None,
            started_at: t(0),
            expires_at: None,
        };
        labels.fill_label(&mut event);
        let Event::BackgroundItemStarted { label, .. } = event else {
            unreachable!()
        };
        assert_eq!(label.as_deref(), Some("adam run"));
    }

    #[test]
    fn shell_items_prefer_the_command_over_the_description() {
        let mut labels = RecentToolLabels::default();
        labels.observe(&Event::ToolCallUpdated {
            tool_call_id: "tc2".into(),
            title: Some("Bash".into()),
            args_preview: Some(
                r#"{"description":"run the tests","command":"cargo test --lib"}"#.into(),
            ),
            started_at: None,
            diffs: None,
        });
        let mut event = Event::BackgroundItemStarted {
            kind: BackgroundKind::Shell,
            id: "sh".into(),
            tool_call_id: Some("tc2".into()),
            label: None,
            started_at: t(0),
            expires_at: None,
        };
        labels.fill_label(&mut event);
        let Event::BackgroundItemStarted { label, .. } = event else {
            unreachable!()
        };
        assert_eq!(label.as_deref(), Some("cargo test --lib"));
    }

    fn tool_call(id: &str, name: &str, args_preview: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            kind: "execute".into(),
            args_preview: args_preview.into(),
            started_at: t(0),
            parent_tool_call_id: None,
            memory_recall: None,
            diffs: Vec::new(),
        }
    }

    #[test]
    fn shell_label_from_tool_call_started_with_parseable_args() {
        // A shell tool call that never got a ToolCallUpdated (parallel or
        // long-running calls sometimes only carry the command in the
        // initial ToolCallStarted frame).
        let mut labels = RecentToolLabels::default();
        labels.observe(&Event::ToolCallStarted {
            tool_call: tool_call("tc3", "Bash", r#"{"command":"cargo test --lib -j 6"}"#),
        });
        let mut event = Event::BackgroundItemStarted {
            kind: BackgroundKind::Shell,
            id: "sh3".into(),
            tool_call_id: Some("tc3".into()),
            label: None,
            started_at: t(0),
            expires_at: None,
        };
        labels.fill_label(&mut event);
        let Event::BackgroundItemStarted { label, .. } = event else {
            unreachable!()
        };
        assert_eq!(label.as_deref(), Some("cargo test --lib -j 6"));
    }

    #[test]
    fn shell_label_falls_back_to_the_title_when_args_preview_is_truncated() {
        // args_preview is capped at ingest, so a long command's JSON can be
        // cut mid-string and fail to parse; the title (the command itself,
        // for a Bash call) is the only usable copy left.
        let mut labels = RecentToolLabels::default();
        labels.observe(&Event::ToolCallStarted {
            tool_call: tool_call(
                "tc4",
                "cargo test --lib -j 6 -- background::tests",
                r#"{"command":"cargo test --lib -j 6 -- bac"#,
            ),
        });
        let mut event = Event::BackgroundItemStarted {
            kind: BackgroundKind::Shell,
            id: "sh4".into(),
            tool_call_id: Some("tc4".into()),
            label: None,
            started_at: t(0),
            expires_at: None,
        };
        labels.fill_label(&mut event);
        let Event::BackgroundItemStarted { label, .. } = event else {
            unreachable!()
        };
        assert_eq!(
            label.as_deref(),
            Some("cargo test --lib -j 6 -- background::tests")
        );
    }

    #[test]
    fn loss_note_lists_every_lost_item_with_its_cause() {
        let items = fold_background(
            vec![
                started(BackgroundKind::Monitor, "m1", 0, None),
                started(BackgroundKind::Shell, "s1", 1, None),
                ended(
                    "m1",
                    BackgroundEndReason::Lost,
                    Some(BackgroundLossCause::NewBuild),
                    10,
                ),
                ended(
                    "s1",
                    BackgroundEndReason::Lost,
                    Some(BackgroundLossCause::NewBuild),
                    10,
                ),
            ],
            t(11),
        );
        let note = loss_note(&items);
        assert!(
            note.starts_with(
                "AoE: your worker was restarted at 12:10 UTC (restart onto a new build)."
            ),
            "{note}"
        );
        assert!(
            note.contains("monitor \"m1 label\" (armed 12:00 UTC)")
                && note.contains("shell \"s1 label\" (armed 12:01 UTC)"),
            "{note}"
        );
        assert!(note.ends_with("Re-arm the ones you still need."), "{note}");
    }
}

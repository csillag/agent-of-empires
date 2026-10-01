//! Ends background items from the `<task-notification>` lines Claude Code
//! writes into the session's own transcript. Background shells almost never
//! end over ACP, because agents rarely poll or stop them.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::mpsc::Sender;
use tokio::task::JoinHandle;

use crate::acp::agent_profiles::AgentProfile;
use crate::acp::background_agent::TranscriptSource;
use crate::acp::state::{BackgroundEndReason, Event};

const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// On start, ends older than this are history rather than ends missed while
/// the daemon was down.
const CATCH_UP_MINUTES: i64 = 10;
/// Bounds the catch-up read; the busiest transcript on this host grew 7.2 MiB
/// in 10 minutes.
const CATCH_UP_BYTES: u64 = 8 * 1024 * 1024;

/// Follows the current ACP session's transcript. Dropping it, or following
/// another id, aborts the running tailer.
pub(crate) struct TaskNotificationFollower {
    /// `<claude home>/projects/<encoded cwd>`, or `None` when out of scope.
    project_dir: Option<PathBuf>,
    event_tx: Sender<Event>,
    tailer: Option<JoinHandle<()>>,
}

impl TaskNotificationFollower {
    /// Only Claude sessions on the host are followed: a sandboxed session's
    /// transcript is inside its container.
    pub(crate) fn new(
        profile: &AgentProfile,
        source: &TranscriptSource,
        cwd: &Path,
        event_tx: Sender<Event>,
    ) -> Self {
        let in_scope = profile.supports_wakeup_tools && matches!(source, TranscriptSource::Host);
        Self {
            project_dir: in_scope.then(|| claude_project_dir(cwd)).flatten(),
            event_tx,
            tailer: None,
        }
    }

    pub(crate) fn follow(&mut self, acp_session_id: &str) {
        if let Some(old) = self.tailer.take() {
            old.abort();
        }
        let Some(dir) = &self.project_dir else {
            return;
        };
        if !crate::session::capture::is_valid_session_id(acp_session_id) {
            return;
        }
        let path = dir.join(format!("{acp_session_id}.jsonl"));
        self.tailer = Some(tokio::spawn(run_tailer(
            path,
            self.event_tx.clone(),
            POLL_INTERVAL,
            CATCH_UP_BYTES,
        )));
    }
}

impl Drop for TaskNotificationFollower {
    fn drop(&mut self) {
        if let Some(tailer) = self.tailer.take() {
            tailer.abort();
        }
    }
}

// A profile-scoped `CLAUDE_CONFIG_DIR` (#3399) is not seen here.
fn claude_project_dir(cwd: &Path) -> Option<PathBuf> {
    use crate::session::capture::{
        canonicalize_or_raw, claude_home_for_host_environment, encode_claude_project_path,
    };
    let home = claude_home_for_host_environment(&[]).ok()?;
    let cwd = canonicalize_or_raw(&cwd.to_string_lossy());
    Some(
        home.join("projects")
            .join(encode_claude_project_path(&cwd.to_string_lossy())),
    )
}

/// The end a transcript line reports, if it is a task notification's
/// `queue-operation` enqueue with a terminal status. Its later `remove` and
/// the repeated `user` line carry the same text and are ignored.
pub(crate) fn task_notification_end(line: &str) -> Option<Event> {
    if !line.contains("<task-notification>") {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v.get("type")?.as_str()? != "queue-operation" || v.get("operation")?.as_str()? != "enqueue" {
        return None;
    }
    let body = v
        .get("content")?
        .as_str()?
        .trim_start()
        .strip_prefix("<task-notification>")?;
    // The summary quotes monitor output, which may contain any tag.
    let header = body.split("<summary>").next()?;
    let reason = match tag(header, "status")? {
        "completed" | "failed" => BackgroundEndReason::Finished,
        "killed" => BackgroundEndReason::Stopped,
        _ => return None,
    };
    let id = tag(header, "task-id").filter(|id| !id.is_empty())?;
    let at = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map_or_else(Utc::now, |t| t.with_timezone(&Utc));
    Some(Event::BackgroundItemEnded {
        id: id.to_string(),
        reason,
        cause: None,
        at,
    })
}

fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = text.find(&open)? + open.len();
    let len = text[start..].find(&format!("</{name}>"))?;
    Some(text[start..start + len].trim())
}

async fn run_tailer(path: PathBuf, event_tx: Sender<Event>, poll: Duration, catch_up_bytes: u64) {
    let cutoff = Utc::now() - chrono::Duration::minutes(CATCH_UP_MINUTES);
    // A file that does not exist yet will be small, so it is read whole.
    let mut offset = tokio::fs::metadata(&path)
        .await
        .map_or(0, |meta| meta.len().saturating_sub(catch_up_bytes));
    // Starting mid-file may land inside a line; drop it.
    let mut cut_line = offset > 0;
    let path = path.to_string_lossy().into_owned();
    // Bytes after the last newline: a line Claude is still writing.
    let mut partial: Vec<u8> = Vec::new();
    loop {
        // A missing file reads as empty; Claude creates it on first content.
        let chunk = TranscriptSource::Host.read_from(&path, offset).await;
        offset += chunk.len() as u64;
        partial.extend_from_slice(&chunk);
        if cut_line {
            match partial.iter().position(|&b| b == b'\n') {
                Some(nl) => {
                    partial.drain(..=nl);
                    cut_line = false;
                }
                None => partial.clear(),
            }
        }
        if let Some(last_nl) = partial.iter().rposition(|&b| b == b'\n') {
            let rest = partial.split_off(last_nl + 1);
            for line in partial.split(|&b| b == b'\n') {
                let Some(event) = std::str::from_utf8(line)
                    .ok()
                    .and_then(task_notification_end)
                else {
                    continue;
                };
                if matches!(&event, Event::BackgroundItemEnded { at, .. } if *at < cutoff) {
                    continue;
                }
                if event_tx.send(event).await.is_err() {
                    return;
                }
            }
            partial = rest;
        }
        tokio::select! {
            _ = tokio::time::sleep(poll) => {}
            _ = event_tx.closed() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::SecondsFormat;

    fn notification(id: &str, status: Option<&str>, summary: &str) -> String {
        let status = status
            .map(|s| format!("<status>{s}</status>\n"))
            .unwrap_or_default();
        format!(
            "<task-notification>\n<task-id>{id}</task-id>\n<tool-use-id>toolu_x</tool-use-id>\n\
             <output-file>/tmp/{id}.output</output-file>\n{status}<summary>{summary}</summary>\n\
             </task-notification>"
        )
    }

    fn queue_line(operation: &str, timestamp: &str, content: &str) -> String {
        serde_json::json!({
            "type": "queue-operation",
            "operation": operation,
            "timestamp": timestamp,
            "sessionId": "s",
            "content": content,
        })
        .to_string()
    }

    fn rfc3339(at: DateTime<Utc>) -> String {
        at.to_rfc3339_opts(SecondsFormat::Millis, true)
    }

    fn end_of(event: Event) -> (String, BackgroundEndReason, DateTime<Utc>) {
        match event {
            Event::BackgroundItemEnded {
                id,
                reason,
                cause: None,
                at,
            } => (id, reason, at),
            other => panic!("expected BackgroundItemEnded without cause, got {other:?}"),
        }
    }

    async fn recv(
        rx: &mut tokio::sync::mpsc::Receiver<Event>,
    ) -> (String, BackgroundEndReason, DateTime<Utc>) {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("tailer emitted nothing")
            .map(end_of)
            .expect("channel closed")
    }

    type MapCase<'a> = (&'a str, String, Option<(&'a str, BackgroundEndReason)>);

    #[test]
    fn task_notification_end_maps_transcript_lines() {
        let ts = "2026-09-13T01:25:43.630Z";
        let at: DateTime<Utc> = ts.parse().unwrap();
        let done = notification("bu4vijwle", Some("completed"), "Background command done");
        let user_line = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": done},
        })
        .to_string();
        let cases: Vec<MapCase<'_>> = vec![
            (
                "completed",
                queue_line("enqueue", ts, &done),
                Some(("bu4vijwle", BackgroundEndReason::Finished)),
            ),
            (
                "failed",
                queue_line("enqueue", ts, &notification("b1", Some("failed"), "exit 1")),
                Some(("b1", BackgroundEndReason::Finished)),
            ),
            (
                "killed",
                queue_line("enqueue", ts, &notification("b2", Some("killed"), "killed")),
                Some(("b2", BackgroundEndReason::Stopped)),
            ),
            (
                "stopped: worker-death marking owns it",
                queue_line("enqueue", ts, &notification("b3", Some("stopped"), "x")),
                None,
            ),
            (
                "monitor firing without status, even quoting a status",
                queue_line(
                    "enqueue",
                    ts,
                    &notification("m1", None, "Monitor event: <status>completed</status>"),
                ),
                None,
            ),
            ("duplicate user line", user_line, None),
            (
                "remove of an enqueued notification",
                queue_line("remove", ts, &done),
                None,
            ),
            (
                "malformed",
                "{\"type\":\"queue-operation\",<task-notification>".into(),
                None,
            ),
            (
                "non-notification queue-operation",
                queue_line("enqueue", ts, "please also check the logs"),
                None,
            ),
        ];
        for (desc, line, expected) in cases {
            let got = task_notification_end(&line).map(end_of);
            match (got, expected) {
                (None, None) => {}
                (Some((id, reason, got_at)), Some((want_id, want_reason))) => {
                    assert_eq!(id, want_id, "{desc}");
                    assert_eq!(reason, want_reason, "{desc}");
                    assert_eq!(got_at, at, "{desc}");
                }
                (got, want) => panic!("{desc}: got {got:?}, want {want:?}"),
            }
        }
    }

    #[tokio::test]
    async fn tailer_emits_recent_and_appended_ends_and_stops_with_the_receiver() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let tailer = tokio::spawn(run_tailer(
            path.clone(),
            tx,
            Duration::from_millis(20),
            CATCH_UP_BYTES,
        ));

        // The file appears only after the tailer started polling.
        tokio::time::sleep(Duration::from_millis(60)).await;
        let now = Utc::now();
        let old = queue_line(
            "enqueue",
            &rfc3339(now - chrono::Duration::minutes(11)),
            &notification("old", Some("completed"), "x"),
        );
        let recent = queue_line(
            "enqueue",
            &rfc3339(now - chrono::Duration::minutes(1)),
            &notification("recent", Some("killed"), "x"),
        );
        std::fs::write(&path, format!("{old}\n{recent}\n")).unwrap();
        let (id, reason, _) = recv(&mut rx).await;
        assert_eq!(
            (id.as_str(), reason),
            ("recent", BackgroundEndReason::Stopped)
        );

        let late = queue_line(
            "enqueue",
            &rfc3339(Utc::now()),
            &notification("late", Some("completed"), "x"),
        );
        let (head, tail) = late.split_at(late.len() / 2);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(head.as_bytes()).unwrap();
        file.flush().unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(rx.try_recv().is_err(), "a partial line must wait");
        writeln!(file, "{tail}").unwrap();
        let (id, reason, _) = recv(&mut rx).await;
        assert_eq!(
            (id.as_str(), reason),
            ("late", BackgroundEndReason::Finished)
        );

        drop(rx);
        tokio::time::timeout(Duration::from_secs(5), tailer)
            .await
            .expect("tailer outlived its receiver")
            .unwrap();
    }

    #[tokio::test]
    async fn tailer_catch_up_reads_only_the_tail_of_a_large_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let recent_ts = rfc3339(Utc::now() - chrono::Duration::minutes(1));
        // Inside the time window but before the byte window, so never read.
        let early = queue_line(
            "enqueue",
            &recent_ts,
            &notification("early", Some("completed"), "x"),
        );
        let filler: String = (0..200)
            .map(|i| {
                format!(
                    "{{\"type\":\"assistant\",\"n\":{i},\"pad\":\"{}\"}}\n",
                    "x".repeat(60)
                )
            })
            .collect();
        let recent = queue_line(
            "enqueue",
            &recent_ts,
            &notification("recent", Some("killed"), "x"),
        );
        let head = format!("{early}\n{filler}");
        let last_filler_start = head[..head.len() - 1].rfind('\n').unwrap() + 1;
        let content = format!("{head}{recent}\n");
        std::fs::write(&path, &content).unwrap();
        // The window starts 5 bytes into the last filler line, cutting it.
        let window = (content.len() - (last_filler_start + 5)) as u64;

        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let tailer = tokio::spawn(run_tailer(path, tx, Duration::from_millis(20), window));
        let (id, reason, _) = recv(&mut rx).await;
        assert_eq!(
            (id.as_str(), reason),
            ("recent", BackgroundEndReason::Stopped),
            "the early end precedes the window and must not be read"
        );

        drop(rx);
        tokio::time::timeout(Duration::from_secs(5), tailer)
            .await
            .expect("tailer outlived its receiver")
            .expect("tailer panicked");
    }
}

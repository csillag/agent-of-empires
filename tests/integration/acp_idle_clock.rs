//! The idle reaper's live activity clock must move on every inbound
//! notification, including one the post-load replay suppression drops before
//! it reaches the event store. Without that, a worker whose agent works on a
//! turn it started itself after a respawn looks idle to the reaper and is
//! stopped mid-work.

use std::time::{Duration, Instant};

use agent_of_empires::acp::acp_client::{AcpClient, SpawnConfig};
use agent_of_empires::acp::agent_registry::AgentSpec;
use agent_of_empires::acp::state::AcpSessionId;

use crate::common::{shim_path, shim_ready};

#[tokio::test]
async fn a_post_load_notification_moves_the_live_clock() {
    if let Err(reason) = shim_ready() {
        eprintln!("skipping: {reason}");
        return;
    }
    let stored = "idle-clock-session";
    let config = SpawnConfig {
        wrapper_substitution: None,
        agent_key: "claude".into(),
        tool: "claude".into(),
        spec: AgentSpec {
            command: crate::common::shim_node()
                .expect("shim prerequisite")
                .to_string_lossy()
                .into_owned(),
            args: vec![shim_path().to_string_lossy().to_string()],
            description: "idle-clock shim".into(),
            env_allowlist: None,
        },
        cwd: std::env::temp_dir(),
        additional_dirs: vec![],
        // Resume via session/load (so replay suppression is armed), then emit
        // one unsolicited chunk after the handshake with no prompt issued.
        provider_env: vec![
            ("SHIM_LOAD_SESSION".into(), "1".into()),
            ("SHIM_PRESEED_SESSION_ID".into(), stored.into()),
            ("SHIM_EMIT_UNSOLICITED_NOTIF".into(), "1500".into()),
        ],
        host_environment: vec![],
        default_effort: None,
        default_effort_explicit: false,
        default_mode: None,
        socket_path: None,
        stored_acp_session_id: Some(stored.into()),
        fork_from: None,
        seed_history_replay: false,
        generation: 0,
        artifact_dir: None,
        sandbox_info: None,
        source_profile: None,
        mcp_servers: Vec::new(),
    };
    let client = AcpClient::spawn(config, AcpSessionId("idle-clock".into()))
        .await
        .expect("spawn shim");
    let deadline = Instant::now() + Duration::from_secs(6);
    while client.last_notification_ms().is_none() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let seen = client.last_notification_ms();
    let _ = client.shutdown().await;
    assert!(
        seen.is_some(),
        "the unsolicited post-load chunk must move the live notification clock"
    );
}

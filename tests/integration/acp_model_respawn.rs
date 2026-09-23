//! A session's pinned model must be applied after every handshake, not only on
//! a fresh `session/new`.
//!
//! `Instance.agent_model` travelled to the agent only as the `AOE_AGENT_MODEL`
//! environment variable, which no adapter but aoe's own reads. A respawn
//! resumes through `session/load`, where claude-agent-acp restores the model
//! recorded in the resumed transcript, so a pick silently reverted on every
//! restart. These tests drive the real `AcpClient` against the test shim and
//! assert the `session/set_config_option` RPC actually fired with the pinned
//! value on the load path as well as the fresh path.

use std::path::PathBuf;
use std::time::Duration;

use agent_of_empires::acp::acp_client::{AcpClient, SpawnConfig};
use agent_of_empires::acp::agent_registry::AgentSpec;
use agent_of_empires::acp::state::{AcpSessionId, Event};

use crate::common::{shim_path, shim_ready};

fn spawn_config(
    shim: PathBuf,
    env: Vec<(String, String)>,
    stored_acp_session_id: Option<String>,
    default_model: Option<String>,
) -> SpawnConfig {
    SpawnConfig {
        generation: 0,
        wrapper_substitution: None,
        agent_key: "claude".into(),
        tool: "claude".into(),
        spec: AgentSpec {
            command: crate::common::shim_node()
                .expect("shim prerequisite")
                .to_string_lossy()
                .into_owned(),
            args: vec![shim.to_string_lossy().to_string()],
            description: "model shim".into(),
            env_allowlist: None,
        },
        cwd: std::env::temp_dir(),
        additional_dirs: vec![],
        read_only_dirs: vec![],
        provider_env: env,
        host_environment: vec![],
        default_effort: None,
        default_effort_explicit: false,
        default_model,
        default_mode: None,
        socket_path: None,
        stored_acp_session_id,
        fork_from: None,
        seed_history_replay: false,
        artifact_dir: None,
        sandbox_info: None,
        source_profile: None,
        mcp_servers: Vec::new(),
    }
}

/// A prompt is only dispatched after the handshake completes, so a `Stopped`
/// proves any post-handshake config-option work already ran.
async fn drive_one_turn(client: &mut AcpClient) {
    client
        .send_prompt("hello", &[])
        .await
        .expect("send_prompt should reach the shim");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), client.next_event()).await {
            Ok(Some(Event::Stopped { .. })) => break,
            Ok(_) | Err(_) => continue,
        }
    }
}

fn shim_env(record_path: &std::path::Path, load_session: bool) -> Vec<(String, String)> {
    let mut env = vec![
        ("SHIM_MODEL".into(), "1".into()),
        (
            "SHIM_CONFIG_OPTION_RECORD_FILE".into(),
            record_path.to_string_lossy().to_string(),
        ),
    ];
    if load_session {
        env.push(("SHIM_LOAD_SESSION".into(), "1".into()));
    }
    env
}

/// The respawn shape, and the regression this fixes: the agent advertises
/// `loadSession` and we hand it a stored id, so the handshake resumes via
/// `session/load`. The pinned model must still be applied, or the session comes
/// back on whatever model its transcript happens to carry.
///
/// The shim names its option `the-model-picker`, so this also proves the client
/// resolves the option by category rather than by a hardcoded `"model"` id.
#[tokio::test]
async fn pinned_model_applied_on_session_load() {
    if let Err(reason) = shim_ready() {
        eprintln!("skipping: {reason}");
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let record_path = temp.path().join("config-option-calls.log");
    let config = spawn_config(
        shim_path(),
        shim_env(&record_path, true),
        Some("stored-model-session".into()),
        Some("shim-pinned-model".into()),
    );

    let mut client = AcpClient::spawn(config, AcpSessionId("model-load".into()))
        .await
        .expect("spawn shim");
    drive_one_turn(&mut client).await;
    let _ = client.shutdown().await;

    let recorded = std::fs::read_to_string(&record_path).unwrap_or_default();
    assert!(
        recorded
            .lines()
            .any(|line| line == "the-model-picker=shim-pinned-model"),
        "a session/load respawn must re-apply the pinned model (recorded: {recorded:?})"
    );
}

/// The fresh-session path applies the model exactly once.
#[tokio::test]
async fn pinned_model_applied_once_on_session_new() {
    if let Err(reason) = shim_ready() {
        eprintln!("skipping: {reason}");
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let record_path = temp.path().join("config-option-calls.log");
    let config = spawn_config(
        shim_path(),
        shim_env(&record_path, false),
        None,
        Some("shim-pinned-model".into()),
    );

    let mut client = AcpClient::spawn(config, AcpSessionId("model-new".into()))
        .await
        .expect("spawn shim");
    drive_one_turn(&mut client).await;
    let _ = client.shutdown().await;

    let recorded = std::fs::read_to_string(&record_path).unwrap_or_default();
    assert_eq!(
        recorded
            .lines()
            .filter(|line| *line == "the-model-picker=shim-pinned-model")
            .count(),
        1,
        "session/new must apply the model exactly once (recorded: {recorded:?})"
    );
}

/// No pin means no RPC: the session keeps whatever model the agent chose for
/// itself. This is the guard on the blast radius, because the daemon only
/// passes a model down when the pick has not reached an agent yet.
#[tokio::test]
async fn no_config_option_rpc_without_a_pinned_model() {
    if let Err(reason) = shim_ready() {
        eprintln!("skipping: {reason}");
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let record_path = temp.path().join("config-option-calls.log");
    let config = spawn_config(shim_path(), shim_env(&record_path, true), None, None);

    let mut client = AcpClient::spawn(config, AcpSessionId("model-none".into()))
        .await
        .expect("spawn shim");
    drive_one_turn(&mut client).await;
    let _ = client.shutdown().await;

    let recorded = std::fs::read_to_string(&record_path).unwrap_or_default();
    assert!(
        recorded.trim().is_empty(),
        "no model pinned means no set_config_option call (recorded: {recorded:?})"
    );
}

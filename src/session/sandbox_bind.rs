//! Bind mounts that confine a sandboxed agent without trusting the agent to
//! confine itself.
//!
//! An absolute host command is mounted read-only at the same path, including
//! the program a small wrapper script `exec`s. The session directory list is
//! mounted at the same paths, read-only when the entry says so. Grok's own
//! config is copied into a private directory and mounted at the container
//! home, so the container does not receive the host `~/.grok` tree.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::containers::{ContainerConfig, EnvEntry, VolumeMount};
use crate::session::session_dirs::{DirAccess, SessionDir};

/// Set in every AoE sandbox container. A host wrapper uses it to skip the
/// agent's own sandbox flag: this process is already confined.
pub(crate) const CONTAINER_SANDBOX_ENV: &str = "AOE_CONTAINER_SANDBOX";

const SCRIPT_READ_LIMIT: u64 = 64 * 1024;
const GROK_SEED_FILES: &[&str] = &["auth.json", "config.toml", "models_cache.json"];

/// Files the container must have before it can exec `command`. A relative
/// command is left to the image. A wrapper script contributes the absolute
/// `exec` targets it names.
pub(crate) fn host_agent_files(command: &str) -> Vec<PathBuf> {
    let Ok(argv) = shell_words::split(command) else {
        return Vec::new();
    };
    let Some(program) = argv.first() else {
        return Vec::new();
    };
    let path = PathBuf::from(program);
    if !path.is_absolute() || !path.is_file() {
        return Vec::new();
    }
    let mut files = vec![path.clone()];
    for target in script_exec_targets(&path) {
        if target != path && !files.contains(&target) {
            files.push(target);
        }
    }
    files
}

fn script_exec_targets(path: &Path) -> Vec<PathBuf> {
    let Ok(meta) = fs::metadata(path) else {
        return Vec::new();
    };
    if !meta.is_file() || meta.len() > SCRIPT_READ_LIMIT {
        return Vec::new();
    }
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    if bytes.contains(&0) {
        return Vec::new();
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return Vec::new();
    };
    let mut targets = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(after) = line.strip_prefix("exec ") else {
            continue;
        };
        let Some(token) = after.split_whitespace().next() else {
            continue;
        };
        let token = token.trim_matches('"');
        if !token.starts_with('/') {
            continue;
        }
        let target = PathBuf::from(token);
        if target.is_file() && !targets.contains(&target) {
            targets.push(target);
        }
    }
    targets
}

pub(crate) fn staged_grok_dir(instance_id: &str) -> Result<PathBuf> {
    crate::session::validate_instance_id(instance_id)?;
    Ok(crate::session::get_app_dir()?
        .join("sandbox-agent-home")
        .join(instance_id)
        .join("grok"))
}

pub(crate) fn remove_staged_agent_home(instance_id: &str) {
    let Ok(()) = crate::session::validate_instance_id(instance_id) else {
        return;
    };
    let Ok(app) = crate::session::get_app_dir() else {
        return;
    };
    let dir = app.join("sandbox-agent-home").join(instance_id);
    if dir.exists() {
        let _ = fs::remove_dir_all(dir);
    }
}

/// Copy the few host files grok needs in order to start. Later files grok
/// writes stay in `dest`; the seed files are refreshed from the host.
pub(crate) fn stage_grok_config(host_grok: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dest, fs::Permissions::from_mode(0o700));
    }
    if !host_grok.is_dir() {
        tracing::warn!(
            target: "session.profile",
            path = %host_grok.display(),
            "no host grok config to seed into the sandbox"
        );
        return Ok(());
    }
    let canon_root = host_grok
        .canonicalize()
        .unwrap_or_else(|_| host_grok.to_path_buf());
    for name in GROK_SEED_FILES {
        let src = host_grok.join(name);
        if !src.is_file() {
            continue;
        }
        match src.canonicalize() {
            Ok(canon) if canon.starts_with(&canon_root) => {
                fs::copy(&canon, dest.join(name))
                    .with_context(|| format!("copying {} into the sandbox", canon.display()))?;
            }
            _ => {
                tracing::warn!(
                    target: "session.profile",
                    path = %src.display(),
                    "skipping grok seed file outside the host config directory"
                );
            }
        }
    }
    Ok(())
}

fn push_mount(volumes: &mut Vec<VolumeMount>, mount: VolumeMount) {
    if volumes
        .iter()
        .any(|existing| existing.container_path == mount.container_path)
    {
        return;
    }
    volumes.push(mount);
}

fn container_home(config: &ContainerConfig) -> String {
    config
        .environment
        .iter()
        .find(|entry| entry.key() == "HOME")
        .map(|entry| entry.value().trim_end_matches('/').to_string())
        .filter(|home| home.starts_with('/'))
        .unwrap_or_else(|| "/root".to_string())
}

/// Mount the host executable and the session directories, and mark the
/// container so a wrapper does not turn on the agent's own sandbox.
pub(crate) fn apply_confinement(
    config: &mut ContainerConfig,
    command: &str,
    session_dirs: &[SessionDir],
    staged_grok: Option<&Path>,
) {
    let files = host_agent_files(command);
    if !files.is_empty() {
        tracing::info!(
            target: "session.profile",
            command,
            mounts = files.len(),
            "bind-mounting the host agent executable into the sandbox"
        );
    }
    for path in files {
        let text = path.to_string_lossy().into_owned();
        push_mount(
            &mut config.volumes,
            VolumeMount {
                host_path: text.clone(),
                container_path: text,
                read_only: true,
            },
        );
    }
    for dir in crate::session::session_dirs::usable(session_dirs) {
        if !Path::new(&dir.path).is_dir() {
            tracing::warn!(
                target: "session.profile",
                path = %dir.path,
                "session directory is not on disk; not mounted into the sandbox"
            );
            continue;
        }
        push_mount(
            &mut config.volumes,
            VolumeMount {
                host_path: dir.path.clone(),
                container_path: dir.path.clone(),
                read_only: dir.access == DirAccess::ReadOnly,
            },
        );
    }
    if let Some(staged) = staged_grok {
        let container_path = format!("{}/.grok", container_home(config));
        push_mount(
            &mut config.volumes,
            VolumeMount {
                host_path: staged.to_string_lossy().into_owned(),
                container_path,
                read_only: false,
            },
        );
    }
    if !config
        .environment
        .iter()
        .any(|entry| entry.key() == CONTAINER_SANDBOX_ENV)
    {
        config.environment.push(EnvEntry::Literal {
            key: CONTAINER_SANDBOX_ENV.to_string(),
            value: "1".to_string(),
        });
    }
}

pub(crate) fn confine_host_agent(
    config: &mut ContainerConfig,
    tool: &str,
    command: &str,
    session_dirs: &[SessionDir],
    instance_id: &str,
) -> Result<()> {
    let wants_grok = tool == "grok"
        || host_agent_files(command).iter().any(|path| {
            matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("grok" | "grok-aoe")
            )
        });
    let staged = if wants_grok {
        let dest = staged_grok_dir(instance_id)?;
        let host_grok = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/"))
            .join(".grok");
        stage_grok_config(&host_grok, &dest)?;
        Some(dest)
    } else {
        None
    };
    apply_confinement(config, command, session_dirs, staged.as_deref());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn absolute_wrapper_mounts_its_exec_target() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("grok");
        fs::write(&bin, "elf").unwrap();
        let wrapper = write_script(
            dir.path(),
            "grok-aoe",
            &format!("#!/bin/sh\nif [ -n \"$AOE_CONTAINER_SANDBOX\" ]; then\n    exec {} \"$@\"\nfi\nexec {} --sandbox aoe \"$@\"\n", bin.display(), bin.display()),
        );
        let files = host_agent_files(&wrapper.to_string_lossy());
        assert_eq!(files, vec![wrapper, bin]);
    }

    #[test]
    fn relative_command_is_left_to_the_image() {
        assert!(host_agent_files("claude").is_empty());
        assert!(host_agent_files("/no/such/agent").is_empty());
    }

    #[test]
    fn session_directories_keep_their_access() {
        let dir = tempfile::tempdir().unwrap();
        let rw = dir.path().join("rw");
        let ro = dir.path().join("ro");
        fs::create_dir(&rw).unwrap();
        fs::create_dir(&ro).unwrap();
        let mut config = ContainerConfig::default();
        apply_confinement(
            &mut config,
            "claude",
            &[
                SessionDir {
                    path: rw.to_string_lossy().into_owned(),
                    access: DirAccess::ReadWrite,
                },
                SessionDir {
                    path: ro.to_string_lossy().into_owned(),
                    access: DirAccess::ReadOnly,
                },
            ],
            None,
        );
        let mounted = |path: &Path| {
            config
                .volumes
                .iter()
                .find(|volume| volume.container_path == path.to_string_lossy())
        };
        assert_eq!(mounted(&rw).unwrap().read_only, false);
        assert_eq!(mounted(&ro).unwrap().read_only, true);
        assert!(config
            .environment
            .iter()
            .any(|entry| entry.key() == CONTAINER_SANDBOX_ENV && entry.value() == "1"));
    }

    #[test]
    fn an_existing_mount_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        let mut config = ContainerConfig {
            volumes: vec![VolumeMount {
                host_path: path.clone(),
                container_path: path.clone(),
                read_only: true,
            }],
            ..ContainerConfig::default()
        };
        apply_confinement(
            &mut config,
            "claude",
            &[SessionDir {
                path: path.clone(),
                access: DirAccess::ReadWrite,
            }],
            None,
        );
        assert_eq!(
            config
                .volumes
                .iter()
                .filter(|volume| volume.container_path == path)
                .count(),
            1
        );
        assert!(config.volumes[0].read_only);
    }

    #[test]
    fn grok_seed_copies_only_named_files() {
        let dir = tempfile::tempdir().unwrap();
        let host = dir.path().join("host");
        let dest = dir.path().join("dest");
        fs::create_dir(&host).unwrap();
        fs::write(host.join("auth.json"), "token").unwrap();
        fs::write(host.join("sessions.json"), "history").unwrap();
        stage_grok_config(&host, &dest).unwrap();
        assert_eq!(fs::read_to_string(dest.join("auth.json")).unwrap(), "token");
        assert!(!dest.join("sessions.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn grok_seed_skips_a_symlink_that_leaves_the_config_dir() {
        let dir = tempfile::tempdir().unwrap();
        let host = dir.path().join("host");
        let outside = dir.path().join("outside");
        let dest = dir.path().join("dest");
        fs::create_dir(&host).unwrap();
        fs::write(&outside, "secret").unwrap();
        std::os::unix::fs::symlink(&outside, host.join("auth.json")).unwrap();
        stage_grok_config(&host, &dest).unwrap();
        assert!(!dest.join("auth.json").exists());
    }
}

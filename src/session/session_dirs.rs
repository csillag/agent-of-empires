//! A session's directory list: the directories an agent may reach on top of
//! its working directory, each read-only or read-write.
//!
//! The list is chosen when the session is created (the web wizard pre-fills
//! it from `[session] default_dirs` in the host config), stored on the
//! `Instance`, and handed to every spawn, respawn and reattach. How it is
//! enforced depends on the agent:
//!
//! - aoe's own `fs/*` handler (`crate::acp::fs_handler::FsPolicy`) allows the
//!   listed directories, and refuses writes into read-only ones.
//! - Every agent process gets [`ENV_READ_ONLY`] / [`ENV_READ_WRITE`], so a
//!   launcher can pass the list to an agent that sandboxes itself (grok's
//!   kernel sandbox takes it through a host wrapper).
//! - ACP `additionalDirectories` carries all of them at `session/new`, for
//!   agents that scope themselves by it (claude). That is advisory, not a
//!   sandbox, and cannot express read-only.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Colon-separated read-only directories, exported to the agent process.
pub const ENV_READ_ONLY: &str = "AOE_SESSION_DIRS_READ_ONLY";
/// Colon-separated read-write directories, exported to the agent process.
pub const ENV_READ_WRITE: &str = "AOE_SESSION_DIRS_READ_WRITE";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DirAccess {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDir {
    pub path: String,
    pub access: DirAccess,
}

/// Checks a requested list and returns it normalized (trailing `/` dropped).
/// Every entry must be an absolute path without `:` (the environment
/// separator) or a control character, and a path may appear once.
pub fn validate(dirs: &[SessionDir]) -> Result<Vec<SessionDir>, String> {
    let mut out: Vec<SessionDir> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let raw = dir.path.as_str();
        if raw.is_empty() || raw.trim() != raw {
            return Err(format!(
                "directory {raw:?}: empty or surrounded by whitespace"
            ));
        }
        if !raw.starts_with('/') {
            return Err(format!("directory {raw:?}: must be an absolute path"));
        }
        if raw.contains(':') || raw.chars().any(char::is_control) {
            return Err(format!(
                "directory {raw:?}: contains ':' or a control character"
            ));
        }
        let path = if raw.len() > 1 {
            raw.trim_end_matches('/')
        } else {
            raw
        };
        if out.iter().any(|d| d.path == path) {
            return Err(format!("directory {path:?} is listed twice"));
        }
        out.push(SessionDir {
            path: path.to_string(),
            access: dir.access,
        });
    }
    Ok(out)
}

/// The environment entries that pass the list to the agent process. Both
/// variables are always set (empty when no entry has that access) so a
/// launcher never inherits a stale value from elsewhere.
pub fn env_pairs(dirs: &[SessionDir]) -> Vec<(String, String)> {
    let join = |access: DirAccess| {
        dirs.iter()
            .filter(|d| d.access == access)
            .map(|d| d.path.as_str())
            .collect::<Vec<_>>()
            .join(":")
    };
    vec![
        (ENV_READ_ONLY.to_string(), join(DirAccess::ReadOnly)),
        (ENV_READ_WRITE.to_string(), join(DirAccess::ReadWrite)),
    ]
}

/// The paths of the entries with `access`.
pub fn paths_with(dirs: &[SessionDir], access: DirAccess) -> Vec<PathBuf> {
    dirs.iter()
        .filter(|d| d.access == access)
        .map(|d| PathBuf::from(&d.path))
        .collect()
}

/// Every listed path, for ACP `additionalDirectories`.
pub fn all_paths(dirs: &[SessionDir]) -> Vec<PathBuf> {
    dirs.iter().map(|d| PathBuf::from(&d.path)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(path: &str, access: DirAccess) -> SessionDir {
        SessionDir {
            path: path.to_string(),
            access,
        }
    }

    #[test]
    fn validate_normalizes_and_rejects() {
        let ok = validate(&[
            dir("/home/u/commissura/", DirAccess::ReadWrite),
            dir("/srv/ref", DirAccess::ReadOnly),
        ])
        .unwrap();
        assert_eq!(ok[0].path, "/home/u/commissura");
        assert_eq!(ok[1].access, DirAccess::ReadOnly);
        for bad in ["", "relative/dir", " /x", "/a:b", "/a\nb"] {
            assert!(
                validate(&[dir(bad, DirAccess::ReadOnly)]).is_err(),
                "{bad:?}"
            );
        }
        assert!(validate(&[
            dir("/x", DirAccess::ReadOnly),
            dir("/x/", DirAccess::ReadWrite)
        ])
        .is_err());
        assert_eq!(
            validate(&[dir("/", DirAccess::ReadOnly)]).unwrap()[0].path,
            "/"
        );
    }

    #[test]
    fn env_pairs_split_by_access_and_are_always_set() {
        let dirs = [
            dir("/a", DirAccess::ReadOnly),
            dir("/b", DirAccess::ReadWrite),
            dir("/c", DirAccess::ReadOnly),
        ];
        assert_eq!(
            env_pairs(&dirs),
            vec![
                (ENV_READ_ONLY.to_string(), "/a:/c".to_string()),
                (ENV_READ_WRITE.to_string(), "/b".to_string()),
            ]
        );
        assert_eq!(
            env_pairs(&[]),
            vec![
                (ENV_READ_ONLY.to_string(), String::new()),
                (ENV_READ_WRITE.to_string(), String::new()),
            ]
        );
    }

    #[test]
    fn serde_uses_kebab_case_access() {
        let d: SessionDir = serde_json::from_str(r#"{"path":"/x","access":"read-only"}"#).unwrap();
        assert_eq!(d.access, DirAccess::ReadOnly);
        let t: SessionDir = toml::from_str("path = \"/y\"\naccess = \"read-write\"\n").unwrap();
        assert_eq!(t.access, DirAccess::ReadWrite);
    }
}

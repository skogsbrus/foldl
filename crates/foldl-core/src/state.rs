// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Session state: the fold's accumulated value, persisted as `state.json`
//! between invocations.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::env::EnvDiff;
use crate::folder::{Hash, Shebang};
use crate::interactive::InteractiveDiff;
use crate::lease::LeaseRef;
use crate::manifest::Scope;
use crate::render::Shell;

/// Current version of every state file foldl writes.
pub const STATE_VERSION: u32 = 1;

/// A script in a saved copy of a migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedScript {
    pub path: PathBuf,
    pub shebang: Shebang,
}

/// One loaded migration: what to undo and how.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loaded {
    pub root: PathBuf,
    pub name: String,
    pub hash: Hash,
    /// Hash of the files matched by `watch` when it loaded.
    pub watch: Hash,
    pub scope: Scope,
    #[serde(default)]
    pub lease: Option<LeaseRef>,
    /// The derived down is `diff.invert()`.
    pub diff: EnvDiff,
    /// Aliases set from a non-interactive `[aliases]` table.
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub interactive: Option<InteractiveDiff>,
    /// The custom down, from the saved copy.
    #[serde(default)]
    pub down: Option<SavedScript>,
    pub down_timeout_ms: u64,
    /// The saved copy of the migration directory.
    pub saved: PathBuf,
    /// `FOLDL_STATE` for this migration.
    pub state_dir: PathBuf,
    pub log: PathBuf,
    /// Why it loaded without running, e.g. a bash-only interactive
    /// migration in zsh.
    #[serde(default)]
    pub note: Option<String>,
}

/// A session: one shell, or one agent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    pub id: String,
    /// The process that owns the session; it ends when this one exits.
    pub parent: u32,
    pub shell: Option<Shell>,
    /// Loaded migrations, outermost folder first, in load order.
    pub loaded: Vec<Loaded>,
    /// Token of the last prompt's scan: unchanged means nothing to do.
    #[serde(default)]
    pub scan: Option<String>,
    /// Files matched by `watch` globs, per folder, stat'ed by the scan.
    #[serde(default)]
    pub watched: BTreeMap<PathBuf, Vec<PathBuf>>,
    /// Folders whose up failed, with the fingerprint at the time; not
    /// retried until it changes or the session leaves the folder.
    #[serde(default)]
    pub failed: BTreeMap<PathBuf, String>,
    /// Folders whose status was already reported, with the fingerprint.
    #[serde(default)]
    pub notified: BTreeMap<PathBuf, String>,
    /// Migrations unloaded by hand with `foldl down`.
    #[serde(default)]
    pub held_down: BTreeSet<(PathBuf, String)>,
    /// PID of the session holder, if one runs.
    #[serde(default)]
    pub holder: Option<u32>,
    /// Lease holders by lease id.
    #[serde(default)]
    pub leases: BTreeMap<String, u32>,
    /// An interactive migration waiting for its result.
    #[serde(default)]
    pub pending: Option<crate::machine::Pending>,
}

impl Session {
    pub fn new(id: String, parent: u32, shell: Option<Shell>) -> Session {
        Session {
            version: STATE_VERSION,
            id,
            parent,
            shell,
            ..Session::default()
        }
    }

    /// The composed diff of everything loaded: baseline to now.
    pub fn cumulative(&self) -> EnvDiff {
        self.loaded
            .iter()
            .fold(EnvDiff::default(), |acc, l| acc.compose(&l.diff))
    }

    /// Whether any loaded migration has a custom down, which needs a
    /// session holder to run if the parent dies.
    pub fn needs_holder(&self) -> bool {
        self.loaded
            .iter()
            .any(|l| l.down.is_some() || l.lease.is_some())
    }
}

/// The directory layout under the XDG base directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub state: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
    pub config: PathBuf,
}

/// A short, stable name for a folder, so two folders' migrations with the
/// same name don't collide.
pub fn folder_key(root: &Path) -> String {
    Hash::of_bytes(root.as_os_str().as_encoded_bytes())
        .short()
        .to_string()
}

impl Paths {
    pub fn sessions(&self) -> PathBuf {
        self.state.join("sessions")
    }

    pub fn session(&self, id: &str) -> PathBuf {
        self.sessions().join(id)
    }

    pub fn leases(&self) -> PathBuf {
        self.state.join("leases")
    }

    pub fn lease(&self, id: &str) -> PathBuf {
        self.leases().join(id)
    }

    pub fn ports(&self) -> PathBuf {
        self.state.join("ports")
    }

    pub fn trust(&self) -> PathBuf {
        self.data.join("trust")
    }

    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }

    /// `<owner>/saved/<name>-<folder key>`, `<owner>/data/...` and
    /// `<owner>/logs/<name>-<folder key>.log` for a migration owned by a
    /// session or lease directory.
    pub fn migration_dirs(owner: &Path, root: &Path, name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let key = format!("{name}-{}", folder_key(root));
        (
            owner.join("saved").join(&key),
            owner.join("data").join(&key),
            owner.join("logs").join(format!("{key}.log")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        let p = Paths {
            state: "/s".into(),
            data: "/d".into(),
            cache: "/c".into(),
            config: "/cfg".into(),
        };
        assert_eq!(p.session("x"), PathBuf::from("/s/sessions/x"));
        assert_eq!(p.lease("l"), PathBuf::from("/s/leases/l"));
        assert_eq!(p.ports(), PathBuf::from("/s/ports"));
        assert_eq!(p.trust(), PathBuf::from("/d/trust"));
        assert_eq!(p.config_file(), PathBuf::from("/cfg/config.toml"));
        let (saved, data, log) = Paths::migration_dirs(Path::new("/o"), Path::new("/r"), "pg");
        let key = folder_key(Path::new("/r"));
        assert_eq!(saved, PathBuf::from(format!("/o/saved/pg-{key}")));
        assert_eq!(data, PathBuf::from(format!("/o/data/pg-{key}")));
        assert_eq!(log, PathBuf::from(format!("/o/logs/pg-{key}.log")));
        assert_eq!(key.len(), 12);
    }

    #[test]
    fn state_roundtrips_through_json() {
        let mut s = Session::new("id".into(), 7, Some(Shell::Bash));
        s.pending = Some(crate::machine::Pending::default());
        s.loaded.push(Loaded {
            root: "/r".into(),
            name: "n".into(),
            hash: Hash("h".into()),
            watch: Hash::default(),
            scope: Scope::Shared,
            lease: Some(LeaseRef {
                id: "l".into(),
                key: "/r".into(),
            }),
            diff: EnvDiff::default(),
            aliases: vec!["t".into()],
            interactive: Some(InteractiveDiff::default()),
            down: Some(SavedScript {
                path: "/s/down.sh".into(),
                shebang: Shebang {
                    program: "bash".into(),
                    shell: crate::folder::ScriptShell::Bash,
                },
            }),
            down_timeout_ms: 5,
            saved: "/s".into(),
            state_dir: "/d".into(),
            log: "/l".into(),
            note: Some("skipped".into()),
        });
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Session>(&json).unwrap(), s);
        assert!(s.needs_holder());
    }

    #[test]
    fn new_session_is_versioned() {
        let s = Session::new("id".into(), 7, Some(Shell::Zsh));
        assert_eq!(s.version, STATE_VERSION);
        assert!(!s.needs_holder());
        assert!(s.cumulative().is_empty());
    }
}

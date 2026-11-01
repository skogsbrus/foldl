// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Leases: shared ownership of a migration's effects across sessions.
//!
//! A lease's state lists the shared migrations that are up (with the env
//! diff joining sessions replay) or dirty (their down failed). It is read
//! and written only under the lease's mutex.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::env::EnvDiff;
use crate::folder::{Hash, LeaseSpec};
use crate::state::{STATE_VERSION, SavedScript};

/// A lease a loaded migration belongs to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LeaseRef {
    /// Directory name under `leases/`: a hash of the key.
    pub id: String,
    /// The key as shown to people: a path, a repository or a custom name.
    pub key: String,
}

/// The lease for a spec. `repo` is the repository's common git dir; a
/// folder outside a repository falls back to its own path.
pub fn lease_ref(spec: &LeaseSpec, root: &Path, repo: Option<&Path>) -> LeaseRef {
    let key = match (spec, repo) {
        (LeaseSpec::Repo, Some(common)) => format!("repo:{}", common.display()),
        (LeaseSpec::Folder | LeaseSpec::Repo, _) => format!("folder:{}", root.display()),
        (LeaseSpec::Custom(name), _) => format!("custom:{name}"),
    };
    let id = Hash::of_bytes(key.as_bytes()).0[..16].to_string();
    LeaseRef { id, key }
}

/// A shared migration that ran, and how to undo it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub hash: Hash,
    /// Replayed by joining sessions.
    pub diff: EnvDiff,
    pub ports: BTreeMap<String, u16>,
    pub down: Option<SavedScript>,
    pub down_timeout_ms: u64,
    pub root: PathBuf,
    pub saved: PathBuf,
    pub state_dir: PathBuf,
    pub log: PathBuf,
    /// Set when its down failed: when (`HH:MM`) and how.
    pub dirty: Option<Dirty>,
    /// Order the entries came up in, for tearing down in reverse.
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dirty {
    pub at: String,
    pub what: String,
}

/// A lease's `state.json`: entries keyed by migration name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseState {
    pub version: u32,
    pub entries: BTreeMap<String, Entry>,
}

impl Default for LeaseState {
    fn default() -> LeaseState {
        LeaseState {
            version: STATE_VERSION,
            entries: BTreeMap::new(),
        }
    }
}

impl LeaseState {
    pub fn next_seq(&self) -> u64 {
        self.entries.values().map(|e| e.seq + 1).max().unwrap_or(0)
    }

    /// Entry names, newest first: the order to tear down in.
    pub fn newest_first(&self) -> Vec<String> {
        let mut names: Vec<(&u64, &String)> =
            self.entries.iter().map(|(n, e)| (&e.seq, n)).collect();
        names.sort();
        names.into_iter().rev().map(|(_, n)| n.clone()).collect()
    }
}

/// A session holding a lease, from its holder's sidecar file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    pub session: String,
    pub pid: u32,
    pub name: String,
    pub cwd: PathBuf,
    pub tty: Option<String>,
}

impl Holder {
    /// `zsh 4121 (~/code/app, ttys003)`
    pub fn describe(&self, home: &str) -> String {
        let cwd = crate::machine::tilde(&self.cwd, home);
        match &self.tty {
            Some(tty) => format!("{} {} ({cwd}, {tty})", self.name, self.pid),
            None => format!("{} {} ({cwd})", self.name, self.pid),
        }
    }
}

/// Ports to try for one name, in order: the whole range starting at a
/// point derived from the lease and name, skipping taken ones.
pub fn port_candidates(range: (u16, u16), seed: &str, taken: &BTreeSet<u16>) -> Vec<u16> {
    let size = u32::from(range.1 - range.0) + 1;
    let digest = Hash::of_bytes(seed.as_bytes());
    let offset = u32::from_str_radix(&digest.0[..8], 16).unwrap_or(0) % size;
    (0..size)
        .map(|i| range.0 + ((offset + i) % size) as u16)
        .filter(|p| !taken.contains(p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_keys() {
        let root = Path::new("/w/a");
        let folder = lease_ref(&LeaseSpec::Folder, root, Some(Path::new("/w/.git")));
        assert_eq!(folder.key, "folder:/w/a");
        assert_eq!(folder.id.len(), 16);
        assert_eq!(
            lease_ref(&LeaseSpec::Repo, root, Some(Path::new("/w/.git"))).key,
            "repo:/w/.git"
        );
        assert_eq!(
            lease_ref(&LeaseSpec::Repo, root, None),
            folder_without_repo(root)
        );
        let custom = lease_ref(&LeaseSpec::Custom("aws:dev".into()), root, None);
        assert_eq!(
            custom,
            lease_ref(&LeaseSpec::Custom("aws:dev".into()), Path::new("/x"), None)
        );
    }

    fn folder_without_repo(root: &Path) -> LeaseRef {
        lease_ref(&LeaseSpec::Folder, root, None)
    }

    #[test]
    fn ordering_and_holders() {
        let entry = |seq| Entry {
            hash: Hash::default(),
            diff: EnvDiff::default(),
            ports: BTreeMap::new(),
            down: None,
            down_timeout_ms: 0,
            root: "/r".into(),
            saved: "/s".into(),
            state_dir: "/d".into(),
            log: "/l".into(),
            dirty: None,
            seq,
        };
        let mut state = LeaseState::default();
        assert_eq!(state.next_seq(), 0);
        state.entries.insert("b".into(), entry(0));
        state.entries.insert("a".into(), entry(1));
        assert_eq!(state.next_seq(), 2);
        assert_eq!(state.newest_first(), ["a", "b"]);
        let h = Holder {
            session: "s".into(),
            pid: 4121,
            name: "zsh".into(),
            cwd: "/h/app".into(),
            tty: Some("ttys003".into()),
        };
        assert_eq!(h.describe("/h"), "zsh 4121 (~/app, ttys003)");
        let agent = Holder {
            tty: None,
            name: "claude".into(),
            ..h
        };
        assert_eq!(agent.describe("/h"), "claude 4121 (~/app)");
    }

    #[test]
    fn ports_cover_the_range_once() {
        let taken: BTreeSet<u16> = [20001].into();
        let c = port_candidates((20000, 20003), "lease/pg", &taken);
        let mut sorted = c.clone();
        sorted.sort();
        assert_eq!(sorted, [20000, 20002, 20003]);
        assert_eq!(c, port_candidates((20000, 20003), "lease/pg", &taken));
        assert_eq!(port_candidates((5, 5), "x", &BTreeSet::new()), [5]);
    }
}

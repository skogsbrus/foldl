// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! The trust store under `$XDG_DATA_HOME/foldl/trust/`: allowed, denied
//! and ignored folders, keyed on path.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use foldl_core::folder::{Entry, FolderFiles, Hash};
use foldl_core::state::{STATE_VERSION, folder_key};
use serde::{Deserialize, Serialize};

use crate::fsx;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustState {
    Allowed,
    Denied,
    Ignored,
}

/// One folder's trust. An allowed entry keeps the text of what was
/// allowed, so the next `allow` can show what changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustEntry {
    pub version: u32,
    pub path: PathBuf,
    pub state: TrustState,
    pub hash: Option<Hash>,
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

impl TrustEntry {
    pub fn allowed(files: &FolderFiles, hash: Hash) -> TrustEntry {
        TrustEntry {
            version: STATE_VERSION,
            path: files.root.clone(),
            state: TrustState::Allowed,
            hash: Some(hash),
            files: text_files(files),
        }
    }

    pub fn marked(root: &Path, state: TrustState) -> TrustEntry {
        TrustEntry {
            version: STATE_VERSION,
            path: root.to_path_buf(),
            state,
            hash: None,
            files: BTreeMap::new(),
        }
    }
}

fn text_files(files: &FolderFiles) -> BTreeMap<String, String> {
    files
        .entries
        .iter()
        .filter_map(|(path, entry)| match entry {
            Entry::File(bytes) => Some((path.clone(), String::from_utf8_lossy(bytes).into_owned())),
            Entry::Dir => None,
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    fn path_of(&self, root: &Path) -> PathBuf {
        self.dir
            .join("paths")
            .join(format!("{}.json", folder_key(root)))
    }

    pub fn get(&self, root: &Path) -> Option<TrustEntry> {
        let bytes = fs::read(self.path_of(root)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn set(&self, entry: &TrustEntry) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(entry).map_err(io::Error::other)?;
        fsx::write_atomic(&self.path_of(&entry.path), &json)
    }

    /// Changes whenever any entry is written, for the prompt's fast path.
    pub fn token(&self) -> String {
        let meta = fs::metadata(self.dir.join("paths"));
        meta.map_or_else(
            |_| "none".to_string(),
            |m| format!("{}.{}", m.mtime(), m.mtime_nsec()),
        )
    }
}

/// A unified diff of what changed since the last allow, file by file.
pub fn changes(old: &BTreeMap<String, String>, files: &FolderFiles) -> String {
    let new = text_files(files);
    let names: std::collections::BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    names
        .into_iter()
        .filter(|name| old.get(*name) != new.get(*name))
        .map(|name| {
            let before = old.get(name).map_or("", String::as_str);
            let after = new.get(name).map_or("", String::as_str);
            let diff = similar::TextDiff::from_lines(before, after);
            let header = (format!("a/.foldl/{name}"), format!("b/.foldl/{name}"));
            diff.unified_diff().header(&header.0, &header.1).to_string()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(root: &Path, pairs: &[(&str, &str)]) -> FolderFiles {
        let mut entries: BTreeMap<String, Entry> = pairs
            .iter()
            .map(|(p, c)| (p.to_string(), Entry::File(c.as_bytes().to_vec())))
            .collect();
        entries.insert("dir".into(), Entry::Dir);
        FolderFiles {
            root: root.to_path_buf(),
            entries,
        }
    }

    #[test]
    fn stores_entries() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store {
            dir: dir.path().join("trust"),
        };
        let root = Path::new("/r");
        assert_eq!(store.get(root), None);
        assert_eq!(store.token(), "none");
        let allowed = TrustEntry::allowed(&files(root, &[("a", "1")]), Hash("h".into()));
        store.set(&allowed).unwrap();
        assert_eq!(store.get(root), Some(allowed.clone()));
        assert_eq!(allowed.files.len(), 1);
        let token = store.token();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store
            .set(&TrustEntry::marked(root, TrustState::Denied))
            .unwrap();
        assert_eq!(store.get(root).unwrap().state, TrustState::Denied);
        assert_ne!(store.token(), token);
        let broken = Store {
            dir: "/dev/null".into(),
        };
        assert!(broken.set(&allowed).is_err());
    }

    #[test]
    fn shows_changes() {
        let root = Path::new("/r");
        let old = text_files(&files(root, &[("a", "1\n"), ("gone", "x\n")]));
        let text = changes(&old, &files(root, &[("a", "2\n"), ("new", "y\n")]));
        assert!(text.contains("--- a/.foldl/a\n+++ b/.foldl/a\n"), "{text}");
        assert!(text.contains("-1\n+2\n"));
        assert!(text.contains("-x\n"));
        assert!(text.contains("+y\n"));
        assert_eq!(
            changes(&old, &files(root, &[("a", "1\n"), ("gone", "x\n")])),
            ""
        );
    }
}

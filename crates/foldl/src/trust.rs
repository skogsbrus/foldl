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
            Entry::File(bytes) => Some((path.clone(), shown(path, bytes))),
            Entry::Dir => None,
        })
        .collect()
}

/// The text kept and diffed for a file. Local migrations hold personal
/// settings and secrets, so for them it is only a hash: `allow` says that
/// they changed without printing, or storing, what is in them.
fn shown(path: &str, bytes: &[u8]) -> String {
    let local = path
        .split('/')
        .next()
        .is_some_and(|d| d.ends_with(".local"));
    match local {
        true => format!(
            "(local file, not shown; sha256 {})\n",
            Hash::of_bytes(bytes).short()
        ),
        false => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Content allowed with `reuse = "repo"`, keyed on its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ContentEntry {
    version: u32,
    hash: Hash,
    roots: Vec<String>,
    /// [`crate::git::identities`] of those repositories. Forks share root
    /// commits, so reuse needs one of these too.
    #[serde(default)]
    repos: Vec<String>,
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

    fn content_path(&self, hash: &Hash) -> PathBuf {
        self.dir.join("content").join(format!("{}.json", hash.0))
    }

    /// Root commits and identities of repositories where this content was
    /// allowed.
    pub fn content_repos(&self, hash: &Hash) -> (Vec<String>, Vec<String>) {
        let bytes = fs::read(self.content_path(hash)).unwrap_or_default();
        serde_json::from_slice::<ContentEntry>(&bytes)
            .map(|e| (e.roots, e.repos))
            .unwrap_or_default()
    }

    /// Records that content was allowed in a repository with these roots
    /// and identities.
    pub fn allow_content(&self, hash: &Hash, roots: &[String], repos: &[String]) -> io::Result<()> {
        let merged = |mut all: Vec<String>, new: &[String]| {
            all.extend(new.iter().cloned());
            all.sort();
            all.dedup();
            all
        };
        let (old_roots, old_repos) = self.content_repos(hash);
        let entry = ContentEntry {
            version: STATE_VERSION,
            hash: hash.clone(),
            roots: merged(old_roots, roots),
            repos: merged(old_repos, repos),
        };
        let json = serde_json::to_vec_pretty(&entry).map_err(io::Error::other)?;
        fsx::write_atomic(&self.content_path(hash), &json)
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
    // Entries stored before local files were hidden still hold their text.
    let old: BTreeMap<String, String> = old
        .iter()
        .map(
            |(name, text)| match text.starts_with("(local file, not shown") {
                true => (name.clone(), text.clone()),
                false => (name.clone(), shown(name, text.as_bytes())),
            },
        )
        .collect();
    let old = &old;
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

    #[test]
    fn hides_local_files() {
        let root = Path::new("/r");
        let key = "5-keys.local/manifest.toml";
        let first = files(root, &[(key, "KEY = \"sk-old\"\n")]);
        let stored = text_files(&first);
        assert!(!stored[key].contains("sk-old"), "{stored:?}");
        let text = changes(&stored, &files(root, &[(key, "KEY = \"sk-new\"\n")]));
        assert!(
            text.contains("+++ b/.foldl/5-keys.local/manifest.toml"),
            "{text}"
        );
        assert!(text.contains("+(local file, not shown; sha256 "), "{text}");
        assert!(!text.contains("sk-"), "{text}");
        assert_eq!(changes(&stored, &first), "");
        let plain: BTreeMap<String, String> = [(key.into(), "KEY = \"sk-old\"\n".into())].into();
        assert_eq!(changes(&plain, &first), "", "older entries hold the text");
    }
}

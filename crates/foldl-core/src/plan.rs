// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Planning a transition between migration stacks.
//!
//! Every ancestor's `.foldl/` forms one flattened stack of migrations,
//! outermost folder first. Moving between folders keeps the common prefix
//! of the loaded and wanted stacks, unloads the rest of the loaded stack
//! from the right, then loads the rest of the wanted stack from the left.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::folder::{Folder, Hash, Issue, Migration};
use crate::lease::LeaseRef;
use crate::manifest::Scope;
use crate::state::{Loaded, Session};

/// What the interpreter found for one ancestor that has a `.foldl/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderInfo {
    pub root: PathBuf,
    /// Changes whenever anything under `.foldl/`, a watched file or the
    /// folder's trust changes.
    pub fingerprint: String,
    pub status: FolderStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderStatus {
    /// Trusted. `watch` holds each migration's watched-files hash.
    Allowed {
        folder: Box<Folder>,
        watch: BTreeMap<String, Hash>,
        /// The lease of each shared migration, resolved by the interpreter.
        leases: BTreeMap<String, LeaseRef>,
    },
    /// Not allowed. `changed`: it was allowed with different content.
    Untrusted {
        changed: bool,
    },
    Denied,
    Ignored,
    Invalid(Vec<Issue>),
}

/// A migration to load.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Fresh {
    pub root: PathBuf,
    pub migration: Migration,
    pub watch: Hash,
    #[serde(default)]
    pub lease: Option<LeaseRef>,
}

/// Something to tell the user about a folder, once per fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Untrusted {
        root: PathBuf,
        changed: bool,
    },
    Denied {
        root: PathBuf,
    },
    Invalid {
        root: PathBuf,
        issues: Vec<Issue>,
    },
    /// Loaded, then changed so it is no longer allowed; kept as loaded.
    ChangedWhileLoaded {
        root: PathBuf,
    },
    /// A shared migration changed; it is not restarted under others.
    SharedChanged {
        root: PathBuf,
        name: String,
    },
}

impl Notice {
    pub fn root(&self) -> &PathBuf {
        match self {
            Notice::Untrusted { root, .. }
            | Notice::Denied { root }
            | Notice::Invalid { root, .. }
            | Notice::ChangedWhileLoaded { root }
            | Notice::SharedChanged { root, .. } => root,
        }
    }
}

/// The transition: how many loaded migrations to unload (from the top of
/// the stack), then which to load, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub downs: usize,
    pub ups: Vec<Fresh>,
    pub notices: Vec<Notice>,
}

enum Want<'a> {
    Fresh(Box<Fresh>),
    Keep(&'a Loaded),
}

impl Want<'_> {
    /// Whether a loaded migration can stay as it is.
    fn keeps(&self, loaded: &Loaded) -> bool {
        match self {
            Want::Keep(kept) => kept.root == loaded.root && kept.name == loaded.name,
            Want::Fresh(f) => {
                let same = f.root == loaded.root && f.migration.name == loaded.name;
                let unchanged = f.migration.hash == loaded.hash && f.watch == loaded.watch;
                let shared = loaded.scope == Scope::Shared && f.migration.scope == Scope::Shared;
                same && (unchanged || shared)
            }
        }
    }
}

/// Plans the transition from what `session` has loaded to what `folders`
/// (every ancestor with a `.foldl/`, outermost first) asks for.
pub fn plan(session: &Session, folders: &[FolderInfo]) -> Plan {
    let (wants, mut notices) = wanted(session, folders);
    let prefix = session
        .loaded
        .iter()
        .zip(&wants)
        .take_while(|(l, w)| {
            w.keeps(l) && !session.redo.contains(&(l.root.clone(), l.name.clone()))
        })
        .count();
    let shared_changed = session.loaded[..prefix]
        .iter()
        .zip(&wants)
        .filter_map(|(l, w)| match w {
            Want::Fresh(f) if f.migration.hash != l.hash => Some(Notice::SharedChanged {
                root: l.root.clone(),
                name: l.name.clone(),
            }),
            _ => None,
        });
    notices.extend(shared_changed);
    let ups = wants
        .into_iter()
        .skip(prefix)
        .filter_map(|w| match w {
            Want::Fresh(f) => Some(*f),
            Want::Keep(_) => None,
        })
        .collect();
    let fingerprints: BTreeMap<&PathBuf, &String> =
        folders.iter().map(|f| (&f.root, &f.fingerprint)).collect();
    let fresh_notices = notices
        .into_iter()
        .filter(|n| session.notified.get(n.root()) != fingerprints.get(n.root()).copied())
        .collect();
    Plan {
        downs: session.loaded.len() - prefix,
        ups,
        notices: fresh_notices,
    }
}

fn wanted<'a>(session: &'a Session, folders: &[FolderInfo]) -> (Vec<Want<'a>>, Vec<Notice>) {
    let mut wants = Vec::new();
    let mut notices = Vec::new();
    for info in folders {
        let root = &info.root;
        let loaded: Vec<&Loaded> = session.loaded.iter().filter(|l| &l.root == root).collect();
        let failed = session.failed.get(root) == Some(&info.fingerprint);
        match &info.status {
            FolderStatus::Ignored => continue,
            FolderStatus::Allowed { .. } if failed => break,
            FolderStatus::Allowed {
                folder,
                watch,
                leases,
            } => {
                let held =
                    |m: &&Migration| !session.held_down.contains(&(root.clone(), m.name.clone()));
                wants.extend(folder.migrations.iter().filter(held).map(|m| {
                    let watch = watch.get(&m.name).cloned().unwrap_or_default();
                    Want::Fresh(Box::new(Fresh {
                        root: root.clone(),
                        migration: m.clone(),
                        watch,
                        lease: leases.get(&m.name).cloned(),
                    }))
                }));
            }
            FolderStatus::Untrusted { .. } | FolderStatus::Invalid(_) if !loaded.is_empty() => {
                wants.extend(loaded.into_iter().map(Want::Keep));
                notices.push(Notice::ChangedWhileLoaded { root: root.clone() });
            }
            FolderStatus::Untrusted { changed } => {
                notices.push(Notice::Untrusted {
                    root: root.clone(),
                    changed: *changed,
                });
                break;
            }
            FolderStatus::Denied => {
                notices.push(Notice::Denied { root: root.clone() });
                break;
            }
            FolderStatus::Invalid(issues) => {
                notices.push(Notice::Invalid {
                    root: root.clone(),
                    issues: issues.clone(),
                });
                break;
            }
        }
    }
    (wants, notices)
}

#[cfg(test)]
mod tests;

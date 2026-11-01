// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! From disk to the core's [`FolderInfo`]: the cheap scan for the prompt's
//! fast path, then full reads, validation, trust and watched files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use foldl_core::config::Config;
use foldl_core::folder::{Host, Issue, Severity, load};
use foldl_core::plan::{FolderInfo, FolderStatus};
use sha2::{Digest, Sha256};

use crate::disk;
use crate::trust::{Store, TrustState};

/// The ancestors with a `.foldl/` and their fingerprints, plus a token
/// that changes when any of them, or trust, changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scan {
    pub folders: Vec<(PathBuf, String)>,
    pub token: String,
}

pub fn scan(cwd: &Path, trust: &Store, watched: &BTreeMap<PathBuf, Vec<PathBuf>>) -> Scan {
    let folders: Vec<(PathBuf, String)> = disk::ancestors(cwd)
        .into_iter()
        .map(|root| {
            let extra = watched.get(&root).map_or(&[][..], Vec::as_slice);
            let fp = disk::fingerprint(&root, extra);
            (root, fp)
        })
        .collect();
    let mut hasher = Sha256::new();
    hasher.update(trust.token());
    folders.iter().for_each(|(root, fp)| {
        hasher.update(root.as_os_str().as_encoded_bytes());
        hasher.update(fp);
    });
    let token = foldl_core::env::hex(&hasher.finalize())[..16].to_string();
    Scan { folders, token }
}

/// Reads and classifies every scanned folder. Also returns the watched
/// files found, per folder.
pub fn infos(
    scan: &Scan,
    trust: &Store,
    config: &Config,
    host: Host,
) -> (Vec<FolderInfo>, BTreeMap<PathBuf, Vec<PathBuf>>) {
    let mut watched = BTreeMap::new();
    let infos = scan
        .folders
        .iter()
        .map(|(root, fingerprint)| {
            let (status, files) = status(root, trust, config, host);
            watched.insert(root.clone(), files);
            FolderInfo {
                root: root.clone(),
                fingerprint: fingerprint.clone(),
                status,
            }
        })
        .collect();
    (infos, watched)
}

/// Each shared migration's lease. Repository leases key on the common git
/// dir, shared by all worktrees; that asks git only when needed.
fn leases(
    root: &Path,
    folder: &foldl_core::folder::Folder,
) -> BTreeMap<String, foldl_core::lease::LeaseRef> {
    let wants_repo = folder
        .migrations
        .iter()
        .any(|m| m.lease == Some(foldl_core::folder::LeaseSpec::Repo));
    let repo = crate::git::common_dir(root)
        .filter(|_| wants_repo)
        .and_then(|d| d.canonicalize().ok());
    folder
        .migrations
        .iter()
        .filter_map(|m| {
            Some((
                m.name.clone(),
                foldl_core::lease::lease_ref(m.lease.as_ref()?, root, repo.as_deref()),
            ))
        })
        .collect()
}

/// Trust keyed on content: the same `.foldl/` was allowed in a repository
/// sharing a root commit with this one.
fn reused(root: &Path, folder: &foldl_core::folder::Folder, trust: &Store) -> bool {
    let allowed_in = trust.content_roots(&folder.hash);
    let here = match (folder.reuse, allowed_in.is_empty()) {
        (foldl_core::manifest::Reuse::Repo, false) => {
            crate::git::root_commits(root).unwrap_or_default()
        }
        _ => vec![],
    };
    here.iter().any(|commit| allowed_in.contains(commit))
}

fn status(root: &Path, trust: &Store, config: &Config, host: Host) -> (FolderStatus, Vec<PathBuf>) {
    let entry = trust.get(root);
    let marked = entry.as_ref().map(|e| e.state);
    let files = match (marked, disk::read_folder(root)) {
        (Some(TrustState::Ignored), _) => return (FolderStatus::Ignored, vec![]),
        (Some(TrustState::Denied), _) => return (FolderStatus::Denied, vec![]),
        (_, Err(e)) => {
            let issue = Issue {
                severity: Severity::Error,
                file: String::new(),
                message: format!("can't read: {e}"),
            };
            return (FolderStatus::Invalid(vec![issue]), vec![]);
        }
        (_, Ok(files)) => files,
    };
    let folder = match load(&files, host) {
        Ok(folder) => folder,
        Err(issues) => return (FolderStatus::Invalid(issues), vec![]),
    };
    let allowed_hash = entry.and_then(|e| e.hash);
    let prefixed = config.trusted_prefixes.iter().any(|p| root.starts_with(p));
    match (allowed_hash, prefixed) {
        (Some(hash), _) if hash == folder.hash => {}
        (_, true) => {}
        _ if reused(root, &folder, trust) => {}
        (hash, false) => {
            return (
                FolderStatus::Untrusted {
                    changed: hash.is_some(),
                },
                vec![],
            );
        }
    }
    let per_migration: Vec<(String, Vec<PathBuf>)> = folder
        .migrations
        .iter()
        .map(|m| (m.name.clone(), disk::expand_watch(root, &m.watch)))
        .collect();
    let watch = per_migration
        .iter()
        .map(|(name, files)| (name.clone(), disk::watch_hash(root, files)))
        .collect();
    let mut all: Vec<PathBuf> = per_migration
        .into_iter()
        .flat_map(|(_, files)| files)
        .collect();
    all.sort();
    all.dedup();
    (
        FolderStatus::Allowed {
            leases: leases(root, &folder),
            folder: Box::new(folder),
            watch,
        },
        all,
    )
}

// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Helpers shared by the core's tests: folders from literal files, and a
//! driver that replays a scripted world against the machine.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::lease::lease_ref;

use crate::config::Config;
use crate::env::{Env, Value};
use crate::folder::{Entry, FolderFiles, Host, load};
use crate::machine::{Ctx, Effect, Event, Finish, Machine, step};
use crate::plan::{FolderInfo, FolderStatus};
use crate::state::{Paths, Session};

/// Builds `.foldl/` from `(path, content)` pairs; parent directories are
/// added automatically.
pub fn files(root: &str, pairs: &[(&str, &str)]) -> FolderFiles {
    let mut entries = BTreeMap::new();
    for (path, content) in pairs {
        let parts: Vec<&str> = path.split('/').collect();
        for depth in 1..parts.len() {
            entries.insert(parts[..depth].join("/"), Entry::Dir);
        }
        entries.insert(path.to_string(), Entry::File(content.as_bytes().to_vec()));
    }
    FolderFiles {
        root: PathBuf::from(root),
        entries,
    }
}

/// An allowed folder.
pub fn allowed(root: &str, fingerprint: &str, pairs: &[(&str, &str)]) -> FolderInfo {
    let folder = load(&files(root, pairs), Host::default()).expect("valid folder");
    FolderInfo {
        root: PathBuf::from(root),
        fingerprint: fingerprint.to_string(),
        status: FolderStatus::Allowed {
            leases: folder
                .migrations
                .iter()
                .filter_map(|m| {
                    Some((
                        m.name.clone(),
                        lease_ref(m.lease.as_ref()?, Path::new(root), None),
                    ))
                })
                .collect(),
            folder: Box::new(folder),
            watch: BTreeMap::new(),
        },
    }
}

pub fn info(root: &str, fingerprint: &str, status: FolderStatus) -> FolderInfo {
    FolderInfo {
        root: PathBuf::from(root),
        fingerprint: fingerprint.to_string(),
        status,
    }
}

pub fn env(pairs: &[(&str, &str)]) -> Env {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), Value::from(*v)))
        .collect()
}

pub fn paths() -> Paths {
    Paths {
        state: "/st".into(),
        data: "/da".into(),
        cache: "/ca".into(),
        config: "/co".into(),
    }
}

pub fn ctx() -> Ctx {
    Ctx {
        paths: paths(),
        config: Config::default(),
        shell: Some(crate::render::Shell::Bash),
        prompt: false,
        holders: false,
    }
}

pub fn session() -> Session {
    Session::new("s1".into(), 100, Some(crate::render::Shell::Bash))
}

/// Everything a run did.
#[derive(Debug)]
pub struct Trace {
    pub effects: Vec<Effect>,
    pub finish: Finish,
}

impl Trace {
    pub fn printed(&self) -> String {
        self.effects
            .iter()
            .filter_map(|e| match e {
                Effect::Print(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Names of runs, as `up:name` or `down:name`.
    pub fn runs(&self) -> Vec<String> {
        self.effects
            .iter()
            .filter_map(|e| match e {
                Effect::Run(run) => {
                    Some(format!("{:?}:{}", run.direction, run.name).to_lowercase())
                }
                _ => None,
            })
            .collect()
    }
}

/// Performs effects with `world` until the machine finishes.
pub fn drive(
    start: (Machine, Vec<Effect>),
    mut world: impl FnMut(&Effect) -> Option<Event>,
) -> Trace {
    let (mut machine, mut pending) = start;
    let mut effects = Vec::new();
    loop {
        let mut reply = None;
        for effect in pending.drain(..) {
            if let Effect::Finish(finish) = &effect {
                let finish = (**finish).clone();
                effects.push(effect);
                return Trace { effects, finish };
            }
            reply = world(&effect);
            effects.push(effect);
        }
        let event = reply.expect("machine waits for an event");
        (machine, pending) = step(machine, event);
    }
}

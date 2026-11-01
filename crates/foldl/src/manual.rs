// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! `foldl up | down | redo <name>`, and `foldl down --force <name>`.
//!
//! The first three mark the migration in this session; the next prompt
//! (or `foldl export`, for agents) carries them out, so the shell's env
//! changes along with them. `down --force` retries a dirty lease's down
//! right away.

use std::path::PathBuf;

use foldl_core::error::{Code, Failure};
use foldl_core::folder::Host;
use foldl_core::lease::LeaseRef;
use foldl_core::machine::{Ctx, start};
use foldl_core::plan::FolderStatus;
use foldl_core::state::{Loaded, Session};

use crate::cli::IoContext;
use crate::interp::{RealWorld, drive};
use crate::runner::Runner;
use crate::sys::Sys;
use crate::{gather, leases, session, settings, trust_cmd};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    Redo,
}

/// The folder (innermost first) that has a migration called `name`, and
/// its lease if it is shared.
pub fn resolve(sys: &Sys, name: &str) -> Result<(PathBuf, Option<LeaseRef>), Failure> {
    let trust = trust_cmd::store(sys);
    let scan = gather::scan(&sys.cwd, &trust, &Default::default());
    let host = Host {
        nix_available: sys.which("nix").is_some(),
    };
    let (infos, _) = gather::infos(&scan, &trust, &settings::load(sys, None), host);
    let found = infos.into_iter().rev().find_map(|info| match info.status {
        FolderStatus::Allowed { folder, leases, .. } => {
            let has = folder.migrations.iter().any(|m| m.name == name);
            has.then(|| (info.root, leases.get(name).cloned()))
        }
        _ => None,
    });
    found.ok_or(Failure::new(
        Code::Usage,
        format!("no migration named '{name}' in an allowed folder here"),
    ))
}

fn current(sys: &Sys) -> Result<Session, Failure> {
    let missing = Failure::new(Code::Usage, "this shell has no foldl session")
        .with_fix("add eval \"$(foldl hook zsh)\" (or bash) to your shell's rc file");
    let id = sys.var("FOLDL_SESSION").ok_or(missing.clone())?;
    session::load(&sys.paths(), &id).ok_or(missing)
}

pub fn mark(sys: &Sys, name: &str, action: Action) -> Result<i32, Failure> {
    let mut session = current(sys)?;
    let (root, _) = resolve(sys, name)?;
    let key = (root.clone(), name.to_string());
    let verb = match action {
        Action::Up => {
            session.held_down.remove(&key);
            session.failed.remove(&root);
            "loads"
        }
        Action::Down => {
            session.held_down.insert(key);
            "unloads"
        }
        Action::Redo => {
            session.redo.insert(key);
            "reloads"
        }
    };
    session.scan = None;
    session::save(&sys.paths(), &session).ctx("saving session")?;
    eprintln!("foldl: {name} {verb} at the next prompt");
    Ok(0)
}

/// Retries the down of a dirty shared migration now, in a session of its
/// own.
pub fn force_down(sys: &Sys, name: &str) -> Result<i32, Failure> {
    let (root, lease) = resolve(sys, name)?;
    let lease = lease.ok_or(Failure::new(
        Code::Usage,
        format!("{name} is not a shared migration"),
    ))?;
    let paths = sys.paths();
    let entry = leases::read_state(&paths, &lease).entries.remove(name);
    let entry = entry.ok_or(Failure::new(
        Code::Usage,
        format!("{name} isn't running; nothing to tear down"),
    ))?;
    let mut session = Session::new(
        session::new_id(std::process::id()),
        std::process::id(),
        None,
    );
    session.loaded.push(Loaded {
        root: root.clone(),
        name: name.to_string(),
        hash: entry.hash,
        watch: Default::default(),
        scope: foldl_core::manifest::Scope::Shared,
        lease: Some(lease),
        diff: Default::default(),
        aliases: vec![],
        interactive: None,
        down: None,
        down_timeout_ms: entry.down_timeout_ms,
        saved: entry.saved,
        state_dir: entry.state_dir,
        log: entry.log,
        note: None,
    });
    session.redo.insert((root, name.to_string()));
    let config = settings::load(sys, None);
    let ctx = Ctx {
        paths: paths.clone(),
        config: config.clone(),
        shell: None,
        prompt: false,
        holders: false,
    };
    let runner = Runner::new(sys.exe.clone(), paths.session(&session.id))
        .ctx("signals")?
        .with_cache(paths.cache.clone());
    let mut world = RealWorld::new(&runner, sys, &session, &config);
    let id = session.id.clone();
    let (finish, _) = drive(
        &mut world,
        start(session, ctx, foldl_core::env::tracked(sys.env()), &[]),
    );
    session::remove(&paths, &id);
    Ok(finish.code.exit())
}

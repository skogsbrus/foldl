// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! `foldl exec` and `foldl export`: load a folder outside a prompt.
//!
//! An interactive shell's session is never changed from here, since the
//! shell's env would no longer match it. `exec` joins a session that
//! already has the folder loaded; `export` reuses an agent session (one
//! without a shell); otherwise both load into an ephemeral session and
//! unload when done.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;

use foldl_core::config::Verbosity;
use foldl_core::env::{EnvDiff, tracked};
use foldl_core::error::{Code, Failure};
use foldl_core::folder::Host;
use foldl_core::machine::{Ctx, Finish, start};
use foldl_core::plan::{FolderInfo, plan};
use foldl_core::render::{Shell, render, render_json};
use foldl_core::state::Session;

use crate::cli::IoContext;
use crate::interp::{RealWorld, drive};
use crate::runner::Runner;
use crate::sys::Sys;
use crate::{gather, session, settings, trust_cmd};

/// A loaded folder stack and how to leave it.
struct Loaded {
    finish: Finish,
    ctx: Ctx,
    ephemeral: bool,
}

fn target(sys: &Sys, dir: Option<PathBuf>) -> Result<PathBuf, Failure> {
    let dir = sys.absolute(&dir.unwrap_or_default());
    dir.canonicalize()
        .map_err(|e| Failure::new(Code::Usage, format!("{}: {e}", dir.display())))
}

fn infos(sys: &Sys, dir: &std::path::Path, config: &foldl_core::config::Config) -> Vec<FolderInfo> {
    let trust = trust_cmd::store(sys);
    let scan = gather::scan(dir, &trust, &Default::default());
    gather::infos(
        &scan,
        &trust,
        config,
        Host {
            nix_available: sys.which("nix").is_some(),
        },
    )
    .0
}

/// Loads `dir`'s stack, joining or reusing `existing` when allowed.
fn load(
    sys: &Sys,
    dir: &std::path::Path,
    verbosity: Option<Verbosity>,
    reuse: fn(&Session) -> bool,
) -> Result<Loaded, Failure> {
    let paths = sys.paths();
    let config = settings::load(sys, verbosity);
    let infos = infos(sys, dir, &config);
    let ctx = Ctx {
        paths: paths.clone(),
        config,
        shell: None,
        prompt: false,
        holders: true,
    };
    let existing = sys
        .var("FOLDL_SESSION")
        .and_then(|id| session::load(&paths, &id));
    let actual = tracked(sys.env());
    let (session, env, ephemeral) = match existing {
        Some(s) if reuse(&s) => (s, actual, false),
        Some(s) => {
            let env = session::revert(actual, &s.cumulative());
            (
                Session::new(
                    session::new_id(std::process::id()),
                    std::process::id(),
                    None,
                ),
                env,
                true,
            )
        }
        None => (
            Session::new(
                session::new_id(std::process::id()),
                std::process::id(),
                None,
            ),
            actual,
            true,
        ),
    };
    let runner = Runner::new(sys.exe.clone(), paths.session(&session.id))
        .ctx("signals")?
        .with_cache(paths.cache.clone());
    let mut world = RealWorld::new(&runner, sys, &session, &ctx.config);
    let (finish, _) = drive(&mut world, start(session, ctx.clone(), env, &infos));
    let loaded = Loaded {
        finish,
        ctx,
        ephemeral,
    };
    match loaded.finish.code {
        Code::Ok => Ok(loaded),
        code => {
            let message = format!("{} didn't load", dir.display());
            unload(sys, loaded)?;
            Err(Failure::new(code, message))
        }
    }
}

/// Leaves an ephemeral session, or saves a reused one.
fn unload(sys: &Sys, loaded: Loaded) -> Result<(), Failure> {
    let paths = sys.paths();
    let Loaded {
        finish,
        ctx,
        ephemeral,
    } = loaded;
    if !ephemeral {
        return session::save(&paths, &finish.session).ctx("saving session");
    }
    let runner = Runner::new(sys.exe.clone(), paths.session(&finish.session.id))
        .ctx("signals")?
        .with_cache(paths.cache.clone());
    let mut world = RealWorld::new(&runner, sys, &finish.session, &ctx.config);
    let (_, _) = drive(
        &mut world,
        start(finish.session.clone(), ctx, finish.env, &[]),
    );
    session::remove(&paths, &finish.session.id);
    Ok(())
}

/// The full process environment after a transition: untracked variables
/// as they were, tracked ones changed by the diff.
fn full_env(sys: &Sys, diff: &EnvDiff) -> Vec<(OsString, OsString)> {
    let mut vars = sys.vars.clone();
    for (name, change) in &diff.0 {
        match &change.after {
            Some(value) => vars.insert(name.into(), OsString::from_vec(value.0.clone())),
            None => vars.remove(&OsString::from(name)),
        };
    }
    vars.into_iter().collect()
}

pub fn exec(
    sys: &Sys,
    dir: Option<PathBuf>,
    cmd: Vec<OsString>,
    verbosity: Option<Verbosity>,
) -> Result<i32, Failure> {
    let dir = target(sys, dir)?;
    let joined = |s: &Session| {
        let wanted = infos(sys, &dir, &foldl_core::config::Config::default());
        let p = plan(s, &wanted);
        p.downs == 0 && p.ups.is_empty() && !s.loaded.is_empty()
    };
    let paths = sys.paths();
    let existing = sys
        .var("FOLDL_SESSION")
        .and_then(|id| session::load(&paths, &id));
    let joins = existing.as_ref().is_some_and(joined);
    let loaded = match joins {
        true => None,
        false => Some(load(sys, &dir, verbosity, |_| false)?),
    };
    let diff = loaded.as_ref().map_or_else(EnvDiff::default, |l| {
        EnvDiff::between(&tracked(sys.env()), &l.finish.env)
    });
    let ignore_ctrl_c = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, ignore_ctrl_c).ctx("signals")?;
    let status = std::process::Command::new(&cmd[0])
        .args(&cmd[1..])
        .current_dir(&dir)
        .env_clear()
        .envs(full_env(sys, &diff))
        .status();
    let code = status.map(|s| s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)));
    loaded.map_or(Ok(()), |l| unload(sys, l))?;
    code.map_err(|e| {
        Failure::new(
            Code::Unavailable,
            format!("{}: {e}", cmd[0].to_string_lossy()),
        )
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    Bash,
    Zsh,
    Json,
}

pub fn export(
    sys: &Sys,
    format: Format,
    dir: Option<PathBuf>,
    verbosity: Option<Verbosity>,
) -> Result<i32, Failure> {
    let dir = target(sys, dir)?;
    let loaded = load(sys, &dir, verbosity, |s| s.shell.is_none())?;
    let diff = loaded.finish.session.cumulative();
    let text = match format {
        Format::Bash => render(Shell::Bash, &diff),
        Format::Zsh => render(Shell::Zsh, &diff),
        Format::Json => render_json(&diff) + "\n",
    };
    unload(sys, loaded)?;
    print!("{text}");
    Ok(0)
}

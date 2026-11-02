// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! `foldl hook <shell>` and the per-prompt `foldl hook-step <shell>`.

use std::io::IsTerminal;

use foldl_core::config::Verbosity;
use foldl_core::env::Env;
use foldl_core::env::{EnvDiff, tracked};
use foldl_core::error::Failure;
use foldl_core::folder::Host;
use foldl_core::interactive;
use foldl_core::machine::{Ctx, Finish, Pending, recovery_script, resume, source_script, start};
use foldl_core::render::{Shell, quote, render};
use foldl_core::state::Session;

use crate::cli::IoContext;
use crate::interp::{RealWorld, drive};
use crate::runner::Runner;
use crate::sys::Sys;
use crate::{fsx, gather, session, settings, trust_cmd};

/// The shell code that installs the hook.
pub fn hook(sys: &Sys, shell: Shell) -> String {
    let exe = quote(sys.exe.as_os_str().as_encoded_bytes());
    let script = match shell {
        Shell::Bash => format!(
            r#"_foldl_hook() {{
  local previous_exit_status=$?
  trap '' INT
  eval "$({exe} hook-step bash --pid $$)"
  trap - INT
  return $previous_exit_status
}}
if [[ ";${{PROMPT_COMMAND:-}};" != *";_foldl_hook;"* ]]; then
  PROMPT_COMMAND="_foldl_hook${{PROMPT_COMMAND:+;$PROMPT_COMMAND}}"
fi
"#
        ),
        Shell::Zsh => format!(
            r#"_foldl_hook() {{
  setopt localoptions localtraps
  trap '' INT
  eval "$({exe} hook-step zsh --pid $$)"
}}
autoload -Uz add-zsh-hook
add-zsh-hook precmd _foldl_hook
add-zsh-hook chpwd _foldl_hook
"#
        ),
    };
    script + interactive::snapshot_function(shell)
}

/// The shell's report after sourcing an interactive migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Callback {
    pub name: String,
    pub status: i32,
}

/// One prompt: bring the session's loaded stack in line with the working
/// directory, and print the shell code that applies the change.
pub fn hook_step(
    sys: &Sys,
    shell: Shell,
    pid: u32,
    verbosity: Option<Verbosity>,
    callback: Option<Callback>,
) -> Result<i32, Failure> {
    let paths = sys.paths();
    let (session, inherited) = session::resolve(
        &paths,
        sys.var("FOLDL_SESSION").as_deref(),
        pid,
        Some(shell),
    );
    let is_new = !session::exists(&paths, &session.id);
    let pending = session.pending.clone();
    match (callback, pending) {
        (Some(cb), Some(p)) if p.source.name == cb.name => {
            return resumed(sys, shell, session, &p, cb.status, verbosity);
        }
        (None, Some(p)) => {
            print!("{}", recovery_script(&p, &sys.exe));
            return Ok(0);
        }
        _ => {}
    }
    let trust = trust_cmd::store(sys);
    let scan = gather::scan(&sys.cwd, &trust, &session.watched);
    if !is_new && session.scan.as_deref() == Some(scan.token.as_str()) {
        return Ok(0);
    }
    let config = settings::load(sys, verbosity);
    let host = Host {
        nix_available: sys.which("nix").is_some(),
    };
    let (mut infos, watched) = gather::infos(&scan, &trust, &config, host);
    // Fingerprints include watched files, which are known only now; rescan
    // so a failure is recorded against the fingerprint the next prompt sees.
    let scan = gather::scan(&sys.cwd, &trust, &watched);
    infos
        .iter_mut()
        .zip(&scan.folders)
        .for_each(|(info, (_, fp))| info.fingerprint = fp.clone());
    let actual = tracked(sys.env());
    let env = inherited.map_or_else(
        || actual.clone(),
        |diff| session::revert(actual.clone(), &diff),
    );
    let ctx = context(sys, shell, config);
    let runner = Runner::new(sys.exe.clone(), paths.session(&session.id))
        .ctx("signals")?
        .with_cache(paths.cache.clone());
    let mut world = RealWorld::new(&runner, sys, &session, &ctx.config);
    let (mut finish, events) = drive(&mut world, start(session, ctx, env, &infos));
    finish.session.scan = Some(scan.token);
    finish.session.watched = watched;
    output(sys, shell, finish, &events, is_new, &actual)
}

fn context(sys: &Sys, shell: Shell, config: foldl_core::config::Config) -> Ctx {
    let prompt = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    Ctx {
        paths: sys.paths(),
        config,
        shell: Some(shell),
        prompt,
        holders: true,
    }
}

/// Continues the transition after the shell sourced an interactive
/// migration; the snapshots are read and deleted.
fn resumed(
    sys: &Sys,
    shell: Shell,
    session: Session,
    pending: &Pending,
    status: i32,
    verbosity: Option<Verbosity>,
) -> Result<i32, Failure> {
    let paths = sys.paths();
    let read = |path: &std::path::Path| {
        std::fs::read_to_string(path)
            .ok()
            .map(|text| interactive::parse(&text))
    };
    let after = read(&pending.source.after).unwrap_or_default();
    // Interrupted before the first snapshot: no definition changes are
    // known, so none are undone (removing "everything new" would remove
    // all of the shell's functions).
    let before = read(&pending.source.before).unwrap_or(after.clone());
    let _ = fsx::remove(&pending.source.before);
    let _ = fsx::remove(&pending.source.after);
    let ctx = context(sys, shell, settings::load(sys, verbosity));
    let runner = Runner::new(sys.exe.clone(), paths.session(&session.id))
        .ctx("signals")?
        .with_cache(paths.cache.clone());
    let mut world = RealWorld::new(&runner, sys, &session, &ctx.config);
    let actual = tracked(sys.env());
    let (finish, events) = drive(
        &mut world,
        resume(session, ctx, actual.clone(), status, &before, &after),
    );
    output(sys, shell, finish, &events, false, &actual)
}

/// Saves the session and prints the shell code: exports, aliases and
/// interactive downs, and the next interactive migration to source.
fn output(
    sys: &Sys,
    shell: Shell,
    finish: Finish,
    events: &[String],
    is_new: bool,
    actual: &Env,
) -> Result<i32, Failure> {
    let paths = sys.paths();
    session::save(&paths, &finish.session).ctx("saving session")?;
    session::log_events(&paths, &finish.session.id, events).ctx("saving events")?;
    let export = match is_new {
        true => format!(
            "export FOLDL_SESSION={}\n",
            quote(finish.session.id.as_bytes())
        ),
        false => String::new(),
    };
    let mut dirs = finish.source.iter().filter_map(|s| s.before.parent());
    dirs.try_for_each(fsx::ensure_dir)
        .ctx("creating the snapshot directory")?;
    let source = finish
        .source
        .as_ref()
        .map(|s| source_script(s, &sys.exe))
        .unwrap_or_default();
    print!(
        "{export}{}{}{source}",
        render(shell, &EnvDiff::between(actual, &finish.env)),
        finish.shell_code
    );
    Ok(0)
}

// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Interactive migrations: sourced into the user's shell by the hook.
//!
//! Reaching one ends the run with a [`Source`]: the hook prints a script
//! that snapshots the shell's definitions, sources the saved `up.sh`,
//! snapshots again and calls `foldl hook-step --interactive-done`. The rest
//! of the transition waits in [`Pending`] until [`resume`] folds the
//! result back in.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Ctx, Effect, Finish, Machine, Op, Wait, complete_noted, fail_up};
use crate::env::{Env, tracked};
use crate::error::Code;
use crate::folder::ScriptShell;
use crate::interactive::{Defs, InteractiveDiff};
use crate::plan::Fresh;
use crate::render::{Shell, quote};
use crate::state::{Session, folder_key};

/// What the hook sources, and where the snapshots go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub name: String,
    pub shell: Shell,
    /// The saved `up.sh`; `None` when only `[aliases]` apply.
    pub script: Option<PathBuf>,
    pub root: PathBuf,
    pub state_dir: PathBuf,
    pub before: PathBuf,
    pub after: PathBuf,
    /// `alias` lines from the manifest's `[aliases]`.
    pub aliases: String,
}

/// The rest of a transition, waiting for an interactive migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub source: Source,
    fresh: Fresh,
    env_before: Env,
    queue: Vec<Op>,
    ups: Vec<String>,
    downs: Vec<String>,
    rolled_back: Vec<String>,
    rolling_back: bool,
    code: Code,
    fingerprints: BTreeMap<PathBuf, String>,
    forced: BTreeSet<(PathBuf, String)>,
}

/// An interactive up: skipped where it can't run, otherwise handed to the
/// shell.
pub(super) fn up(
    m: Machine,
    fresh: Fresh,
    before: Env,
    prepared: Env,
    saved: PathBuf,
    state_dir: PathBuf,
) -> (Machine, Vec<Effect>) {
    let script = fresh.migration.up.clone();
    let Some(shell) = m.ctx.shell else {
        let note = "skipped: interactive migrations load only in a shell with the hook".to_string();
        return complete_noted(m, fresh, before.clone(), before, 0, Some(note), None);
    };
    let other = match (shell, script.as_ref().map(|s| s.shebang.shell)) {
        (Shell::Bash, Some(ScriptShell::Zsh)) => Some("zsh"),
        (Shell::Zsh, Some(ScriptShell::Bash)) => Some("bash"),
        _ => None,
    };
    if let Some(other) = other {
        let note = format!("skipped: written for {other} (shebang)");
        return complete_noted(m, fresh, before.clone(), before, 0, Some(note), None);
    }
    let mut m = m;
    let dir = m.ctx.paths.session(&m.session.id).join("interactive");
    let stem = format!("{}-{}", fresh.migration.name, folder_key(&fresh.root));
    let aliases = fresh
        .migration
        .aliases
        .iter()
        .map(|(n, v)| format!("alias {n}={}\n", quote(v.as_bytes())))
        .collect();
    let source = Source {
        name: fresh.migration.name.clone(),
        shell,
        script: script.map(|s| saved.join(s.path)),
        root: fresh.root.clone(),
        state_dir,
        before: dir.join(format!("{stem}.before")),
        after: dir.join(format!("{stem}.after")),
        aliases,
    };
    m.session.pending = Some(Pending {
        source: source.clone(),
        fresh,
        env_before: before,
        queue: m.queue.drain(..).collect(),
        ups: std::mem::take(&mut m.ups),
        downs: std::mem::take(&mut m.downs),
        rolled_back: std::mem::take(&mut m.rolled_back),
        rolling_back: m.rolling_back,
        code: m.code,
        fingerprints: std::mem::take(&mut m.fingerprints),
        forced: std::mem::take(&mut m.forced),
    });
    m.env = prepared;
    let finish = Finish {
        session: m.session.clone(),
        start_env: m.start_env.clone(),
        env: m.env.clone(),
        shell_code: m.shell_code.clone(),
        code: m.code,
        source: Some(source),
    };
    (m, vec![Effect::Finish(Box::new(finish))])
}

/// Continues a transition once the shell has sourced an interactive
/// migration: `status` is its exit status, `env` the shell's environment
/// afterwards, `before` and `after` the snapshots.
pub fn resume(
    session: Session,
    ctx: Ctx,
    env: Env,
    status: i32,
    before: &Defs,
    after: &Defs,
) -> (Machine, Vec<Effect>) {
    let mut session = session;
    let pending = session.pending.take();
    let Some(p) = pending else {
        return super::start(session, ctx, env, &[]);
    };
    let env = tracked(env);
    let shell = p.source.shell;
    let m = Machine {
        session,
        ctx,
        start_env: env.clone(),
        env: env.clone(),
        queue: p.queue.into(),
        wait: Wait::Idle,
        ups: p.ups,
        downs: p.downs,
        rolled_back: p.rolled_back,
        rolling_back: p.rolling_back,
        shell_code: String::new(),
        code: p.code,
        fingerprints: p.fingerprints,
        forced: p.forced,
    };
    let defs = InteractiveDiff::between(shell, before, after);
    match status {
        0 => complete_noted(m, p.fresh, p.env_before, env, 0, None, Some(defs)),
        status => {
            let mut m = m;
            m.shell_code.push_str(&defs.undo());
            m.env = p.env_before;
            let code = match status {
                130 => Code::Interrupted,
                _ => Code::UpFailed,
            };
            let message = format!("{}: up failed (exit {status})", p.fresh.migration.name);
            fail_up(m, &p.fresh, code, message, vec![])
        }
    }
}

fn path(p: &Path) -> String {
    quote(p.as_os_str().as_encoded_bytes())
}

/// Calls foldl back with the result.
fn callback(source: &Source, exe: &Path, status: &str) -> String {
    format!(
        "eval \"$({} hook-step {} --pid $$ --interactive-done {} --status {status})\"\n",
        path(exe),
        source.shell.name(),
        quote(source.name.as_bytes())
    )
}

/// The script the hook evaluates to source an interactive migration. Ctrl-C
/// works while it runs; if it stops the script, the next prompt recovers.
pub fn source_script(source: &Source, exe: &Path) -> String {
    let sourced = source
        .script
        .as_ref()
        .map(|s| format!(". {}\n", path(s)))
        .unwrap_or_default();
    [
        format!("__foldl_snap {}\n", path(&source.before)),
        format!(
            "FOLDL_ROOT={}; FOLDL_STATE={}\n",
            path(&source.root),
            path(&source.state_dir)
        ),
        "trap - INT\n__foldl_status=0\n".to_string(),
        sourced,
        "__foldl_status=$?\ntrap '' INT\n".to_string(),
        source.aliases.clone(),
        "unset FOLDL_ROOT FOLDL_STATE\n".to_string(),
        format!("__foldl_snap {}\n", path(&source.after)),
        callback(source, exe, "$__foldl_status"),
    ]
    .concat()
}

/// When sourcing was interrupted: take the "after" snapshot now and report
/// it as Ctrl-C, so the partial changes roll back.
pub fn recovery_script(pending: &Pending, exe: &Path) -> String {
    format!(
        "__foldl_snap {}\n{}",
        path(&pending.source.after),
        callback(&pending.source, exe, "130")
    )
}

#[cfg(test)]
mod tests;

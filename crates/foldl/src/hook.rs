// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! `foldl hook <shell>` and the per-prompt `foldl hook-step <shell>`.

use std::io::IsTerminal;

use foldl_core::config::Verbosity;
use foldl_core::env::{EnvDiff, tracked};
use foldl_core::error::Failure;
use foldl_core::folder::Host;
use foldl_core::machine::{Ctx, start};
use foldl_core::render::{Shell, quote, render};

use crate::cli::IoContext;
use crate::interp::{RealWorld, drive};
use crate::runner::Runner;
use crate::sys::Sys;
use crate::{gather, session, settings, trust_cmd};

/// The shell code that installs the hook.
pub fn hook(sys: &Sys, shell: Shell) -> String {
    let exe = quote(sys.exe.as_os_str().as_encoded_bytes());
    match shell {
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
    }
}

/// One prompt: bring the session's loaded stack in line with the working
/// directory, and print the shell code that applies the change.
pub fn hook_step(
    sys: &Sys,
    shell: Shell,
    pid: u32,
    verbosity: Option<Verbosity>,
) -> Result<i32, Failure> {
    let paths = sys.paths();
    let (session, inherited) = session::resolve(
        &paths,
        sys.var("FOLDL_SESSION").as_deref(),
        pid,
        Some(shell),
    );
    let is_new = !session::exists(&paths, &session.id);
    let trust = trust_cmd::store(sys);
    let scan = gather::scan(&sys.cwd, &trust, &session.watched);
    if !is_new && session.scan.as_deref() == Some(scan.token.as_str()) {
        return Ok(0);
    }
    let config = settings::load(sys, verbosity);
    let host = Host {
        nix_available: sys.which("nix").is_some(),
    };
    let (infos, watched) = gather::infos(&scan, &trust, &config, host);
    let actual = tracked(sys.env());
    let env = inherited.map_or_else(
        || actual.clone(),
        |diff| session::revert(actual.clone(), &diff),
    );
    let prompt = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let ctx = Ctx {
        paths: paths.clone(),
        config,
        shell: Some(shell),
        prompt,
    };
    let runner = Runner::new(sys.exe.clone(), paths.session(&session.id)).ctx("signals")?;
    let (finish, events) = drive(
        &mut RealWorld { runner: &runner },
        start(session, ctx, env, &infos),
    );
    let mut session = finish.session;
    session.scan = Some(scan.token);
    session.watched = watched;
    session::save(&paths, &session).ctx("saving session")?;
    session::log_events(&paths, &session.id, &events).ctx("saving events")?;
    let export = match is_new {
        true => format!("export FOLDL_SESSION={}\n", quote(session.id.as_bytes())),
        false => String::new(),
    };
    print!(
        "{export}{}{}",
        render(shell, &EnvDiff::between(&actual, &finish.env)),
        finish.shell_code
    );
    Ok(0)
}

// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Agent support: named sessions owned by a long-lived agent process, and
//! the Claude Code integration.
//!
//! An agent session has no shell; its env reaches the agent's commands as
//! text (`foldl export`, or `CLAUDE_ENV_FILE`). Its holders follow the
//! agent's PID, so leases last as long as the agent and are released when
//! it exits.

use std::io::Read;
use std::path::PathBuf;

use foldl_core::env::{EnvDiff, tracked};
use foldl_core::error::{Code, Failure};
use foldl_core::machine::{Ctx, start};
use foldl_core::render::{Shell, quote, render};
use foldl_core::state::Session;
use serde_json::{Value, json};

use crate::cli::IoContext;
use crate::interp::{RealWorld, drive};
use crate::runner::Runner;
use crate::sys::Sys;
use crate::{fsx, session, settings};

/// The process that ran foldl.
fn parent_pid() -> u32 {
    rustix::process::getppid().map_or(1, |p| p.as_raw_nonzero().get() as u32)
}

/// `foldl session start`: a session for `parent` (default: the caller).
pub fn start_session(
    sys: &Sys,
    id: Option<String>,
    parent: Option<u32>,
    json: bool,
) -> Result<i32, Failure> {
    let paths = sys.paths();
    let parent = parent.unwrap_or_else(parent_pid);
    let id = id.unwrap_or_else(|| session::new_id(parent));
    if !session::valid_id(&id) {
        let message = format!("invalid session id {}", quote(id.as_bytes()));
        let fix = "use letters, digits, '.', '_' and '-'";
        return Err(Failure::new(Code::Usage, message).with_fix(fix));
    }
    let existing = session::load(&paths, &id);
    let session = existing.unwrap_or_else(|| Session::new(id.clone(), parent, None));
    session::save(&paths, &session).ctx("saving session")?;
    let text = match json {
        true => json!({"session": id, "parent": session.parent}).to_string() + "\n",
        false => format!("export FOLDL_SESSION={}\n", quote(id.as_bytes())),
    };
    print!("{text}");
    Ok(0)
}

/// Moves an agent session to `infos` (a folder's stack, or nothing);
/// returns the env change and the outcome.
fn transition(
    sys: &Sys,
    session: Session,
    infos: &[foldl_core::plan::FolderInfo],
) -> Result<(EnvDiff, Code), Failure> {
    let paths = sys.paths();
    let config = settings::load(sys, None);
    let before = session.cumulative().apply(tracked(sys.env()));
    let ctx = Ctx {
        paths: paths.clone(),
        config: config.clone(),
        shell: None,
        prompt: false,
        holders: true,
    };
    let runner = Runner::new(sys.exe.clone(), paths.session(&session.id))
        .ctx("signals")?
        .with_cache(paths.cache.clone());
    let mut world = RealWorld::new(&runner, sys, &session, &config);
    let (finish, _) = drive(&mut world, start(session, ctx, before.clone(), infos));
    session::save(&paths, &finish.session).ctx("saving session")?;
    Ok((EnvDiff::between(&before, &finish.env), finish.code))
}

/// `foldl session stop`: unloads everything and ends the session.
pub fn stop_session(sys: &Sys, id: Option<String>) -> Result<i32, Failure> {
    let paths = sys.paths();
    let missing = Failure::new(Code::Usage, "no such session").with_fix("foldl session start");
    let id = id.or(sys.var("FOLDL_SESSION")).ok_or(missing.clone())?;
    let session = session::load(&paths, &id).ok_or(missing)?;
    let (_, code) = transition(sys, session, &[])?;
    session::remove(&paths, &id);
    Ok(code.exit())
}

/// An agent tool `foldl integrate` sets up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
}

impl Agent {
    /// The subcommand, the session prefix and the agent's process name.
    fn name(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
        }
    }

    /// Where the hooks go, under the project or the home directory.
    fn settings(self, base: PathBuf) -> PathBuf {
        match self {
            Agent::Claude => base.join(".claude/settings.json"),
        }
    }

    fn events(self) -> &'static [&'static str] {
        match self {
            Agent::Claude => &["SessionStart", "CwdChanged"],
        }
    }
}

/// `foldl integrate <agent>`: adds foldl's hooks to the agent's settings.
pub fn integrate(sys: &Sys, agent: Agent, project: bool) -> Result<i32, Failure> {
    let base = match project {
        true => sys.cwd.clone(),
        false => PathBuf::from(sys.var("HOME").unwrap_or_default()),
    };
    let path = agent.settings(base);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| "{}".to_string());
    let mut settings: Value = serde_json::from_str(&text)
        .map_err(|e| Failure::new(Code::Invalid, format!("{}: {e}", path.display())))?;
    let command = format!(
        "{} integrate {} --hook",
        quote(sys.exe.as_os_str().as_encoded_bytes()),
        agent.name()
    );
    let added: Vec<&str> = agent
        .events()
        .iter()
        .copied()
        .filter(|event| add_hook(&mut settings, event, &command))
        .collect();
    fsx::write_json(&path, &settings).ctx(&format!("writing {}", path.display()))?;
    eprintln!(
        "foldl: {}: {}",
        path.display(),
        match added.is_empty() {
            true => "hooks already installed".to_string(),
            false => format!("added {} hooks", added.join(" and ")),
        }
    );
    Ok(0)
}

/// Adds `command` under `hooks.<event>` unless it is there; true if added.
fn add_hook(settings: &mut Value, event: &str, command: &str) -> bool {
    let groups = &mut settings["hooks"][event];
    let present = groups
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|g| g["hooks"].as_array().into_iter().flatten())
        .any(|h| h["command"] == command);
    let entry = json!({"hooks": [{"type": "command", "command": command}]});
    match (present, groups.as_array_mut()) {
        (true, _) => false,
        (false, Some(list)) => {
            list.push(entry);
            true
        }
        (false, None) => {
            *groups = json!([entry]);
            true
        }
    }
}

/// The nearest ancestor named `name`, else `start`.
fn ancestor_named(start: u32, name: &str) -> u32 {
    let ancestry = std::iter::successors(Some(start), |pid| {
        let out = std::process::Command::new("ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .ok()
            .filter(|p: &u32| *p > 1)
    });
    let named = |pid: &u32| {
        let out = std::process::Command::new("ps")
            .args(["-o", "comm=", "-p", &pid.to_string()])
            .output();
        let comm = out
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        comm.rsplit('/').next() == Some(name)
    };
    ancestry.take(8).find(named).unwrap_or(start)
}

/// What a hook run did: the agent session's id, whether it was new, the
/// env change and how loading went.
struct HookRun {
    id: String,
    first: bool,
    diff: EnvDiff,
    code: Code,
}

/// The common part of every agent's hook: reads the event from stdin and
/// loads its `cwd` into the session `<agent>-<session_id>`, owned by the
/// nearest ancestor process named after the agent, so leases are held for
/// as long as the agent runs.
fn hook_run(sys: &Sys, agent: Agent) -> Result<HookRun, Failure> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .ctx("reading the hook input")?;
    let event: Value = serde_json::from_str(&input)
        .map_err(|e| Failure::new(Code::Usage, format!("hook input: {e}")))?;
    let agent_session = event["session_id"]
        .as_str()
        .filter(|s| session::valid_id(s));
    let id = format!("{}-{}", agent.name(), agent_session.unwrap_or("unknown"));
    let cwd = event["cwd"].as_str().map_or(sys.cwd.clone(), PathBuf::from);
    let paths = sys.paths();
    let existing = session::load(&paths, &id);
    let first = existing.is_none();
    let owner = || ancestor_named(parent_pid(), agent.name());
    let session = existing.unwrap_or_else(|| Session::new(id.clone(), owner(), None));
    let infos = crate::oneshot::infos(sys, &cwd, &settings::load(sys, None));
    let (diff, code) = transition(sys, session, &infos)?;
    Ok(HookRun {
        id,
        first,
        diff,
        code,
    })
}

/// `foldl integrate claude --hook`: run by Claude Code at `SessionStart`
/// and `CwdChanged`. Loads the new directory's stack into the Claude
/// session's agent session and appends the env change to
/// `CLAUDE_ENV_FILE`, which every Bash command sources in order.
pub fn claude_hook(sys: &Sys) -> Result<i32, Failure> {
    let env_file = sys
        .var("CLAUDE_ENV_FILE")
        .ok_or(Failure::new(Code::Usage, "CLAUDE_ENV_FILE is not set"))?;
    let run = hook_run(sys, Agent::Claude)?;
    let export = match run.first {
        true => format!("export FOLDL_SESSION={}\n", quote(run.id.as_bytes())),
        false => String::new(),
    };
    let text = export + &render(Shell::Bash, &run.diff);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&env_file)
        .ctx("opening CLAUDE_ENV_FILE")?;
    std::io::Write::write_all(&mut file, text.as_bytes()).ctx("writing CLAUDE_ENV_FILE")?;
    Ok(run.code.exit())
}

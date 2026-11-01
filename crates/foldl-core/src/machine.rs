// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! The transition machine: `step(machine, event) -> (machine, effects)`.
//!
//! [`start`] plans a transition and returns the first effects. The
//! interpreter performs them in order; at most the last effect of a batch
//! produces an [`Event`], which goes back into [`step`]. A batch ending in
//! [`Effect::Finish`] ends the run.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::{Config, Verbosity};
use crate::env::{Env, EnvDiff, Value, tracked};
use crate::error::{Code, Failure};
use crate::folder::{Builtin, Shebang, expand};
use crate::manifest::format_duration;
use crate::plan::{FolderInfo, Fresh, Notice, Plan, plan};
use crate::render::{Shell, quote};
use crate::state::{Loaded, Paths, SavedScript, Session};

/// Settings for one run, passed in rather than read from the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ctx {
    pub paths: Paths,
    pub config: Config,
    /// The shell aliases and interactive migrations go to; `None` for
    /// `exec`, `export` and holders.
    pub shell: Option<Shell>,
    /// Whether foldl may ask the user questions.
    pub prompt: bool,
}

/// An interactive migration waiting for its result (milestone M3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {}

/// What a script or built-in run is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

/// What to execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exec {
    /// Source `path` in the shell its shebang names, then capture the
    /// environment.
    Script {
        shebang: Shebang,
        path: PathBuf,
    },
    Builtin(Builtin),
}

/// A script or built-in to run in a subprocess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub name: String,
    pub direction: Direction,
    pub exec: Exec,
    pub env: Env,
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub log: PathBuf,
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Exit status; the environment afterwards when it was captured.
    Exited {
        status: i32,
        env: Option<Env>,
    },
    TimedOut,
    Interrupted,
    /// The program (a shell, `nix`) is missing.
    Unavailable(String),
}

/// The result of an [`Effect::Run`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub outcome: Outcome,
    /// Last lines of the log.
    pub tail: Vec<String>,
    pub millis: u64,
}

/// The end of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finish {
    pub session: Session,
    /// The environment at the start and at the end.
    pub start_env: Env,
    pub env: Env,
    /// Shell code beyond `export`/`unset`: aliases and interactive downs.
    pub shell_code: String,
    pub code: Code,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// A line (or lines) for the user, on stderr.
    Print(String),
    /// Copy the migration directory to `saved` (replacing it), create the
    /// state directory and the log's directory. Answered by `Done`.
    Prepare {
        from: PathBuf,
        saved: PathBuf,
        state_dir: PathBuf,
        log: PathBuf,
    },
    /// Answered by `Ran`.
    Run(Box<Run>),
    /// Delete a saved copy. No answer.
    Remove(PathBuf),
    Finish(Box<Finish>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Done,
    /// An effect failed; the message says what and where.
    Io(String),
    Ran(Ran),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    /// Unload the top of the loaded stack.
    Down,
    Up(Box<Fresh>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Wait {
    Idle,
    Prepared(Box<Fresh>),
    UpRan(Box<Fresh>, Env),
    DownRan(Box<Loaded>),
}

/// One run's accumulated value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub session: Session,
    ctx: Ctx,
    env: Env,
    start_env: Env,
    queue: VecDeque<Op>,
    wait: Wait,
    ups: Vec<String>,
    downs: Vec<String>,
    shell_code: String,
    code: Code,
    fingerprints: BTreeMap<PathBuf, String>,
}

/// Plans the transition from what the session has loaded to what
/// `folders` asks for, and returns the first effects.
pub fn start(
    session: Session,
    ctx: Ctx,
    env: Env,
    folders: &[FolderInfo],
) -> (Machine, Vec<Effect>) {
    let Plan {
        downs,
        ups,
        notices,
    } = plan(&session, folders);
    let mut session = session;
    let fingerprints: BTreeMap<PathBuf, String> = folders
        .iter()
        .map(|f| (f.root.clone(), f.fingerprint.clone()))
        .collect();
    session
        .failed
        .retain(|root, _| fingerprints.contains_key(root));
    session
        .notified
        .retain(|root, fp| fingerprints.get(root) == Some(fp));
    let marks = notices
        .iter()
        .filter_map(|n| Some((n.root().clone(), fingerprints.get(n.root())?.clone())));
    session.notified.extend(marks);
    let home = env.get("HOME").map(Value::lossy).unwrap_or_default();
    let code = notices
        .iter()
        .fold(Code::Ok, |code, n| code.worst(notice_code(n)));
    let printed = notices.iter().map(|n| Effect::Print(notice_text(n, &home)));
    let env = tracked(env);
    let queue =
        std::iter::repeat_n(Op::Down, downs).chain(ups.into_iter().map(|f| Op::Up(Box::new(f))));
    let machine = Machine {
        session,
        ctx,
        start_env: env.clone(),
        env,
        queue: queue.collect(),
        wait: Wait::Idle,
        ups: vec![],
        downs: vec![],
        shell_code: String::new(),
        code,
        fingerprints,
    };
    then(printed.collect(), advance(machine))
}

/// Folds one event into the machine.
pub fn step(m: Machine, event: Event) -> (Machine, Vec<Effect>) {
    let mut m = m;
    let wait = std::mem::replace(&mut m.wait, Wait::Idle);
    match (wait, event) {
        (Wait::Prepared(fresh), Event::Done) => run_up(m, *fresh),
        (Wait::Prepared(fresh), Event::Io(message)) => {
            fail_up(m, &fresh, Code::Internal, message, vec![])
        }
        (Wait::UpRan(fresh, before), Event::Ran(ran)) => up_ran(m, *fresh, before, ran),
        (Wait::DownRan(loaded), Event::Ran(ran)) => then(
            down_warning(&loaded.name, &ran).into_iter().collect(),
            finish_down(m, *loaded),
        ),
        (wait, event) => {
            let message =
                format!("internal error: unexpected {event:?} while waiting for {wait:?}");
            m.code = m.code.worst(Code::Internal);
            m.queue.clear();
            then(
                vec![Effect::Print(format!("foldl ✗ {message}\n"))],
                advance(m),
            )
        }
    }
}

/// Starts the next op, or finishes.
fn advance(m: Machine) -> (Machine, Vec<Effect>) {
    let mut m = m;
    match m.queue.pop_front() {
        Some(Op::Down) => start_down(m),
        Some(Op::Up(fresh)) => start_up(m, *fresh),
        None => finish(m),
    }
}

fn finish(m: Machine) -> (Machine, Vec<Effect>) {
    let summary = summary(&m);
    let finish = Finish {
        session: m.session.clone(),
        start_env: m.start_env.clone(),
        env: m.env.clone(),
        shell_code: m.shell_code.clone(),
        code: m.code,
    };
    let printed = summary.map(Effect::Print);
    (
        m,
        printed
            .into_iter()
            .chain([Effect::Finish(Box::new(finish))])
            .collect(),
    )
}

fn summary(m: &Machine) -> Option<String> {
    let line = |arrow: &str, items: &[String]| match items {
        [] => String::new(),
        _ => format!("foldl {arrow} {}\n", items.join(" · ")),
    };
    let text = line("↓", &m.downs) + &line("↑", &m.ups);
    (m.ctx.config.verbosity != Verbosity::Quiet && !text.is_empty()).then_some(text)
}

/// Where a migration of this session keeps its saved copy, state and log.
fn dirs(m: &Machine, root: &Path, name: &str) -> (PathBuf, PathBuf, PathBuf) {
    Paths::migration_dirs(&m.ctx.paths.session(&m.session.id), root, name)
}

fn start_up(m: Machine, fresh: Fresh) -> (Machine, Vec<Effect>) {
    let mut m = m;
    let (saved, state_dir, log) = dirs(&m, &fresh.root, &fresh.migration.name);
    let from = fresh.root.join(".foldl").join(&fresh.migration.dir);
    m.wait = Wait::Prepared(Box::new(fresh));
    (
        m,
        vec![Effect::Prepare {
            from,
            saved,
            state_dir,
            log,
        }],
    )
}

/// The environment a migration's own steps produce before its script:
/// `path_add`, `[env]`, `unset`, then `[ports]`.
fn prepared_env(
    env: &Env,
    fresh: &Fresh,
    state_dir: &Path,
    ports: &BTreeMap<String, String>,
) -> Env {
    let m = &fresh.migration;
    let root = fresh.root.to_string_lossy().into_owned();
    let added: Vec<String> = m.path_add.iter().map(|p| format!("{root}/{p}")).collect();
    let old_path = env.get("PATH").map(Value::lossy);
    let path = added
        .into_iter()
        .chain(old_path)
        .collect::<Vec<_>>()
        .join(":");
    let mut env = env.clone();
    if !m.path_add.is_empty() {
        env.insert("PATH".into(), Value::from(path.as_str()));
    }
    let home = env.get("HOME").map(Value::lossy).unwrap_or_default();
    let vars: BTreeMap<String, String> = [
        ("FOLDL_ROOT".to_string(), root),
        (
            "FOLDL_STATE".to_string(),
            state_dir.to_string_lossy().into_owned(),
        ),
        ("HOME".to_string(), home),
    ]
    .into_iter()
    .chain(ports.clone())
    .collect();
    env.extend(
        m.env
            .iter()
            .map(|(name, pieces)| (name.clone(), Value::from(expand(pieces, &vars).as_str()))),
    );
    m.unset.iter().for_each(|name| {
        env.remove(name);
    });
    env.extend(
        ports
            .iter()
            .map(|(name, port)| (name.clone(), Value::from(port.as_str()))),
    );
    env
}

/// Adds the variables foldl passes to every script.
fn script_env(env: &Env, root: &Path, state_dir: &Path) -> Env {
    let mut env = env.clone();
    env.insert("FOLDL_ROOT".into(), Value::from(&*root.to_string_lossy()));
    env.insert(
        "FOLDL_STATE".into(),
        Value::from(&*state_dir.to_string_lossy()),
    );
    env
}

fn run_up(m: Machine, fresh: Fresh) -> (Machine, Vec<Effect>) {
    let mut m = m;
    let (saved, state_dir, log) = dirs(&m, &fresh.root, &fresh.migration.name);
    let before = m.env.clone();
    let prepared = prepared_env(&m.env, &fresh, &state_dir, &BTreeMap::new());
    let migration = &fresh.migration;
    let exec = match (&migration.builtin, &migration.up) {
        (Some(builtin), _) => Exec::Builtin(builtin.clone()),
        (None, Some(script)) => Exec::Script {
            shebang: script.shebang.clone(),
            path: saved.join(&script.path),
        },
        (None, None) => return complete_up(m, fresh, before, prepared, 0),
    };
    let default = match migration.builtin {
        Some(Builtin::NixFlake { .. }) => m.ctx.config.nix_timeout,
        _ => m.ctx.config.timeout,
    };
    let run = Run {
        name: migration.name.clone(),
        direction: Direction::Up,
        exec,
        env: script_env(&prepared, &fresh.root, &state_dir),
        cwd: fresh.root.clone(),
        timeout: migration.timeout.unwrap_or(default),
        log,
    };
    m.wait = Wait::UpRan(Box::new(fresh), before);
    (m, vec![Effect::Run(Box::new(run))])
}

fn up_ran(m: Machine, fresh: Fresh, before: Env, ran: Ran) -> (Machine, Vec<Effect>) {
    let timeout = fresh.migration.timeout.unwrap_or(m.ctx.config.timeout);
    match ran.outcome {
        Outcome::Exited {
            status: 0,
            env: Some(after),
        } => complete_up(m, fresh, before, tracked(after), ran.millis),
        Outcome::Exited {
            status: 0,
            env: None,
        } => {
            let message = format!(
                "{}: up exited without reporting its environment",
                fresh.migration.name
            );
            fail_up(m, &fresh, Code::UpFailed, message, ran.tail)
        }
        Outcome::Exited { status, .. } => {
            let message = format!("{}: up failed (exit {status})", fresh.migration.name);
            fail_up(m, &fresh, Code::UpFailed, message, ran.tail)
        }
        Outcome::TimedOut => {
            let message = format!(
                "{}: up timed out after {}",
                fresh.migration.name,
                format_duration(timeout)
            );
            fail_up(m, &fresh, Code::Timeout, message, ran.tail)
        }
        Outcome::Interrupted => {
            let message = format!("{}: interrupted", fresh.migration.name);
            fail_up(m, &fresh, Code::Interrupted, message, ran.tail)
        }
        Outcome::Unavailable(what) => {
            let message = format!("{}: {what}", fresh.migration.name);
            fail_up(m, &fresh, Code::Unavailable, message, ran.tail)
        }
    }
}

fn complete_up(
    m: Machine,
    fresh: Fresh,
    before: Env,
    after: Env,
    millis: u64,
) -> (Machine, Vec<Effect>) {
    let mut m = m;
    let (saved, state_dir, log) = dirs(&m, &fresh.root, &fresh.migration.name);
    let migration = fresh.migration;
    let diff = EnvDiff::between(&before, &after);
    let aliases: Vec<String> = match m.ctx.shell {
        Some(_) => migration.aliases.keys().cloned().collect(),
        None => vec![],
    };
    let alias_code: String = aliases
        .iter()
        .map(|name| {
            format!(
                "alias {name}={}\n",
                quote(migration.aliases[name].as_bytes())
            )
        })
        .collect();
    m.shell_code.push_str(&alias_code);
    m.ups.push(up_item(
        &migration.name,
        &diff,
        millis,
        m.ctx.config.verbosity,
    ));
    let down = migration.down.map(|s| SavedScript {
        path: saved.join(&s.path),
        shebang: s.shebang,
    });
    let down_timeout = migration.down_timeout.unwrap_or(m.ctx.config.down_timeout);
    m.session.loaded.push(Loaded {
        root: fresh.root,
        name: migration.name,
        hash: migration.hash,
        watch: fresh.watch,
        scope: migration.scope,
        lease: None,
        diff,
        aliases,
        interactive: None,
        down,
        down_timeout_ms: down_timeout.as_millis() as u64,
        saved,
        state_dir,
        log,
        note: None,
    });
    m.env = after;
    advance(m)
}

fn up_item(name: &str, diff: &EnvDiff, millis: u64, verbosity: Verbosity) -> String {
    let vars = match (diff.len(), verbosity) {
        (0, _) => None,
        (_, Verbosity::Verbose) => Some(diff.summary()),
        (1, _) => Some("+1 var".to_string()),
        (n, _) => Some(format!("+{n} vars")),
    };
    let time = (millis >= 100).then(|| format!("{:.1}s", millis as f64 / 1000.0));
    let details: Vec<String> = vars.into_iter().chain(time).collect();
    match details.as_slice() {
        [] => name.to_string(),
        _ => format!("{name} ({})", details.join(", ")),
    }
}

/// An up failed: report it, mark the folder failed and unload what of it
/// already loaded, in reverse.
fn fail_up(
    m: Machine,
    fresh: &Fresh,
    code: Code,
    message: String,
    tail: Vec<String>,
) -> (Machine, Vec<Effect>) {
    let mut m = m;
    m.code = m.code.worst(code);
    let fingerprint = m.fingerprints.get(&fresh.root).cloned().unwrap_or_default();
    m.session.failed.insert(fresh.root.clone(), fingerprint);
    let loaded_here = m
        .session
        .loaded
        .iter()
        .rev()
        .take_while(|l| l.root == fresh.root)
        .count();
    m.queue = std::iter::repeat_n(Op::Down, loaded_here).collect();
    let text = failure_text(&message, &fresh.migration.name, &tail);
    then(vec![Effect::Print(text)], advance(m))
}

/// Effects `first`, then those of the continuation.
fn then(first: Vec<Effect>, (m, rest): (Machine, Vec<Effect>)) -> (Machine, Vec<Effect>) {
    (m, first.into_iter().chain(rest).collect())
}

fn failure_text(message: &str, name: &str, tail: &[String]) -> String {
    let lines: String = tail.iter().map(|l| format!("  │ {l}\n")).collect();
    format!("foldl ✗ {message}\n{lines}  full log: foldl logs {name}\n")
}

fn start_down(m: Machine) -> (Machine, Vec<Effect>) {
    let mut m = m;
    let Some(loaded) = m.session.loaded.pop() else {
        return advance(m);
    };
    match &loaded.down {
        Some(script) => {
            let run = Run {
                name: loaded.name.clone(),
                direction: Direction::Down,
                exec: Exec::Script {
                    shebang: script.shebang.clone(),
                    path: script.path.clone(),
                },
                env: script_env(&m.env, &loaded.root, &loaded.state_dir),
                cwd: loaded.root.clone(),
                timeout: Duration::from_millis(loaded.down_timeout_ms),
                log: loaded.log.clone(),
            };
            m.wait = Wait::DownRan(Box::new(loaded));
            (m, vec![Effect::Run(Box::new(run))])
        }
        None => finish_down(m, loaded),
    }
}

/// Warns about a custom down that didn't succeed. The derived down still
/// applies.
fn down_warning(name: &str, ran: &Ran) -> Option<Effect> {
    let what = match &ran.outcome {
        Outcome::Exited { status: 0, .. } => return None,
        Outcome::Exited { status, .. } => format!("exit {status}"),
        Outcome::TimedOut => "timed out".to_string(),
        Outcome::Interrupted => "interrupted".to_string(),
        Outcome::Unavailable(what) => what.clone(),
    };
    Some(Effect::Print(failure_text(
        &format!("{name}: down failed ({what})"),
        name,
        &ran.tail,
    )))
}

fn finish_down(m: Machine, loaded: Loaded) -> (Machine, Vec<Effect>) {
    let mut m = m;
    m.env = loaded.diff.invert().apply(std::mem::take(&mut m.env));
    let unalias: String = loaded
        .aliases
        .iter()
        .map(|a| format!("unalias {a} 2>/dev/null\n"))
        .collect();
    m.shell_code.push_str(&unalias);
    m.downs.push(loaded.name.clone());
    then(vec![Effect::Remove(loaded.saved)], advance(m))
}

fn notice_code(notice: &Notice) -> Code {
    match notice {
        Notice::Untrusted { .. } => Code::Untrusted,
        Notice::Denied { .. } => Code::Denied,
        Notice::Invalid { .. } => Code::Invalid,
        Notice::ChangedWhileLoaded { .. } | Notice::SharedChanged { .. } => Code::Ok,
    }
}

/// `/home/me/x` as `~/x`.
pub fn tilde(path: &Path, home: &str) -> String {
    let text = path.to_string_lossy();
    match text.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => text.into_owned(),
    }
}

fn notice_text(notice: &Notice, home: &str) -> String {
    match notice {
        Notice::Untrusted { root, changed } => {
            let shown = tilde(root, home);
            let what = match changed {
                true => format!("{shown}/.foldl changed since it was allowed"),
                false => format!("{shown} is not allowed"),
            };
            Failure::new(Code::Untrusted, what)
                .with_fix(format!("foldl allow {shown}"))
                .human()
        }
        Notice::Denied { root } => format!(
            "foldl: {} is denied; `foldl allow` loads it\n",
            tilde(root, home)
        ),
        Notice::Invalid { root, issues } => {
            let shown = tilde(root, home);
            let lines: String = issues
                .iter()
                .map(|i| format!("  {}\n", i.display()))
                .collect();
            format!(
                "foldl ✗ {shown}/.foldl is invalid\n{lines}  fix: edit it, then run foldl check {shown}\n"
            )
        }
        Notice::ChangedWhileLoaded { root } => {
            let shown = tilde(root, home);
            format!("foldl ! {shown}/.foldl changed; run 'foldl allow {shown}' to reload it\n")
        }
        Notice::SharedChanged { name, .. } => {
            format!("foldl ! {name} changed; run 'foldl redo {name}'\n")
        }
    }
}

#[cfg(test)]
mod tests;

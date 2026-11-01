// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Shared migrations: the lease protocol as states of the machine.
//!
//! Enter, under the lease's mutex: a session that doesn't hold the lease
//! tries `LOCK_EX | LOCK_NB`; granted means no other session holds it, so
//! anything still recorded as up is stale and torn down first. Then the
//! migration is joined (its env replayed) if it is up, or started. The
//! lease state is written and a lease holder keeps a shared lock.
//!
//! Leave, under the mutex: the session's holder is stopped once nothing
//! else it has loaded uses the lease; if `LOCK_EX | LOCK_NB` is then
//! granted, this session was last and runs the saved down.

use std::collections::BTreeMap;
use std::time::Duration;

use super::{
    Direction, Effect, Event, Exec, Machine, Outcome, Ran, Shared, Wait as Outer, advance, fail_up,
    failure_text, finish_down, prepared_env, script_env, vars_detail,
};
use crate::config::OnDirty;
use crate::env::{Env, EnvDiff, Value, tracked};
use crate::error::Code;
use crate::folder::Builtin;
use crate::lease::{Dirty, Entry, Holder, LeaseRef, LeaseState};
use crate::manifest::format_duration;
use crate::plan::Fresh;
use crate::state::{Loaded, Paths, SavedScript};

/// What a session knows about a lease while it holds the mutex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    lease: LeaseRef,
    state: LeaseState,
    /// This session already holds the lease through a lease holder.
    held: bool,
    holders: Vec<Holder>,
}

// One value per run, boxed by the outer machine; the size difference
// between loading and unloading doesn't matter.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wait {
    Up(Fresh, Up),
    Down(Loaded, Down),
}

/// Loading a shared migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Up {
    Locked(LeaseRef),
    Inspected(LeaseRef, bool),
    /// Tearing down stale entries; the rest to go.
    Stale(Lease, String, Vec<String>),
    Answer(Lease),
    Retried(Lease),
    Ports(Lease),
    Prepared(Lease, BTreeMap<String, u16>),
    Ran(Lease, Env, BTreeMap<String, u16>),
    /// The up failed or was aborted; fail once the mutex is released.
    Aborted(Code, String, Vec<String>),
    Committed(Box<Loaded>, String),
}

/// Unloading a shared migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Down {
    Locked(LeaseRef),
    Released(LeaseRef),
    Inspected(LeaseRef),
    Ran(Lease),
    Committed(Option<String>),
}

fn wait(m: Machine, w: Wait, effects: Vec<Effect>) -> (Machine, Vec<Effect>) {
    let mut m = m;
    m.wait = Outer::Shared(Box::new(w));
    (m, effects)
}

pub fn start_up(m: Machine, fresh: Fresh, lease: LeaseRef) -> (Machine, Vec<Effect>) {
    wait(
        m,
        Wait::Up(fresh, Up::Locked(lease.clone())),
        vec![Effect::Shared(Shared::Lock(lease))],
    )
}

pub fn start_down(m: Machine, loaded: Loaded, lease: LeaseRef) -> (Machine, Vec<Effect>) {
    wait(
        m,
        Wait::Down(loaded, Down::Locked(lease.clone())),
        vec![Effect::Shared(Shared::Lock(lease))],
    )
}

pub fn fold(m: Machine, w: Wait, event: Event) -> (Machine, Vec<Effect>) {
    let mut m = m;
    match (w, event) {
        (Wait::Up(fresh, Up::Locked(lease)), Event::Done) => {
            let held = m.session.leases.contains_key(&lease.id);
            let inspect = Effect::Shared(Shared::Inspect {
                lease: lease.clone(),
                exclusive: !held,
            });
            wait(
                m,
                Wait::Up(fresh, Up::Inspected(lease, held)),
                vec![inspect],
            )
        }
        (
            Wait::Up(fresh, Up::Inspected(lease, held)),
            Event::Inspected {
                exclusive,
                state,
                holders,
            },
        ) => {
            let lease = Lease {
                lease,
                state,
                held,
                holders,
            };
            let stale = match exclusive {
                true => lease.state.newest_first(),
                false => vec![],
            };
            next_stale(m, fresh, lease, stale)
        }
        (Wait::Up(fresh, Up::Stale(mut lease, name, rest)), Event::Ran(ran)) => {
            let freed = settle_down(&mut lease, &name, &ran);
            then(freed, next_stale(m, fresh, lease, rest))
        }
        (Wait::Up(fresh, Up::Answer(lease)), Event::Answer(answer)) => {
            dirty_choice(m, fresh, lease, answer)
        }
        (Wait::Up(fresh, Up::Retried(mut lease)), Event::Ran(ran)) => {
            let name = fresh.migration.name.clone();
            let freed = settle_down(&mut lease, &name, &ran);
            match lease.state.entries.contains_key(&name) {
                true => abort(
                    m,
                    fresh,
                    lease,
                    Code::Dirty,
                    format!("{name}: the down failed again"),
                    ran.tail,
                ),
                false => then(freed, start_fresh(m, fresh, lease)),
            }
        }
        (Wait::Up(fresh, Up::Ports(lease)), Event::Ports(ports)) => prepare(m, fresh, lease, ports),
        (Wait::Up(fresh, Up::Ports(lease)), Event::Io(message)) => {
            abort(m, fresh, lease, Code::Internal, message, vec![])
        }
        (Wait::Up(fresh, Up::Prepared(lease, ports)), Event::Done) => {
            run_up(m, fresh, lease, ports)
        }
        (Wait::Up(fresh, Up::Prepared(lease, ports)), Event::Io(message)) => {
            let free = free_ports(&lease.lease, &fresh.migration.name, &ports);
            then(
                free,
                abort(m, fresh, lease, Code::Internal, message, vec![]),
            )
        }
        (Wait::Up(fresh, Up::Ran(lease, before, ports)), Event::Ran(ran)) => {
            up_ran(m, fresh, lease, before, ports, ran)
        }
        (Wait::Up(fresh, Up::Aborted(code, message, tail)), Event::Committed { .. }) => {
            fail_up(m, &fresh, code, message, tail)
        }
        (Wait::Up(_, Up::Committed(loaded, item)), Event::Committed { holder }) => {
            let id = loaded
                .lease
                .as_ref()
                .map(|l| l.id.clone())
                .unwrap_or_default();
            m.session.leases.extend(holder.map(|pid| (id, pid)));
            m.env = loaded.diff.apply(std::mem::take(&mut m.env));
            m.ups.push(item);
            m.session.loaded.push(*loaded);
            advance(m)
        }
        (Wait::Down(loaded, Down::Locked(lease)), Event::Done) => {
            match m.session.leases.remove(&lease.id) {
                Some(pid) => wait(
                    m,
                    Wait::Down(loaded, Down::Released(lease)),
                    vec![Effect::Shared(Shared::Release(pid))],
                ),
                None => inspect_down(m, loaded, lease),
            }
        }
        (Wait::Down(loaded, Down::Released(lease)), Event::Done) => inspect_down(m, loaded, lease),
        (
            Wait::Down(loaded, Down::Inspected(lease)),
            Event::Inspected {
                exclusive,
                state,
                holders,
            },
        ) => {
            let lease = Lease {
                lease,
                state,
                held: false,
                holders,
            };
            shared_down(m, loaded, lease, exclusive)
        }
        (Wait::Down(loaded, Down::Ran(mut lease)), Event::Ran(ran)) => {
            let freed = settle_down(&mut lease, &loaded.name, &ran);
            let failed = lease.state.entries.contains_key(&loaded.name);
            let (detail, report) = match failed {
                true => {
                    m.code = m.code.worst(Code::DownFailed);
                    let what = problem(&ran.outcome, Duration::from_millis(loaded.down_timeout_ms));
                    let text = failure_text(
                        &format!("{}: down failed ({what}); the lease is dirty", loaded.name),
                        &loaded.name,
                        &ran.tail,
                    );
                    (Some("DIRTY".to_string()), vec![Effect::Print(text)])
                }
                false => (None, vec![]),
            };
            let commit = down_commit(&m, lease);
            let effects = report.into_iter().chain(freed).chain([commit]).collect();
            wait(m, Wait::Down(loaded, Down::Committed(detail)), effects)
        }
        (Wait::Down(loaded, Down::Committed(detail)), Event::Committed { holder }) => {
            let id = loaded
                .lease
                .as_ref()
                .map(|l| l.id.clone())
                .unwrap_or_default();
            m.session.leases.extend(holder.map(|pid| (id, pid)));
            finish_down(m, loaded, detail)
        }
        (w, Event::Io(message)) => io_failed(m, w, message),
        (w, event) => super::unexpected(m, &w, &event),
    }
}

/// Writes the lease after a down. The session's hold was released to test
/// for "last"; it holds again if it still has other migrations of the
/// lease loaded.
fn down_commit(m: &Machine, lease: Lease) -> Effect {
    let id = &lease.lease.id;
    let others = m
        .session
        .loaded
        .iter()
        .any(|l| l.lease.as_ref().is_some_and(|o| &o.id == id));
    Effect::Shared(Shared::Commit {
        lease: lease.lease,
        state: lease.state,
        hold: others.then_some(m.session.parent),
    })
}

/// A lease file couldn't be read or written. An up fails (and its folder
/// rolls back); a down still applies the derived down.
fn io_failed(m: Machine, w: Wait, message: String) -> (Machine, Vec<Effect>) {
    let mut m = m;
    match w {
        Wait::Up(fresh, _) => {
            let message = format!("{}: lease: {message}", fresh.migration.name);
            fail_up(m, &fresh, Code::Internal, message, vec![])
        }
        Wait::Down(loaded, _) => {
            m.code = m.code.worst(Code::Internal);
            let text = Effect::Print(format!("foldl ✗ {}: lease: {message}\n", loaded.name));
            then(
                vec![text],
                finish_down(m, loaded, Some("(lease error)".to_string())),
            )
        }
    }
}

fn inspect_down(m: Machine, loaded: Loaded, lease: LeaseRef) -> (Machine, Vec<Effect>) {
    let inspect = Effect::Shared(Shared::Inspect {
        lease: lease.clone(),
        exclusive: true,
    });
    wait(m, Wait::Down(loaded, Down::Inspected(lease)), vec![inspect])
}

fn then(first: Vec<Effect>, rest: (Machine, Vec<Effect>)) -> (Machine, Vec<Effect>) {
    super::then(first, rest)
}

/// Why a run failed, in a few words.
fn problem(outcome: &Outcome, timeout: Duration) -> String {
    match outcome {
        Outcome::Exited { status, .. } => format!("exit {status}"),
        Outcome::TimedOut => format!("timed out after {}", format_duration(timeout)),
        Outcome::Interrupted => "interrupted".to_string(),
        Outcome::Unavailable(what) => what.clone(),
    }
}

/// The run of an entry's saved down.
fn down_run(name: &str, entry: &Entry, env: &Env) -> Option<Effect> {
    let script = entry.down.as_ref()?;
    let env = script_env(
        &entry.diff.apply(env.clone()),
        &entry.root,
        &entry.state_dir,
    );
    Some(Effect::Run(Box::new(super::Run {
        name: name.to_string(),
        direction: Direction::Down,
        exec: Exec::Script {
            shebang: script.shebang.clone(),
            path: script.path.clone(),
        },
        env,
        cwd: entry.root.clone(),
        timeout: Duration::from_millis(entry.down_timeout_ms),
        log: entry.log.clone(),
    })))
}

/// Records a down's result: success removes the entry and frees its
/// ports; failure marks it dirty.
fn settle_down(lease: &mut Lease, name: &str, ran: &Ran) -> Vec<Effect> {
    let ok = matches!(ran.outcome, Outcome::Exited { status: 0, .. });
    let entry = lease.state.entries.get_mut(name);
    match (ok, entry) {
        (true, Some(entry)) => {
            let ports = entry.ports.clone();
            let saved = entry.saved.clone();
            lease.state.entries.remove(name);
            [
                vec![Effect::Remove(saved)],
                free_ports(&lease.lease, name, &ports),
            ]
            .concat()
        }
        (false, Some(entry)) => {
            let what = problem(&ran.outcome, Duration::from_millis(entry.down_timeout_ms));
            entry.dirty = Some(Dirty {
                at: ran.at.clone(),
                what,
            });
            vec![]
        }
        (_, None) => vec![],
    }
}

fn free_ports(lease: &LeaseRef, name: &str, ports: &BTreeMap<String, u16>) -> Vec<Effect> {
    match ports.is_empty() {
        true => vec![],
        false => vec![Effect::Shared(Shared::FreePorts {
            lease: lease.clone(),
            name: name.to_string(),
        })],
    }
}

/// Tears down the next stale entry left by sessions that died, or moves
/// on to this migration once none are left. Dirty entries stay.
fn next_stale(m: Machine, fresh: Fresh, lease: Lease, rest: Vec<String>) -> (Machine, Vec<Effect>) {
    let mut rest = rest;
    let Some(name) = first_clean(&lease, &mut rest) else {
        return decide(m, fresh, lease);
    };
    let entry = &lease.state.entries[&name];
    match down_run(&name, entry, &m.env) {
        Some(run) => {
            let note = Effect::Print(format!(
                "foldl ⋯ {name}: stopping what a closed session left running\n"
            ));
            wait(
                m,
                Wait::Up(fresh, Up::Stale(lease, name, rest)),
                vec![note, run],
            )
        }
        None => {
            let mut lease = lease;
            lease.state.entries.remove(&name);
            next_stale(m, fresh, lease, rest)
        }
    }
}

/// The first entry in `rest` that isn't dirty, removing it and the dirty
/// ones before it from `rest`.
fn first_clean(lease: &Lease, rest: &mut Vec<String>) -> Option<String> {
    let at = rest.iter().position(|n| {
        lease
            .state
            .entries
            .get(n)
            .is_some_and(|e| e.dirty.is_none())
    })?;
    let name = rest.remove(at);
    rest.drain(..at);
    Some(name)
}

/// Join, start, or deal with a dirty entry.
fn decide(m: Machine, fresh: Fresh, lease: Lease) -> (Machine, Vec<Effect>) {
    let name = fresh.migration.name.clone();
    let entry = lease.state.entries.get(&name).cloned();
    match entry {
        Some(Entry {
            dirty: Some(dirty), ..
        }) => {
            let question = format!(
                "foldl ! {name}: last down failed at {} ({}), see `foldl logs {name}`\n  [r] retry the down, then up\n  [s] skip the down and run up\n  [a] abort, don't load this folder\n",
                dirty.at, dirty.what
            );
            match (m.ctx.prompt, m.ctx.config.on_dirty) {
                (true, _) => wait(
                    m,
                    Wait::Up(fresh, Up::Answer(lease)),
                    vec![Effect::Shared(Shared::Ask {
                        question,
                        choices: vec!['r', 's', 'a'],
                    })],
                ),
                (false, OnDirty::Retry) => dirty_choice(m, fresh, lease, 'r'),
                (false, OnDirty::Skip) => dirty_choice(m, fresh, lease, 's'),
                (false, OnDirty::Abort) => dirty_choice(m, fresh, lease, 'a'),
            }
        }
        Some(entry) => join(m, fresh, lease, entry),
        None => start_fresh(m, fresh, lease),
    }
}

fn dirty_choice(m: Machine, fresh: Fresh, lease: Lease, answer: char) -> (Machine, Vec<Effect>) {
    let name = fresh.migration.name.clone();
    let entry = lease.state.entries[&name].clone();
    let report = |what: &str| Effect::Print(format!("foldl ! {name}: dirty lease; {what}\n"));
    match (answer, down_run(&name, &entry, &m.env)) {
        ('r', Some(run)) => wait(
            m,
            Wait::Up(fresh, Up::Retried(lease)),
            vec![report("retrying the down"), run],
        ),
        ('s', _) | ('r', None) => {
            let mut lease = lease;
            lease.state.entries.remove(&name);
            let freed = free_ports(&lease.lease, &name, &entry.ports);
            then(
                [vec![report("skipping the down")], freed].concat(),
                start_fresh(m, fresh, lease),
            )
        }
        _ => {
            let message = format!("{name}: the lease is dirty; not loading");
            abort(m, fresh, lease, Code::Dirty, message, vec![])
        }
    }
}

/// Releases the mutex without changes, then fails the up.
fn abort(
    m: Machine,
    fresh: Fresh,
    lease: Lease,
    code: Code,
    message: String,
    tail: Vec<String>,
) -> (Machine, Vec<Effect>) {
    let commit = Effect::Shared(Shared::Commit {
        lease: lease.lease,
        state: lease.state,
        hold: None,
    });
    wait(
        m,
        Wait::Up(fresh, Up::Aborted(code, message, tail)),
        vec![commit],
    )
}

/// Replays a running migration's env.
fn join(m: Machine, fresh: Fresh, lease: Lease, entry: Entry) -> (Machine, Vec<Effect>) {
    let after = entry.diff.apply(m.env.clone());
    let detail = match entry.hash == fresh.migration.hash {
        true => "joined".to_string(),
        false => format!("joined; changed, run 'foldl redo {}'", fresh.migration.name),
    };
    commit_up(m, fresh, lease, &entry, after, detail)
}

fn start_fresh(m: Machine, fresh: Fresh, lease: Lease) -> (Machine, Vec<Effect>) {
    let names = fresh.migration.ports.clone();
    match names.is_empty() {
        true => prepare(m, fresh, lease, BTreeMap::new()),
        false => {
            let alloc = Effect::Shared(Shared::AllocPorts {
                lease: lease.lease.clone(),
                name: fresh.migration.name.clone(),
                names,
            });
            wait(m, Wait::Up(fresh, Up::Ports(lease)), vec![alloc])
        }
    }
}

fn lease_dirs(
    m: &Machine,
    lease: &LeaseRef,
    fresh: &Fresh,
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    Paths::migration_dirs(
        &m.ctx.paths.lease(&lease.id),
        &fresh.root,
        &fresh.migration.name,
    )
}

fn prepare(
    m: Machine,
    fresh: Fresh,
    lease: Lease,
    ports: BTreeMap<String, u16>,
) -> (Machine, Vec<Effect>) {
    let (saved, state_dir, log) = lease_dirs(&m, &lease.lease, &fresh);
    let from = fresh.root.join(".foldl").join(&fresh.migration.dir);
    wait(
        m,
        Wait::Up(fresh, Up::Prepared(lease, ports)),
        vec![Effect::Prepare {
            from,
            saved,
            state_dir,
            log,
        }],
    )
}

fn run_up(
    m: Machine,
    fresh: Fresh,
    lease: Lease,
    ports: BTreeMap<String, u16>,
) -> (Machine, Vec<Effect>) {
    let (saved, state_dir, log) = lease_dirs(&m, &lease.lease, &fresh);
    let texts: BTreeMap<String, String> = ports
        .iter()
        .map(|(k, v)| (k.clone(), v.to_string()))
        .collect();
    let prepared = prepared_env(&m.env, &fresh, &state_dir, &texts);
    let migration = &fresh.migration;
    let exec = match (&migration.builtin, &migration.up) {
        (Some(builtin), _) => Exec::Builtin {
            builtin: builtin.clone(),
            watch: fresh.watch.clone(),
        },
        (None, Some(script)) => Exec::Script {
            shebang: script.shebang.clone(),
            path: saved.join(&script.path),
        },
        (None, None) => {
            let ran = Ran {
                outcome: Outcome::Exited {
                    status: 0,
                    env: Some(prepared),
                },
                tail: vec![],
                millis: 0,
                warnings: vec![],
                at: String::new(),
            };
            let before = m.env.clone();
            return up_ran(m, fresh, lease, before, ports, ran);
        }
    };
    let default = match migration.builtin {
        Some(Builtin::NixFlake { .. }) => m.ctx.config.nix_timeout,
        _ => m.ctx.config.timeout,
    };
    let run = super::Run {
        name: migration.name.clone(),
        direction: Direction::Up,
        exec,
        env: script_env(&prepared, &fresh.root, &state_dir),
        cwd: fresh.root.clone(),
        timeout: migration.timeout.unwrap_or(default),
        log,
    };
    let before = m.env.clone();
    wait(
        m,
        Wait::Up(fresh, Up::Ran(lease, before, ports)),
        vec![Effect::Run(Box::new(run))],
    )
}

fn up_ran(
    m: Machine,
    fresh: Fresh,
    lease: Lease,
    before: Env,
    ports: BTreeMap<String, u16>,
    ran: Ran,
) -> (Machine, Vec<Effect>) {
    let name = fresh.migration.name.clone();
    let after = match ran.outcome {
        Outcome::Exited {
            status: 0,
            env: Some(after),
        } => tracked(after),
        ref outcome => {
            let timeout = fresh.migration.timeout.unwrap_or(m.ctx.config.timeout);
            let code = match outcome {
                Outcome::TimedOut => Code::Timeout,
                Outcome::Interrupted => Code::Interrupted,
                Outcome::Unavailable(_) => Code::Unavailable,
                Outcome::Exited { .. } => Code::UpFailed,
            };
            let message = format!("{name}: up failed ({})", problem(outcome, timeout));
            let free = free_ports(&lease.lease, &name, &ports);
            return then(free, abort(m, fresh, lease, code, message, ran.tail));
        }
    };
    let (saved, state_dir, log) = lease_dirs(&m, &lease.lease, &fresh);
    let down_timeout = fresh
        .migration
        .down_timeout
        .unwrap_or(m.ctx.config.down_timeout);
    let entry = Entry {
        hash: fresh.migration.hash.clone(),
        diff: EnvDiff::between(&before, &after),
        ports,
        down: fresh.migration.down.as_ref().map(|s| SavedScript {
            path: saved.join(&s.path),
            shebang: s.shebang.clone(),
        }),
        down_timeout_ms: down_timeout.as_millis() as u64,
        root: fresh.root.clone(),
        saved,
        state_dir,
        log,
        dirty: None,
        seq: lease.state.next_seq(),
    };
    let mut lease = lease;
    lease.state.entries.insert(name, entry.clone());
    let time = match ran.millis >= 100 {
        true => format!(", {:.1}s", ran.millis as f64 / 1000.0),
        false => String::new(),
    };
    commit_up(m, fresh, lease, &entry, after, format!("started{time}"))
}

/// Records the migration as loaded in this session (the env diff is this
/// session's own copy), writes the lease and takes a hold on it.
fn commit_up(
    m: Machine,
    fresh: Fresh,
    lease: Lease,
    entry: &Entry,
    after: Env,
    detail: String,
) -> (Machine, Vec<Effect>) {
    let diff = EnvDiff::between(&m.env, &after);
    let vars = vars_detail(&diff, m.ctx.config.verbosity);
    let details: Vec<String> = [detail].into_iter().chain(vars).collect();
    let item = format!("{} ({})", fresh.migration.name, details.join(", "));
    let loaded = Loaded {
        root: fresh.root.clone(),
        name: fresh.migration.name.clone(),
        hash: fresh.migration.hash.clone(),
        watch: fresh.watch.clone(),
        scope: fresh.migration.scope,
        lease: Some(lease.lease.clone()),
        diff,
        aliases: vec![],
        interactive: None,
        down: None,
        down_timeout_ms: entry.down_timeout_ms,
        saved: entry.saved.clone(),
        state_dir: entry.state_dir.clone(),
        log: entry.log.clone(),
        note: None,
    };
    let hold = (!lease.held).then_some(m.session.parent);
    let commit = Effect::Shared(Shared::Commit {
        lease: lease.lease,
        state: lease.state,
        hold,
    });
    wait(
        m,
        Wait::Up(fresh, Up::Committed(Box::new(loaded), item)),
        vec![commit],
    )
}

/// Leaving: run the down if this session was last; otherwise say who
/// keeps it.
fn shared_down(
    m: Machine,
    loaded: Loaded,
    lease: Lease,
    exclusive: bool,
) -> (Machine, Vec<Effect>) {
    let entry = lease.state.entries.get(&loaded.name).cloned();
    let forced = m
        .forced
        .contains(&(loaded.root.clone(), loaded.name.clone()));
    let runnable = entry.filter(|e| forced || e.dirty.is_none());
    let home = m.env.get("HOME").map(Value::lossy).unwrap_or_default();
    let who: Vec<String> = lease.holders.iter().map(|h| h.describe(&home)).collect();
    let warning = (forced && !exclusive && runnable.is_some()).then(|| {
        Effect::Print(format!(
            "foldl ! {}: restarting while in use by {}\n",
            loaded.name,
            who.join(", ")
        ))
    });
    match (exclusive || forced, runnable) {
        (true, Some(entry)) => match down_run(&loaded.name, &entry, &m.env) {
            Some(run) => wait(
                m,
                Wait::Down(loaded, Down::Ran(lease)),
                warning.into_iter().chain([run]).collect(),
            ),
            None => {
                let mut lease = lease;
                lease.state.entries.remove(&loaded.name);
                let commit = down_commit(&m, lease);
                wait(m, Wait::Down(loaded, Down::Committed(None)), vec![commit])
            }
        },
        (false, _) => {
            let detail = match who.is_empty() {
                true => "kept: in use".to_string(),
                false => format!("kept: in use by {}", who.join(", ")),
            };
            let commit = down_commit(&m, lease);
            wait(
                m,
                Wait::Down(loaded, Down::Committed(Some(detail))),
                vec![commit],
            )
        }
        (true, None) => {
            let commit = down_commit(&m, lease);
            wait(m, Wait::Down(loaded, Down::Committed(None)), vec![commit])
        }
    }
}

#[cfg(test)]
mod tests;

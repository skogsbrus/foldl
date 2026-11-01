// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! The lease protocol replayed against a fake kernel shared by several
//! sessions, interleaving their effects one at a time. Covers the races
//! from flox#4791 and flox#4781 without processes, locks or timing.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::super::{Ctx, Effect, Event, Finish, Machine, Outcome, Ran, Shared, start, step};
use crate::config::OnDirty;
use crate::env::{Env, Value};
use crate::error::Code;
use crate::lease::{Holder, LeaseState};
use crate::plan::FolderInfo;
use crate::state::Session;
use crate::testkit::{allowed, ctx, env};

const PG: &[(&str, &str)] = &[
    (
        "1-pg/manifest.toml",
        "version = 1\nscope = \"shared\"\n[ports]\nPGPORT = \"auto\"",
    ),
    ("1-pg/up.sh", "#!/bin/sh\n"),
    ("1-pg/down.sh", "#!/bin/sh\n"),
    ("2-cache/manifest.toml", "version = 1\nscope = \"shared\""),
    ("2-cache/up.sh", "#!/bin/sh\n"),
    ("2-cache/down.sh", "#!/bin/sh\n"),
];

fn folder() -> Vec<FolderInfo> {
    vec![allowed("/app", "f", PG)]
}

/// One lease's kernel-side state: the mutex, flock holders and the
/// lease's `state.json`.
#[derive(Default)]
struct Kernel {
    mutex: Option<usize>,
    exclusive: Option<usize>,
    holders: BTreeMap<u32, usize>,
    state: LeaseState,
    next_pid: u32,
    log: Vec<String>,
    failing_downs: usize,
    answer: char,
    ports: BTreeSet<u16>,
}

struct Run {
    machine: Option<Machine>,
    pending: VecDeque<Effect>,
    finish: Option<Finish>,
    printed: String,
}

impl Kernel {
    fn new() -> Kernel {
        Kernel {
            next_pid: 1000,
            answer: 'r',
            ..Kernel::default()
        }
    }

    /// Performs one effect for session `who`; `None` means it must wait.
    fn perform(&mut self, who: usize, effect: &Effect) -> Option<Option<Event>> {
        let reply = match effect {
            Effect::Shared(Shared::Lock(_)) if self.mutex.is_some_and(|o| o != who) => return None,
            Effect::Shared(Shared::Lock(_)) => {
                self.mutex = Some(who);
                Some(Event::Done)
            }
            Effect::Shared(Shared::Inspect { exclusive, .. }) => {
                let granted = *exclusive && self.holders.is_empty() && self.exclusive.is_none();
                self.exclusive = granted.then_some(who);
                let holders = self
                    .holders
                    .iter()
                    .map(|(pid, s)| holder(*pid, *s))
                    .collect();
                Some(Event::Inspected {
                    exclusive: granted,
                    state: self.state.clone(),
                    holders,
                })
            }
            Effect::Shared(Shared::Commit { state, hold, .. }) => {
                self.state = state.clone();
                let holder = hold.map(|_| {
                    self.next_pid += 1;
                    self.holders.insert(self.next_pid, who);
                    self.next_pid
                });
                self.exclusive = None;
                self.mutex = None;
                Some(Event::Committed { holder })
            }
            Effect::Shared(Shared::Release(pid)) => {
                self.holders.remove(pid);
                Some(Event::Done)
            }
            Effect::Shared(Shared::AllocPorts { names, .. }) => {
                let start = self.ports.last().map_or(20000, |p| p + 1);
                let ports: BTreeMap<String, u16> = names
                    .iter()
                    .zip(start..)
                    .map(|(n, p)| (n.clone(), p))
                    .collect();
                self.ports.extend(ports.values());
                Some(Event::Ports(ports))
            }
            Effect::Shared(Shared::FreePorts { .. }) => {
                self.log.push("free-ports".into());
                None
            }
            Effect::Run(run) => {
                let dir = format!("{:?}", run.direction).to_lowercase();
                self.log.push(format!("{dir}:{}", run.name));
                let failing = dir == "down" && self.failing_downs > 0;
                self.failing_downs -= usize::from(failing);
                let mut after: Env = run.env.clone();
                after.insert(format!("{}_UP", run.name.to_uppercase()), Value::from("1"));
                let outcome = match failing {
                    true => Outcome::Exited {
                        status: 1,
                        env: None,
                    },
                    false => Outcome::Exited {
                        status: 0,
                        env: Some(after),
                    },
                };
                Some(Event::Ran(Ran {
                    outcome,
                    tail: vec![],
                    millis: 1,
                    warnings: vec![],
                    at: "10:42".into(),
                }))
            }
            Effect::Prepare { .. } => Some(Event::Done),
            Effect::Shared(Shared::Ask { .. }) => Some(Event::Answer(self.answer)),
            Effect::Shared(Shared::SpawnHolder { .. }) => {
                self.next_pid += 1;
                Some(Event::Held(self.next_pid))
            }
            Effect::Print(_)
            | Effect::Remove(_)
            | Effect::Shared(Shared::StopHolder(_))
            | Effect::Finish(_) => None,
        };
        Some(reply)
    }

    /// A session's process died: the kernel drops its locks.
    fn kill(&mut self, who: usize) {
        self.holders.retain(|_, s| *s != who);
    }
}

fn holder(pid: u32, session: usize) -> Holder {
    Holder {
        session: format!("s{session}"),
        pid,
        name: "zsh".into(),
        cwd: "/app".into(),
        tty: None,
    }
}

fn session(i: usize) -> Session {
    Session::new(
        format!("s{i}"),
        100 + i as u32,
        Some(crate::render::Shell::Zsh),
    )
}

/// Runs several transitions at once, one effect per session per turn.
fn interleave(kernel: &mut Kernel, starts: Vec<(Machine, Vec<Effect>)>) -> Vec<Run> {
    let mut runs: Vec<Run> = starts
        .into_iter()
        .map(|(m, e)| Run {
            machine: Some(m),
            pending: e.into(),
            finish: None,
            printed: String::new(),
        })
        .collect();
    loop {
        let mut progressed = false;
        for (who, run) in runs.iter_mut().enumerate() {
            let Some(effect) = run.pending.front().cloned() else {
                continue;
            };
            let Some(reply) = kernel.perform(who, &effect) else {
                continue;
            };
            run.pending.pop_front();
            progressed = true;
            match effect {
                Effect::Finish(finish) => run.finish = Some(*finish),
                Effect::Print(text) => run.printed.push_str(&text),
                _ => {}
            }
            if let Some(event) = reply {
                let (m, more) = step(run.machine.take().unwrap(), event);
                run.machine = Some(m);
                run.pending.extend(more);
            }
        }
        if !progressed {
            assert!(runs.iter().all(|r| r.pending.is_empty()), "deadlock");
            return runs;
        }
    }
}

fn enter(kernel: &mut Kernel, sessions: Vec<Session>, c: &Ctx) -> Vec<Run> {
    let starts = sessions
        .into_iter()
        .map(|s| start(s, c.clone(), env(&[]), &folder()))
        .collect();
    interleave(kernel, starts)
}

fn leave(kernel: &mut Kernel, runs: Vec<&Run>, c: &Ctx) -> Vec<Run> {
    let starts = runs
        .into_iter()
        .map(|r| {
            let f = r.finish.as_ref().unwrap();
            start(f.session.clone(), c.clone(), f.env.clone(), &[])
        })
        .collect();
    interleave(kernel, starts)
}

fn finish(run: &Run) -> &Finish {
    run.finish.as_ref().unwrap()
}

#[test]
fn two_sessions_entering_at_once_start_one_instance() {
    let mut k = Kernel::new();
    let runs = enter(&mut k, vec![session(0), session(1)], &ctx());
    assert_eq!(k.log, ["up:pg", "up:cache"], "flox#4791: started once");
    assert_eq!(k.holders.len(), 2);
    for run in &runs {
        assert_eq!(finish(run).env["PGPORT"], Value::from("20000"));
        assert_eq!(finish(run).session.leases.len(), 1);
    }
    assert!(
        runs[0].printed.contains("pg (started, +2 vars)"),
        "{}",
        runs[0].printed
    );
    assert!(
        runs[1]
            .printed
            .contains("pg (joined, +2 vars) · cache (joined, +1 var)"),
        "{}",
        runs[1].printed
    );
}

#[test]
fn entering_during_the_last_down_waits_and_starts_fresh() {
    let only_pg = || vec![allowed("/app", "f", &PG[..3])];
    let mut k = Kernel::new();
    let first = interleave(&mut k, vec![start(session(0), ctx(), env(&[]), &only_pg())]);
    let f = finish(&first[0]);
    let leaving = start(f.session.clone(), ctx(), f.env.clone(), &[]);
    let entering = start(session(1), ctx(), env(&[]), &only_pg());
    let runs = interleave(&mut k, vec![leaving, entering]);
    assert_eq!(
        k.log,
        ["up:pg", "down:pg", "free-ports", "up:pg"],
        "flox#4781: fresh up after the down"
    );
    assert!(
        runs[1].printed.contains("pg (started"),
        "{}",
        runs[1].printed
    );
}

#[test]
fn entering_between_two_downs_joins_what_is_still_up() {
    let mut k = Kernel::new();
    let first = enter(&mut k, vec![session(0)], &ctx());
    let f = finish(&first[0]);
    let leaving = start(f.session.clone(), ctx(), f.env.clone(), &[]);
    let entering = start(session(1), ctx(), env(&[]), &folder());
    let runs = interleave(&mut k, vec![leaving, entering]);
    assert_eq!(
        k.log,
        ["up:pg", "up:cache", "down:cache", "up:cache"],
        "never overlapping"
    );
    assert!(
        runs[0].printed.contains("pg kept: in use by"),
        "{}",
        runs[0].printed
    );
    assert!(
        runs[1].printed.contains("pg (joined"),
        "{}",
        runs[1].printed
    );
}

#[test]
fn leaving_while_others_hold_the_lease_keeps_it() {
    let mut k = Kernel::new();
    let runs = enter(&mut k, vec![session(0), session(1)], &ctx());
    let left = leave(&mut k, vec![&runs[1]], &ctx());
    assert_eq!(k.log, ["up:pg", "up:cache"]);
    assert!(
        left[0]
            .printed
            .contains("cache kept: in use by zsh 1001 (/app) · pg kept: in use by zsh 1001 (/app)"),
        "{}",
        left[0].printed
    );
    assert_eq!(finish(&left[0]).env, env(&[]));
    let last = leave(&mut k, vec![&runs[0]], &ctx());
    assert_eq!(
        k.log,
        ["up:pg", "up:cache", "down:cache", "down:pg", "free-ports"]
    );
    assert!(
        last[0].printed.contains("foldl ↓ cache · pg\n"),
        "{}",
        last[0].printed
    );
    assert!(k.state.entries.is_empty());
}

#[test]
fn a_crashed_last_session_is_cleaned_up_by_the_next() {
    let mut k = Kernel::new();
    enter(&mut k, vec![session(0)], &ctx());
    k.kill(0);
    let runs = enter(&mut k, vec![session(1)], &ctx());
    assert_eq!(
        k.log,
        [
            "up:pg",
            "up:cache",
            "down:cache",
            "down:pg",
            "free-ports",
            "up:pg",
            "up:cache"
        ]
    );
    assert!(
        runs[0]
            .printed
            .contains("stopping what a closed session left running")
    );
}

#[test]
fn a_failed_down_marks_the_lease_dirty() {
    let mut k = Kernel::new();
    let runs = enter(&mut k, vec![session(0)], &ctx());
    k.failing_downs = 1;
    let left = leave(&mut k, vec![&runs[0]], &ctx());
    assert_eq!(finish(&left[0]).code, Code::DownFailed);
    assert!(
        left[0]
            .printed
            .contains("cache: down failed (exit 1); the lease is dirty"),
        "{}",
        left[0].printed
    );
    assert!(
        left[0].printed.contains("foldl ↓ cache DIRTY · pg\n"),
        "{}",
        left[0].printed
    );
    assert_eq!(k.state.entries["cache"].dirty.as_ref().unwrap().at, "10:42");
}

fn dirty_kernel() -> Kernel {
    let mut k = Kernel::new();
    let runs = enter(&mut k, vec![session(0)], &ctx());
    k.failing_downs = 1;
    leave(&mut k, vec![&runs[0]], &ctx());
    k.log.clear();
    k
}

#[test]
fn entering_a_dirty_lease_asks() {
    let mut prompt = ctx();
    prompt.prompt = true;
    let mut k = dirty_kernel();
    let runs = enter(&mut k, vec![session(1)], &prompt);
    assert!(
        runs[0]
            .printed
            .contains("foldl ! cache: dirty lease; retrying the down"),
        "{}",
        runs[0].printed
    );
    assert_eq!(k.log, ["up:pg", "down:cache", "up:cache"]);
    let mut k = dirty_kernel();
    k.answer = 's';
    enter(&mut k, vec![session(1)], &prompt);
    assert_eq!(k.log, ["up:pg", "up:cache"]);
    let mut k = dirty_kernel();
    k.answer = 'a';
    let runs = enter(&mut k, vec![session(1)], &prompt);
    assert_eq!(finish(&runs[0]).code, Code::Dirty);
    assert_eq!(k.log, ["up:pg", "down:pg", "free-ports"], "rolled back");
    let mut k = dirty_kernel();
    k.failing_downs = 1;
    let runs = enter(&mut k, vec![session(1)], &prompt);
    assert!(
        runs[0].printed.contains("cache: the down failed again"),
        "{}",
        runs[0].printed
    );
}

#[test]
fn non_interactive_runs_follow_on_dirty() {
    for (policy, log) in [
        (OnDirty::Retry, vec!["up:pg", "down:cache", "up:cache"]),
        (OnDirty::Skip, vec!["up:pg", "up:cache"]),
        (OnDirty::Abort, vec!["up:pg", "down:pg", "free-ports"]),
    ] {
        let mut c = ctx();
        c.config.on_dirty = policy;
        let mut k = dirty_kernel();
        enter(&mut k, vec![session(1)], &c);
        assert_eq!(k.log, log, "{policy:?}");
    }
}

#[test]
fn a_failed_shared_up_releases_the_mutex_and_rolls_back() {
    let mut k = Kernel::new();
    let start = start(session(0), ctx(), env(&[]), &folder());
    let mut runs = interleave(&mut k, vec![start]);
    assert_eq!(finish(&runs[0]).code, Code::Ok);
    k = Kernel::new();
    k.failing_downs = 0;
    let (m, effects) = super::super::start(session(1), ctx(), env(&[]), &folder());
    let mut run = Run {
        machine: Some(m),
        pending: effects.into(),
        finish: None,
        printed: String::new(),
    };
    while let Some(effect) = run.pending.pop_front() {
        let reply = match &effect {
            Effect::Run(r) if r.name == "cache" => Some(Some(Event::Ran(Ran {
                outcome: Outcome::TimedOut,
                tail: vec![],
                millis: 0,
                warnings: vec![],
                at: String::new(),
            }))),
            other => k.perform(1, other),
        };
        if let Effect::Finish(f) = &effect {
            run.finish = Some((**f).clone());
        }
        if let Effect::Print(text) = &effect {
            run.printed.push_str(text);
        }
        if let Some(Some(event)) = reply {
            let (m, more) = step(run.machine.take().unwrap(), event);
            run.machine = Some(m);
            run.pending.extend(more);
        }
    }
    assert_eq!(k.mutex, None);
    assert_eq!(finish(&run).code, Code::Timeout);
    assert!(
        run.printed
            .contains("cache: up failed (timed out after 1m)"),
        "{}",
        run.printed
    );
    runs.clear();
}

#[test]
fn session_holders_come_and_go() {
    let mut c = ctx();
    c.holders = true;
    let mut k = Kernel::new();
    let runs = enter(&mut k, vec![session(0)], &c);
    let f = finish(&runs[0]);
    assert!(f.session.holder.is_some());
    let left = leave(&mut k, vec![&runs[0]], &c);
    assert_eq!(finish(&left[0]).session.holder, None);
}

#[test]
fn unexpected_events_in_shared_waits_are_internal_errors() {
    let (m, _) = start(session(0), ctx(), env(&[]), &folder());
    let (_, effects) = step(m, Event::Answer('x'));
    assert!(matches!(effects.last(), Some(Effect::Finish(f)) if f.code == Code::Internal));
}

/// Drives one session, answering effects with `kernel` unless `answer`
/// overrides them.
fn drive_one(
    k: &mut Kernel,
    start: (Machine, Vec<Effect>),
    mut answer: impl FnMut(&Effect) -> Option<Event>,
) -> Run {
    let (m, effects) = start;
    let mut run = Run {
        machine: Some(m),
        pending: effects.into(),
        finish: None,
        printed: String::new(),
    };
    while let Some(effect) = run.pending.pop_front() {
        let reply = answer(&effect).or_else(|| k.perform(0, &effect).flatten());
        match &effect {
            Effect::Finish(f) => run.finish = Some((**f).clone()),
            Effect::Print(text) => run.printed.push_str(text),
            _ => {}
        }
        if let Some(event) = reply {
            let (m, more) = step(run.machine.take().unwrap(), event);
            run.machine = Some(m);
            run.pending.extend(more);
        }
    }
    run
}

fn ran(outcome: Outcome, millis: u64) -> Option<Event> {
    Some(Event::Ran(Ran {
        outcome,
        tail: vec![],
        millis,
        warnings: vec![],
        at: "09:00".into(),
    }))
}

#[test]
fn io_errors_while_starting_abort() {
    for fail in ["ports", "prepare"] {
        let mut k = Kernel::new();
        let run = drive_one(
            &mut k,
            start(session(0), ctx(), env(&[]), &folder()),
            |e| match (e, fail) {
                (Effect::Shared(Shared::AllocPorts { .. }), "ports")
                | (Effect::Prepare { .. }, "prepare") => Some(Event::Io("disk".into())),
                _ => None,
            },
        );
        assert_eq!(finish(&run).code, Code::Internal, "{fail}");
        assert_eq!(k.mutex, None);
    }
}

#[test]
fn up_failure_kinds() {
    for (outcome, code) in [
        (Outcome::Interrupted, Code::Interrupted),
        (Outcome::Unavailable("sh".into()), Code::Unavailable),
        (
            Outcome::Exited {
                status: 2,
                env: None,
            },
            Code::UpFailed,
        ),
    ] {
        let mut k = Kernel::new();
        let run = drive_one(
            &mut k,
            start(session(0), ctx(), env(&[]), &folder()),
            |e| match e {
                Effect::Run(r) if r.name == "pg" => ran(outcome.clone(), 0),
                _ => None,
            },
        );
        assert_eq!(finish(&run).code, code);
    }
}

#[test]
fn slow_ups_show_their_time_and_changed_ones_say_redo() {
    let mut k = Kernel::new();
    let run = drive_one(
        &mut k,
        start(session(0), ctx(), env(&[]), &folder()),
        |e| match e {
            Effect::Run(r) if r.name == "pg" => ran(
                Outcome::Exited {
                    status: 0,
                    env: Some(r.env.clone()),
                },
                1500,
            ),
            _ => None,
        },
    );
    assert!(
        run.printed.contains("pg (started, 1.5s, +1 var)"),
        "{}",
        run.printed
    );
    let changed: Vec<(&str, &str)> = vec![PG[0], ("1-pg/up.sh", "#!/bin/sh\n# v2\n"), PG[2]];
    let other = drive_one(
        &mut k,
        start(
            session(1),
            ctx(),
            env(&[]),
            &[allowed("/app", "g", &changed)],
        ),
        |_| None,
    );
    assert!(
        other
            .printed
            .contains("pg (joined; changed, run 'foldl redo pg'"),
        "{}",
        other.printed
    );
}

#[test]
fn shared_builtins_and_manifest_only_ups() {
    let files: &[(&str, &str)] = &[
        (
            "1-nix/manifest.toml",
            "version = 1\nscope = \"shared\"\nbuiltin = \"nix-flake\"",
        ),
        ("1-nix/down.sh", "#!/bin/sh\n"),
        (
            "2-env/manifest.toml",
            "version = 1\nscope = \"shared\"\n[env]\nE = \"1\"",
        ),
        ("2-env/down.sh", "#!/bin/sh\n"),
    ];
    let mut k = Kernel::new();
    let mut timeouts = vec![];
    let run = drive_one(
        &mut k,
        start(session(0), ctx(), env(&[]), &[allowed("/app", "f", files)]),
        |e| {
            if let Effect::Run(r) = e {
                timeouts.push(r.timeout.as_secs());
            }
            None
        },
    );
    assert_eq!(timeouts, [600]);
    assert_eq!(finish(&run).env["E"], Value::from("1"));
}

#[test]
fn down_edge_cases() {
    let mut k = Kernel::new();
    let entered = drive_one(
        &mut k,
        start(session(0), ctx(), env(&[]), &folder()),
        |_| None,
    );
    let mut s = finish(&entered).session.clone();
    s.leases.clear();
    let mut entry = k.state.entries["pg"].clone();
    entry.down = None;
    k.state.entries.insert("pg".into(), entry);
    k.state.entries.remove("cache");
    k.holders.clear();
    let left = drive_one(
        &mut k,
        start(s, ctx(), finish(&entered).env.clone(), &[]),
        |_| None,
    );
    assert_eq!(
        k.log,
        ["up:pg", "up:cache"],
        "no saved down, nothing to run: {}",
        left.printed
    );
    assert!(k.state.entries.is_empty());
    for outcome in [
        Outcome::Interrupted,
        Outcome::Unavailable("sh missing".into()),
        Outcome::TimedOut,
    ] {
        let mut k = Kernel::new();
        let entered = drive_one(
            &mut k,
            start(session(0), ctx(), env(&[]), &folder()),
            |_| None,
        );
        let f = finish(&entered);
        let left = drive_one(
            &mut k,
            start(f.session.clone(), ctx(), f.env.clone(), &[]),
            |e| match e {
                Effect::Run(r) if r.name == "cache" => ran(outcome.clone(), 0),
                _ => None,
            },
        );
        assert!(
            left.printed.contains("cache: down failed ("),
            "{}",
            left.printed
        );
    }
}

#[test]
fn kept_without_known_holders_and_stale_entries_without_downs() {
    let mut k = Kernel::new();
    let entered = drive_one(
        &mut k,
        start(session(0), ctx(), env(&[]), &folder()),
        |_| None,
    );
    let f = finish(&entered);
    let left = drive_one(
        &mut k,
        start(f.session.clone(), ctx(), f.env.clone(), &[]),
        |e| match e {
            Effect::Shared(Shared::Inspect { .. }) => Some(Event::Inspected {
                exclusive: false,
                state: LeaseState::default(),
                holders: vec![],
            }),
            _ => None,
        },
    );
    assert!(
        left.printed.contains("pg kept: in use\n"),
        "{}",
        left.printed
    );
    let mut k = Kernel::new();
    drive_one(
        &mut k,
        start(session(0), ctx(), env(&[]), &folder()),
        |_| None,
    );
    k.holders.clear();
    k.state.entries.values_mut().for_each(|e| e.down = None);
    k.log.clear();
    drive_one(
        &mut k,
        start(session(1), ctx(), env(&[]), &folder()),
        |_| None,
    );
    assert_eq!(
        k.log,
        ["up:pg", "up:cache"],
        "stale entries without downs are just dropped"
    );
}

#[test]
fn settling_a_missing_entry_does_nothing() {
    let mut lease = super::Lease {
        lease: crate::lease::LeaseRef {
            id: "l".into(),
            key: "k".into(),
        },
        state: LeaseState::default(),
        held: false,
        holders: vec![],
    };
    let r = Ran {
        outcome: Outcome::TimedOut,
        tail: vec![],
        millis: 0,
        warnings: vec![],
        at: String::new(),
    };
    assert!(super::settle_down(&mut lease, "x", &r).is_empty());
    assert_eq!(
        super::problem(&Outcome::Interrupted, std::time::Duration::ZERO),
        "interrupted"
    );
}

#[test]
fn lease_errors_while_leaving_still_apply_the_derived_down() {
    let mut k = Kernel::new();
    let entered = drive_one(
        &mut k,
        start(session(0), ctx(), env(&[]), &folder()),
        |_| None,
    );
    let f = finish(&entered);
    let left = drive_one(
        &mut k,
        start(f.session.clone(), ctx(), f.env.clone(), &[]),
        |e| match e {
            Effect::Shared(Shared::Lock(_)) => Some(Event::Io("no space".into())),
            _ => None,
        },
    );
    assert_eq!(finish(&left).env, env(&[]));
    assert_eq!(finish(&left).code, Code::Internal);
    assert!(
        left.printed.contains("foldl ✗ cache: lease: no space"),
        "{}",
        left.printed
    );
    assert!(
        left.printed.contains("cache (lease error)"),
        "{}",
        left.printed
    );
    let failed = drive_one(
        &mut Kernel::new(),
        start(session(1), ctx(), env(&[]), &folder()),
        |e| match e {
            Effect::Shared(Shared::Commit { .. }) => Some(Event::Io("disk".into())),
            _ => None,
        },
    );
    assert_eq!(finish(&failed).code, Code::Internal);
    assert!(finish(&failed).session.loaded.is_empty());
}

// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! The interpreter: performs the machine's effects through a [`World`] and
//! folds each result back in, until the machine finishes.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use foldl_core::lease::Holder;
use foldl_core::machine::{Effect, Event, Finish, Machine, Ran, Run, Shared, step};
use foldl_core::state::Paths;

use crate::leases::{self, Locks};
use crate::runner::Runner;
use crate::{fsx, hold};

/// The I/O the machine's effects need. Tests swap in fakes.
pub trait World {
    fn print(&mut self, text: &str);
    fn prepare(
        &mut self,
        from: &Path,
        saved: &Path,
        state_dir: &Path,
        log: &Path,
    ) -> Result<(), String>;
    fn run(&mut self, run: &Run) -> Ran;
    fn remove(&mut self, path: &Path);
    /// Leases, holders, ports and questions.
    fn shared(&mut self, effect: &Shared) -> Option<Event>;
}

/// Runs the machine to completion. Also returns one JSON line per run,
/// for `events.jsonl`.
pub fn drive(world: &mut dyn World, start: (Machine, Vec<Effect>)) -> (Finish, Vec<String>) {
    let (mut machine, mut effects) = start;
    let mut events = Vec::new();
    loop {
        let mut reply = None;
        for effect in effects {
            reply = match effect {
                Effect::Print(text) => {
                    world.print(&text);
                    None
                }
                Effect::Prepare {
                    from,
                    saved,
                    state_dir,
                    log,
                } => Some(
                    world
                        .prepare(&from, &saved, &state_dir, &log)
                        .map_or_else(Event::Io, |()| Event::Done),
                ),
                Effect::Run(run) => {
                    let ran = world.run(&run);
                    events.push(event_line(&run, &ran));
                    Some(Event::Ran(ran))
                }
                Effect::Remove(path) => {
                    world.remove(&path);
                    None
                }
                Effect::Finish(finish) => return (*finish, events),
                Effect::Shared(effect) => world.shared(&effect),
            };
        }
        let event = reply.unwrap_or(Event::Io(String::from("internal error: no event to fold")));
        (machine, effects) = step(machine, event);
    }
}

fn event_line(run: &Run, ran: &Ran) -> String {
    let record = serde_json::json!({
        "at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
        "run": format!("{:?}", run.direction).to_lowercase(),
        "name": run.name,
        "outcome": format!("{:?}", ran.outcome).split([' ', '{', '(']).next().unwrap_or_default(),
        "millis": ran.millis,
    });
    record.to_string()
}

/// The real world: files, processes, locks and stderr.
#[derive(Debug)]
pub struct RealWorld<'a> {
    pub runner: &'a Runner,
    pub paths: Paths,
    pub exe: PathBuf,
    /// This session, as lease holders describe it.
    pub me: Holder,
    pub port_range: (u16, u16),
    pub locks: Locks,
}

impl<'a> RealWorld<'a> {
    pub fn new(
        runner: &'a Runner,
        sys: &crate::sys::Sys,
        session: &foldl_core::state::Session,
        config: &foldl_core::config::Config,
    ) -> RealWorld<'a> {
        let name = session
            .shell
            .map(foldl_core::render::Shell::name)
            .map(String::from)
            .unwrap_or_default();
        let me = Holder {
            session: session.id.clone(),
            pid: session.parent,
            name,
            cwd: sys.cwd.clone(),
            tty: None,
        };
        RealWorld {
            runner,
            paths: sys.paths(),
            exe: sys.exe.clone(),
            me,
            port_range: config.port_range,
            locks: Locks::default(),
        }
    }

    /// Fills in the parent's process name and terminal, from `ps`.
    fn described(&self, parent: u32) -> Holder {
        let out = std::process::Command::new("ps")
            .args(["-o", "tty=,comm=", "-p", &parent.to_string()])
            .output();
        let text = out
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let (tty, comm) = text.split_once(char::is_whitespace).unwrap_or(("?", ""));
        let comm = comm
            .trim()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .trim_start_matches('-');
        let candidates = [self.me.name.as_str(), comm, "process"];
        let name = candidates
            .into_iter()
            .find(|n| !n.is_empty())
            .unwrap_or_default()
            .to_string();
        let tty = Some(tty.to_string()).filter(|t| !t.starts_with('?') && !t.is_empty());
        Holder {
            pid: parent,
            name,
            tty,
            ..self.me.clone()
        }
    }

    fn io(result: std::io::Result<Event>) -> Event {
        result.unwrap_or_else(|e| Event::Io(e.to_string()))
    }

    /// Asks on stderr and reads the answer from `input`; anything not
    /// offered (or no answer) is `a`, abort.
    fn ask(question: &str, choices: &[char], input: Option<impl BufRead>) -> char {
        eprint!("{question}> ");
        let mut answer = String::new();
        let _ = input.map(|mut i| i.read_line(&mut answer));
        let first = answer
            .trim()
            .chars()
            .next()
            .unwrap_or('a')
            .to_ascii_lowercase();
        choices.iter().copied().find(|c| *c == first).unwrap_or('a')
    }
}

impl World for RealWorld<'_> {
    fn print(&mut self, text: &str) {
        eprint!("{text}");
    }

    fn prepare(
        &mut self,
        from: &Path,
        saved: &Path,
        state_dir: &Path,
        log: &Path,
    ) -> Result<(), String> {
        let result = fsx::copy_dir(from, saved)
            .and_then(|()| fsx::ensure_dir(state_dir))
            .and_then(|()| fsx::ensure_dir(log.parent().unwrap_or(Path::new("/"))));
        match result {
            Ok(()) => Ok(()),
            Err(e) => Err(format!("can't save a copy of {}: {e}", from.display())),
        }
    }

    fn run(&mut self, run: &Run) -> Ran {
        self.runner.run(run)
    }

    fn remove(&mut self, path: &Path) {
        let _ = fsx::remove(path);
    }

    fn shared(&mut self, effect: &Shared) -> Option<Event> {
        let paths = self.paths.clone();
        match effect {
            Shared::Lock(lease) => Some(Self::io(
                self.locks.lock(&paths, lease).map(|()| Event::Done),
            )),
            Shared::Inspect { lease, exclusive } => {
                let granted = match exclusive {
                    true => self.locks.try_exclusive(&paths, lease),
                    false => Ok(false),
                };
                let state = leases::read_state(&paths, lease);
                let holders = leases::holders(&paths, lease);
                Some(Self::io(granted.map(|exclusive| Event::Inspected {
                    exclusive,
                    state,
                    holders,
                })))
            }
            Shared::Commit { lease, state, hold } => {
                let written = leases::write_state(&paths, lease, state);
                let held = hold.map(|parent| {
                    let lock = self.locks.shared(&paths, lease)?;
                    let sidecar = leases::sidecar_path(&paths, lease, &self.me.session);
                    hold::spawn_lease_holder(&self.exe, lock, &sidecar, self.described(parent))
                });
                self.locks.unlock(lease);
                let held = held.transpose();
                Some(Self::io(
                    written.and(held).map(|holder| Event::Committed { holder }),
                ))
            }
            Shared::Release(pid) => {
                hold::stop(*pid);
                Some(Event::Done)
            }
            Shared::AllocPorts { lease, name, names } => Some(Self::io(
                leases::alloc_ports(&paths, self.port_range, lease, name, names).map(Event::Ports),
            )),
            Shared::FreePorts { lease, name } => {
                let _ = leases::free_ports(&paths, lease, name);
                None
            }
            Shared::Ask { question, choices } => {
                let tty = std::fs::File::open("/dev/tty").map(std::io::BufReader::new);
                Some(Event::Answer(Self::ask(question, choices, tty.ok())))
            }
            Shared::SpawnHolder { parent } => Some(Self::io(
                hold::spawn_session_holder(&self.exe, *parent, &self.me.session).map(Event::Held),
            )),
            Shared::StopHolder(pid) => {
                hold::stop(*pid);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use foldl_core::config::Config;
    use foldl_core::folder::{Entry, FolderFiles, Host, load};
    use foldl_core::machine::{Ctx, Direction, Exec, Outcome, start};
    use foldl_core::plan::{FolderInfo, FolderStatus};
    use foldl_core::state::{Paths, Session};

    #[derive(Default)]
    struct Fake {
        printed: String,
        removed: usize,
        fail_prepare: bool,
    }

    impl World for Fake {
        fn print(&mut self, text: &str) {
            self.printed.push_str(text);
        }
        fn prepare(&mut self, _: &Path, _: &Path, _: &Path, _: &Path) -> Result<(), String> {
            match self.fail_prepare {
                true => Err("no space".into()),
                false => Ok(()),
            }
        }
        fn run(&mut self, run: &Run) -> Ran {
            let env = Some(run.env.clone());
            Ran {
                outcome: Outcome::Exited { status: 0, env },
                tail: vec![],
                millis: 0,
                warnings: vec![],
                at: String::new(),
            }
        }
        fn remove(&mut self, _: &Path) {
            self.removed += 1;
        }
        fn shared(&mut self, _: &Shared) -> Option<Event> {
            None
        }
    }

    fn folder() -> FolderInfo {
        let entries: BTreeMap<String, Entry> = [
            ("1-a".to_string(), Entry::Dir),
            (
                "1-a/manifest.toml".to_string(),
                Entry::File(b"version = 1".to_vec()),
            ),
            (
                "1-a/up.sh".to_string(),
                Entry::File(b"#!/bin/sh\n".to_vec()),
            ),
        ]
        .into();
        let folder = load(
            &FolderFiles {
                root: "/r".into(),
                entries,
            },
            Host::default(),
        )
        .unwrap();
        FolderInfo {
            root: "/r".into(),
            fingerprint: "f".into(),
            status: FolderStatus::Allowed {
                leases: BTreeMap::new(),
                folder: Box::new(folder),
                watch: BTreeMap::new(),
            },
        }
    }

    fn ctx() -> Ctx {
        let paths = Paths {
            state: "/s".into(),
            data: "/d".into(),
            cache: "/c".into(),
            config: "/cfg".into(),
        };
        Ctx {
            paths,
            config: Config::default(),
            shell: None,
            prompt: false,
            holders: false,
        }
    }

    #[test]
    fn drives_to_the_end() {
        let mut world = Fake::default();
        let (finish, events) = drive(
            &mut world,
            start(
                Session::new("s".into(), 1, None),
                ctx(),
                Default::default(),
                &[folder()],
            ),
        );
        assert_eq!(finish.session.loaded.len(), 1);
        assert_eq!(events.len(), 1);
        assert!(events[0].contains(r#""outcome":"Exited""#), "{}", events[0]);
        assert_eq!(world.printed, "foldl ↑ a\n");
        let (_, _) = drive(&mut world, start(finish.session, ctx(), finish.env, &[]));
        assert_eq!(world.removed, 1);
        assert_eq!(world.shared(&Shared::Release(1)), None);
    }

    #[test]
    fn reports_failed_effects() {
        let mut world = Fake {
            fail_prepare: true,
            ..Fake::default()
        };
        let (finish, _) = drive(
            &mut world,
            start(
                Session::new("s".into(), 1, None),
                ctx(),
                Default::default(),
                &[folder()],
            ),
        );
        assert!(world.printed.contains("no space"));
        assert_eq!(finish.code.exit(), 70);
    }

    /// The PID a lease commit or a holder spawn answered with.
    fn pid_of(event: Option<Event>) -> Option<u32> {
        match event {
            Some(Event::Committed { holder }) => holder,
            Some(Event::Held(pid)) => Some(pid),
            _ => None,
        }
    }

    #[test]
    fn real_world_leases() {
        use foldl_core::lease::{LeaseRef, LeaseState};
        let dir = tempfile::tempdir().unwrap();
        let runner = Runner::new("/usr/bin/true".into(), dir.path().into()).unwrap();
        let sys = crate::sys::Sys {
            vars: [("HOME".into(), dir.path().as_os_str().to_owned())].into(),
            cwd: "/".into(),
            exe: "/usr/bin/true".into(),
        };
        let session = Session::new(
            "s".into(),
            std::process::id(),
            Some(foldl_core::render::Shell::Zsh),
        );
        let mut world = RealWorld::new(&runner, &sys, &session, &Config::default());
        let lease = LeaseRef {
            id: "l".into(),
            key: "custom:l".into(),
        };
        assert_eq!(
            world.shared(&Shared::Lock(lease.clone())),
            Some(Event::Done)
        );
        let inspected = world.shared(&Shared::Inspect {
            lease: lease.clone(),
            exclusive: false,
        });
        assert!(matches!(
            inspected,
            Some(Event::Inspected {
                exclusive: false,
                ..
            })
        ));
        let commit = Shared::Commit {
            lease: lease.clone(),
            state: LeaseState::default(),
            hold: Some(std::process::id()),
        };
        let pid = pid_of(world.shared(&commit)).unwrap();
        assert_eq!(world.shared(&Shared::Release(pid)), Some(Event::Done));
        let ports = Shared::AllocPorts {
            lease: lease.clone(),
            name: "pg".into(),
            names: vec!["P".into()],
        };
        assert!(matches!(world.shared(&ports), Some(Event::Ports(_))));
        assert_eq!(
            world.shared(&Shared::FreePorts {
                lease,
                name: "pg".into()
            }),
            None
        );
        let holder = pid_of(world.shared(&Shared::SpawnHolder { parent: 1 })).unwrap();
        assert_eq!(pid_of(None), None);
        assert_eq!(world.shared(&Shared::StopHolder(holder)), None);
        assert!(matches!(
            world.shared(&Shared::Ask {
                question: String::new(),
                choices: vec!['a']
            }),
            Some(Event::Answer(_))
        ));
    }

    #[test]
    fn real_world_files() {
        let dir = tempfile::tempdir().unwrap();
        let runner = Runner::new("/bin/false".into(), dir.path().into()).unwrap();
        let sys = crate::sys::Sys {
            vars: Default::default(),
            cwd: "/".into(),
            exe: "/bin/false".into(),
        };
        let session = Session::new("s".into(), 1, None);
        let mut world = RealWorld::new(&runner, &sys, &session, &Config::default());
        let from = dir.path().join("from");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join("up.sh"), "echo hi\n").unwrap();
        let saved = dir.path().join("saved");
        world
            .prepare(
                &from,
                &saved,
                &dir.path().join("data"),
                &dir.path().join("logs/x.log"),
            )
            .unwrap();
        assert!(saved.join("up.sh").exists() && dir.path().join("logs").is_dir());
        let ran = world.run(&Run {
            name: "x".into(),
            direction: Direction::Down,
            exec: Exec::Script {
                shebang: foldl_core::folder::parse_shebang(b"#!/bin/sh").unwrap(),
                path: saved.join("up.sh"),
            },
            env: Default::default(),
            cwd: dir.path().into(),
            timeout: std::time::Duration::from_secs(5),
            log: dir.path().join("logs/x.log"),
        });
        assert_eq!(ran.tail, ["hi"]);
        let err = world
            .prepare(&dir.path().join("missing"), &saved, &saved, &saved)
            .unwrap_err();
        assert!(err.starts_with("can't save a copy of"));
        world.remove(&saved);
        assert_eq!(
            RealWorld::ask("q", &['r', 's'], Some(std::io::Cursor::new("S\n"))),
            's'
        );
        assert_eq!(
            RealWorld::ask("q", &['r', 's'], Some(std::io::Cursor::new("x\n"))),
            'a'
        );
        assert_eq!(
            RealWorld::ask("q", &['r'], None::<std::io::Cursor<&str>>),
            'a'
        );
        assert_eq!(world.described(999_999_999).name, "process");
        let null = std::io::BufReader::new(std::fs::File::open("/dev/null").unwrap());
        assert_eq!(RealWorld::ask("q", &['r'], Some(null)), 'a');
        let failed = RealWorld::io(Err(std::io::Error::other("boom")));
        assert_eq!(failed, Event::Io("boom".into()));
        assert!(!saved.exists());
        world.print("");
    }
}

// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! The interpreter: performs the machine's effects through a [`World`] and
//! folds each result back in, until the machine finishes.

use std::path::Path;

use foldl_core::machine::{Effect, Event, Finish, Machine, Ran, Run, step};

use crate::fsx;
use crate::runner::Runner;

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

/// The real world: files, processes and stderr.
#[derive(Debug)]
pub struct RealWorld<'a> {
    pub runner: &'a Runner,
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
        result.map_err(|e| format!("can't save a copy of {}: {e}", from.display()))
    }

    fn run(&mut self, run: &Run) -> Ran {
        self.runner.run(run)
    }

    fn remove(&mut self, path: &Path) {
        let _ = fsx::remove(path);
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
            }
        }
        fn remove(&mut self, _: &Path) {
            self.removed += 1;
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

    #[test]
    fn real_world_files() {
        let dir = tempfile::tempdir().unwrap();
        let runner = Runner::new("/bin/false".into(), dir.path().into()).unwrap();
        let mut world = RealWorld { runner: &runner };
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
        assert!(!saved.exists());
        world.print("");
    }
}

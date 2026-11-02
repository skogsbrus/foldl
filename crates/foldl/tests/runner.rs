// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! The script runner against real shells and processes.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use foldl::runner::{Runner, parse_env, which};
use foldl_core::env::{Env, Value};
use foldl_core::folder::{Builtin, ScriptShell, Shebang};
use foldl_core::machine::{Direction, Exec, Outcome, Run};

struct Bench {
    dir: tempfile::TempDir,
    runner: Runner,
}

impl Bench {
    fn new() -> Bench {
        let dir = tempfile::tempdir().unwrap();
        let runner = Runner::new(common::foldl(), dir.path().to_path_buf()).unwrap();
        Bench { dir, runner }
    }

    fn script(&self, body: &str) -> PathBuf {
        let path = self.dir.path().join("up.sh");
        std::fs::write(&path, body).unwrap();
        path
    }

    fn run(
        &self,
        program: &str,
        shell: ScriptShell,
        body: &str,
        direction: Direction,
        timeout_ms: u64,
    ) -> foldl_core::machine::Ran {
        let path = self.script(body);
        let env: Env = [
            (
                "PATH".to_string(),
                Value::from(std::env::var("PATH").unwrap().as_str()),
            ),
            ("KEEP".into(), Value::from("k")),
        ]
        .into();
        self.runner.run(&Run {
            name: "m".into(),
            direction,
            exec: Exec::Script {
                shebang: Shebang {
                    program: program.into(),
                    shell,
                },
                path,
            },
            env,
            cwd: self.dir.path().to_path_buf(),
            timeout: Duration::from_millis(timeout_ms),
            log: self.dir.path().join("logs/m.log"),
        })
    }
}

fn exited(outcome: &Outcome) -> (i32, Option<Env>) {
    match outcome {
        Outcome::Exited { status, env } => (*status, env.clone()),
        other => panic!("not exited: {other:?}"),
    }
}

#[test]
fn up_captures_the_environment_in_every_shell() {
    let bench = Bench::new();
    for shell in common::shells() {
        let kind = match shell.kind {
            "zsh" => ScriptShell::Zsh,
            _ => ScriptShell::Bash,
        };
        let program = shell.program.to_string_lossy().into_owned();
        let ran = bench.run(
            &program,
            kind,
            "export NEW='a b'\nunset KEEP\necho hello\n",
            Direction::Up,
            10_000,
        );
        let (status, env) = exited(&ran.outcome);
        let env = env.expect("env captured");
        assert_eq!(
            (status, env.get("NEW")),
            (0, Some(&Value::from("a b"))),
            "{}",
            shell.label
        );
        assert!(!env.contains_key("KEEP"));
        assert_eq!(ran.tail, ["hello"]);
        let old = shell.old_bash;
        assert_eq!(
            ran.warnings.len(),
            usize::from(old),
            "{}: {:?}",
            shell.label,
            ran.warnings
        );
    }
}

#[test]
fn exit_inside_the_script_still_reports() {
    let bench = Bench::new();
    let ran = bench.run(
        "sh",
        ScriptShell::Sh,
        "export X=1\nexit 3\n",
        Direction::Up,
        10_000,
    );
    let (status, env) = exited(&ran.outcome);
    assert_eq!(status, 3);
    assert_eq!(env.unwrap()["X"], Value::from("1"));
}

#[test]
fn downs_dont_capture() {
    let bench = Bench::new();
    let ran = bench.run(
        "/bin/sh",
        ScriptShell::Sh,
        "echo down >&2\n",
        Direction::Down,
        10_000,
    );
    assert_eq!(exited(&ran.outcome), (0, None));
    assert_eq!(ran.tail, ["down"]);
}

#[test]
fn timeouts_kill_the_process_group() {
    let bench = Bench::new();
    let ran = bench.run(
        "sh",
        ScriptShell::Sh,
        "sleep 30 & wait\n",
        Direction::Down,
        400,
    );
    assert_eq!(ran.outcome, Outcome::TimedOut);
    let stubborn = bench.run(
        "sh",
        ScriptShell::Sh,
        "trap '' TERM\nwhile :; do sleep 0.05; done\n",
        Direction::Down,
        100,
    );
    assert_eq!(stubborn.outcome, Outcome::TimedOut);
}

#[test]
fn ctrl_c_interrupts() {
    let bench = Bench::new();
    bench.runner.interrupt();
    let ran = bench.run("sh", ScriptShell::Sh, "sleep 30\n", Direction::Up, 10_000);
    assert_eq!(ran.outcome, Outcome::Interrupted);
}

#[test]
fn missing_programs_are_unavailable() {
    let bench = Bench::new();
    let ran = bench.run("no-such-shell", ScriptShell::Bash, "", Direction::Up, 1000);
    assert_eq!(
        ran.outcome,
        Outcome::Unavailable("no-such-shell not found on PATH".into())
    );
    let ran = bench.run("/no/bash", ScriptShell::Bash, "", Direction::Up, 1000);
    assert_eq!(
        ran.outcome,
        Outcome::Unavailable("/no/bash not found on PATH".into())
    );
    let ran = bench.run("/dev/null", ScriptShell::Sh, "", Direction::Up, 1000);
    assert!(
        matches!(ran.outcome, Outcome::Unavailable(ref m) if m.starts_with("can't start /dev/null")),
        "{ran:?}"
    );
    let ran = bench.runner.run(&Run {
        name: "b".into(),
        direction: Direction::Up,
        exec: Exec::Builtin {
            builtin: Builtin::Dotenv {
                file: ".env".into(),
                required: true,
            },
            watch: Default::default(),
        },
        env: Env::new(),
        cwd: "/".into(),
        timeout: Duration::from_secs(1),
        log: "/dev/null/b.log".into(),
    });
    assert!(
        matches!(ran.outcome, Outcome::Unavailable(ref m) if m.starts_with("can't write")),
        "{ran:?}"
    );
}

#[test]
fn helpers() {
    assert_eq!(
        which("sh", Some(&Value::from("/nope:/bin"))),
        Some(Path::new("/bin/sh").to_path_buf())
    );
    assert_eq!(which("sh", None), None);
    let env = parse_env(b"A=1\0B=x=y\0junk\0\xff=1\0");
    assert_eq!(
        env,
        [
            ("A".to_string(), Value::from("1")),
            ("B".to_string(), Value::from("x=y"))
        ]
        .into()
    );
}

#[test]
fn dump_env_command() {
    let f = common::Fixture::new();
    let out = f.dir.path().join("env");
    assert_eq!(f.run(&["__dump-env", out.to_str().unwrap()]).code, 0);
    assert_eq!(
        parse_env(&std::fs::read(&out).unwrap())["HOME"],
        Value::from(f.home.to_str().unwrap())
    );
    assert_eq!(f.run(&["__dump-env", "/dev/null/x"]).code, 70);
}

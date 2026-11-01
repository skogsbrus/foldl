// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Shared helpers for integration tests.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;

/// A shell under test: a display name, the program, and which dialect.
#[derive(Debug, Clone)]
pub struct TestShell {
    pub label: &'static str,
    pub program: PathBuf,
    pub kind: &'static str,
}

/// The shells to test against: pinned through Nix when the flake sets
/// `FOLDL_TEST_*`, otherwise whatever the system has (macOS `/bin/bash` is
/// 3.2). A shell that isn't installed is skipped, but at least one bash
/// and one zsh must exist.
pub fn shells() -> Vec<TestShell> {
    let from_env = |var: &str| std::env::var_os(var).map(PathBuf::from);
    let candidates = [
        (
            "bash3",
            from_env("FOLDL_TEST_BASH3").or(Some("/bin/bash".into())),
            "bash",
        ),
        (
            "bash5",
            from_env("FOLDL_TEST_BASH5").or(Some("bash".into())),
            "bash",
        ),
        (
            "zsh",
            from_env("FOLDL_TEST_ZSH").or(Some("zsh".into())),
            "zsh",
        ),
    ];
    let found: Vec<TestShell> = candidates
        .into_iter()
        .filter_map(|(label, program, kind)| {
            let program = program?;
            let ok = Command::new(&program)
                .arg("-c")
                .arg("true")
                .status()
                .is_ok_and(|s| s.success());
            ok.then_some(TestShell {
                label,
                program,
                kind,
            })
        })
        .collect();
    assert!(found.iter().any(|s| s.kind == "bash"), "no bash found");
    assert!(found.iter().any(|s| s.kind == "zsh"), "no zsh found");
    found
}

pub fn foldl() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_foldl"))
}

/// An isolated world: its own HOME, XDG directories and a project folder.
pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub home: PathBuf,
    pub project: PathBuf,
}

impl Fixture {
    pub fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap().join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project).unwrap();
        Fixture { dir, home, project }
    }

    /// Writes a file under the project, creating directories.
    pub fn write(&self, rel: &str, content: &str) -> PathBuf {
        let path = self.project.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    /// A `foldl` command with a clean environment, run in the project.
    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(foldl());
        cmd.args(args);
        self.prepare(cmd)
    }

    /// Gives any command the fixture's clean environment and directory.
    pub fn prepare(&self, mut cmd: Command) -> Command {
        cmd.current_dir(&self.project).env_clear();
        cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        cmd.env("HOME", &self.home)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default());
        for var in [
            "LLVM_PROFILE_FILE",
            "FOLDL_TEST_BASH3",
            "FOLDL_TEST_BASH5",
            "FOLDL_TEST_ZSH",
        ] {
            if let Some(value) = std::env::var_os(var) {
                cmd.env(var, value);
            }
        }
        cmd
    }

    pub fn run(&self, args: &[&str]) -> Out {
        Out::from(self.cmd(args).output().unwrap())
    }
}

/// A finished command's exit code and text output.
#[derive(Debug)]
pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl From<std::process::Output> for Out {
    fn from(o: std::process::Output) -> Out {
        Out {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    }
}

impl Fixture {
    /// Runs `script` in `shell` with the hook installed, from `$HOME`.
    pub fn shell(&self, shell: &TestShell, script: &str) -> Out {
        let setup = format!("eval \"$('{}' hook {})\"\n", foldl().display(), shell.kind);
        let mut cmd = self.cmd(&[]);
        cmd.current_dir(&self.home);
        let mut cmd = std::process::Command::new(&shell.program);
        cmd.arg("-c")
            .arg(setup + script)
            .current_dir(&self.home)
            .env_clear();
        cmd.env("HOME", &self.home)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default());
        if let Some(value) = std::env::var_os("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", value);
        }
        Out::from(cmd.output().unwrap())
    }

    /// Writes and allows a folder's migrations: `(path under .foldl, content)`.
    pub fn folder(&self, rel: &str, files: &[(&str, &str)]) {
        for (path, content) in files {
            self.write(&format!("{rel}/.foldl/{path}"), content);
        }
        let out = self.run(&["allow", self.project.join(rel).to_str().unwrap()]);
        assert_eq!(out.code, 0, "{out:?}");
    }
}

impl Fixture {
    /// Runs git in `dir` with a neutral config; panics on failure.
    pub fn git(&self, dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

impl Fixture {
    /// The calls migrations appended to `$HOME/calls`.
    pub fn calls(&self) -> String {
        std::fs::read_to_string(self.home.join("calls")).unwrap_or_default()
    }

    /// Waits until no holder of this fixture is running, so their effects
    /// (and coverage data) are complete. Reads foldl's own state: session
    /// holders from `state.json`, lease holders from their sidecars.
    pub fn settle(&self) {
        let state = self.home.join(".local/state/foldl");
        let pids = |pattern: &str, key: &str| -> Vec<String> {
            let files = glob(&state.join(pattern));
            files
                .iter()
                .filter_map(|f| std::fs::read_to_string(f).ok())
                .filter_map(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .filter_map(|v| v.get(key).and_then(|p| p.as_u64()).map(|p| p.to_string()))
                .collect()
        };
        for _ in 0..400 {
            let all = [
                pids("sessions/*/state.json", "holder"),
                pids("leases/*/holders/*.json", "holder_pid"),
            ]
            .concat();
            let alive = all.iter().any(|pid| alive(pid));
            if !alive {
                std::thread::sleep(std::time::Duration::from_millis(50));
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!("holders still running for {}", state.display());
    }

    /// Starts `script` in a shell in the background.
    pub fn spawn_shell(&self, shell: &TestShell, script: &str) -> std::process::Child {
        let setup = format!("eval \"$('{}' hook {})\"\n", foldl().display(), shell.kind);
        let mut cmd = std::process::Command::new(&shell.program);
        cmd.arg("-c")
            .arg(setup + script)
            .current_dir(&self.home)
            .env_clear();
        cmd.env("HOME", &self.home)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default());
        if let Some(value) = std::env::var_os("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", value);
        }
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    }
}

/// `kill -0`, through the shell builtin.
fn alive(pid: &str) -> bool {
    std::process::Command::new("sh")
        .args(["-c", &format!("kill -0 {pid} 2>/dev/null")])
        .status()
        .is_ok_and(|s| s.success())
}

/// Paths matching `*` segments, without a glob crate.
fn glob(pattern: &std::path::Path) -> Vec<PathBuf> {
    let parts: Vec<String> = pattern
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts
        .iter()
        .skip(1)
        .fold(vec![PathBuf::from(&parts[0])], |found, part| {
            found
                .into_iter()
                .flat_map(|dir| match part.as_str() {
                    "*" => std::fs::read_dir(&dir)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .map(|e| e.path())
                        .collect(),
                    p if p.starts_with("*.") => {
                        let ext = &p[2..];
                        std::fs::read_dir(&dir)
                            .into_iter()
                            .flatten()
                            .flatten()
                            .map(|e| e.path())
                            .filter(|e| e.extension().is_some_and(|x| x == ext))
                            .collect()
                    }
                    p => vec![dir.join(p)],
                })
                .filter(|p| p.exists())
                .collect()
        })
}

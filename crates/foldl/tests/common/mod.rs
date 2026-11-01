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
        cmd.args(args).current_dir(&self.project).env_clear();
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

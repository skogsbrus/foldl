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

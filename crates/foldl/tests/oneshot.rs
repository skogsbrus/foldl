// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! `foldl exec` and `foldl export`.

mod common;

use common::{Fixture, shells};

const SERVICE: &[(&str, &str)] = &[
    ("1-env/manifest.toml", "version = 1\n[env]\nA = \"1\""),
    ("2-svc/manifest.toml", "version = 1"),
    (
        "2-svc/up.sh",
        "#!/bin/sh\necho up >> \"$HOME/calls\"\nexport B=2\n",
    ),
    ("2-svc/down.sh", "#!/bin/sh\necho down >> \"$HOME/calls\"\n"),
];

fn calls(f: &Fixture) -> String {
    std::fs::read_to_string(f.home.join("calls")).unwrap_or_default()
}

#[test]
fn exec_loads_runs_and_unloads() {
    let f = Fixture::new();
    f.folder(".", SERVICE);
    let out = f.run(&["exec", "--", "sh", "-c", "echo \"$A $B\"; exit 7"]);
    assert_eq!((out.code, out.stdout.as_str()), (7, "1 2\n"), "{out:?}");
    assert_eq!(calls(&f), "up\ndown\n");
    assert!(
        !f.home
            .join(".local/state/foldl/sessions")
            .read_dir()
            .unwrap()
            .any(|_| true),
        "session removed"
    );
    let sub = f.project.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let out = f.run(&["exec", "sub", "--", "pwd"]);
    assert_eq!(out.stdout.trim(), sub.to_str().unwrap());
}

#[test]
fn exec_failures() {
    let f = Fixture::new();
    f.write(
        ".foldl/1-env/manifest.toml",
        "version = 1\n[env]\nA = \"1\"",
    );
    let out = f.run(&["exec", "--json", "--", "true"]);
    assert_eq!(out.code, 77);
    assert!(
        out.stdout
            .starts_with(r#"{"error":{"code":"untrusted","exit":77,"message":"#),
        "{out:?}"
    );
    assert!(out.stderr.contains("fix: foldl allow ~/project"));
    f.folder(
        ".",
        &[
            ("2-bad/manifest.toml", "version = 1"),
            ("2-bad/up.sh", "#!/bin/sh\nexit 1\n"),
        ],
    );
    assert_eq!(f.run(&["exec", "--", "true"]).code, 80);
    assert_eq!(f.run(&["exec", "/nonexistent", "--", "true"]).code, 64);
    std::fs::remove_dir_all(f.project.join(".foldl/2-bad")).unwrap();
    assert_eq!(f.run(&["allow"]).code, 0);
    let out = f.run(&["exec", "--", "/no/such/program"]);
    assert_eq!(out.code, 69, "{out:?}");
    assert_eq!(
        f.run(&["exec", "--", "sh", "-c", "kill -TERM $$"]).code,
        143
    );
}

#[test]
fn exec_joins_a_session_that_has_the_folder_loaded() {
    let f = Fixture::new();
    f.folder(".", SERVICE);
    for shell in shells() {
        let script = format!(
            "cd project; _foldl_hook; '{}' exec -- sh -c 'echo $A$B'; cd /; _foldl_hook",
            common::foldl().display()
        );
        let out = f.shell(&shell, &script);
        assert_eq!(out.stdout, "12\n", "{out:?}");
        assert_eq!(
            calls(&f),
            "up\ndown\n",
            "one up for the shell, none for exec"
        );
        std::fs::remove_file(f.home.join("calls")).unwrap();
    }
}

#[test]
fn exec_from_another_folders_session_loads_fresh() {
    let f = Fixture::new();
    f.folder(".", SERVICE);
    f.folder(
        "other",
        &[("1-o/manifest.toml", "version = 1\n[env]\nO = \"o\"")],
    );
    let script = format!(
        "cd project/other; _foldl_hook; '{}' exec ~/project -- sh -c 'echo \"$A$B${{O-none}}\"'",
        common::foldl().display()
    );
    let out = f.shell(&shells()[0], &script);
    assert_eq!(out.stdout, "12none\n", "{out:?}");
}

#[test]
fn export_formats() {
    let f = Fixture::new();
    f.folder(".", SERVICE);
    let bash = f.run(&["export", "bash"]);
    assert_eq!(bash.stdout, "export A='1'\nexport B='2'\n", "{bash:?}");
    assert_eq!(f.run(&["export", "zsh"]).stdout, bash.stdout);
    assert_eq!(
        f.run(&["export", "json", "."]).stdout,
        "{\"A\":\"1\",\"B\":\"2\"}\n"
    );
    assert_eq!(calls(&f), "up\ndown\n".repeat(3));
    let out = f.run(&["export", "json", "--json", "/nonexistent"]);
    assert!(
        out.stdout.starts_with(r#"{"error":{"code":"usage""#),
        "{out:?}"
    );
}

#[test]
fn export_reuses_an_agent_session() {
    let f = Fixture::new();
    f.folder(".", SERVICE);
    // A live owner: a parent of PID 1 means the agent is gone, and its
    // holders would tear the session down at once.
    let mut agent = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let dir = f.home.join(".local/state/foldl/sessions/agent-1");
    std::fs::create_dir_all(&dir).unwrap();
    let state = format!(
        r#"{{"version":1,"id":"agent-1","parent":{},"shell":null,"loaded":[]}}"#,
        agent.id()
    );
    std::fs::write(dir.join("state.json"), state).unwrap();
    let out = f
        .cmd(&["export", "json"])
        .env("FOLDL_SESSION", "agent-1")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "{\"A\":\"1\",\"B\":\"2\"}\n"
    );
    let state = std::fs::read_to_string(dir.join("state.json")).unwrap();
    assert!(state.contains("\"name\":\"svc\""), "kept loaded: {state}");
    assert_eq!(calls(&f), "up\n");
    agent.kill().unwrap();
    agent.wait().unwrap();
    for _ in 0..400 {
        if calls(&f) == "up\ndown\n" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert_eq!(
        calls(&f),
        "up\ndown\n",
        "the agent's exit releases the lease"
    );
    f.settle();
}

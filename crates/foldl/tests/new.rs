// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! `foldl new` and the shared-or-private choice.

mod common;

use std::process::Stdio;

use common::Fixture;

fn read(f: &Fixture, rel: &str) -> String {
    std::fs::read_to_string(f.project.join(rel)).unwrap_or_default()
}

#[test]
fn creates_numbered_migrations_that_validate() {
    let f = Fixture::new();
    assert_eq!(
        f.run(&["new", "env"]).stderr,
        "foldl: created .foldl/001-env; edit it, then run foldl allow\n"
    );
    assert_eq!(read(&f, ".foldl/001-env/manifest.toml"), "version = 1\n");
    assert!(read(&f, ".foldl/001-env/up.sh").starts_with("#!/usr/bin/env bash\n"));
    assert_eq!(f.run(&["new", "--shared", "pg"]).code, 0);
    assert_eq!(
        read(&f, ".foldl/002-pg/manifest.toml"),
        "version = 1\nscope = \"shared\"\n"
    );
    assert!(read(&f, ".foldl/002-pg/down.sh").starts_with("#!/usr/bin/env bash"));
    assert_eq!(f.run(&["new", "--interactive", "tools"]).code, 0);
    assert_eq!(
        read(&f, ".foldl/003-tools/manifest.toml"),
        "version = 1\ninteractive = true\n"
    );
    assert_eq!(f.run(&["new", "--local", "mine"]).code, 0);
    assert!(f.project.join(".foldl/004-mine.local/up.sh").exists());
    assert_eq!(f.run(&["check"]).code, 0);
    std::fs::create_dir_all(f.project.join(".foldl/0099-x")).unwrap();
    assert_eq!(f.run(&["new", "next"]).code, 0);
    assert!(f.project.join(".foldl/0100-next").exists());
}

#[test]
fn rejects_bad_names_and_unwritable_folders() {
    let f = Fixture::new();
    assert_eq!(f.run(&["new", "Bad_Name"]).code, 64);
    std::fs::create_dir_all(f.project.join(".foldl")).unwrap();
    std::fs::set_permissions(
        f.project.join(".foldl"),
        std::os::unix::fs::PermissionsExt::from_mode(0o500),
    )
    .unwrap();
    let out = f.run(&["new", "x"]);
    std::fs::set_permissions(
        f.project.join(".foldl"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    assert_eq!(out.code, 70, "{out:?}");
}

#[test]
fn in_a_repository_local_migrations_are_ignored() {
    let f = Fixture::new();
    f.git(&f.project, &["init", "-q"]);
    std::fs::write(f.project.join(".gitignore"), "target").unwrap();
    assert_eq!(f.run(&["new", "--committed", "env"]).code, 0);
    assert_eq!(f.run(&["new", "--local", "secrets"]).code, 0);
    assert_eq!(f.run(&["new", "--local", "more"]).code, 0);
    assert_eq!(read(&f, ".gitignore"), "target\n/.foldl/*.local/\n");
    assert!(
        !f.git(&f.project, &["status", "--porcelain", "-uall"])
            .contains(".local")
    );
    assert!(
        f.git(&f.project, &["status", "--porcelain", "-uall"])
            .contains("001-env")
    );
}

#[test]
fn private_folders_go_to_info_exclude() {
    let f = Fixture::new();
    f.git(&f.project, &["init", "-q"]);
    assert_eq!(f.run(&["new", "--private", "env"]).code, 0);
    assert_eq!(f.run(&["new", "--local", "mine"]).code, 0);
    assert_eq!(
        read(&f, ".git/info/exclude").lines().last(),
        Some("/.foldl/")
    );
    assert_eq!(f.git(&f.project, &["status", "--porcelain"]), "");
    assert!(!f.project.join(".gitignore").exists());
    let sub = f.project.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let out = f.cmd(&["new", "inner"]).current_dir(&sub).output().unwrap();
    assert!(out.status.success());
    assert!(
        f.project.join(".foldl/003-inner").exists(),
        "nearest .foldl, not cwd"
    );
    assert_eq!(f.run(&["allow"]).code, 0);
    let status = f.run(&["status"]).stdout;
    assert!(
        status.starts_with("~/project  (0 sessions, private (not committed))\n"),
        "{status}"
    );
}

/// Runs `foldl new` on a terminal (via script(1)) answering `answer`.
fn on_a_terminal(f: &Fixture, name: &str, answer: &str) -> String {
    let foldl = common::foldl();
    let program = match cfg!(target_os = "macos") {
        true => "/usr/bin/script",
        false => "script",
    };
    let mut cmd = std::process::Command::new(program);
    match cfg!(target_os = "macos") {
        true => cmd
            .args(["-q", "/dev/null"])
            .arg(&foldl)
            .args(["new", name]),
        false => cmd
            .args(["-qec"])
            .arg(format!("{} new {name}", foldl.display()))
            .arg("/dev/null"),
    };
    let mut child = f
        .prepare(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // script(1) drops input that arrives before the prompt or right before
    // EOF, so answer after the prompt and keep stdin open a moment.
    std::thread::sleep(std::time::Duration::from_millis(500));
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), answer.as_bytes()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn asks_on_a_terminal_showing_what_private_loses() {
    let f = Fixture::new();
    f.git(&f.project, &["init", "-q"]);
    let shown = on_a_terminal(&f, "env", "n\n");
    assert!(
        shown.contains("✗ teammates don't get the same setup"),
        "{shown}"
    );
    assert!(read(&f, ".git/info/exclude").contains("/.foldl/"));
    let quiet = Fixture::new();
    quiet.git(&quiet.project, &["init", "-q"]);
    assert_eq!(quiet.run(&["new", "env"]).code, 0, "no terminal: shared");
    assert!(!read(&quiet, ".git/info/exclude").contains("/.foldl/"));
    let g = Fixture::new();
    g.git(&g.project, &["init", "-q"]);
    on_a_terminal(&g, "env", "\n");
    assert!(!read(&g, ".git/info/exclude").contains("/.foldl/"));
}

// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Shared migrations with real shells, processes and locks.

mod common;

use std::time::Duration;

use common::{Fixture, Out, shells};

fn pg(up_sleep: &str, down: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "1-pg/manifest.toml",
            "version = 1\nscope = \"shared\"\n[ports]\nPGPORT = \"auto\"".to_string(),
        ),
        (
            "1-pg/up.sh",
            format!("#!/bin/sh\nsleep {up_sleep}\necho up >> \"$HOME/calls\"\nexport PG=on\n"),
        ),
        ("1-pg/down.sh", format!("#!/bin/sh\n{down}\n")),
    ]
}

fn folder(f: &Fixture, rel: &str, files: &[(&str, String)]) {
    let pairs: Vec<(&str, &str)> = files.iter().map(|(p, c)| (*p, c.as_str())).collect();
    f.folder(rel, &pairs);
}

fn wait_for(path: &std::path::Path) {
    while !path.exists() {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn two_shells_entering_at_once_start_one_instance() {
    let f = Fixture::new();
    folder(&f, ".", &pg("0.3", "echo down >> \"$HOME/calls\""));
    let shell = &shells()[0];
    let script = "while [ ! -e \"$HOME/go\" ]; do sleep 0.01; done; cd project; _foldl_hook; echo \"$PGPORT\"; cd /; _foldl_hook";
    let a = f.spawn_shell(shell, script);
    let b = f.spawn_shell(shell, script);
    std::fs::write(f.home.join("go"), "").unwrap();
    let (a, b) = (
        Out::from(a.wait_with_output().unwrap()),
        Out::from(b.wait_with_output().unwrap()),
    );
    f.settle();
    assert_eq!(
        f.calls(),
        "up\ndown\n",
        "flox#4791: one up, one down\n{a:?}\n{b:?}"
    );
    assert_eq!(a.stdout, b.stdout, "both see the same port");
}

#[test]
fn entering_during_the_last_down_waits_for_it() {
    let f = Fixture::new();
    folder(
        &f,
        ".",
        &pg(
            "0",
            "echo down-start >> \"$HOME/calls\"; touch \"$HOME/downing\"; sleep 0.5; echo down-end >> \"$HOME/calls\"",
        ),
    );
    let shell = &shells()[0];
    let leaving = f.spawn_shell(shell, "cd project; _foldl_hook; touch \"$HOME/ready\"; while [ ! -e \"$HOME/leave\" ]; do sleep 0.01; done; cd /; _foldl_hook");
    wait_for(&f.home.join("ready"));
    std::fs::write(f.home.join("leave"), "").unwrap();
    wait_for(&f.home.join("downing"));
    let entering = f.shell(
        shell,
        "cd project; _foldl_hook; echo \"PG=$PG\"; cd /; _foldl_hook",
    );
    leaving.wait_with_output().unwrap();
    f.settle();
    assert_eq!(entering.stdout, "PG=on\n");
    assert_eq!(
        f.calls(),
        "up\ndown-start\ndown-end\nup\ndown-start\ndown-end\n",
        "flox#4781: fresh up after the down"
    );
}

#[test]
fn a_killed_terminal_runs_its_downs_and_the_last_session_tears_down() {
    let f = Fixture::new();
    let mut files = pg("0", "echo pg-down >> \"$HOME/calls\"");
    files.push(("2-agent/manifest.toml", "version = 1".to_string()));
    files.push((
        "2-agent/up.sh",
        "#!/bin/sh\necho agent-up >> \"$HOME/calls\"\n".to_string(),
    ));
    files.push((
        "2-agent/down.sh",
        "#!/bin/sh\necho agent-down >> \"$HOME/calls\"\n".to_string(),
    ));
    folder(&f, ".", &files);
    for shell in shells() {
        let mut killed = f.spawn_shell(
            &shell,
            "cd project; _foldl_hook; touch \"$HOME/ready\"; sleep 60",
        );
        wait_for(&f.home.join("ready"));
        let survivor = f.spawn_shell(&shell, "cd project; _foldl_hook; touch \"$HOME/ready2\"; while [ ! -e \"$HOME/leave\" ]; do sleep 0.01; done; cd /; _foldl_hook");
        wait_for(&f.home.join("ready2"));
        killed.kill().unwrap();
        killed.wait().unwrap();
        let calls = |f: &Fixture| f.calls();
        for _ in 0..200 {
            if calls(&f).contains("agent-down\nagent-up")
                || calls(&f).matches("agent-down").count() == 1
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            f.calls(),
            "up\nagent-up\nagent-up\nagent-down\n",
            "{}: killed session's custom down ran; pg kept",
            shell.label
        );
        std::fs::write(f.home.join("leave"), "").unwrap();
        let out = Out::from(survivor.wait_with_output().unwrap());
        f.settle();
        assert_eq!(
            f.calls(),
            "up\nagent-up\nagent-up\nagent-down\nagent-down\npg-down\n",
            "{}: {out:?}",
            shell.label
        );
        for name in ["calls", "ready", "ready2", "leave"] {
            std::fs::remove_file(f.home.join(name)).unwrap();
        }
    }
}

#[test]
fn ports_never_collide_across_worktrees() {
    let f = Fixture::new();
    folder(&f, "a", &pg("0", "true"));
    folder(&f, "b", &pg("0", "true"));
    let out = f.shell(&shells()[0], "cd project/a; _foldl_hook; A=$PGPORT; cd ../b; _foldl_hook; echo \"$A $PGPORT\"; cd /; _foldl_hook");
    f.settle();
    let ports: Vec<&str> = out.stdout.split_whitespace().collect();
    assert_eq!(ports.len(), 2, "{out:?}");
    assert_ne!(ports[0], ports[1]);
}

#[test]
fn a_failed_down_leaves_the_lease_dirty() {
    let f = Fixture::new();
    folder(&f, ".", &pg("0", "echo broken; exit 3"));
    let out = f.run(&["exec", "--", "true"]);
    assert_eq!(out.code, 0, "the command ran: {out:?}");
    assert!(
        out.stderr
            .contains("foldl ✗ pg: down failed (exit 3); the lease is dirty"),
        "{out:?}"
    );
    let again = f.run(&["exec", "--json", "--", "true"]);
    assert_eq!(again.code, 75, "on_dirty defaults to abort: {again:?}");
    assert!(
        again
            .stderr
            .contains("foldl ! pg: dirty lease; not loading")
            || again.stderr.contains("the lease is dirty; not loading"),
        "{again:?}"
    );
    f.write("../.config/foldl/config.toml", "on_dirty = \"skip\"");
    let skipped = f.run(&["exec", "--", "sh", "-c", "echo $PG"]);
    assert_eq!(skipped.stdout, "on\n", "{skipped:?}");
    f.settle();
}

#[test]
fn several_shared_migrations_in_one_lease() {
    let f = Fixture::new();
    let mut files = pg("0", "echo pg-down >> \"$HOME/calls\"");
    files.push((
        "2-cache/manifest.toml",
        "version = 1\nscope = \"shared\"".to_string(),
    ));
    files.push((
        "2-cache/up.sh",
        "#!/bin/sh\necho cache-up >> \"$HOME/calls\"\n".to_string(),
    ));
    files.push((
        "2-cache/down.sh",
        "#!/bin/sh\necho cache-down >> \"$HOME/calls\"\n".to_string(),
    ));
    folder(&f, ".", &files);
    let out = f.shell(&shells()[0], "cd project; _foldl_hook; cd /; _foldl_hook");
    f.settle();
    assert_eq!(f.calls(), "up\ncache-up\ncache-down\npg-down\n", "{out:?}");
}

/// Runs a bash script on a terminal (script(1)), answering `answer`.
fn on_a_terminal(f: &Fixture, script: &str, answer: &str) -> String {
    let setup = format!(
        "eval \"$('{}' hook bash)\"; {script}",
        common::foldl().display()
    );
    let mut cmd = std::process::Command::new(if cfg!(target_os = "macos") {
        "/usr/bin/script"
    } else {
        "script"
    });
    match cfg!(target_os = "macos") {
        true => cmd.args(["-q", "/dev/null", "bash", "-c", &setup]),
        false => cmd.args(["-qec", &format!("bash -c '{setup}'"), "/dev/null"]),
    };
    let mut child = f
        .prepare(cmd)
        .current_dir(&f.home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), answer.as_bytes()).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into_owned()
}

#[test]
fn a_dirty_lease_asks_on_a_terminal() {
    let f = Fixture::new();
    folder(&f, ".", &pg("0", "[ -e \"$HOME/fixed\" ] || exit 3"));
    assert_eq!(f.run(&["exec", "--", "true"]).code, 0);
    std::fs::write(f.home.join("fixed"), "").unwrap();
    let shown = on_a_terminal(
        &f,
        "cd project; _foldl_hook; echo \"PG=$PG\"; cd /; _foldl_hook",
        "r\n",
    );
    f.settle();
    assert!(shown.contains("last down failed at "), "{shown}");
    assert!(shown.contains("retrying the down"), "{shown}");
    assert!(shown.contains("PG=on"), "{shown}");
}

#[test]
fn repo_leases_are_shared_by_worktrees() {
    let f = Fixture::new();
    let mut files = pg("0", "echo down >> \"$HOME/calls\"");
    files[0].1 = "version = 1\nscope = \"shared\"\nlease = \"repo\"".to_string();
    let pairs: Vec<(&str, &str)> = files.iter().map(|(p, c)| (*p, c.as_str())).collect();
    for (path, content) in &pairs {
        f.write(&format!(".foldl/{path}"), content);
    }
    f.git(&f.project, &["init", "-q"]);
    f.git(&f.project, &["add", "."]);
    f.git(&f.project, &["commit", "-q", "-m", "one"]);
    let wt = f.home.join("wt");
    f.git(&f.project, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    assert_eq!(f.run(&["allow"]).code, 0);
    assert_eq!(f.run(&["allow", wt.to_str().unwrap()]).code, 0);
    let shell = &shells()[0];
    let first = f.spawn_shell(shell, "cd project; _foldl_hook; touch \"$HOME/ready\"; while [ ! -e \"$HOME/leave\" ]; do sleep 0.01; done; cd /; _foldl_hook");
    wait_for(&f.home.join("ready"));
    let out = f.shell(shell, "cd wt; _foldl_hook; cd /; _foldl_hook");
    std::fs::write(f.home.join("leave"), "").unwrap();
    first.wait_with_output().unwrap();
    f.settle();
    assert_eq!(f.calls(), "up\ndown\n", "{out:?}");
    assert!(out.stderr.contains("pg (joined"), "{out:?}");
}

#[test]
fn lease_errors_fail_the_up() {
    let f = Fixture::new();
    folder(&f, ".", &pg("0", "true"));
    let state = f.home.join(".local/state/foldl");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("leases"), "not a directory").unwrap();
    let out = f.run(&["exec", "--", "true"]);
    assert_eq!(out.code, 70, "{out:?}");
    assert!(out.stderr.contains("foldl ✗ pg: lease: "), "{out:?}");
}

#[test]
fn a_holder_whose_parent_is_gone_exits_at_once() {
    let f = Fixture::new();
    let sidecar = f.home.join("sidecar.json");
    std::fs::write(&sidecar, "{}").unwrap();
    let out = f.run(&[
        "hold",
        "lease",
        "--parent",
        "0",
        "--fd",
        "9",
        "--sidecar",
        sidecar.to_str().unwrap(),
    ]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(!sidecar.exists());
    assert_eq!(
        f.run(&["hold", "session", "--parent", "0", "--session", "none"])
            .code,
        0
    );
}

#[test]
fn an_unwritable_state_directory_fails_the_load() {
    let f = Fixture::new();
    f.folder(
        ".",
        &[("1-a/manifest.toml", "version = 1\n[env]\nA = \"1\"")],
    );
    let state = f.home.join(".local/state/foldl");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::set_permissions(&state, std::os::unix::fs::PermissionsExt::from_mode(0o500)).unwrap();
    let out = f.run(&["hook-step", "bash", "--pid", "1"]);
    std::fs::set_permissions(&state, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    assert!(out.stderr.contains("can't save a copy of"), "{out:?}");
}

#[test]
fn no_free_port_fails_the_up() {
    let f = Fixture::new();
    folder(&f, ".", &pg("0", "true"));
    let busy = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = busy.local_addr().unwrap().port();
    f.write(
        "../.config/foldl/config.toml",
        &format!("port_range = [{port}, {port}]"),
    );
    let out = f.run(&["exec", "--", "true"]);
    assert_eq!(out.code, 70, "{out:?}");
    assert!(
        out.stderr
            .contains(&format!("no free port in {port}-{port}")),
        "{out:?}"
    );
}

// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! `foldl status` and `foldl logs`.

mod common;

use common::{Fixture, shells};

const FILES: &[(&str, &str)] = &[
    ("001-env/manifest.toml", "version = 1\n[env]\nA = \"1\""),
    ("002-script/manifest.toml", "version = 1"),
    ("002-script/up.sh", "#!/bin/sh\necho from-the-script\n"),
];

fn foldl() -> String {
    format!("'{}'", common::foldl().display())
}

#[test]
fn status_shows_this_sessions_migrations() {
    let f = Fixture::new();
    f.folder(".", FILES);
    let script = format!(
        "cd project; _foldl_hook; {0} status; {0} status --json; {0} status --all; {0} logs script; {0} status --events",
        foldl()
    );
    let out = f.shell(&shells()[0], &script);
    let mut lines = out.stdout.lines();
    assert_eq!(lines.next(), Some("~/project  (1 session)"));
    assert_eq!(lines.next(), Some("  001 env     session up"));
    assert_eq!(lines.next(), Some("  002 script  session up"));
    let json: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(json["folders"][0]["migrations"][1]["name"], "script");
    assert_eq!(json["sessions"], serde_json::json!([]));
    assert_eq!(
        lines.nth(3).map(|l| l.starts_with("session ")
            && l.contains("(bash ")
            && l.ends_with("env, script")),
        Some(true),
        "{out:?}"
    );
    assert_eq!(lines.next(), Some("from-the-script"));
    assert!(
        lines.next().unwrap().contains(r#""name":"script""#),
        "{out:?}"
    );
}

#[test]
fn status_outside_a_session() {
    let f = Fixture::new();
    f.folder(".", FILES);
    let x = [("1-x/manifest.toml", "version = 1\nunset = [\"X\"]")];
    for dir in ["denied", "denied/ignored", "denied/ignored/invalid"] {
        f.folder(dir, &x);
    }
    assert_eq!(f.run(&["deny", "denied"]).code, 0);
    assert_eq!(f.run(&["ignore", "denied/ignored"]).code, 0);
    f.write("denied/ignored/invalid/.foldl/junk", "");
    f.write(
        "denied/ignored/invalid/untrusted/.foldl/1-x/manifest.toml",
        x[0].1,
    );
    let deepest = f.project.join("denied/ignored/invalid/untrusted");
    let dir = f.home.join(".local/state/foldl/sessions/agent");
    std::fs::create_dir_all(&dir).unwrap();
    let loaded = format!(
        r#"{{"root":"{}","name":"env","hash":"h","watch":"","scope":"session","diff":{{}},"down_timeout_ms":1,"saved":"/s","state_dir":"/d","log":"/l","note":"bash only"}}"#,
        f.project.display()
    );
    let state =
        format!(r#"{{"version":1,"id":"agent","parent":1,"shell":null,"loaded":[{loaded}]}}"#);
    std::fs::write(dir.join("state.json"), state).unwrap();
    let other = f.home.join(".local/state/foldl/sessions/agent2");
    std::fs::create_dir_all(&other).unwrap();
    let empty = r#"{"version":1,"id":"agent2","parent":1,"shell":null,"loaded":[]}"#;
    std::fs::write(other.join("state.json"), empty).unwrap();
    let out = f
        .cmd(&["status", "--all"])
        .current_dir(&deepest)
        .env("FOLDL_SESSION", "agent")
        .output()
        .unwrap();
    let want = [
        "~/project  (1 session)",
        "  001 env     session up  bash only",
        "  002 script  session down",
        "~/project/denied  (0 sessions, denied)",
        "~/project/denied/ignored  (0 sessions, ignored)",
        "~/project/denied/ignored/invalid  (0 sessions, invalid)",
        "~/project/denied/ignored/invalid/untrusted  (0 sessions, not allowed)",
        "session agent (agent 1): env",
        "session agent2 (agent 1): nothing loaded",
        "",
    ];
    assert_eq!(String::from_utf8_lossy(&out.stdout), want.join("\n"));
    assert_eq!(f.run(&["status", "--events"]).stdout, "");
}

#[test]
fn logs_need_a_run() {
    let f = Fixture::new();
    f.folder(".", FILES);
    let out = f.run(&["logs", "script"]);
    assert_eq!(
        (out.code, out.stderr.as_str()),
        (64, "foldl ✗ no log for script in this session\n")
    );
}

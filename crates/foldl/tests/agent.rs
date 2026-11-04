// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Agent sessions and the Claude Code integration.

mod common;

use std::process::{Command, Stdio};
use std::time::Duration;

use common::Fixture;

const FILES: &[(&str, &str)] = &[
    ("1-env/manifest.toml", "version = 1\n[env]\nA = \"1\""),
    ("2-svc/manifest.toml", "version = 1\nscope = \"shared\""),
    ("2-svc/up.sh", "#!/bin/sh\necho up >> \"$HOME/calls\"\n"),
    ("2-svc/down.sh", "#!/bin/sh\necho down >> \"$HOME/calls\"\n"),
];

fn wait_until(what: impl Fn() -> bool) {
    for _ in 0..400 {
        if what() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out");
}

#[test]
fn an_agent_session_holds_leases_while_its_process_lives() {
    let f = Fixture::new();
    f.folder(".", FILES);
    let mut agent = Command::new("sleep").arg("60").spawn().unwrap();
    let pid = agent.id().to_string();
    let started = f.run(&["session", "start", "--id", "agent-1", "--parent", &pid]);
    assert_eq!(started.stdout, "export FOLDL_SESSION='agent-1'\n");
    let json = f.run(&["session", "start", "--id", "agent-1", "--json"]);
    assert_eq!(
        json.stdout,
        format!("{{\"parent\":{pid},\"session\":\"agent-1\"}}\n"),
        "reused, parent kept"
    );
    let out = f
        .cmd(&["export", "bash"])
        .env("FOLDL_SESSION", "agent-1")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "export A='1'\n");
    let joined = f
        .cmd(&["exec", "--", "sh", "-c", "echo $A"])
        .env("FOLDL_SESSION", "agent-1")
        .env("A", "1")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&joined.stdout), "1\n");
    let flag = ["exec", "--session", "agent-1", "--", "sh", "-c", "echo $A"];
    let without_env = f.run(&flag);
    assert_eq!(without_env.stdout, "1\n", "joined and given the env");
    assert_eq!(f.calls(), "up\n", "exec joined; the lease stays up");
    agent.kill().unwrap();
    agent.wait().unwrap();
    wait_until(|| f.calls() == "up\ndown\n");
    f.settle();
}

#[test]
fn session_stop_unloads_everything() {
    let f = Fixture::new();
    f.folder(".", FILES);
    let mut agent = Command::new("sleep").arg("60").spawn().unwrap();
    let pid = agent.id().to_string();
    let started = f.run(&["session", "start", "--parent", &pid]);
    let id = started
        .stdout
        .trim()
        .trim_start_matches("export FOLDL_SESSION='")
        .trim_end_matches('\'')
        .to_string();
    assert!(
        f.cmd(&["export", "json"])
            .env("FOLDL_SESSION", &id)
            .status()
            .unwrap()
            .success()
    );
    let stop = f
        .cmd(&["session", "stop"])
        .env("FOLDL_SESSION", &id)
        .output()
        .unwrap();
    assert!(stop.status.success(), "{stop:?}");
    assert_eq!(f.calls(), "up\ndown\n");
    assert!(
        !f.home
            .join(".local/state/foldl/sessions")
            .join(&id)
            .exists()
    );
    assert_eq!(f.run(&["session", "stop", "--id", "nope"]).code, 64);
    let victim = f.home.join("victim");
    std::fs::create_dir_all(&victim).unwrap();
    let escape = "../../../../victim";
    let bad = f.run(&["session", "start", "--id", escape]);
    assert_eq!(bad.code, 64, "{bad:?}");
    assert!(bad.stderr.contains("invalid session id"), "{bad:?}");
    assert!(!victim.join("state.json").exists());
    assert_eq!(f.run(&["session", "stop", "--id", escape]).code, 64);
    assert!(
        victim.exists(),
        "stop removed a directory outside sessions/"
    );
    agent.kill().unwrap();
    agent.wait().unwrap();
    f.settle();
}

#[test]
fn integrate_installs_hooks_once_and_keeps_others() {
    let f = Fixture::new();
    let settings = f.home.join(".claude/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(&settings, r#"{"model": "x", "hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "other"}]}]}}"#).unwrap();
    assert_eq!(
        f.run(&["integrate", "claude"]).stderr,
        format!(
            "foldl: {}: added SessionStart and CwdChanged hooks\n",
            settings.display()
        )
    );
    assert_eq!(
        f.run(&["integrate", "claude"]).stderr,
        format!("foldl: {}: hooks already installed\n", settings.display())
    );
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(written["model"], "x");
    assert_eq!(
        written["hooks"]["SessionStart"].as_array().unwrap().len(),
        2
    );
    assert!(
        written["hooks"]["CwdChanged"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .ends_with("integrate claude --hook")
    );
    assert_eq!(f.run(&["integrate", "claude", "--project"]).code, 0);
    assert!(f.project.join(".claude/settings.json").exists());
    std::fs::write(&settings, "{").unwrap();
    assert_eq!(f.run(&["integrate", "claude"]).code, 78);
}

/// A process named `claude` that runs the hook for each `$HOME/event-N`
/// file it finds, the way Claude Code does, until `$HOME/quit` appears
/// (or ten seconds pass).
fn fake_claude(f: &Fixture, env_file: &std::path::Path) -> std::process::Child {
    let bin = f.home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(common::which("sh"), bin.join("claude")).unwrap();
    let script = format!(
        "n=0; i=1; while [ ! -e \"$HOME/quit\" ] && [ $n -lt 500 ]; do\n\
         if [ -e \"$HOME/event-$i\" ]; then '{}' integrate claude --hook < \"$HOME/event-$i\"; touch \"$HOME/done-$i\"; i=$((i+1)); fi\n\
         sleep 0.02; n=$((n+1)); done",
        common::foldl().display()
    );
    let mut cmd = Command::new(bin.join("claude"));
    cmd.arg("-c").arg(script);
    f.prepare(cmd)
        .env("CLAUDE_ENV_FILE", env_file)
        .stdout(Stdio::null())
        .spawn()
        .unwrap()
}

/// Hands the fake Claude event `n` and waits for its hook to finish.
fn event(f: &Fixture, n: usize, name: &str, cwd: &std::path::Path) {
    let input = format!(
        r#"{{"session_id": "s1", "cwd": "{}", "hook_event_name": "{name}"}}"#,
        cwd.display()
    );
    std::fs::write(f.home.join(format!("event-{n}")), input).unwrap();
    wait_until(|| f.home.join(format!("done-{n}")).exists());
}

/// What a Bash tool call would see: the env file sourced, then `cmd`.
fn bash_call(env_file: &std::path::Path, cmd: &str) -> String {
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!(". '{}'; {cmd}", env_file.display()))
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn claude_sessions_get_the_env_and_hold_leases_until_claude_exits() {
    let f = Fixture::new();
    f.folder(".", FILES);
    let env_file = f.home.join("claude.env");
    let mut claude = fake_claude(&f, &env_file);
    event(&f, 1, "SessionStart", &f.project);
    assert_eq!(
        bash_call(&env_file, "echo \"$A $FOLDL_SESSION\""),
        "1 claude-s1\n"
    );
    assert_eq!(f.calls(), "up\n");
    event(&f, 2, "CwdChanged", &f.home);
    assert_eq!(
        bash_call(&env_file, "echo \"${A-unset}\""),
        "unset\n",
        "leaving appends the unset"
    );
    assert_eq!(
        f.calls(),
        "up\ndown\n",
        "the Claude session left the folder"
    );
    event(&f, 3, "CwdChanged", &f.project);
    assert_eq!(bash_call(&env_file, "echo \"$A\""), "1\n");
    std::fs::write(f.home.join("quit"), "").unwrap();
    claude.wait().unwrap();
    wait_until(|| f.calls() == "up\ndown\nup\ndown\n");
    f.settle();
}

#[test]
fn the_hook_needs_its_inputs() {
    let f = Fixture::new();
    f.folder(".", &FILES[..1]);
    let mut no_file = f.cmd(&["integrate", "claude", "--hook"]);
    no_file
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = no_file.spawn().unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), b"{}").unwrap();
    assert_eq!(child.wait_with_output().unwrap().status.code(), Some(64));
    let mut bad = f.cmd(&["integrate", "claude", "--hook"]);
    bad.env("CLAUDE_ENV_FILE", f.home.join("e"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = bad.spawn().unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), b"not json").unwrap();
    assert_eq!(child.wait_with_output().unwrap().status.code(), Some(64));
    let mut direct = f.cmd(&["integrate", "claude", "--hook"]);
    direct
        .env("CLAUDE_ENV_FILE", f.home.join("e"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = direct.spawn().unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), b"{\"session_id\": \"s2\"}").unwrap();
    assert!(
        child.wait_with_output().unwrap().status.success(),
        "no claude ancestor, no cwd: falls back"
    );
    assert_eq!(bash_call(&f.home.join("e"), "echo $A"), "1\n");
}

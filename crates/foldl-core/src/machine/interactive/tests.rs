// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

use super::*;
use crate::env::Value;
use crate::interactive::parse;
use crate::machine::{Effect, Finish, start};
use crate::testkit::{allowed, ctx, drive, env, session};

const FILES: &[(&str, &str)] = &[
    ("1-env/manifest.toml", "version = 1\n[env]\nA = \"1\""),
    (
        "2-tools/manifest.toml",
        "version = 1\ninteractive = true\n[aliases]\nt = \"just test\"",
    ),
    ("2-tools/up.sh", "#!/usr/bin/env bash\nt() { :; }\n"),
    ("3-after/manifest.toml", "version = 1\n[env]\nC = \"3\""),
];

fn ok(effect: &Effect) -> Option<crate::machine::Event> {
    matches!(effect, Effect::Prepare { .. }).then_some(crate::machine::Event::Done)
}

fn suspended() -> Finish {
    drive(
        start(session(), ctx(), env(&[]), &[allowed("/r", "f", FILES)]),
        ok,
    )
    .finish
}

#[test]
fn an_interactive_up_hands_over_to_the_shell() {
    let f = suspended();
    let source = f.source.clone().expect("sourcing");
    assert_eq!(source.name, "tools");
    assert!(source.script.as_ref().unwrap().ends_with("up.sh"));
    assert_eq!(source.aliases, "alias t='just test'\n");
    assert_eq!(
        f.env,
        env(&[("A", "1")]),
        "env so far is exported before sourcing"
    );
    let pending = f.session.pending.as_ref().unwrap();
    assert_eq!(pending.queue.len(), 1, "3-after waits");
    let json = serde_json::to_string(&f.session).unwrap();
    assert_eq!(serde_json::from_str::<Session>(&json).unwrap(), f.session);
    let script = source_script(&source, Path::new("/bin/foldl"));
    assert!(script.starts_with("__foldl_snap '"), "{script}");
    assert!(
        script.contains("trap - INT\n__foldl_status=0\n. '"),
        "{script}"
    );
    assert!(
        script.ends_with("eval \"$('/bin/foldl' hook-step bash --pid $$ --interactive-done 'tools' --status $__foldl_status)\"\n"),
        "{script}"
    );
    let recovery = recovery_script(pending, Path::new("/bin/foldl"));
    assert!(recovery.contains("--status 130"), "{recovery}");
}

#[test]
fn resuming_records_the_definitions_and_continues() {
    let f = suspended();
    let before = parse("alias ll='ls'\n__foldl_section__\n__foldl_section__\n");
    let after = parse(
        "alias ll='ls'\nalias t='just test'\n__foldl_section__\nt () \n{ \n    :\n}\n__foldl_section__\n",
    );
    let shell_env = env(&[("A", "1"), ("SET_BY_UP", "x")]);
    let t = drive(resume(f.session, ctx(), shell_env, 0, &before, &after), ok);
    assert_eq!(
        t.finish.env,
        env(&[("A", "1"), ("SET_BY_UP", "x"), ("C", "3")])
    );
    assert!(t.finish.session.pending.is_none());
    let tools = &t.finish.session.loaded[1];
    assert_eq!(tools.interactive.as_ref().unwrap().changes.len(), 2);
    assert_eq!(tools.diff.len(), 1);
    assert!(
        t.printed().contains("tools (+1 var) [1 alias, 1 function]"),
        "{}",
        t.printed()
    );
    let left = drive(start(t.finish.session, ctx(), t.finish.env, &[]), ok);
    assert!(
        left.finish
            .shell_code
            .contains("unalias -- t 2>/dev/null\nunset -f t 2>/dev/null\n"),
        "{}",
        left.finish.shell_code
    );
    assert_eq!(left.finish.env, env(&[]));
}

#[test]
fn a_failed_source_is_undone_and_rolls_back() {
    for (status, code) in [(1, Code::UpFailed), (130, Code::Interrupted)] {
        let f = suspended();
        let after = parse("__foldl_section__\nhalf () \n{ \n    :\n}\n__foldl_section__\n");
        let t = drive(
            resume(
                f.session,
                ctx(),
                env(&[("A", "1"), ("HALF", "1")]),
                status,
                &Defs::new(),
                &after,
            ),
            ok,
        );
        assert_eq!(t.finish.code, code);
        assert_eq!(
            t.finish.env,
            env(&[]),
            "partial env and the earlier migration rolled back"
        );
        assert!(
            t.finish
                .shell_code
                .starts_with("unset -f half 2>/dev/null\n"),
            "{}",
            t.finish.shell_code
        );
        assert!(
            t.printed()
                .contains(&format!("tools: up failed (exit {status})"))
        );
    }
}

#[test]
fn where_it_cant_run_it_is_skipped_with_a_note() {
    let mut exec = ctx();
    exec.shell = None;
    let t = drive(
        start(session(), exec, env(&[]), &[allowed("/r", "f", FILES)]),
        ok,
    );
    let note = "skipped: interactive migrations load only in a shell with the hook";
    assert_eq!(t.finish.session.loaded[1].note.as_deref(), Some(note));
    assert!(t.printed().contains("tools [skipped]"), "{}", t.printed());
    let zsh_only: &[(&str, &str)] = &[
        ("1-z/manifest.toml", "version = 1\ninteractive = true"),
        ("1-z/up.sh", "#!/bin/zsh\n"),
    ];
    let t = drive(
        start(session(), ctx(), env(&[]), &[allowed("/r", "f", zsh_only)]),
        ok,
    );
    assert_eq!(
        t.finish.session.loaded[0].note.as_deref(),
        Some("skipped: written for zsh (shebang)")
    );
    let bash_only: &[(&str, &str)] = &[
        ("1-b/manifest.toml", "version = 1\ninteractive = true"),
        ("1-b/up.sh", "#!/bin/bash\n"),
    ];
    let mut zsh = ctx();
    zsh.shell = Some(Shell::Zsh);
    let t = drive(
        start(session(), zsh, env(&[]), &[allowed("/r", "f", bash_only)]),
        ok,
    );
    assert_eq!(
        t.finish.session.loaded[0].note.as_deref(),
        Some("skipped: written for bash (shebang)")
    );
}

#[test]
fn aliases_only_and_stale_callbacks() {
    let aliases: &[(&str, &str)] = &[(
        "1-a/manifest.toml",
        "version = 1\ninteractive = true\n[aliases]\nx = \"y\"",
    )];
    let f = drive(
        start(session(), ctx(), env(&[]), &[allowed("/r", "f", aliases)]),
        ok,
    )
    .finish;
    let source = f.source.unwrap();
    assert_eq!(source.script, None);
    assert!(!source_script(&source, Path::new("/f")).contains("\n. "));
    let t = drive(
        resume(
            session(),
            ctx(),
            env(&[("Q", "1")]),
            0,
            &Defs::new(),
            &Defs::new(),
        ),
        ok,
    );
    assert_eq!(
        t.finish.env["Q"],
        Value::from("1"),
        "no pending: nothing to resume"
    );
}

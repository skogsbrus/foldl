// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! `foldl up | down | redo` and `foldl down --force`.

mod common;

use common::{Fixture, shells};

const FILES: &[(&str, &str)] = &[
    ("2-env/manifest.toml", "version = 1\n[env]\nA = \"1\""),
    ("1-svc/manifest.toml", "version = 1\nscope = \"shared\""),
    ("1-svc/up.sh", "#!/bin/sh\necho up >> \"$HOME/calls\"\n"),
    (
        "1-svc/down.sh",
        "#!/bin/sh\n[ -e \"$HOME/broken\" ] && exit 2\necho down >> \"$HOME/calls\"\n",
    ),
];

#[test]
fn up_down_and_redo_apply_at_the_next_prompt() {
    let f = Fixture::new();
    f.folder(".", FILES);
    let foldl = common::foldl().display().to_string();
    for shell in shells() {
        let script = format!(
            "cd project; _foldl_hook\n'{foldl}' down env; _foldl_hook; echo \"A=${{A-unset}}\"\n\
             '{foldl}' up env; _foldl_hook; echo \"A=$A\"\n'{foldl}' redo svc; _foldl_hook\ncd /; _foldl_hook"
        );
        let out = f.shell(&shell, &script);
        f.settle();
        assert_eq!(out.stdout, "A=unset\nA=1\n", "{}: {out:?}", shell.label);
        assert!(
            out.stderr.contains("foldl: env unloads at the next prompt"),
            "{out:?}"
        );
        assert_eq!(
            f.calls(),
            "up\ndown\nup\ndown\n",
            "{}: redo restarted svc",
            shell.label
        );
        std::fs::remove_file(f.home.join("calls")).unwrap();
    }
}

#[test]
fn they_need_a_session_and_a_known_name() {
    let f = Fixture::new();
    f.folder(".", FILES);
    let out = f.run(&["down", "env"]);
    assert_eq!(out.code, 64);
    assert!(
        out.stderr.contains("this shell has no foldl session"),
        "{out:?}"
    );
    let script = format!(
        "cd project; _foldl_hook; '{}' redo nope",
        common::foldl().display()
    );
    let out = f.shell(&shells()[0], &script);
    assert!(out.stderr.contains("no migration named 'nope'"), "{out:?}");
    f.settle();
}

#[test]
fn down_force_retries_a_dirty_down() {
    let f = Fixture::new();
    f.folder(".", FILES);
    f.folder(
        "other",
        &[("1-x/manifest.toml", "version = 1\n[env]\nX = \"1\"")],
    );
    std::fs::write(f.home.join("broken"), "").unwrap();
    assert_eq!(f.run(&["exec", "--", "true"]).code, 0);
    assert_eq!(f.calls(), "up\n");
    std::fs::remove_file(f.home.join("broken")).unwrap();
    let out = f.run(&["down", "--force", "svc"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(f.calls(), "up\ndown\n");
    assert_eq!(f.run(&["down", "--force", "svc"]).code, 64, "nothing left");
    assert_eq!(f.run(&["down", "--force", "env"]).code, 64, "not shared");
    let elsewhere = f
        .cmd(&["down", "--force", "svc"])
        .current_dir(f.project.join("other"))
        .output()
        .unwrap();
    assert_eq!(elsewhere.status.code(), Some(64));
    f.write(
        "untrusted/.foldl/1-y/manifest.toml",
        "version = 1\n[env]\nY = \"1\"",
    );
    let inner = f
        .cmd(&["down", "--force", "svc"])
        .current_dir(f.project.join("untrusted"))
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&inner.stderr).contains("svc isn't running"),
        "found svc past an untrusted folder"
    );
}

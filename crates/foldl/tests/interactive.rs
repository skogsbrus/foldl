// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Interactive migrations in real bash (3.2 and 5) and zsh.

mod common;

use common::{Fixture, shells};

const TOOLS: &[(&str, &str)] = &[
    ("1-env/manifest.toml", "version = 1\n[env]\nE = \"1\""),
    (
        "2-tools/manifest.toml",
        "version = 1\ninteractive = true\n[aliases]\nt = \"echo t\"",
    ),
    (
        "2-tools/up.sh",
        "#!/bin/sh\nshadow() { echo replaced; }\ntool() { echo tool; }\nexport FROM_TOOLS=\"$FOLDL_ROOT\"\nif [ -n \"$ZSH_VERSION\" ]; then compdef _files tool; else complete -F _tool tool; fi\n",
    ),
];

/// Stops zsh running the hook on `cd`, so a test can step it by hand.
fn no_chpwd(kind: &str) -> &'static str {
    match kind {
        "zsh" => "add-zsh-hook -d chpwd _foldl_hook\n",
        _ => "",
    }
}

/// Starts zsh's completion system, so `compdef` exists.
fn prelude(kind: &str) -> &'static str {
    match kind {
        "zsh" => "autoload -Uz compinit; compinit -u -D\n",
        _ => "",
    }
}

fn completion(kind: &str) -> &'static str {
    match kind {
        "zsh" => "print -r -- \"${_comps[tool]-no-completion}\"",
        _ => "complete -p tool 2>/dev/null || echo no-completion",
    }
}

#[test]
fn definitions_come_and_go_and_shadowed_ones_return() {
    let f = Fixture::new();
    f.folder(".", TOOLS);
    for shell in shells() {
        let script = format!(
            "{}shadow() {{ echo original; }}\ncd project; _foldl_hook\nshadow; tool; alias t; {}; echo \"E=$E FROM=$FROM_TOOLS\"\ncd ..; _foldl_hook\nshadow; type tool >/dev/null 2>&1 || echo no-tool; alias t 2>/dev/null || echo no-alias; {}; echo \"E=${{E-unset}} FROM=${{FROM_TOOLS-unset}}\"\n",
            prelude(shell.kind),
            completion(shell.kind),
            completion(shell.kind)
        );
        let out = f.shell(&shell, &script);
        let lines: Vec<&str> = out.stdout.lines().collect();
        let root = f.project.display().to_string();
        assert_eq!(
            lines[0..2],
            ["replaced", "tool"],
            "{}: {out:?}",
            shell.label
        );
        assert!(
            lines[2].contains("t=") && lines[2].contains("echo t"),
            "{}: {out:?}",
            shell.label
        );
        assert!(
            lines[3].contains("_tool") || lines[3] == "_files",
            "{}: {out:?}",
            shell.label
        );
        assert_eq!(lines[4], format!("E=1 FROM={root}"), "{}", shell.label);
        assert_eq!(
            lines[5..],
            [
                "original",
                "no-tool",
                "no-alias",
                "no-completion",
                "E=unset FROM=unset"
            ],
            "{}: {out:?}",
            shell.label
        );
        assert!(
            out.stderr
                .contains("tools (+1 var) [1 alias, 2 functions, 1 completion]"),
            "{}: {out:?}",
            shell.label
        );
    }
}

#[test]
fn a_failing_up_is_undone_and_rolled_back() {
    let f = Fixture::new();
    f.folder(
        ".",
        &[
            TOOLS[0],
            ("2-bad/manifest.toml", "version = 1\ninteractive = true"),
            (
                "2-bad/up.sh",
                "#!/bin/sh\nhalf() { :; }\nexport HALF=1\nfalse\n",
            ),
        ],
    );
    for shell in shells() {
        let out = f.shell(&shell, "cd project; _foldl_hook; type half >/dev/null 2>&1 || echo no-half; echo \"E=${E-unset} HALF=${HALF-unset}\"");
        assert_eq!(
            out.stdout, "no-half\nE=unset HALF=unset\n",
            "{}: {out:?}",
            shell.label
        );
        assert!(
            out.stderr.contains("foldl ✗ bad: up failed (exit 1)"),
            "{}: {out:?}",
            shell.label
        );
    }
}

#[test]
fn a_bash_only_migration_is_skipped_in_zsh() {
    let f = Fixture::new();
    f.folder(
        ".",
        &[
            ("1-b/manifest.toml", "version = 1\ninteractive = true"),
            ("1-b/up.sh", "#!/usr/bin/env bash\nbonly() { :; }\n"),
        ],
    );
    let foldl = common::foldl().display().to_string();
    for shell in shells() {
        let out = f.shell(&shell, &format!("cd project; _foldl_hook; type bonly >/dev/null 2>&1 && echo loaded; '{foldl}' status"));
        match shell.kind {
            "zsh" => assert!(
                out.stdout
                    .contains("1 b  session up  skipped: written for bash (shebang)"),
                "{out:?}"
            ),
            _ => assert!(
                out.stdout.starts_with("loaded\n"),
                "{}: {out:?}",
                shell.label
            ),
        }
    }
}

#[test]
fn an_interrupted_source_recovers_at_the_next_prompt() {
    let f = Fixture::new();
    f.folder(
        ".",
        &[
            ("1-t/manifest.toml", "version = 1\ninteractive = true"),
            ("1-t/up.sh", "#!/bin/sh\nt() { :; }\n"),
        ],
    );
    let foldl = common::foldl().display().to_string();
    for shell in shells() {
        let script = format!(
            "{}_foldl_hook\ncd project\neval \"$('{foldl}' hook-step {} --pid $$ | head -n 1)\"\nt() {{ :; }}\n_foldl_hook\ntype t >/dev/null 2>&1 && echo still-there\n\
             cd ..; _foldl_hook; cd project; '{foldl}' hook-step {} --pid $$ >/dev/null; _foldl_hook; type _foldl_hook >/dev/null && echo hook-kept",
            no_chpwd(shell.kind),
            shell.kind,
            shell.kind
        );
        let out = f.shell(&shell, &script);
        assert!(
            out.stderr.contains("foldl ✗ t: up failed (exit 130)"),
            "{}: {out:?}",
            shell.label
        );
        assert_eq!(
            out.stdout, "hook-kept\n",
            "{}: the function defined after the before-snapshot is undone",
            shell.label
        );
    }
}

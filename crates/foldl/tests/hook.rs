// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! The prompt hook in real bash (3.2 and 5) and zsh. Scripts call
//! `_foldl_hook` where an interactive shell would show a prompt.

mod common;

use common::{Fixture, shells};

const DUMP: &str = "env | grep -vE '^(_|SHLVL|PWD|OLDPWD|FOLDL_SESSION)=' | sort";

#[test]
fn cd_in_loads_and_cd_out_restores_exactly() {
    let f = Fixture::new();
    f.folder(".", &[
        ("1-env/manifest.toml", "version = 1\npath_add = [\"bin\"]\nunset = [\"DROP\"]\n[env]\nA = \"it's ${FOLDL_ROOT}\"\nC = \"${HOME}\""),
        ("2-script/manifest.toml", "version = 1\n[aliases]\nhi = \"echo hello\""),
        ("2-script/up.sh", "#!/bin/sh\nexport B=\"$A!\"\necho ran\n"),
    ]);
    for shell in shells() {
        let script = format!(
            "export DROP=x\n{DUMP} > before\ncd project; _foldl_hook\necho \"A=$A B=$B DROP=${{DROP-unset}}\"\n\
             case \"$PATH\" in \"$PWD/bin:\"*) echo path-ok;; esac\nalias hi\ncd ..; _foldl_hook\n{DUMP} > after\ndiff before after && echo same\nalias hi 2>/dev/null || echo no-alias"
        );
        let out = f.shell(&shell, &script);
        let root = f.project.display();
        assert!(
            out.stdout.contains(&format!(
                "A=it's {root} B=it's {root}! DROP=unset\npath-ok\n"
            )),
            "{}: {out:?}",
            shell.label
        );
        assert!(out.stdout.contains("hi="), "{}: {out:?}", shell.label);
        assert!(
            out.stdout.ends_with("same\nno-alias\n"),
            "{}: {out:?}",
            shell.label
        );
        assert!(
            out.stderr
                .contains("foldl ↑ env (+4 vars) · script (+1 var)\n"),
            "{}: {out:?}",
            shell.label
        );
        assert!(
            out.stderr.contains("foldl ↓ script · env\n"),
            "{}: {out:?}",
            shell.label
        );
    }
}

#[test]
fn nested_folders_load_outermost_first() {
    let f = Fixture::new();
    f.folder(
        ".",
        &[(
            "1-outer/manifest.toml",
            "version = 1\n[env]\nO = \"outer\"\nX = \"outer\"",
        )],
    );
    f.folder(
        "inner",
        &[
            ("1-inner/manifest.toml", "version = 1"),
            ("1-inner/up.sh", "#!/bin/sh\nexport X=\"$O+inner\"\n"),
        ],
    );
    for shell in shells() {
        let out = f.shell(&shell, "cd project/inner; _foldl_hook\necho \"X=$X\"\ncd ..; _foldl_hook\necho \"X=$X\"\ncd /; _foldl_hook\necho \"X=${X-unset}\"");
        assert_eq!(
            out.stdout, "X=outer+inner\nX=outer\nX=unset\n",
            "{}: {out:?}",
            shell.label
        );
        let order = out.stderr.lines().collect::<Vec<_>>();
        assert_eq!(
            order,
            [
                "foldl ↑ outer (+2 vars) · inner (+1 var)",
                "foldl ↓ inner",
                "foldl ↓ outer"
            ],
            "{}",
            shell.label
        );
    }
}

#[test]
fn a_failed_up_rolls_back_and_is_not_retried() {
    let f = Fixture::new();
    f.folder(
        ".",
        &[
            ("1-ok/manifest.toml", "version = 1\n[env]\nOK = \"1\""),
            ("2-bad/manifest.toml", "version = 1"),
            (
                "2-bad/up.sh",
                "#!/bin/sh\necho about to fail\nexport HALF=1\nexit 4\n",
            ),
        ],
    );
    for shell in shells() {
        let out = f.shell(&shell, "cd project; _foldl_hook\necho \"OK=${OK-unset} HALF=${HALF-unset}\"\n_foldl_hook\ncd /; _foldl_hook; cd ~/project; _foldl_hook");
        assert!(
            out.stdout.starts_with("OK=unset HALF=unset\n"),
            "{}: {out:?}",
            shell.label
        );
        let failures = out
            .stderr
            .matches(
                "foldl ✗ bad: up failed (exit 4)\n  │ about to fail\n  full log: foldl logs bad\n",
            )
            .count();
        assert_eq!(
            failures, 2,
            "retried only after leaving: {}: {out:?}",
            shell.label
        );
    }
}

#[test]
fn untrusted_folders_say_how_to_fix_once() {
    let f = Fixture::new();
    f.write(
        ".foldl/1-env/manifest.toml",
        "version = 1\n[env]\nA = \"1\"",
    );
    for shell in shells() {
        let out = f.shell(
            &shell,
            "cd project; _foldl_hook; _foldl_hook; echo \"A=${A-unset}\"",
        );
        assert_eq!(out.stdout, "A=unset\n");
        assert_eq!(
            out.stderr, "foldl ✗ ~/project is not allowed\n  fix: foldl allow ~/project\n",
            "{}",
            shell.label
        );
    }
}

#[test]
fn child_shells_start_from_the_parents_baseline() {
    let f = Fixture::new();
    f.folder(
        ".",
        &[("1-env/manifest.toml", "version = 1\npath_add = [\"bin\"]")],
    );
    for shell in shells() {
        let program = shell.program.display();
        let child = format!(
            "eval \"$('{}' hook {})\"; cd ~/project; _foldl_hook; echo \"$PATH\"",
            common::foldl().display(),
            shell.kind
        );
        let out = f.shell(
            &shell,
            &format!("cd project; _foldl_hook; {program} -c '{child}'"),
        );
        let root = f.project.display().to_string();
        assert_eq!(
            out.stdout.matches(&format!("{root}/bin")).count(),
            1,
            "{}: {out:?}",
            shell.label
        );
    }
}

#[test]
fn a_broken_config_warns_and_uses_defaults() {
    let f = Fixture::new();
    f.folder(
        ".",
        &[("1-env/manifest.toml", "version = 1\n[env]\nA = \"1\"")],
    );
    f.write("../.config/foldl/config.toml", "verbosity = \"loud\"");
    let out = f.shell(&shells()[0], "cd project; _foldl_hook; echo \"A=$A\"");
    assert!(
        out.stderr.contains("config.toml: unknown variant"),
        "{out:?}"
    );
    assert_eq!(out.stdout, "A=1\n");
}

#[test]
fn quiet_and_verbose() {
    let f = Fixture::new();
    f.folder(
        ".",
        &[("1-env/manifest.toml", "version = 1\n[env]\nA = \"1\"")],
    );
    let quiet = f
        .cmd(&["-q", "hook-step", "bash", "--pid", "1"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&quiet.stderr), "");
    let verbose = f
        .cmd(&["-v", "hook-step", "zsh", "--pid", "2"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&verbose.stderr),
        "foldl ↑ env (+A)\n"
    );
    assert!(String::from_utf8_lossy(&verbose.stdout).starts_with("export FOLDL_SESSION='2-"));
}

#[test]
fn unwritable_state_is_an_internal_error() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.home.join(".local")).unwrap();
    std::fs::write(f.home.join(".local/state"), "file").unwrap();
    let out = f.run(&["hook-step", "bash", "--pid", "1"]);
    assert_eq!(out.code, 70, "{out:?}");
    assert!(out.stderr.starts_with("foldl ✗ saving session: "));
}

#[test]
fn ignored_denied_invalid_and_unreadable_folders() {
    let f = Fixture::new();
    let env = "version = 1\n[env]\nA = \"1\"";
    for name in ["ignored", "denied", "invalid", "unreadable"] {
        f.folder(name, &[("1-env/manifest.toml", env)]);
    }
    assert_eq!(f.run(&["ignore", "ignored"]).code, 0);
    assert_eq!(f.run(&["deny", "denied"]).code, 0);
    f.write("invalid/.foldl/2-x/manifest.toml", "version = 1");
    let unreadable = f.project.join("unreadable/.foldl/1-env");
    std::fs::set_permissions(
        &unreadable,
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .unwrap();
    let out = f.shell(&shells()[0], "for d in ignored denied invalid unreadable; do cd ~/project/$d; _foldl_hook; echo \"$d A=${A-unset}\"; done");
    std::fs::set_permissions(
        &unreadable,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    assert_eq!(
        out.stdout, "ignored A=unset\ndenied A=unset\ninvalid A=unset\nunreadable A=unset\n",
        "{out:?}"
    );
    assert!(
        out.stderr
            .contains("foldl: ~/project/denied is denied; `foldl allow` loads it\n"),
        "{out:?}"
    );
    assert!(out.stderr.contains("foldl ✗ ~/project/invalid/.foldl is invalid\n  error: .foldl/2-x/manifest.toml: does nothing"));
    assert!(out.stderr.contains(
        "foldl ✗ ~/project/unreadable/.foldl is invalid\n  error: .foldl/: can't read: "
    ));
}

#[test]
fn trusted_prefixes_load_without_allow() {
    let f = Fixture::new();
    f.write(
        ".foldl/1-env/manifest.toml",
        "version = 1\n[env]\nA = \"1\"",
    );
    f.write(
        "../.config/foldl/config.toml",
        "trusted_prefixes = [\"~/project\"]",
    );
    let out = f.shell(&shells()[0], "cd project; _foldl_hook; echo \"A=$A\"");
    assert_eq!(out.stdout, "A=1\n", "{out:?}");
}

#[test]
fn changes_while_loaded_reload_only_what_changed() {
    let f = Fixture::new();
    f.folder(".", &[
        ("1-a/manifest.toml", "version = 1"),
        ("1-a/up.sh", "#!/bin/sh\necho a >> \"$HOME/calls\"\nexport A=\"$(cat \"$FOLDL_ROOT/config\")\"\n"),
        ("2-b/manifest.toml", "version = 1\nwatch = [\"config\"]"),
        ("2-b/up.sh", "#!/bin/sh\necho b >> \"$HOME/calls\"\nexport B=\"$(cat \"$FOLDL_ROOT/config\")\"\n"),
    ]);
    f.write("config", "one");
    let foldl = common::foldl().display().to_string();
    let script = format!(
        "cd project; _foldl_hook; echo \"$A $B\"\n\
         sleep 0.01; echo two > config; _foldl_hook; echo \"$A $B\"\n\
         echo '# edited' >> .foldl/2-b/up.sh; _foldl_hook; echo \"$A $B\"\n\
         '{foldl}' allow >/dev/null 2>&1; _foldl_hook; echo \"$A $B\""
    );
    let out = f.shell(&shells()[0], &script);
    assert_eq!(
        out.stdout, "one one\none two\none two\none two\n",
        "{out:?}"
    );
    assert_eq!(
        std::fs::read_to_string(f.home.join("calls")).unwrap(),
        "a\nb\nb\nb\n"
    );
    assert!(
        out.stderr.contains(
            "foldl ! ~/project/.foldl changed; run 'foldl allow ~/project' to reload it\n"
        ),
        "{out:?}"
    );
}

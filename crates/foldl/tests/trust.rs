// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! `foldl allow | deny | ignore | check` and CLI basics.

mod common;

use common::Fixture;

const ENV: &str = "version = 1\n[env]\nA = \"1\"\n";

#[test]
fn allow_shows_changes_since_the_last_allow() {
    let f = Fixture::new();
    f.write(".foldl/1-env/manifest.toml", ENV);
    let first = f.run(&["allow"]);
    assert_eq!(first.code, 0, "{first:?}");
    assert!(first.stdout.contains("+++ b/.foldl/1-env/manifest.toml"));
    assert_eq!(first.stderr, "foldl: allowed ~/project\n");
    f.write(
        ".foldl/1-env/manifest.toml",
        "version = 1\n[env]\nA = \"2\"\n",
    );
    let second = f.run(&["allow", "."]);
    assert!(
        second.stdout.contains("-A = \"1\"\n+A = \"2\"\n"),
        "{second:?}"
    );
    assert_eq!(f.run(&["allow"]).stdout, "");
}

#[test]
fn symlinks_are_refused_unless_the_config_allows_them() {
    let f = Fixture::new();
    f.write(".foldl/1-env/manifest.toml", ENV);
    let secret = f.home.join(".aws/credentials");
    std::fs::create_dir_all(secret.parent().unwrap()).unwrap();
    std::fs::write(&secret, "aws_secret_access_key=hunter2\n").unwrap();
    let link = f.project.join(".foldl/1-env/README");
    std::os::unix::fs::symlink(&secret, link).unwrap();
    let refused = f.run(&["allow"]);
    assert_ne!(refused.code, 0, "{refused:?}");
    assert!(
        refused.stderr.contains("1-env/README is a symlink"),
        "{refused:?}"
    );
    assert!(!format!("{refused:?}").contains("hunter2"), "{refused:?}");
    f.write("../.config/foldl/config.toml", "follow_symlinks = true\n");
    let followed = f.run(&["allow"]);
    assert_eq!(followed.code, 0, "{followed:?}");
}

#[test]
fn allow_refuses_invalid_folders() {
    let f = Fixture::new();
    f.write(".foldl/1-env/manifest.toml", "version = 1\n");
    let out = f.run(&["allow"]);
    assert_eq!(out.code, 78);
    assert!(
        out.stderr
            .contains("error: .foldl/1-env/manifest.toml: does nothing"),
        "{out:?}"
    );
    assert!(out.stderr.contains(
        "foldl ✗ ~/project/.foldl is invalid\n  fix: edit it, then run foldl check ~/project\n"
    ));
}

#[test]
fn deny_ignore_and_check() {
    let f = Fixture::new();
    f.write(".foldl/1-env/manifest.toml", ENV);
    f.write(".foldl/2-up/manifest.toml", "version = 1\n");
    f.write(".foldl/2-up/up.sh", "#!/bin/bash\ndocker run x\n");
    std::fs::create_dir_all(f.project.join("sub")).unwrap();
    let sub = f.project.join("sub");
    let deny = f.cmd(&["deny"]).current_dir(&sub).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&deny.stderr),
        "foldl: denied ~/project\n"
    );
    assert_eq!(f.run(&["ignore"]).stderr, "foldl: ignored ~/project\n");
    let check = f.run(&["check"]);
    assert_eq!(check.code, 0);
    assert!(
        check
            .stderr
            .contains("warning: .foldl/2-up/manifest.toml: up runs 'docker run'"),
        "{check:?}"
    );
    assert!(
        check
            .stderr
            .ends_with("foldl: ~/project/.foldl is valid (2 migrations)\n")
    );
}

#[test]
fn commands_need_a_folder() {
    let f = Fixture::new();
    let out = f.run(&["check", "/"]);
    assert_eq!(out.code, 64);
    assert_eq!(
        out.stderr,
        "foldl ✗ no .foldl/ in / or its parents\n  fix: foldl new <name>\n"
    );
}

#[test]
fn trust_store_errors_are_internal() {
    let f = Fixture::new();
    f.write(".foldl/1-env/manifest.toml", ENV);
    let blocked = f.home.join(".local/share/foldl");
    std::fs::create_dir_all(blocked.parent().unwrap()).unwrap();
    std::fs::write(&blocked, "not a directory").unwrap();
    assert_eq!(f.run(&["allow"]).code, 70);
    assert_eq!(f.run(&["deny"]).code, 70);
}

#[test]
fn unreadable_folders_are_internal_errors() {
    let f = Fixture::new();
    let manifest = f.write(".foldl/1-env/manifest.toml", ENV);
    std::fs::set_permissions(
        &manifest,
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .unwrap();
    let out = f.run(&["check"]);
    assert_eq!(out.code, 70, "{out:?}");
    assert!(out.stderr.starts_with("foldl ✗ reading .foldl: "));
}

#[test]
fn usage_errors_and_help() {
    let f = Fixture::new();
    assert_eq!(f.run(&["frobnicate"]).code, 64);
    let help = f.run(&["--help"]);
    assert_eq!(help.code, 0);
    assert!(
        help.stdout
            .contains("Run code when a shell enters or leaves a folder")
    );
}

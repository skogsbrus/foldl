// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! `foldl import direnv`, against the real direnv.

mod common;

use std::process::Stdio;

use common::{Fixture, path_without};

const ENVRC: &str = "export DATABASE_URL=postgres://localhost/app\nPATH_add bin\ndotenv_if_exists\nwatch_file config.toml\nexport GREETING=\"$(echo hi)\"\n";

/// Whether direnv is installed; tests that need it skip without it (the
/// flake's checks always have it).
fn has_direnv() -> bool {
    std::process::Command::new("direnv")
        .arg("version")
        .output()
        .is_ok()
}

fn direnv(f: &Fixture, args: &[&str]) -> std::process::Output {
    let mut cmd = std::process::Command::new("direnv");
    cmd.args(args);
    f.prepare(cmd).output().unwrap()
}

fn read(f: &Fixture, rel: &str) -> String {
    std::fs::read_to_string(f.project.join(rel)).unwrap_or_default()
}

#[test]
fn imports_checks_parity_and_denies_direnv() {
    if !has_direnv() {
        return;
    }
    let f = Fixture::new();
    f.write(".envrc", ENVRC);
    assert!(direnv(&f, &["allow"]).status.success());
    let out = f.run(&["import", "direnv", "--yes"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(
        out.stderr
            .contains("parity: direnv and foldl export the same environment"),
        "{out:?}"
    );
    assert!(out.stderr.contains("ran direnv deny"), "{out:?}");
    assert!(
        out.stderr
            .contains("kept as shell code, review: export GREETING"),
        "{out:?}"
    );
    assert!(
        read(&f, ".foldl/001-env/manifest.toml")
            .starts_with("version = 1\nwatch = [\"config.toml\"]\npath_add = [\"bin\"]\n")
    );
    assert!(read(&f, ".foldl/003-envrc/up.sh").contains("export GREETING=\"$(echo hi)\"\n"));
    let loaded = f.run(&["exec", "--", "sh", "-c", "echo \"$DATABASE_URL $GREETING\""]);
    assert_eq!(loaded.stdout, "postgres://localhost/app hi\n");
    let status = direnv(&f, &["status"]);
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("Found RC allowed 2"),
        "denied: {status:?}"
    );
    assert!(f.project.join(".envrc").exists(), "kept for others");
    let again = f.run(&["import", "direnv"]);
    assert_eq!(again.code, 64, "already has .foldl");
}

#[test]
fn differences_and_unchecked_parity_are_reported() {
    if !has_direnv() {
        return;
    }
    let f = Fixture::new();
    f.write(".envrc", "export HAS=\"$(type -t layout)\"\n");
    assert!(direnv(&f, &["allow"]).status.success());
    let out = f.run(&["import", "direnv", "--yes", "--remove-envrc"]);
    assert!(
        out.stderr
            .contains("parity: differences to review\n  HAS: direnv \"function\" · foldl \"\""),
        "{out:?}"
    );
    assert!(!f.project.join(".envrc").exists());
}

#[test]
fn an_envrc_direnv_never_allowed_is_not_run_or_allowed() {
    if !has_direnv() {
        return;
    }
    let f = Fixture::new();
    f.write(".envrc", "touch ran\nexport A=1\n");
    let out = f.run(&["import", "direnv", "--yes"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(
        out.stderr
            .contains("direnv hasn't allowed this .envrc, so foldl didn't allow it either"),
        "{out:?}"
    );
    assert!(out.stderr.contains("they load once allowed"), "{out:?}");
    assert!(!f.project.join("ran").exists(), "the .envrc's code ran");
    assert!(read(&f, ".foldl/002-envrc/up.sh").contains("touch ran\n"));
    assert_eq!(f.run(&["exec", "--", "true"]).code, 77, "untrusted");
}

#[test]
fn without_direnv_it_still_imports() {
    let f = Fixture::new();
    f.write(".envrc", "export A=1\n");
    let out = f
        .cmd(&["import", "direnv", "--yes"])
        .env("PATH", path_without("direnv"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("direnv hasn't allowed this .envrc, so foldl didn't allow it"),
        "{stderr}"
    );
    assert!(stderr.contains("couldn't run direnv deny"), "{stderr}");
}

#[test]
fn dry_runs_shared_services_and_errors() {
    let f = Fixture::new();
    assert_eq!(f.run(&["import", "direnv"]).code, 64, "no .envrc");
    f.write(".envrc", "use flake\ndocker compose up -d\n");
    let dry = f.run(&["import", "direnv", "--dry-run", "--shared", "services"]);
    assert!(
        dry.stdout
            .starts_with("==> .foldl/001-flake/manifest.toml\n"),
        "{dry:?}"
    );
    assert!(dry.stdout.contains("==> .foldl/002-services/down.sh\n#!/usr/bin/env bash\n# Stop what up.sh started. Suggested:\n# docker compose down\n"), "{dry:?}");
    assert!(!f.project.join(".foldl").exists());
    let out = f
        .cmd(&["import", "direnv", "--shared", "services"])
        .env("PATH", path_without("direnv"))
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(read(&f, ".foldl/002-services/manifest.toml").contains("scope = \"shared\""));
    let missing = f.run(&["import", "direnv", "/nonexistent"]);
    assert_eq!(missing.code, 64);
}

#[test]
fn a_failing_import_is_reported_by_parity() {
    if !has_direnv() {
        return;
    }
    let f = Fixture::new();
    f.write(".envrc", "false_command_that_does_not_exist_xyz\n");
    assert!(direnv(&f, &["allow"]).status.success());
    let out = f.run(&["import", "direnv", "--yes"]);
    assert!(out.stderr.contains("foldl couldn't load it ("), "{out:?}");
}

#[test]
fn private_imports_in_a_repository() {
    let f = Fixture::new();
    f.git(&f.project, &["init", "-q"]);
    f.write(".envrc", "export A=1\n");
    let out = f
        .cmd(&["import", "direnv", "--yes", "--private"])
        .env("PATH", path_without("direnv"))
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(read(&f, ".git/info/exclude").contains("/.foldl/"));
    let g = Fixture::new();
    g.git(&g.project, &["init", "-q"]);
    g.write(".envrc", "export A=1\n");
    let out = g
        .cmd(&["import", "direnv", "--committed"])
        .env("PATH", path_without("direnv"))
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!read(&g, ".git/info/exclude").contains("/.foldl/"));
}

#[test]
fn asks_about_services_on_a_terminal() {
    let f = Fixture::new();
    f.write(".envrc", "pg_ctl start\n");
    let foldl = common::foldl();
    let mut cmd = std::process::Command::new(if cfg!(target_os = "macos") {
        "/usr/bin/script"
    } else {
        "script"
    });
    match cfg!(target_os = "macos") {
        true => cmd
            .args(["-q", "/dev/null"])
            .arg(&foldl)
            .args(["import", "direnv"]),
        false => cmd
            .args(["-qec"])
            .arg(format!("{} import direnv", foldl.display()))
            .arg("/dev/null"),
    };
    let path = path_without("direnv");
    let mut child = f
        .terminal(cmd)
        .env("PATH", path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(800));
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), b"y\n").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(800));
    let shown = String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into_owned();
    assert!(shown.contains("stop with: pg_ctl stop"), "{shown}");
    assert!(
        read(&f, ".foldl/001-services/manifest.toml").contains("scope = \"shared\""),
        "{shown}"
    );
}

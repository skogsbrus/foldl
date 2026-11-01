// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! The `dotenv` and `nix-flake` built-ins. `nix` is a fake that records its
//! calls; see `real_nix` for the real one.

mod common;

use common::{Fixture, path_without};

const FAKE_NIX: &str = r#"#!/bin/sh
echo "$*" >> "$HOME/nixcalls"
case "$(cat "$5/flake.nix" 2>/dev/null)" in *broken*) echo "error: broken flake" >&2; exit 1;; *garbage*) echo nope; exit 0;; esac
printf '{"variables":{"DEV":{"type":"exported","value":"yes"},"PATH":{"type":"exported","value":"/nix/dev/bin"},"HOME":{"type":"exported","value":"/homeless"}}}'
"#;

/// A fake `nix` in a directory put first on `PATH`.
fn fake_nix(f: &Fixture) -> String {
    let bin = f.home.join("fakebin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("nix"), FAKE_NIX).unwrap();
    std::fs::set_permissions(
        bin.join("nix"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    format!("{}:{}", bin.display(), path_without("nix"))
}

fn nix_calls(f: &Fixture) -> usize {
    std::fs::read_to_string(f.home.join("nixcalls"))
        .unwrap_or_default()
        .lines()
        .count()
}

const FLAKE: &[(&str, &str)] = &[
    ("1-first/manifest.toml", "version = 1"),
    (
        "1-first/up.sh",
        "#!/bin/sh\necho first-up >> \"$HOME/calls\"\n",
    ),
    (
        "1-first/down.sh",
        "#!/bin/sh\necho first-down >> \"$HOME/calls\"\n",
    ),
    (
        "2-flake/manifest.toml",
        "version = 1\nbuiltin = \"nix-flake\"\nwatch = [\"shell.nix\"]",
    ),
];

fn flake_folder(f: &Fixture, rel: &str) {
    f.write(&format!("{rel}/flake.nix"), "{ outputs = _: {}; }");
    f.write(&format!("{rel}/flake.lock"), "{}");
    f.folder(rel, FLAKE);
}

#[test]
fn dotenv_loads_and_reports_problems() {
    let f = Fixture::new();
    f.folder(".", &[
        ("1-env/manifest.toml", "version = 1\nbuiltin = \"dotenv\""),
        ("2-opt/manifest.toml", "version = 1\nbuiltin = \"dotenv\"\n[builtin_options]\nfile = \"missing.env\"\nrequired = false"),
    ]);
    f.write(".env", "A=1\nB=\"$A two\"\n");
    let out = f.run(&["exec", "--", "sh", "-c", "echo \"$A $B\""]);
    assert_eq!(out.stdout, "1 1 two\n", "{out:?}");
    f.write(".env", "not valid\n");
    let out = f.run(&["exec", "--", "true"]);
    assert_eq!(out.code, 80, "{out:?}");
    assert!(
        out.stderr
            .contains("dotenv: .env: line 1: expected NAME=value"),
        "{out:?}"
    );
    std::fs::remove_file(f.project.join(".env")).unwrap();
    let out = f.run(&["exec", "--", "true"]);
    assert!(out.stderr.contains("dotenv: .env: No such file"), "{out:?}");
}

#[test]
fn nix_flake_caches_by_content_across_worktrees() {
    let f = Fixture::new();
    let path = fake_nix(&f);
    flake_folder(&f, "a");
    flake_folder(&f, "b");
    for dir in ["a", "b", "a"] {
        let out = f
            .cmd(&["exec", dir, "--", "sh", "-c", "echo \"$DEV $PATH\""])
            .env("PATH", &path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.starts_with("yes /nix/dev/bin:"), "{out:?}");
    }
    assert_eq!(
        nix_calls(&f),
        1,
        "the second worktree and the rerun used the cache"
    );
    f.write("b/shell.nix", "watched");
    let out = f
        .cmd(&["exec", "b", "--", "true"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(nix_calls(&f), 2, "a watched file is part of the key");
}

#[test]
fn a_broken_flake_falls_back_to_the_last_good_env() {
    let f = Fixture::new();
    let path = fake_nix(&f);
    flake_folder(&f, ".");
    assert!(
        f.cmd(&["exec", "--", "true"])
            .env("PATH", &path)
            .status()
            .unwrap()
            .success()
    );
    f.write("flake.nix", "broken");
    let out = f
        .cmd(&["exec", "--", "sh", "-c", "echo $DEV"])
        .env("PATH", &path)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "yes\n");
    assert!(
        stderr.contains(
            "foldl ! flake: nix-flake evaluation failed; using the last good environment"
        ),
        "{stderr}"
    );
    let fresh = Fixture::new();
    let path = fake_nix(&fresh);
    fresh.write("flake.nix", "broken");
    fresh.folder(".", &FLAKE[3..]);
    let out = fresh
        .cmd(&["exec", "--", "true"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(80), "no last good env: {out:?}");
    fresh.write("flake.nix", "garbage");
    let out = fresh
        .cmd(&["exec", "--", "true"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(80),
        "unreadable print-dev-env output: {out:?}"
    );
    let log = fresh.home.join(".local/state/foldl");
    let logged = std::process::Command::new("grep")
        .args(["-r", "bad print-dev-env output"])
        .arg(&log)
        .output()
        .unwrap();
    assert!(
        logged.status.success()
            || String::from_utf8_lossy(&out.stderr).contains("bad print-dev-env"),
        "{out:?}"
    );
}

#[test]
fn without_nix_it_fails_clearly_and_rolls_back() {
    let f = Fixture::new();
    flake_folder(&f, ".");
    let out = f
        .cmd(&["exec", "--", "true"])
        .env("PATH", path_without("nix"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(69), "{stderr}");
    assert!(
        stderr.contains("nix-flake needs nix on PATH; see https://"),
        "{stderr}"
    );
    assert_eq!(f.calls(), "first-up\nfirst-down\n", "rolled back");
}

#[test]
fn an_unwritable_cache_is_reported() {
    let f = Fixture::new();
    let path = fake_nix(&f);
    flake_folder(&f, ".");
    std::fs::write(f.home.join(".cache"), "not a directory").unwrap();
    let out = f
        .cmd(&["exec", "--", "true"])
        .env("PATH", &path)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(69), "{stderr}");
    assert!(stderr.contains("nix-flake: "), "{stderr}");
}

/// The real thing, opt-in: `FOLDL_TEST_REAL_NIX=1`. Needs network or a
/// populated store for nixpkgs.
#[test]
fn real_nix() {
    if std::env::var_os("FOLDL_TEST_REAL_NIX").is_none() {
        return;
    }
    let f = Fixture::new();
    f.write("flake.nix", "{ inputs.nixpkgs.url = \"github:NixOS/nixpkgs/nixpkgs-unstable\"; outputs = { nixpkgs, ... }: { devShells.aarch64-darwin.default = nixpkgs.legacyPackages.aarch64-darwin.mkShell { packages = [ nixpkgs.legacyPackages.aarch64-darwin.hello ]; HELLO_FROM = \"flake\"; }; }; }");
    f.folder(
        ".",
        &[(
            "1-flake/manifest.toml",
            "version = 1\nbuiltin = \"nix-flake\"",
        )],
    );
    let mut cmd = f.cmd(&["exec", "--", "sh", "-c", "hello && echo $HELLO_FROM"]);
    cmd.env("NIX_CONFIG", "experimental-features = nix-command flakes");
    let out = cmd.output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Hello, world!\nflake\n",
        "{out:?}"
    );
}

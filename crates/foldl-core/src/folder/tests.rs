// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

use super::*;

const BASH: &str = "#!/usr/bin/env bash\necho hi\n";

/// Builds a `.foldl/` from `(path, content)` pairs; parent directories are
/// added automatically.
pub(crate) fn files(pairs: &[(&str, &str)]) -> FolderFiles {
    let mut entries = BTreeMap::new();
    for (path, content) in pairs {
        let parts: Vec<&str> = path.split('/').collect();
        for depth in 1..parts.len() {
            entries.insert(parts[..depth].join("/"), Entry::Dir);
        }
        entries.insert(path.to_string(), Entry::File(content.as_bytes().to_vec()));
    }
    FolderFiles {
        root: PathBuf::from("/r"),
        entries,
    }
}

fn errors(pairs: &[(&str, &str)]) -> Vec<String> {
    let issues = load(&files(pairs), Host::default()).expect_err("expected errors");
    issues
        .iter()
        .filter(|i| i.is_error())
        .map(Issue::display)
        .collect()
}

fn one_error(pairs: &[(&str, &str)], needle: &str) {
    let found = errors(pairs);
    assert!(
        found.iter().any(|e| e.contains(needle)),
        "no error containing {needle:?} in {found:#?}"
    );
}

fn manifest(body: &str) -> Vec<(&'static str, String)> {
    vec![("001-m/manifest.toml", format!("version = 1\n{body}"))]
}

fn check(body: &str, extra: &[(&str, &str)], needle: &str) {
    let m = manifest(body);
    let mut pairs: Vec<(&str, &str)> = m.iter().map(|(p, c)| (*p, c.as_str())).collect();
    pairs.extend_from_slice(extra);
    one_error(&pairs, needle);
}

#[test]
fn loads_and_orders_migrations() {
    let folder = load(
        &files(&[
            ("10-b/manifest.toml", "version = 1\n[env]\nA = \"1\""),
            ("9-c/manifest.toml", "version = 1\nunset = [\"X\"]"),
            (
                "10-a.local/manifest.toml",
                "version = 1\npath_add = [\"bin\"]",
            ),
            ("002-up/manifest.toml", "version = 1"),
            ("002-up/up.sh", BASH),
            ("002-up/down.sh", "#!/bin/sh\n"),
            ("foldl.toml", "version = 1\n[trust]\nreuse = \"repo\""),
            (".DS_Store", ""),
        ]),
        Host::default(),
    )
    .unwrap();
    let names: Vec<&str> = folder.migrations.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["up", "c", "a", "b"]);
    assert_eq!(folder.reuse, Reuse::Repo);
    let up = &folder.migrations[0];
    assert_eq!(up.up.as_ref().unwrap().shebang.shell, ScriptShell::Bash);
    assert_eq!(up.down.as_ref().unwrap().shebang.program, "/bin/sh");
    assert!(folder.migrations[2].local);
    assert_eq!(folder.hash.short().len(), 12);
}

#[test]
fn hash_covers_content_and_names() {
    let a = load(
        &files(&[("1-a/manifest.toml", "version = 1\n[env]\nA = \"1\"")]),
        Host::default(),
    )
    .unwrap();
    let b = load(
        &files(&[("1-a/manifest.toml", "version = 1\n[env]\nA = \"2\"")]),
        Host::default(),
    )
    .unwrap();
    assert_ne!(a.hash, b.hash);
    assert_ne!(a.migrations[0].hash, b.migrations[0].hash);
    let mut with_dir = files(&[("1-a/manifest.toml", "version = 1\n[env]\nA = \"1\"")]);
    with_dir.entries.insert("1-a/empty".into(), Entry::Dir);
    assert_ne!(load(&with_dir, Host::default()).unwrap().hash, a.hash);
}

#[test]
fn dir_names() {
    assert_eq!(parse_dir_name("001-env"), Some((1, "env".into(), false)));
    assert_eq!(parse_dir_name("7-a-b.local"), Some((7, "a-b".into(), true)));
    for bad in [
        "env",
        "-env",
        "01-",
        "01-Env",
        "01--x",
        "x1-env",
        "01-e_v",
        "99999999999999999999999-a",
    ] {
        assert_eq!(parse_dir_name(bad), None, "{bad}");
    }
}

#[test]
fn shebangs() {
    let s = |t: &str| parse_shebang(t.as_bytes());
    assert_eq!(s("#!/usr/bin/env bash\n").unwrap().program, "bash");
    assert_eq!(
        s("#!/usr/bin/env -S zsh -e\n").unwrap().shell,
        ScriptShell::Zsh
    );
    assert_eq!(s("#!/bin/sh").unwrap().program, "/bin/sh");
    assert_eq!(s("#!/usr/bin/python3\n"), None);
    assert_eq!(s("echo hi\n"), None);
    assert_eq!(s("#!\n"), None);
    assert_eq!(s("#!/usr/bin/env\n"), None);
    assert_eq!(parse_shebang(b"#!\xff\n"), None);
    assert_eq!(parse_shebang(b""), None);
}

#[test]
fn interpolation() {
    use Piece::*;
    assert_eq!(
        interpolate("${FOLDL_ROOT}/x"),
        vec![Var("FOLDL_ROOT".into()), Text("/x".into())]
    );
    assert_eq!(interpolate("a$$b$c${"), vec![Text("a$b$c${".into())]);
    assert_eq!(
        interpolate("p${A}${B}"),
        vec![Text("p".into()), Var("A".into()), Var("B".into())]
    );
    assert_eq!(interpolate(""), vec![]);
    let vars = [("A".to_string(), "1".to_string())].into();
    assert_eq!(expand(&interpolate("x${A}${Z}"), &vars), "x1");
}

#[test]
fn rejects_unknown_top_level_entries() {
    one_error(
        &[("env/manifest.toml", "version = 1")],
        "not a migration directory",
    );
    one_error(&[("README.md", "hi")], "unexpected file");
}

#[test]
fn rejects_duplicate_names() {
    one_error(
        &[
            ("1-a/manifest.toml", "version = 1\nunset=[\"A\"]"),
            ("2-a/manifest.toml", "version = 1\nunset=[\"A\"]"),
        ],
        "duplicate migration name 'a'",
    );
}

#[test]
fn rejects_bad_folder_config() {
    one_error(&[("foldl.toml", "version = 2")], "unsupported version 2");
    one_error(&[("foldl.toml", "")], "missing required key 'version'");
    one_error(&[("foldl.toml", "version = 1\nshell = 1")], "unknown field");
    let mut f = files(&[]);
    f.entries.insert("foldl.toml".into(), Entry::Dir);
    let found = load(&f, Host::default()).unwrap_err();
    assert!(
        found[0].message.contains("not a migration directory"),
        "{found:?}"
    );
}

#[test]
fn rejects_missing_or_broken_manifest() {
    one_error(&[("1-a/up.sh", BASH)], "missing manifest.toml");
    one_error(&[("1-a/manifest.toml", "version = ")], "1-a/manifest.toml");
    one_error(
        &[("1-a/manifest.toml", "version = 1\nshell = \"bash\"")],
        "unknown field",
    );
    one_error(
        &[("1-a/manifest.toml", "version = 1\nscope = \"global\"")],
        "unknown variant",
    );
}

#[test]
fn rejects_versions() {
    one_error(
        &[("1-a/manifest.toml", "unset = [\"A\"]")],
        "missing required key 'version'",
    );
    one_error(
        &[("1-a/manifest.toml", "version = 3\nunset = [\"A\"]")],
        "unsupported version 3",
    );
}

#[test]
fn rejects_scope_mismatches() {
    let down = ("001-m/down.sh", BASH);
    check(
        "scope = \"shared\"\ninteractive = true",
        &[("001-m/up.sh", BASH), down],
        "interactive = true is only allowed",
    );
    check(
        "scope = \"shared\"\n[aliases]\nt = \"x\"",
        &[down],
        "[aliases] is only allowed",
    );
    check(
        "lease = \"repo\"\nunset = [\"A\"]",
        &[],
        "lease is only allowed",
    );
    check(
        "unset = [\"A\"]\n[ports]\nP = \"auto\"",
        &[],
        "[ports] is only allowed",
    );
    check(
        "scope = \"shared\"\nunset = [\"A\"]",
        &[],
        "requires a custom down",
    );
}

#[test]
fn rejects_builtin_conflicts() {
    check(
        "builtin = \"dotenv\"",
        &[("001-m/up.sh", BASH)],
        "builtin excludes up",
    );
    check(
        "builtin = \"dotenv\"\nup = \"up.sh\"",
        &[("001-m/up.sh", BASH)],
        "builtin excludes up",
    );
    check(
        "builtin = \"dotenv\"\ninteractive = true",
        &[],
        "builtin excludes interactive",
    );
    check("builtin = \"python\"", &[], "unknown variant");
    check(
        "unset = [\"A\"]\n[builtin_options]\nfile = \"x\"",
        &[],
        "needs builtin",
    );
    check(
        "builtin = \"dotenv\"\n[builtin_options]\nflake = \".\"",
        &[],
        "unknown option 'flake'",
    );
    check(
        "builtin = \"dotenv\"\n[builtin_options]\nrequired = \"yes\"",
        &[],
        "wrong type",
    );
    check(
        "builtin = \"nix-flake\"\n[builtin_options]\nflake = 1",
        &[],
        "wrong type",
    );
}

#[test]
fn resolves_builtin_options() {
    let f = load(
        &files(&[
            ("1-a/manifest.toml", "version = 1\nbuiltin = \"dotenv\"\n[builtin_options]\nfile = \"x.env\"\nrequired = false"),
            ("2-b/manifest.toml", "version = 1\nbuiltin = \"nix-flake\""),
        ]),
        Host { nix_available: true },
    )
    .unwrap();
    assert_eq!(
        f.migrations[0].builtin,
        Some(Builtin::Dotenv {
            file: "x.env".into(),
            required: false
        })
    );
    assert_eq!(
        f.migrations[1].builtin,
        Some(Builtin::NixFlake { flake: ".".into() })
    );
    assert!(f.warnings.is_empty());
}

#[test]
fn rejects_manifests_that_do_nothing() {
    check("", &[], "does nothing");
    check("path_add = []\n[env]\n[aliases]", &[], "does nothing");
}

#[test]
fn rejects_scripts() {
    check("up = \"run.sh\"", &[], "up = \"run.sh\": no such file");
    check(
        "up = \"../up.sh\"",
        &[],
        "must be inside the migration directory",
    );
    check(
        "down = \"/bin/down.sh\"\nunset=[\"A\"]",
        &[],
        "must be inside the migration directory",
    );
    check(
        "",
        &[("001-m/up.sh", "echo no shebang\n")],
        "needs a shebang",
    );
    check(
        "",
        &[("001-m/up.sh", "#!/usr/bin/env python3\n")],
        "needs a shebang",
    );
    check(
        "up = \"sub\"",
        &[("001-m/sub/x.sh", BASH)],
        "up = \"sub\": no such file",
    );
}

#[test]
fn rejects_bad_names_and_values() {
    check(
        "[env]\n\"A-B\" = \"x\"",
        &[],
        "'A-B' is not a valid variable name",
    );
    check("unset = [\"1X\"]", &[], "'1X' is not a valid variable name");
    check("[aliases]\n\"a b\" = \"x\"", &[], "not a valid alias name");
    check("[aliases]\n\"\" = \"x\"", &[], "not a valid alias name");
    check(
        "path_add = [\"/usr/bin\"]",
        &[],
        "must be relative to FOLDL_ROOT",
    );
    check("[env]\nA = \"${USER}\"", &[], "${USER} can't be expanded");
    check(
        "scope = \"shared\"\n[ports]\nP = \"5432\"",
        &[("001-m/down.sh", BASH)],
        "[ports] P",
    );
    check(
        "scope = \"shared\"\n[ports]\n\"p-1\" = \"auto\"",
        &[("001-m/down.sh", BASH)],
        "[ports] p-1",
    );
}

#[test]
fn rejects_durations_and_globs() {
    check(
        "unset = [\"A\"]\ntimeout = \"soon\"",
        &[],
        "timeout = \"soon\" is not a duration",
    );
    check(
        "unset = [\"A\"]\ndown_timeout = \"5\"",
        &[],
        "down_timeout = \"5\"",
    );
    check("unset = [\"A\"]\nwatch = [\"a/[\"]", &[], "watch: 'a/['");
    check(
        "unset = [\"A\"]\nwatch = [\"/etc/x\"]",
        &[],
        "watch: '/etc/x'",
    );
    check(
        "scope = \"shared\"\nlease = \"\"",
        &[("001-m/down.sh", BASH)],
        "lease must not be empty",
    );
}

#[test]
fn resolves_leases_ports_and_timeouts() {
    let f = load(
        &files(&[
            ("1-a/manifest.toml", "version = 1\nscope = \"shared\"\ntimeout = \"5s\"\ndown_timeout = \"1m\"\nwatch = [\"*.nix\"]\n[ports]\nP = \"auto\"\n[env]\nU = \"${P}\""),
            ("1-a/down.sh", BASH),
            ("2-b/manifest.toml", "version = 1\nscope = \"shared\"\nlease = \"repo\""),
            ("2-b/down.sh", BASH),
            ("2-b/up.sh", BASH),
            ("3-c/manifest.toml", "version = 1\nscope = \"shared\"\nlease = \"aws:dev\""),
            ("3-c/down.sh", BASH),
            ("3-c/up.sh", BASH),
        ]),
        Host::default(),
    )
    .unwrap();
    let m = &f.migrations;
    assert_eq!(m[0].lease, Some(LeaseSpec::Folder));
    assert_eq!(m[0].ports, vec!["P".to_string()]);
    assert_eq!(m[0].timeout, Some(Duration::from_secs(5)));
    assert_eq!(m[0].down_timeout, Some(Duration::from_secs(60)));
    assert_eq!(m[1].lease, Some(LeaseSpec::Repo));
    assert_eq!(m[2].lease, Some(LeaseSpec::Custom("aws:dev".into())));
}

#[test]
fn warns_without_failing() {
    let f = load(
        &files(&[
            ("1-a/manifest.toml", "version = 1"),
            (
                "1-a/up.sh",
                "#!/bin/bash\nsource ../../secrets.sh\ndocker compose up -d\n",
            ),
            ("2-b/manifest.toml", "version = 1\nbuiltin = \"nix-flake\""),
            ("3-c/manifest.toml", "version = 1"),
            ("3-c/up.sh", "#!/bin/bash\nssh-agent\n"),
            ("3-c/down.sh", "#!/bin/bash\nssh-agent -k\n"),
        ]),
        Host {
            nix_available: false,
        },
    )
    .unwrap();
    let w: Vec<String> = f.warnings.iter().map(Issue::display).collect();
    assert_eq!(w.len(), 3, "{w:#?}");
    assert!(w.iter().any(|x| x.contains("sources ../../secrets.sh")));
    assert!(w.iter().any(|x| x.contains("runs 'docker compose'")));
    assert!(w.iter().any(|x| {
        x.starts_with("warning: .foldl/2-b/manifest.toml: builtin = \"nix-flake\" needs nix")
    }));
}

// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

use super::*;
use crate::folder::{Entry, FolderFiles, Host, load};

/// Every translation must produce a valid `.foldl/`.
fn validate(files: &[File]) {
    let mut entries = BTreeMap::new();
    for (path, content) in files {
        entries.insert(path.split('/').next().unwrap().to_string(), Entry::Dir);
        entries.insert(path.clone(), Entry::File(content.as_bytes().to_vec()));
    }
    let checked = load(
        &FolderFiles {
            root: "/r".into(),
            entries,
        },
        Host {
            nix_available: true,
        },
    );
    assert!(checked.is_ok(), "{checked:?}\n{files:#?}");
}

const CORPUS: &[(&str, &str)] = &[
    (
        "plain",
        "export DATABASE_URL=postgres://localhost/app\nexport NAME='it is'\nexport ROOT=\"$PWD/tools\"\nPATH_add bin\nPATH_add ./node_modules/.bin\n",
    ),
    ("nix", "use flake\nwatch_file shell.nix flake.lock\n"),
    ("nix-ref", "use flake github:owner/repo#dev\n"),
    ("dotenv", "dotenv\ndotenv_if_exists .env.local\n"),
    (
        "mixed",
        "source_up\nuse flake\nexport VERSION=$(git describe)\nlayout python3\ndocker compose up -d\nexport TOKEN=abc\n",
    ),
    ("services", "pg_ctl start -D \"$PWD/data\"\nnpm run dev &\n"),
];

#[test]
fn translates_a_corpus_into_valid_migrations() {
    for (name, envrc) in CORPUS {
        let t = translate(envrc);
        validate(&migrations(&t, None));
        validate(&migrations(&t, Some("services")));
        assert!(!migrations(&t, None).is_empty(), "{name}");
    }
}

#[test]
fn plain_exports_and_paths_become_a_manifest() {
    let t = translate(CORPUS[0].1);
    assert_eq!(t.env["DATABASE_URL"], "postgres://localhost/app");
    assert_eq!(t.env["NAME"], "it is");
    assert_eq!(t.env["ROOT"], "${FOLDL_ROOT}/tools");
    assert_eq!(t.path_add, ["bin", "node_modules/.bin"]);
    let files = migrations(&t, None);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].0, "001-env/manifest.toml");
    assert!(
        files[0]
            .1
            .contains("path_add = [\"bin\", \"node_modules/.bin\"]\n\n[env]\n"),
        "{}",
        files[0].1
    );
}

#[test]
fn stdlib_calls_become_builtins() {
    let nix = migrations(&translate(CORPUS[1].1), None);
    assert_eq!(nix[0].0, "001-flake/manifest.toml");
    assert!(
        nix[0].1.contains(
            "watch = [\"flake.nix\", \"flake.lock\", \"shell.nix\"]
"
        ),
        "{}",
        nix[0].1
    );
    let watched = migrations(
        &translate(
            "export A=1
watch_file x.txt
",
        ),
        None,
    );
    assert!(
        watched[0].1.starts_with(
            "version = 1
watch = [\"x.txt\"]
"
        ),
        "{}",
        watched[0].1
    );
    let reference = migrations(&translate(CORPUS[2].1), None);
    assert!(reference[0].1.contains("flake = \"github:owner/repo#dev\""));
    assert_eq!(translate("use_flake .#dev").flake.as_deref(), Some(".#dev"));
    let dotenv = translate(CORPUS[3].1);
    assert_eq!(
        dotenv.dotenv,
        [
            (".env".to_string(), true),
            (".env.local".to_string(), false)
        ]
    );
}

#[test]
fn the_rest_is_kept_as_reviewed_shell_code() {
    let t = translate(CORPUS[4].1);
    assert_eq!(
        t.script,
        ["export VERSION=$(git describe)", "layout python3"]
    );
    assert_eq!(
        t.side_effects,
        [(
            "docker compose up -d".to_string(),
            "docker compose down".to_string()
        )]
    );
    assert_eq!(t.env["TOKEN"], "abc");
    assert!(t.notes.iter().any(|n| n.starts_with("source_up dropped")));
    assert!(
        t.notes
            .iter()
            .any(|n| n == "kept as shell code, review: layout python3")
    );
    let files = migrations(&t, None);
    let script = files
        .iter()
        .find(|(p, _)| p.ends_with("envrc/up.sh"))
        .unwrap();
    assert!(
        script.1.contains("layout python3\ndocker compose up -d\n"),
        "side effects stay in the script: {}",
        script.1
    );
}

#[test]
fn side_effects_can_go_to_a_shared_migration() {
    let t = translate(CORPUS[5].1);
    let files = migrations(&t, Some("services"));
    let names: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        names,
        [
            "001-services/manifest.toml",
            "001-services/up.sh",
            "001-services/down.sh"
        ]
    );
    assert!(
        files[2]
            .1
            .contains("# pg_ctl stop\n# kill the background process\n"),
        "{}",
        files[2].1
    );
}

#[test]
fn literal_values() {
    assert_eq!(literal("'a\"b'"), Some("a\"b".to_string()));
    assert_eq!(literal("'unterminated"), None);
    assert_eq!(literal("\"a b\""), Some("a b".to_string()));
    assert_eq!(literal("a b"), None);
    assert_eq!(literal("$OTHER"), None);
    assert_eq!(literal("\"$HOME/x\""), Some("${HOME}/x".to_string()));
    assert_eq!(literal("\"open"), None);
    assert!(translate("export 1BAD=x").script.len() == 1);
}

#[test]
fn values_are_valid_toml() {
    let t = translate("PATH_add bin\nexport ESC=\"tab\there\"\n");
    let only_path = migrations(&translate("PATH_add bin"), None);
    assert_eq!(only_path[0].1, "version = 1\npath_add = [\"bin\"]\n");
    validate(&migrations(&t, None));
}

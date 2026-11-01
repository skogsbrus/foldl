// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Trust keyed on content: `[trust] reuse = "repo"`.

mod common;

use common::{Fixture, shells};

fn repo_with_foldl(f: &Fixture, reuse: &str) {
    f.write(
        ".foldl/foldl.toml",
        &format!("version = 1\n[trust]\nreuse = \"{reuse}\"\n"),
    );
    f.write(
        ".foldl/1-env/manifest.toml",
        "version = 1\n[env]\nA = \"1\"\n",
    );
    f.git(&f.project, &["init", "-q"]);
    f.git(&f.project, &["add", "."]);
    f.git(&f.project, &["commit", "-q", "-m", "one"]);
    f.git(&f.project, &["commit", "-q", "--allow-empty", "-m", "two"]);
    assert_eq!(f.run(&["allow"]).code, 0);
}

fn loads(f: &Fixture, dir: &std::path::Path) -> String {
    let script = format!(
        "cd '{}'; _foldl_hook; echo \"A=${{A-unset}}\"",
        dir.display()
    );
    f.shell(&shells()[0], &script).stdout
}

#[test]
fn a_new_worktree_loads_without_allow() {
    let f = Fixture::new();
    repo_with_foldl(&f, "repo");
    let wt = f.home.join("wt");
    f.git(&f.project, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    assert_eq!(loads(&f, &wt), "A=1\n");
    std::fs::write(
        wt.join(".foldl/1-env/manifest.toml"),
        "version = 1\n[env]\nA = \"2\"\n",
    )
    .unwrap();
    assert_eq!(
        loads(&f, &wt),
        "A=unset\n",
        "a changed migration needs allow"
    );
}

#[test]
fn shallow_clones_fall_back_to_path_trust() {
    let f = Fixture::new();
    repo_with_foldl(&f, "repo");
    let clone = f.home.join("clone");
    let url = format!("file://{}", f.project.display());
    f.git(
        &f.home,
        &["clone", "-q", "--depth", "1", &url, clone.to_str().unwrap()],
    );
    assert_eq!(loads(&f, &clone), "A=unset\n");
    let full = f.home.join("full");
    f.git(&f.home, &["clone", "-q", &url, full.to_str().unwrap()]);
    assert_eq!(loads(&f, &full), "A=1\n");
}

#[test]
fn without_reuse_every_folder_needs_allow() {
    let f = Fixture::new();
    repo_with_foldl(&f, "none");
    let wt = f.home.join("wt");
    f.git(&f.project, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    assert_eq!(loads(&f, &wt), "A=unset\n");
}

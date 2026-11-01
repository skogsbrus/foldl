// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! The little foldl needs from git, by running `git`. Every query answers
//! `None` outside a repository or without git.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    out.status.success().then_some(text)
}

/// The repository's root commits, or `None` outside a repository and in
/// shallow clones, where they are unknown.
pub fn root_commits(dir: &Path) -> Option<Vec<String>> {
    let shallow = git(dir, &["rev-parse", "--is-shallow-repository"])?;
    let roots =
        git(dir, &["rev-list", "--max-parents=0", "HEAD"]).filter(|_| shallow == "false")?;
    Some(roots.lines().map(str::to_string).collect())
}

/// The working tree's top directory.
pub fn toplevel(dir: &Path) -> Option<PathBuf> {
    git(dir, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

/// The directory shared by all worktrees (holds `info/exclude`).
pub fn common_dir(dir: &Path) -> Option<PathBuf> {
    let common = git(dir, &["rev-parse", "--git-common-dir"])?;
    Some(dir.join(common))
}

/// Whether git ignores `path` (relative to `dir`).
pub fn is_ignored(dir: &Path, path: &str) -> bool {
    git(dir, &["check-ignore", "-q", path]).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn repo(dir: &Path) {
        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(ok.status.success(), "{ok:?}");
        };
        run(&["init", "-q"]);
        run(&["commit", "-q", "--allow-empty", "-m", "root"]);
        run(&["commit", "-q", "--allow-empty", "-m", "second"]);
    }

    #[test]
    fn queries() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert_eq!(root_commits(&root), None);
        assert_eq!(toplevel(&root), None);
        assert_eq!(common_dir(&root), None);
        assert!(!is_ignored(&root, "x"));
        repo(&root);
        assert_eq!(root_commits(&root).unwrap().len(), 1);
        assert_eq!(toplevel(&root), Some(root.clone()));
        assert_eq!(
            common_dir(&root).unwrap().canonicalize().unwrap(),
            root.join(".git")
        );
        std::fs::write(root.join(".gitignore"), "x\n").unwrap();
        assert!(is_ignored(&root, "x"));
        let shallow = root.join("shallow");
        let url = format!("file://{}", root.display());
        Command::new("git")
            .args(["clone", "-q", "--depth", "1", &url])
            .arg(&shallow)
            .output()
            .unwrap();
        assert!(shallow.join(".git/shallow").exists(), "really shallow");
        assert_eq!(root_commits(&shallow), None);
    }
}

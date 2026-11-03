// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Reading folders from disk: which ancestors have a `.foldl/`, their
//! files, fingerprints for the prompt's fast path, and watched files.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use foldl_core::env::hex;
use foldl_core::folder::{Entry, FolderFiles, Hash, hash_entries};
use sha2::{Digest, Sha256};

/// Every ancestor of `cwd` (itself included) with a `.foldl/` directory,
/// outermost first. One `stat` per ancestor.
pub fn ancestors(cwd: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = cwd
        .ancestors()
        .filter(|dir| dir.join(".foldl").is_dir())
        .map(Path::to_path_buf)
        .collect();
    found.reverse();
    found
}

/// The nearest folder with a `.foldl/`, from `dir` up.
pub fn nearest(dir: &Path) -> Option<PathBuf> {
    ancestors(dir).pop()
}

/// Reads everything under `<root>/.foldl/`. A symlink is an error unless
/// `follow` is set: it would let a repository make foldl read, show and
/// hash files from anywhere, such as `~/.aws/credentials`.
pub fn read_folder(root: &Path, follow: bool) -> io::Result<FolderFiles> {
    let mut entries = BTreeMap::new();
    read_into(&root.join(".foldl"), "", follow, &mut entries)?;
    Ok(FolderFiles {
        root: root.to_path_buf(),
        entries,
    })
}

fn read_into(
    dir: &Path,
    prefix: &str,
    follow: bool,
    entries: &mut BTreeMap<String, Entry>,
) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        let path = entry.path();
        if !follow && fs::symlink_metadata(&path)?.is_symlink() {
            return Err(io::Error::other(format!(
                ".foldl/{name} is a symlink; foldl follows them only with follow_symlinks = true in config.toml"
            )));
        }
        match fs::metadata(&path)?.is_dir() {
            true => {
                entries.insert(name.clone(), Entry::Dir);
                read_into(&path, &format!("{name}/"), follow, entries)?;
            }
            false => {
                entries.insert(name, Entry::File(fs::read(&path)?));
            }
        }
    }
    Ok(())
}

/// Hashes the metadata (not contents) of everything under `.foldl/` and of
/// `extra` files: cheap, and changes whenever any of them does.
pub fn fingerprint(root: &Path, extra: &[PathBuf]) -> String {
    let mut hasher = Sha256::new();
    stat_into(&root.join(".foldl"), &mut hasher);
    extra.iter().for_each(|path| stat_one(path, &mut hasher));
    hex(&hasher.finalize())[..16].to_string()
}

fn stat_one(path: &Path, hasher: &mut Sha256) {
    hasher.update(path.as_os_str().as_encoded_bytes());
    let meta = fs::metadata(path).map(|m| {
        [
            m.len() as i64,
            m.mtime(),
            m.mtime_nsec(),
            m.ino() as i64,
            m.mode() as i64,
        ]
    });
    let words = meta.unwrap_or([-1; 5]);
    words.iter().for_each(|w| hasher.update(w.to_le_bytes()));
}

fn stat_into(dir: &Path, hasher: &mut Sha256) {
    stat_one(dir, hasher);
    let mut children: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    children.sort();
    // This runs at every prompt, before trust: never walk into a linked
    // directory, which could be the whole home directory.
    for child in children {
        match fs::symlink_metadata(&child).is_ok_and(|m| m.is_dir()) {
            true => stat_into(&child, hasher),
            false => stat_one(&child, hasher),
        }
    }
}

/// Files under `root` matching the globs, sorted.
pub fn expand_watch(root: &Path, globs: &[String]) -> Vec<PathBuf> {
    let pattern_root = glob::Pattern::escape(&root.to_string_lossy());
    let mut files: Vec<PathBuf> = globs
        .iter()
        .filter_map(|g| glob::glob(&format!("{pattern_root}/{g}")).ok())
        .flat_map(|paths| paths.flatten())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    files.dedup();
    files
}

/// Paths whose metadata changes when a file matching a glob appears or
/// goes: the path itself for a literal glob, else the directory holding its
/// first wildcard.
pub fn watch_probes(root: &Path, globs: &[String]) -> Vec<PathBuf> {
    globs
        .iter()
        .map(|g| {
            let literal: PathBuf = Path::new(g)
                .components()
                .take_while(|c| !c.as_os_str().to_string_lossy().contains(['*', '?', '[']))
                .collect();
            root.join(literal)
        })
        .collect()
}

/// Hash of the watched files' paths and contents.
pub fn watch_hash(root: &Path, files: &[PathBuf]) -> Hash {
    let entries: Vec<(String, Entry)> = files
        .iter()
        .map(|f| {
            let rel = f
                .strip_prefix(root)
                .unwrap_or(f)
                .to_string_lossy()
                .into_owned();
            (rel, Entry::File(fs::read(f).unwrap_or_default()))
        })
        .collect();
    match entries.is_empty() {
        true => Hash::default(),
        false => hash_entries(entries.iter().map(|(p, e)| (p.as_str(), e))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_and_reads_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let inner = root.join("a/b");
        fs::create_dir_all(root.join(".foldl/1-x/sub")).unwrap();
        fs::write(root.join(".foldl/1-x/manifest.toml"), "m").unwrap();
        fs::create_dir_all(inner.join(".foldl")).unwrap();
        assert_eq!(ancestors(&inner), vec![root.clone(), inner.clone()]);
        assert_eq!(nearest(&root.join("a")), Some(root.clone()));
        let files = read_folder(&root, false).unwrap();
        let keys: Vec<&String> = files.entries.keys().collect();
        assert_eq!(keys, ["1-x", "1-x/manifest.toml", "1-x/sub"]);
        assert!(read_folder(&root.join("a"), false).is_err());
        let secret = root.join("secret");
        fs::write(&secret, "key").unwrap();
        std::os::unix::fs::symlink(&secret, root.join(".foldl/1-x/link")).unwrap();
        let refused = read_folder(&root, false).unwrap_err().to_string();
        assert!(
            refused.contains(".foldl/1-x/link is a symlink"),
            "{refused}"
        );
        let followed = read_folder(&root, true).unwrap();
        assert_eq!(followed.entries["1-x/link"], Entry::File(b"key".to_vec()));
        fs::remove_file(root.join(".foldl/1-x/link")).unwrap();
        fs::write(root.join(".foldl/1-x/sub/unreadable"), "").unwrap();
        fs::set_permissions(
            root.join(".foldl/1-x/sub/unreadable"),
            std::os::unix::fs::PermissionsExt::from_mode(0o000),
        )
        .unwrap();
        assert!(read_folder(&root, false).is_err());
    }

    #[test]
    fn fingerprints_change_with_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join(".foldl/1-x")).unwrap();
        let watched = root.join("flake.nix");
        let before = fingerprint(root, std::slice::from_ref(&watched));
        assert_eq!(before, fingerprint(root, std::slice::from_ref(&watched)));
        fs::write(&watched, "x").unwrap();
        let after = fingerprint(root, std::slice::from_ref(&watched));
        assert_ne!(before, after);
        fs::write(root.join(".foldl/1-x/f"), "").unwrap();
        assert_ne!(after, fingerprint(root, &[watched]));
        let outside = root.join("outside");
        fs::create_dir_all(outside.join("sub")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join(".foldl/1-x/home")).unwrap();
        let linked = fingerprint(root, &[]);
        fs::write(outside.join("sub/deep"), "").unwrap();
        assert_eq!(linked, fingerprint(root, &[]), "doesn't walk into links");
    }

    #[test]
    fn expands_and_hashes_watched_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.nix"), "1").unwrap();
        fs::write(root.join("sub/b.nix"), "2").unwrap();
        let files = expand_watch(
            root,
            &["*.nix".into(), "**/*.nix".into(), "[".into(), "sub".into()],
        );
        assert_eq!(files, vec![root.join("a.nix"), root.join("sub/b.nix")]);
        let h = watch_hash(root, &files);
        fs::write(root.join("a.nix"), "3").unwrap();
        assert_ne!(h, watch_hash(root, &files));
        assert_eq!(watch_hash(root, &[]), Hash::default());
    }

    #[test]
    fn probes_notice_files_that_appear() {
        let root = Path::new("/r");
        let globs = [".env.local".into(), "src/*.rs".into(), "*.nix".into()];
        assert_eq!(
            watch_probes(root, &globs),
            vec![
                root.join(".env.local"),
                root.join("src"),
                root.to_path_buf()
            ]
        );
    }
}

// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Filesystem helpers. State files are written to a temporary file and
//! renamed into place, never edited.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

/// Creates a directory and its parents, mode 0700.
pub fn ensure_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

/// Writes `bytes` to `path` atomically, mode 0600.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("/"));
    ensure_dir(dir)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(bytes)?;
    fs::rename(&tmp, path)
}

/// Opens a file for writing from scratch, mode 0600.
pub fn create_private(path: &Path) -> io::Result<fs::File> {
    ensure_dir(path.parent().unwrap_or(Path::new("/")))?;
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

/// Appends a line, mode 0600.
pub fn append_line(path: &Path, line: &str) -> io::Result<()> {
    ensure_dir(path.parent().unwrap_or(Path::new("/")))?;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)?;
    writeln!(file, "{line}")
}

/// Removes a file or directory tree; a missing one is fine.
pub fn remove(path: &Path) -> io::Result<()> {
    let result = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) => Err(e),
    };
    match result {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Replaces `to` with a copy of the directory `from`. Symlinks are copied
/// as the files they point to.
pub fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    remove(to)?;
    ensure_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        match fs::metadata(entry.path())?.is_dir() {
            true => copy_dir(&entry.path(), &target)?,
            false => fs::copy(entry.path(), &target).map(drop)?,
        }
    }
    Ok(())
}

/// The last `n` lines of a file, empty if it can't be read.
pub fn tail(path: &Path, n: usize) -> Vec<String> {
    let text = fs::read(path).unwrap_or_default();
    let text = String::from_utf8_lossy(&text);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| l.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn writes_atomically_and_privately() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/state.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        assert!(write_atomic(Path::new("/dev/null/x"), b"").is_err());
    }

    #[test]
    fn copies_and_removes_trees() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        fs::create_dir_all(from.join("sub")).unwrap();
        fs::write(from.join("sub/f"), "x").unwrap();
        std::os::unix::fs::symlink(from.join("sub/f"), from.join("link")).unwrap();
        let to = dir.path().join("to");
        fs::create_dir_all(&to).unwrap();
        fs::write(to.join("stale"), "").unwrap();
        copy_dir(&from, &to).unwrap();
        assert_eq!(fs::read_to_string(to.join("sub/f")).unwrap(), "x");
        assert_eq!(fs::read_to_string(to.join("link")).unwrap(), "x");
        assert!(!to.join("stale").exists());
        remove(&to.join("sub/f")).unwrap();
        assert!(!to.join("sub/f").exists());
        assert!(copy_dir(&dir.path().join("missing"), &to).is_err());
        remove(&to).unwrap();
        remove(&to).unwrap();
        assert!(!to.exists());
        assert!(remove(Path::new("/dev/null/x")).is_err());
    }

    #[test]
    fn appends_and_tails() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("l/x.log");
        (1..=12).for_each(|i| append_line(&log, &i.to_string()).unwrap());
        assert_eq!(tail(&log, 2), ["11", "12"]);
        assert_eq!(tail(&log, 20).len(), 12);
        assert!(tail(&dir.path().join("none"), 3).is_empty());
        let mut f = create_private(&dir.path().join("p/f")).unwrap();
        f.write_all(b"x").unwrap();
    }
}

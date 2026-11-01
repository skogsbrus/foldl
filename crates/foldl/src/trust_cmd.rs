// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! `foldl allow | deny | ignore | check`.

use std::path::{Path, PathBuf};

use foldl_core::error::{Code, Failure};
use foldl_core::folder::{Folder, FolderFiles, Host, Issue, load};
use foldl_core::machine::tilde;

use crate::cli::IoContext;
use crate::disk;
use crate::sys::Sys;
use crate::trust::{Store, TrustEntry, TrustState, changes};

/// The nearest folder with a `.foldl/`, from `dir` (default: cwd) up.
pub fn resolve_root(sys: &Sys, dir: Option<PathBuf>) -> Result<PathBuf, Failure> {
    let start = sys.absolute(&dir.unwrap_or_default());
    let start = start.canonicalize().unwrap_or(start);
    disk::nearest(&start).ok_or_else(|| {
        Failure::new(
            Code::Usage,
            format!("no .foldl/ in {} or its parents", start.display()),
        )
        .with_fix("foldl new <name>")
    })
}

pub fn store(sys: &Sys) -> Store {
    Store {
        dir: sys.paths().trust(),
    }
}

fn home(sys: &Sys) -> String {
    sys.var("HOME").unwrap_or_default()
}

/// Reads and validates a folder, printing its issues.
fn validated(sys: &Sys, root: &Path) -> Result<(FolderFiles, Folder), Failure> {
    let files = disk::read_folder(root).ctx("reading .foldl")?;
    let host = Host {
        nix_available: sys.which("nix").is_some(),
    };
    let shown = tilde(root, &home(sys));
    let print = |issues: &[Issue]| issues.iter().for_each(|i| eprintln!("{}", i.display()));
    match load(&files, host) {
        Ok(folder) => {
            print(&folder.warnings);
            Ok((files, folder))
        }
        Err(issues) => {
            print(&issues);
            let failure = Failure::new(Code::Invalid, format!("{shown}/.foldl is invalid"));
            Err(failure.with_fix(format!("edit it, then run foldl check {shown}")))
        }
    }
}

pub fn allow(sys: &Sys, dir: Option<PathBuf>) -> Result<i32, Failure> {
    let root = resolve_root(sys, dir)?;
    let (files, folder) = validated(sys, &root)?;
    let store = store(sys);
    let previous = store.get(&root).map(|e| e.files).unwrap_or_default();
    print!("{}", changes(&previous, &files));
    store
        .set(&TrustEntry::allowed(&files, folder.hash))
        .ctx("writing trust")?;
    eprintln!("foldl: allowed {}", tilde(&root, &home(sys)));
    Ok(0)
}

pub fn mark(sys: &Sys, dir: Option<PathBuf>, state: TrustState) -> Result<i32, Failure> {
    let root = resolve_root(sys, dir)?;
    store(sys)
        .set(&TrustEntry::marked(&root, state))
        .ctx("writing trust")?;
    let word = match state {
        TrustState::Denied => "denied",
        _ => "ignored",
    };
    eprintln!("foldl: {word} {}", tilde(&root, &home(sys)));
    Ok(0)
}

pub fn check(sys: &Sys, dir: Option<PathBuf>) -> Result<i32, Failure> {
    let root = resolve_root(sys, dir)?;
    let (_, folder) = validated(sys, &root)?;
    let count = folder.migrations.len();
    eprintln!(
        "foldl: {}/.foldl is valid ({count} migrations)",
        tilde(&root, &home(sys))
    );
    Ok(0)
}

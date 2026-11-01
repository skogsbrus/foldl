// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! `foldl new`: create the next numbered migration, and on first use in a
//! repository, decide whether `.foldl/` is shared or private.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use foldl_core::error::{Code, Failure};
use foldl_core::folder::parse_dir_name;

use crate::cli::IoContext;
use crate::sys::Sys;
use crate::{disk, fsx, git};

/// What a private `.foldl/` gives up; shown before the choice.
pub const PRIVATE_LOSES: &str = "\
.foldl/ can be shared (committed) or private (listed in .git/info/exclude).
Private works for you in this checkout, but:
  ✗ teammates don't get the same setup
  ✗ new worktrees and clones (including agents') don't have the migrations; copy .foldl/ by hand
  ✗ content-based trust (reuse = \"repo\") doesn't apply; each folder needs foldl allow
  ✗ caches are shared only between folders that have .foldl/
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Privacy {
    Shared,
    Private,
}

/// Asks, or uses the flag; without a terminal the default is shared.
pub fn choose(flag: Option<Privacy>) -> Privacy {
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    match (flag, interactive) {
        (Some(choice), _) => choice,
        (None, false) => Privacy::Shared,
        (None, true) => {
            eprint!("{PRIVATE_LOSES}Share .foldl/ (commit it)? [Y/n] ");
            let mut answer = String::new();
            let _ = std::io::stdin().lock().read_line(&mut answer);
            match answer.trim() {
                "n" | "N" | "no" => Privacy::Private,
                _ => Privacy::Shared,
            }
        }
    }
}

/// Adds a line to a file unless it is already there.
fn add_line(path: &Path, line: &str) -> Result<(), Failure> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    if text.lines().any(|l| l == line) {
        return Ok(());
    }
    let sep = match text.is_empty() || text.ends_with('\n') {
        true => "",
        false => "\n",
    };
    fsx::ensure_dir(path.parent().unwrap_or(Path::new("/"))).ctx("creating directory")?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ctx("updating ignore file")?;
    writeln!(file, "{sep}{line}").ctx("updating ignore file")
}

/// Keeps `.foldl/` out of git for this user only.
pub fn make_private(root: &Path) -> Result<(), Failure> {
    let not_a_repo = Failure::new(
        Code::Usage,
        format!("{} is not in a git repository", root.display()),
    );
    let common = git::common_dir(root).ok_or(not_a_repo)?;
    add_line(&common.join("info/exclude"), "/.foldl/")
}

#[derive(Debug, Clone, Copy)]
pub struct Kind {
    pub shared: bool,
    pub interactive: bool,
    pub local: bool,
}

pub fn new(sys: &Sys, name: &str, kind: Kind, privacy: Option<Privacy>) -> Result<i32, Failure> {
    if parse_dir_name(&format!("1-{name}")).is_none() {
        return Err(Failure::new(
            Code::Usage,
            format!("'{name}' is not a migration name: use [a-z0-9][a-z0-9-]*"),
        ));
    }
    let repo = git::toplevel(&sys.cwd);
    let root = disk::nearest(&sys.cwd)
        .or_else(|| repo.clone())
        .unwrap_or_else(|| sys.cwd.clone());
    let foldl = root.join(".foldl");
    let first = !foldl.exists();
    let private = match (first, &repo) {
        (true, Some(_)) => choose(privacy) == Privacy::Private,
        _ => git::is_ignored(&root, ".foldl"),
    };
    if first && private {
        make_private(&root)?;
    }
    let numbers: Vec<(u64, usize)> = std::fs::read_dir(&foldl)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let dir = e.file_name().to_string_lossy().into_owned();
            let (number, _, _) = parse_dir_name(&dir)?;
            Some((number, dir.split('-').next().unwrap_or_default().len()))
        })
        .collect();
    let next = numbers.iter().map(|(n, _)| n + 1).max().unwrap_or(1);
    let width = numbers.iter().map(|(_, w)| *w).max().unwrap_or(3);
    let suffix = match kind.local {
        true => ".local",
        false => "",
    };
    let dir = format!("{next:0width$}-{name}{suffix}");
    let path: PathBuf = foldl.join(&dir);
    let (scope, down) = match kind.shared {
        true => ("scope = \"shared\"\n", true),
        false => ("", false),
    };
    let interactive = match kind.interactive {
        true => "interactive = true\n",
        false => "",
    };
    let up = "#!/usr/bin/env bash\n# Runs when a session enters the folder. Exported variables are undone on\n# leave; see `foldl help`.\n";
    fsx::ensure_dir(&path).ctx("creating migration")?;
    std::fs::write(
        path.join("manifest.toml"),
        format!("version = 1\n{scope}{interactive}"),
    )
    .ctx("writing manifest")?;
    std::fs::write(path.join("up.sh"), up).ctx("writing up.sh")?;
    if down {
        std::fs::write(
            path.join("down.sh"),
            "#!/usr/bin/env bash\n# Runs when the last session leaves.\n",
        )
        .ctx("writing down.sh")?;
    }
    if kind.local && !private && repo.is_some() {
        add_line(&root.join(".gitignore"), "/.foldl/*.local/")?;
    }
    eprintln!("foldl: created .foldl/{dir}; edit it, then run foldl allow");
    Ok(0)
}

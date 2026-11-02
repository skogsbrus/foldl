// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! The README documents every user-facing command, so the docs can't fall
//! behind the CLI.

use clap::CommandFactory;
use foldl::cli::Cli;

#[test]
fn readme_covers_every_command() {
    let readme =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md")).unwrap();
    let missing: Vec<String> = Cli::command()
        .get_subcommands()
        .filter(|c| !c.is_hide_set() && c.get_name() != "help")
        .map(|c| format!("`foldl {}`", c.get_name()))
        .filter(|heading| !readme.contains(heading.as_str()))
        .collect();
    assert!(missing.is_empty(), "README.md lacks {missing:?}");
}

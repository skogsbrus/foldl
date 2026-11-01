// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Command-line parsing and dispatch. Every command returns an exit code
//! or a [`Failure`], printed here as text or, with `--json`, as JSON.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use foldl_core::error::{Code, Failure};

use crate::sys::Sys;
use crate::trust_cmd;

#[derive(Debug, Parser)]
#[command(
    name = "foldl",
    version,
    about = "Run code when a shell enters or leaves a folder"
)]
pub struct Cli {
    /// Print less: errors only.
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// Print more: variable names and timings.
    #[arg(short, long, global = true)]
    pub verbose: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Allow the folder's .foldl/ to load, showing what changed.
    Allow { dir: Option<PathBuf> },
    /// Don't load the folder, and say so.
    Deny { dir: Option<PathBuf> },
    /// Don't load the folder and don't mention it.
    Ignore { dir: Option<PathBuf> },
    /// Validate the folder's manifests.
    Check { dir: Option<PathBuf> },
    /// Internal: write the environment to a file, for script runs.
    #[command(name = "__dump-env", hide = true)]
    DumpEnv { file: PathBuf },
}

/// Parses `args` and runs the command; returns the exit code.
pub fn run(args: Vec<OsString>, sys: Sys) -> i32 {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            let code = match e.use_stderr() {
                true => Code::Usage.exit(),
                false => 0,
            };
            let _ = e.print();
            return code;
        }
    };
    match dispatch(cli, &sys) {
        Ok(code) => code,
        Err(failure) => {
            eprint!("{}", failure.human());
            failure.code.exit()
        }
    }
}

fn dispatch(cli: Cli, sys: &Sys) -> Result<i32, Failure> {
    match cli.command {
        Command::Allow { dir } => trust_cmd::allow(sys, dir),
        Command::Deny { dir } => trust_cmd::mark(sys, dir, crate::trust::TrustState::Denied),
        Command::Ignore { dir } => trust_cmd::mark(sys, dir, crate::trust::TrustState::Ignored),
        Command::Check { dir } => trust_cmd::check(sys, dir),
        Command::DumpEnv { file } => {
            crate::runner::dump_env(&file).map_err(|e| io_failure("writing environment", e))?;
            Ok(0)
        }
    }
}

/// An I/O error as an internal failure.
pub fn io_failure(what: &str, e: std::io::Error) -> Failure {
    Failure::new(Code::Internal, format!("{what}: {e}"))
}

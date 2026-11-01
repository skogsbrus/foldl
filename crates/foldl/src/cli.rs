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

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ShellArg {
    Bash,
    Zsh,
}

impl ShellArg {
    pub fn shell(self) -> foldl_core::render::Shell {
        match self {
            ShellArg::Bash => foldl_core::render::Shell::Bash,
            ShellArg::Zsh => foldl_core::render::Shell::Zsh,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the shell hook: eval "$(foldl hook zsh)" in your rc file.
    Hook { shell: ShellArg },
    /// Internal: one prompt's transition, printed as shell code.
    #[command(hide = true)]
    HookStep {
        shell: ShellArg,
        /// The shell's PID ($$).
        #[arg(long)]
        pid: u32,
    },
    /// Allow the folder's .foldl/ to load, showing what changed.
    Allow { dir: Option<PathBuf> },
    /// Don't load the folder, and say so.
    Deny { dir: Option<PathBuf> },
    /// Don't load the folder and don't mention it.
    Ignore { dir: Option<PathBuf> },
    /// Validate the folder's manifests.
    Check { dir: Option<PathBuf> },
    /// Migrations, their state and sessions.
    Status {
        /// Also list every session.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
        /// Print this session's event log.
        #[arg(long)]
        events: bool,
    },
    /// Full output of a migration's last run in this session.
    Logs { name: String },
    /// Load the folder, run a command in it, unload.
    Exec {
        /// Folder to run in (default: the current one).
        dir: Option<PathBuf>,
        /// Print errors as JSON.
        #[arg(long)]
        json: bool,
        #[arg(last = true, required = true)]
        cmd: Vec<OsString>,
    },
    /// Print the folder's environment.
    Export {
        format: crate::oneshot::Format,
        dir: Option<PathBuf>,
        /// Print errors as JSON.
        #[arg(long)]
        json: bool,
    },
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
    let json = matches!(
        cli.command,
        Command::Exec { json: true, .. }
            | Command::Export { json: true, .. }
            | Command::Status { json: true, .. }
    );
    match dispatch(cli, &sys) {
        Ok(code) => code,
        Err(failure) => {
            match json {
                true => println!("{}", failure.json()),
                false => eprint!("{}", failure.human()),
            }
            failure.code.exit()
        }
    }
}

fn dispatch(cli: Cli, sys: &Sys) -> Result<i32, Failure> {
    let verbosity = match (cli.quiet, cli.verbose) {
        (true, _) => Some(foldl_core::config::Verbosity::Quiet),
        (false, true) => Some(foldl_core::config::Verbosity::Verbose),
        (false, false) => None,
    };
    match cli.command {
        Command::Hook { shell } => {
            print!("{}", crate::hook::hook(sys, shell.shell()));
            Ok(0)
        }
        Command::HookStep { shell, pid } => {
            crate::hook::hook_step(sys, shell.shell(), pid, verbosity)
        }
        Command::Allow { dir } => trust_cmd::allow(sys, dir),
        Command::Deny { dir } => trust_cmd::mark(sys, dir, crate::trust::TrustState::Denied),
        Command::Ignore { dir } => trust_cmd::mark(sys, dir, crate::trust::TrustState::Ignored),
        Command::Check { dir } => trust_cmd::check(sys, dir),
        Command::Status { all, json, events } => crate::status::status(sys, all, json, events),
        Command::Logs { name } => crate::status::logs(sys, &name),
        Command::Exec { dir, cmd, .. } => crate::oneshot::exec(sys, dir, cmd, verbosity),
        Command::Export { format, dir, .. } => crate::oneshot::export(sys, format, dir, verbosity),
        Command::DumpEnv { file } => {
            crate::runner::dump_env(&file).ctx("writing environment")?;
            Ok(0)
        }
    }
}

/// I/O errors as internal failures, with what was being done.
pub trait IoContext<T> {
    fn ctx(self, what: &str) -> Result<T, Failure>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn ctx(self, what: &str) -> Result<T, Failure> {
        match self {
            Ok(value) => Ok(value),
            Err(e) => Err(Failure::new(Code::Internal, format!("{what}: {e}"))),
        }
    }
}

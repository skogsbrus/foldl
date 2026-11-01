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
    /// Create the next numbered migration.
    New {
        name: String,
        /// Owned by a lease: up for the first session, down for the last.
        #[arg(long, conflicts_with = "interactive")]
        shared: bool,
        /// Sourced into your shell, for aliases, functions, completions.
        #[arg(long)]
        interactive: bool,
        /// Personal: <number>-<name>.local, ignored by git.
        #[arg(long)]
        local: bool,
        /// On first use, keep .foldl/ out of git (.git/info/exclude).
        #[arg(long, conflicts_with = "committed")]
        private: bool,
        /// On first use, plan to commit .foldl/ (the default without a terminal).
        #[arg(long)]
        committed: bool,
    },
    /// Load a migration unloaded with `foldl down`, at the next prompt.
    Up { name: String },
    /// Unload a migration at the next prompt.
    Down {
        name: String,
        /// Retry a dirty shared migration's down now.
        #[arg(long)]
        force: bool,
    },
    /// Reload a migration at the next prompt; restarts a shared one.
    Redo { name: String },
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
    /// Internal: a holder process for a lease or a session.
    #[command(hide = true)]
    Hold {
        #[command(subcommand)]
        what: Hold,
    },
    /// Internal: write the environment to a file, for script runs.
    #[command(name = "__dump-env", hide = true)]
    DumpEnv { file: PathBuf },
}

#[derive(Debug, Subcommand)]
pub enum Hold {
    /// Keep the inherited lock until the parent exits.
    Lease {
        #[arg(long)]
        parent: u32,
        /// The inherited descriptor holding the lock (kept open).
        #[arg(long)]
        fd: i32,
        #[arg(long)]
        sidecar: PathBuf,
    },
    /// Tear the session down if the parent exits without leaving.
    Session {
        #[arg(long)]
        parent: u32,
        #[arg(long)]
        session: String,
    },
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
        Command::New {
            name,
            shared,
            interactive,
            local,
            private,
            committed,
        } => {
            let privacy = match (private, committed) {
                (true, _) => Some(crate::new_cmd::Privacy::Private),
                (_, true) => Some(crate::new_cmd::Privacy::Shared),
                _ => None,
            };
            crate::new_cmd::new(
                sys,
                &name,
                crate::new_cmd::Kind {
                    shared,
                    interactive,
                    local,
                },
                privacy,
            )
        }
        Command::Up { name } => crate::manual::mark(sys, &name, crate::manual::Action::Up),
        Command::Down { name, force: false } => {
            crate::manual::mark(sys, &name, crate::manual::Action::Down)
        }
        Command::Down { name, force: true } => crate::manual::force_down(sys, &name),
        Command::Redo { name } => crate::manual::mark(sys, &name, crate::manual::Action::Redo),
        Command::Status { all, json, events } => crate::status::status(sys, all, json, events),
        Command::Logs { name } => crate::status::logs(sys, &name),
        Command::Exec { dir, cmd, .. } => crate::oneshot::exec(sys, dir, cmd, verbosity),
        Command::Export { format, dir, .. } => crate::oneshot::export(sys, format, dir, verbosity),
        Command::Hold {
            what: Hold::Lease {
                parent, sidecar, ..
            },
        } => {
            crate::hold::hold_lease(parent, &sidecar).ctx("holding the lease")?;
            Ok(0)
        }
        Command::Hold {
            what: Hold::Session { parent, session },
        } => crate::hold::hold_session(sys, parent, &session),
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

// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Running migration scripts: a subprocess of the shell the shebang names,
//! in its own process group, with output to a private log. Ups report the
//! environment they leave behind through an EXIT trap that runs
//! `foldl __dump-env`.

use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use foldl_core::env::{Env, Value};
use foldl_core::folder::Builtin;
use foldl_core::folder::ScriptShell;
use foldl_core::machine::{Direction, Exec, Outcome, Ran, Run};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::process::{Pid, Signal, kill_process_group};

use crate::fsx;

/// When to show that a run is still going.
const PROGRESS_AFTER: Duration = Duration::from_millis(300);
/// How long a signalled script gets to exit before SIGKILL.
const GRACE: Duration = Duration::from_secs(2);

const UP_DRIVER: &str = r#"trap '__foldl_s=$?; "$__FOLDL_EXE" __dump-env "$__FOLDL_ENVOUT"; exit $__foldl_s' EXIT
case "$__FOLDL_CHECK_BASH${BASH_VERSION-}" in 1[123].*) export __FOLDL_OLD_BASH="$BASH_VERSION";; esac
. "$__FOLDL_SCRIPT""#;
const DOWN_DRIVER: &str = r#". "$__FOLDL_SCRIPT""#;

/// Runs scripts, waking on child exit (SIGCHLD) and Ctrl-C (SIGINT).
#[derive(Debug)]
pub struct Runner {
    exe: PathBuf,
    /// Where env dumps go while a script runs.
    scratch: PathBuf,
    interrupted: Arc<AtomicBool>,
    wake: UnixStream,
    /// `$XDG_CACHE_HOME/foldl`, for built-ins' caches.
    cache: PathBuf,
}

impl Runner {
    pub fn new(exe: PathBuf, scratch: PathBuf) -> std::io::Result<Runner> {
        let (wake, ring) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        let interrupted = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(signal_hook::consts::SIGINT, interrupted.clone())?;
        signal_hook::low_level::pipe::register(signal_hook::consts::SIGINT, ring.try_clone()?)?;
        signal_hook::low_level::pipe::register(signal_hook::consts::SIGCHLD, ring)?;
        let cache = scratch.join("cache");
        Ok(Runner {
            exe,
            scratch,
            interrupted,
            wake,
            cache,
        })
    }

    /// Uses `cache` (`$XDG_CACHE_HOME/foldl`) for built-ins' caches.
    pub fn with_cache(self, cache: PathBuf) -> Runner {
        Runner { cache, ..self }
    }

    /// Marks the next wait as interrupted, as Ctrl-C would.
    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::SeqCst);
    }

    pub fn run(&self, run: &Run) -> Ran {
        let started = Instant::now();
        let mut ran = match &run.exec {
            Exec::Script { shebang, path } => {
                self.script(run, &shebang.program, shebang.shell, path)
            }
            Exec::Builtin { builtin, watch } => self.builtin(builtin, watch, run),
        };
        ran.tail = fsx::tail(&run.log, 10);
        ran.millis = started.elapsed().as_millis() as u64;
        ran.at = clock();
        ran
    }

    fn script(&self, run: &Run, program: &str, shell: ScriptShell, path: &Path) -> Ran {
        let resolved = match program.contains('/') {
            true => Some(PathBuf::from(program)).filter(|p| p.exists()),
            false => which(program, run.env.get("PATH")),
        };
        let Some(program) = resolved else {
            return unavailable(&format!("{program} not found on PATH"));
        };
        let env_out = self
            .scratch
            .join(format!("env-{}-{}", std::process::id(), run.name));
        let (driver, check_bash) = match (run.direction, shell) {
            (Direction::Up, ScriptShell::Bash) => (UP_DRIVER, "1"),
            (Direction::Up, _) => (UP_DRIVER, "0"),
            (Direction::Down, _) => (DOWN_DRIVER, "0"),
        };
        let spawned = fsx::create_private(&run.log).and_then(|log| {
            let mut cmd = Command::new(&program);
            cmd.arg("-c").arg(driver).arg(&run.name).env_clear();
            cmd.envs(
                run.env
                    .iter()
                    .map(|(k, v)| (k, std::ffi::OsStr::from_bytes(&v.0))),
            );
            cmd.env("__FOLDL_EXE", &self.exe)
                .env("__FOLDL_ENVOUT", &env_out);
            cmd.env("__FOLDL_SCRIPT", path)
                .env("__FOLDL_CHECK_BASH", check_bash);
            cmd.current_dir(&run.cwd)
                .stdin(Stdio::null())
                .stderr(log.try_clone()?)
                .stdout(log);
            cmd.process_group(0).spawn()
        });
        let outcome = match spawned {
            Ok(child) => self.wait(child, run),
            Err(e) => Outcome::Unavailable(format!("can't start {}: {e}", program.display())),
        };
        let env = std::fs::read(&env_out).ok().map(|bytes| parse_env(&bytes));
        let _ = fsx::remove(&env_out);
        let warnings = env
            .as_ref()
            .and_then(|e| e.get("__FOLDL_OLD_BASH"))
            .map(|v| {
                format!(
                    "{} ran in bash {}; bash 4 or newer is recommended",
                    run.name,
                    v.lossy()
                )
            });
        let outcome = match outcome {
            Outcome::Exited { status, .. } => Outcome::Exited { status, env },
            other => other,
        };
        Ran {
            outcome,
            tail: vec![],
            millis: 0,
            warnings: warnings.into_iter().collect(),
            at: String::new(),
        }
    }

    fn builtin(&self, builtin: &Builtin, watch: &foldl_core::folder::Hash, run: &Run) -> Ran {
        let log = fsx::create_private(&run.log);
        let Ok(mut log) = log else {
            return unavailable(&format!("can't write {}", run.log.display()));
        };
        let (outcome, warnings) = match builtin {
            Builtin::Dotenv { file, required } => (
                crate::builtins::dotenv(file, *required, run, &mut log),
                vec![],
            ),
            Builtin::NixFlake { flake } => {
                let cache = crate::nix::Cache {
                    dir: self.cache.join("nix"),
                };
                crate::builtins::nix_flake(self, &cache, flake, watch, run, &mut log)
            }
        };
        Ran {
            outcome,
            tail: vec![],
            millis: 0,
            warnings,
            at: String::new(),
        }
    }

    /// Waits for the child, forwarding Ctrl-C to its process group and
    /// enforcing the timeout.
    pub(crate) fn wait(&self, mut child: Child, run: &Run) -> Outcome {
        let group = Pid::from_child(&child);
        let started = Instant::now();
        let mut deadline = started + run.timeout;
        let mut timed_out = false;
        let mut interrupted = false;
        let mut progress_shown = false;
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                let code = status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
                return match (timed_out, interrupted) {
                    (true, _) => Outcome::TimedOut,
                    (_, true) => Outcome::Interrupted,
                    _ => Outcome::Exited {
                        status: code,
                        env: None,
                    },
                };
            }
            let now = Instant::now();
            let signal = match (
                self.interrupted.swap(false, Ordering::SeqCst),
                now >= deadline,
            ) {
                (true, _) => Some(Signal::INT),
                (false, true) if timed_out || interrupted => Some(Signal::KILL),
                (false, true) => Some(Signal::TERM),
                (false, false) => None,
            };
            if let Some(signal) = signal {
                let _ = kill_process_group(group, signal);
                interrupted |= signal == Signal::INT;
                timed_out |= signal == Signal::TERM;
                deadline = now + GRACE;
            }
            if !progress_shown && now >= started + PROGRESS_AFTER {
                eprintln!(
                    "foldl ⋯ {} (still running; log: foldl logs {})",
                    run.name, run.name
                );
                progress_shown = true;
            }
            let wake_at = match progress_shown {
                false => deadline.min(started + PROGRESS_AFTER),
                true => deadline,
            };
            self.sleep(wake_at.saturating_duration_since(now));
        }
    }

    /// Sleeps until a signal arrives or `limit` passes.
    fn sleep(&self, limit: Duration) {
        let limit = limit.max(Duration::from_millis(1));
        let timeout = Timespec {
            tv_sec: limit.as_secs() as i64,
            tv_nsec: limit.subsec_nanos() as i64,
        };
        let mut fds = [PollFd::new(&self.wake, PollFlags::IN)];
        let _ = poll(&mut fds, Some(&timeout));
        let mut drain = [0u8; 64];
        let _ = (&self.wake).read(&mut drain);
    }
}

fn unavailable(what: &str) -> Ran {
    Ran {
        outcome: Outcome::Unavailable(what.to_string()),
        tail: vec![],
        millis: 0,
        warnings: vec![],
        at: String::new(),
    }
}

/// The local time as `HH:MM`.
pub fn clock() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as libc::time_t;
    // SAFETY: localtime_r only writes the `tm` we own.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm
    };
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

/// Looks a program up on a `PATH` value.
pub fn which(program: &str, path: Option<&Value>) -> Option<PathBuf> {
    let path = path.map(Value::lossy).unwrap_or_default();
    path.split(':')
        .map(|dir| Path::new(dir).join(program))
        .find(|p| p.is_file())
}

/// Parses `__dump-env` output: `NAME=value` entries separated by NUL.
pub fn parse_env(bytes: &[u8]) -> Env {
    bytes
        .split(|&b| b == 0)
        .filter_map(|entry| {
            let at = entry.iter().position(|&b| b == b'=')?;
            let name = std::str::from_utf8(&entry[..at]).ok()?;
            Some((name.to_string(), Value(entry[at + 1..].to_vec())))
        })
        .collect()
}

/// `foldl __dump-env FILE`: writes this process's environment to FILE.
pub fn dump_env(file: &Path) -> std::io::Result<()> {
    let bytes: Vec<u8> = std::env::vars_os()
        .flat_map(|(k, v)| [k.as_bytes(), b"=", v.as_bytes(), b"\0"].concat())
        .collect();
    fsx::write_atomic(file, &bytes)
}

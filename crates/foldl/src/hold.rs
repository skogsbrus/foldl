// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Holder processes. Not daemons: each belongs to one session and exits
//! when the session's parent process dies or when told to.
//!
//! - `foldl hold lease` keeps a shared lock on a lease, inherited as a file
//!   descriptor from the `foldl` that took it.
//! - `foldl hold session` runs the session's teardown if its parent dies
//!   without leaving.

use std::fs::File;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use foldl_core::lease::Holder;
use rustix::io::{FdFlags, fcntl_setfd};
use rustix::process::{Pid, Signal, WaitOptions, kill_process, waitpid};

use crate::fsx;
use crate::leases::Sidecar;
use crate::session::alive;

/// Why a holder woke up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Woke {
    ParentExited,
    Told,
}

/// A socket that becomes readable on SIGTERM. SIGINT and SIGHUP are
/// caught and ignored: a holder outlives Ctrl-C and terminal hangups and
/// only follows its parent.
fn signals() -> std::io::Result<UnixStream> {
    let (wake, ring) = UnixStream::pair()?;
    let ignored = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, ignored.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGHUP, ignored)?;
    signal_hook::low_level::pipe::register(signal_hook::consts::SIGTERM, ring)?;
    Ok(wake)
}

/// Waits until `parent` exits or SIGTERM arrives.
pub fn wait(parent: u32) -> std::io::Result<Woke> {
    let wake = signals()?;
    platform::wait(parent, &wake)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use rustix::buffer::spare_capacity;
    use rustix::event::kqueue::{Event, EventFilter, EventFlags, ProcessEvents, kevent, kqueue};

    pub fn wait(parent: u32, wake: &UnixStream) -> std::io::Result<Woke> {
        let kq = kqueue()?;
        // PID 1 adopts orphans: a parent of 1 is already gone.
        let Some(pid) = Pid::from_raw(parent as i32).filter(|_| parent > 1) else {
            return Ok(Woke::ParentExited);
        };
        let exit = Event::new(
            EventFilter::Proc {
                pid,
                flags: ProcessEvents::EXIT,
            },
            EventFlags::ADD,
            std::ptr::null_mut(),
        );
        let told = Event::new(
            EventFilter::Read(wake.as_raw_fd()),
            EventFlags::ADD,
            std::ptr::null_mut(),
        );
        let mut out: Vec<Event> = Vec::with_capacity(2);
        // A caught signal interrupts the wait; SIGTERM then shows up as the
        // socket being readable, ignored signals just wait again. A parent
        // that is already gone comes back as an error event for its filter.
        let _ = loop {
            // SAFETY: the socket registered for reading outlives the kqueue,
            // which is dropped at the end of this function.
            match unsafe { kevent(&kq, &[exit, told], spare_capacity(&mut out), None) } {
                Err(rustix::io::Errno::INTR) => continue,
                other => break other?,
            }
        };
        let parent_exited = out
            .iter()
            .any(|e| matches!(e.filter(), EventFilter::Proc { .. }));
        Ok(match parent_exited {
            true => Woke::ParentExited,
            false => Woke::Told,
        })
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use rustix::event::{PollFd, PollFlags, poll};
    use rustix::process::{PidfdFlags, pidfd_open};

    pub fn wait(parent: u32, wake: &UnixStream) -> std::io::Result<Woke> {
        // PID 1 adopts orphans: a parent of 1 is already gone.
        let Some(pid) = Pid::from_raw(parent as i32).filter(|_| parent > 1) else {
            return Ok(Woke::ParentExited);
        };
        let pidfd = match pidfd_open(pid, PidfdFlags::empty()) {
            Err(rustix::io::Errno::SRCH) => return Ok(Woke::ParentExited),
            other => other?,
        };
        let mut fds = [
            PollFd::new(&pidfd, PollFlags::IN),
            PollFd::new(wake, PollFlags::IN),
        ];
        while let Err(rustix::io::Errno::INTR) = poll(&mut fds, None) {}
        Ok(match fds[0].revents().contains(PollFlags::IN) {
            true => Woke::ParentExited,
            false => Woke::Told,
        })
    }
}

/// `foldl hold lease`: keeps the inherited lock open until the parent dies
/// or it is told to stop, then removes its sidecar.
pub fn hold_lease(parent: u32, sidecar: &Path) -> std::io::Result<Woke> {
    let woke = wait(parent);
    let _ = fsx::remove(sidecar);
    woke
}

fn detached(exe: &Path, args: &[String]) -> Command {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    cmd
}

/// Hands `lock` to a new lease holder by inheritance, and records who
/// holds the lease in a sidecar.
pub fn spawn_lease_holder(
    exe: &Path,
    lock: File,
    sidecar: &Path,
    holder: Holder,
) -> std::io::Result<u32> {
    fcntl_setfd(lock.as_fd(), FdFlags::empty())?;
    let args = [
        "hold",
        "lease",
        "--parent",
        &holder.pid.to_string(),
        "--fd",
        &lock.as_raw_fd().to_string(),
        "--sidecar",
    ];
    let mut args: Vec<String> = args.iter().map(ToString::to_string).collect();
    args.push(sidecar.to_string_lossy().into_owned());
    let child = detached(exe, &args).spawn()?;
    drop(lock);
    let record = Sidecar {
        version: foldl_core::state::STATE_VERSION,
        holder,
        holder_pid: child.id(),
    };
    fsx::write_json(sidecar, &record)?;
    Ok(child.id())
}

pub fn spawn_session_holder(exe: &Path, parent: u32, session: &str) -> std::io::Result<u32> {
    let args = [
        "hold",
        "session",
        "--parent",
        &parent.to_string(),
        "--session",
        session,
    ];
    let child = detached(exe, &args.map(String::from)).spawn()?;
    Ok(child.id())
}

/// Stops a holder and waits for it to exit (SIGKILL after five seconds).
pub fn stop(pid: u32) {
    Pid::from_raw(pid as i32).into_iter().for_each(|target| {
        let _ = kill_process(target, Signal::TERM);
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(pid) && Instant::now() < deadline {
            let _ = waitpid(Some(target), WaitOptions::NOHANG);
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = kill_process(target, Signal::KILL);
        let _ = waitpid(Some(target), WaitOptions::NOHANG);
    });
}

/// Where holders log what they did.
pub fn log_path(state: &Path) -> PathBuf {
    state.join("holder.log")
}

/// `foldl hold session`: if the parent dies without leaving, unload what
/// the session had loaded, running custom downs and leaving its leases,
/// with output in `holder.log`.
pub fn hold_session(
    sys: &crate::sys::Sys,
    parent: u32,
    id: &str,
) -> Result<i32, foldl_core::error::Failure> {
    use crate::cli::IoContext;
    let woke = wait(parent).ctx("waiting for the parent")?;
    let paths = sys.paths();
    let session = match woke {
        Woke::ParentExited => crate::session::load(&paths, id),
        Woke::Told => None,
    };
    let Some(session) = session else { return Ok(0) };
    let log = fsx::create_private(&log_path(&paths.state)).ctx("opening holder.log")?;
    rustix::stdio::dup2_stderr(log.as_fd())
        .map_err(std::io::Error::from)
        .ctx("redirecting stderr")?;
    eprintln!("foldl: session {id}: parent {parent} exited; unloading");
    let config = crate::settings::load(sys, None);
    let ctx = foldl_core::machine::Ctx {
        paths: paths.clone(),
        config: config.clone(),
        shell: None,
        prompt: false,
        holders: false,
    };
    let runner = crate::runner::Runner::new(sys.exe.clone(), paths.session(id))
        .ctx("signals")?
        .with_cache(paths.cache.clone());
    let mut world = crate::interp::RealWorld::new(&runner, sys, &session, &config);
    let env = foldl_core::env::tracked(sys.env());
    let start = foldl_core::machine::start(session, ctx, env, &[]);
    let (finish, _) = crate::interp::drive(&mut world, start);
    crate::session::remove(&paths, id);
    Ok(finish.code.exit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parents_that_are_gone() {
        assert_eq!(wait(0).unwrap(), Woke::ParentExited);
        assert_eq!(
            wait(1).unwrap(),
            Woke::ParentExited,
            "orphaned: launchd/init adopted it"
        );
        assert_eq!(wait(999_999_999).unwrap(), Woke::ParentExited);
        stop(0);
        assert_eq!(log_path(Path::new("/s")), PathBuf::from("/s/holder.log"));
    }

    #[test]
    fn a_parent_that_exits_wakes_the_holder() {
        let mut parent = Command::new("sleep").arg("0.2").spawn().unwrap();
        assert_eq!(wait(parent.id()).unwrap(), Woke::ParentExited);
        parent.wait().unwrap();
    }

    #[test]
    fn spawning_and_holding() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("holders/s.json");
        let lock = std::fs::File::create(dir.path().join("lease.lock")).unwrap();
        let holder = Holder {
            session: "s".into(),
            pid: 1,
            name: "zsh".into(),
            cwd: "/".into(),
            tty: None,
        };
        let truth = crate::sys::true_program();
        let pid = spawn_lease_holder(&truth, lock, &sidecar, holder).unwrap();
        assert!(pid > 0 && sidecar.exists());
        stop(pid);
        assert!(spawn_session_holder(&truth, 1, "s").unwrap() > 0);
        assert_eq!(hold_lease(0, &sidecar).unwrap(), Woke::ParentExited);
        assert!(!sidecar.exists());
        let sys = crate::sys::Sys {
            vars: [("HOME".into(), dir.path().as_os_str().to_owned())].into(),
            cwd: "/".into(),
            exe: truth,
        };
        assert_eq!(hold_session(&sys, 0, "missing").unwrap(), 0);
    }
}

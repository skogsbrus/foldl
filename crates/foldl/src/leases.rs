// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Lease locks on disk, under `$XDG_STATE_HOME/foldl/leases/<id>/`:
//! `mutex.lock` around transitions, `lease.lock` whose shared holders are
//! the sessions using the lease, `state.json`, and `holders/` sidecars.
//! Also the port registry under `ports/`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

use foldl_core::lease::{Holder, LeaseRef, LeaseState, port_candidates};
use foldl_core::state::{Paths, STATE_VERSION};
use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};

use crate::{fsx, session};

/// A holder's sidecar: who holds the lease, and the holder's own PID to
/// tell live entries from stale ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sidecar {
    pub holder: Holder,
    pub holder_pid: u32,
}

fn open_lock(path: &Path) -> io::Result<File> {
    fsx::ensure_dir(path.parent().unwrap_or(Path::new("/")))?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// Locks held by this process, per lease id.
#[derive(Debug, Default)]
pub struct Locks {
    mutexes: HashMap<String, File>,
    exclusive: HashMap<String, File>,
}

impl Locks {
    /// Takes the lease's mutex, waiting for other transitions.
    pub fn lock(&mut self, paths: &Paths, lease: &LeaseRef) -> io::Result<()> {
        // Dropping a mutex this process still holds (after an error) keeps
        // it from waiting on itself.
        self.mutexes.remove(&lease.id);
        let dir = paths.lease(&lease.id);
        let file = open_lock(&dir.join("mutex.lock"))?;
        flock(file.as_fd(), FlockOperation::LockExclusive)?;
        fsx::write_atomic(&dir.join("key"), lease.key.as_bytes())?;
        self.mutexes.insert(lease.id.clone(), file);
        Ok(())
    }

    /// Tries `LOCK_EX | LOCK_NB` on a fresh `lease.lock`: granted means no
    /// session holds the lease.
    pub fn try_exclusive(&mut self, paths: &Paths, lease: &LeaseRef) -> io::Result<bool> {
        let file = open_lock(&paths.lease(&lease.id).join("lease.lock"))?;
        let granted = flock(file.as_fd(), FlockOperation::NonBlockingLockExclusive).is_ok();
        if granted {
            self.exclusive.insert(lease.id.clone(), file);
        }
        Ok(granted)
    }

    /// A `lease.lock` descriptor holding a shared lock, converted from the
    /// exclusive one if this process has it.
    pub fn shared(&mut self, paths: &Paths, lease: &LeaseRef) -> io::Result<File> {
        let file = match self.exclusive.remove(&lease.id) {
            Some(file) => file,
            None => open_lock(&paths.lease(&lease.id).join("lease.lock"))?,
        };
        flock(file.as_fd(), FlockOperation::LockShared)?;
        Ok(file)
    }

    /// Releases the mutex and any exclusive lock.
    pub fn unlock(&mut self, lease: &LeaseRef) {
        self.exclusive.remove(&lease.id);
        self.mutexes.remove(&lease.id);
    }
}

pub fn read_state(paths: &Paths, lease: &LeaseRef) -> LeaseState {
    let bytes = std::fs::read(paths.lease(&lease.id).join("state.json")).unwrap_or_default();
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Writes the lease state; a lease that ended clean keeps only its locks,
/// data and logs.
pub fn write_state(paths: &Paths, lease: &LeaseRef, state: &LeaseState) -> io::Result<()> {
    let path = paths.lease(&lease.id).join("state.json");
    match state.entries.is_empty() {
        true => fsx::remove(&path),
        false => fsx::write_json(&path, state),
    }
}

/// Live holders, from their sidecars; stale sidecars are removed.
pub fn holders(paths: &Paths, lease: &LeaseRef) -> Vec<Holder> {
    let dir = paths.lease(&lease.id).join("holders");
    let files = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path());
    let found: BTreeMap<PathBuf, Option<Sidecar>> = files
        .map(|p| {
            (
                p.clone(),
                std::fs::read(&p)
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok()),
            )
        })
        .collect();
    found
        .into_iter()
        .filter_map(
            |(path, sidecar)| match sidecar.filter(|s| session::alive(s.holder_pid)) {
                Some(s) => Some(s.holder),
                None => {
                    let _ = fsx::remove(&path);
                    None
                }
            },
        )
        .collect()
}

pub fn sidecar_path(paths: &Paths, lease: &LeaseRef, session: &str) -> PathBuf {
    paths
        .lease(&lease.id)
        .join("holders")
        .join(format!("{session}.json"))
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    version: u32,
    ports: BTreeMap<u16, (String, String)>,
}

fn with_registry<T>(paths: &Paths, f: impl FnOnce(&mut Registry) -> T) -> io::Result<T> {
    let lock = open_lock(&paths.ports().join("registry.lock"))?;
    flock(lock.as_fd(), FlockOperation::LockExclusive)?;
    let path = paths.ports().join("registry.json");
    let bytes = std::fs::read(&path).unwrap_or_default();
    let mut registry: Registry = serde_json::from_slice(&bytes).unwrap_or_default();
    let result = f(&mut registry);
    registry.version = STATE_VERSION;
    fsx::write_json(&path, &registry)?;
    Ok(result)
}

/// Picks a free port for each name, registers it to the lease, and checks
/// it can be bound right now.
pub fn alloc_ports(
    paths: &Paths,
    range: (u16, u16),
    lease: &LeaseRef,
    name: &str,
    names: &[String],
) -> io::Result<BTreeMap<String, u16>> {
    let picked = with_registry(paths, |registry| pick(registry, range, lease, name, names))?;
    picked.ok_or(io::Error::other(format!(
        "no free port in {}-{}",
        range.0, range.1
    )))
}

/// Picks and registers a port for each name; `None` if one can't be found.
fn pick(
    registry: &mut Registry,
    range: (u16, u16),
    lease: &LeaseRef,
    name: &str,
    names: &[String],
) -> Option<BTreeMap<String, u16>> {
    names
        .iter()
        .map(|port_name| {
            let taken: BTreeSet<u16> = registry.ports.keys().copied().collect();
            let seed = format!("{}/{name}/{port_name}", lease.id);
            let port = port_candidates(range, &seed, &taken)
                .into_iter()
                .find(|p| bindable(*p))?;
            registry
                .ports
                .insert(port, (lease.id.clone(), name.to_string()));
            Some((port_name.clone(), port))
        })
        .collect()
}

fn bindable(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

pub fn free_ports(paths: &Paths, lease: &LeaseRef, name: &str) -> io::Result<()> {
    with_registry(paths, |registry| {
        registry
            .ports
            .retain(|_, (id, n)| !(id == &lease.id && n == name))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &Path) -> Paths {
        Paths {
            state: dir.join("s"),
            data: dir.join("d"),
            cache: dir.join("c"),
            config: dir.join("cfg"),
        }
    }

    fn lease(id: &str) -> LeaseRef {
        LeaseRef {
            id: id.into(),
            key: format!("custom:{id}"),
        }
    }

    #[test]
    fn exclusive_means_nobody_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let l = lease("a");
        let mut mine = Locks::default();
        mine.lock(&p, &l).unwrap();
        mine.lock(&p, &l).unwrap();
        assert!(mine.try_exclusive(&p, &l).unwrap());
        let held = mine.shared(&p, &l).unwrap();
        mine.unlock(&l);
        let mut other = Locks::default();
        other.lock(&p, &l).unwrap();
        assert!(
            !other.try_exclusive(&p, &l).unwrap(),
            "a shared holder exists"
        );
        let joined = other.shared(&p, &l).unwrap();
        drop(held);
        drop(joined);
        // Other test threads forking at this moment briefly share the
        // descriptors until they exec; foldl itself is single-threaded.
        let granted = (0..100).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(2));
            other.try_exclusive(&p, &l).unwrap()
        });
        assert!(granted, "holders gone");
        other.unlock(&l);
        assert_eq!(
            std::fs::read_to_string(p.lease("a").join("key")).unwrap(),
            "custom:a"
        );
        let blocked = Paths {
            state: "/dev/null".into(),
            ..p.clone()
        };
        assert!(Locks::default().lock(&blocked, &l).is_err());
        assert!(Locks::default().try_exclusive(&blocked, &l).is_err());
        assert!(Locks::default().shared(&blocked, &l).is_err());
    }

    #[test]
    fn state_and_holders() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let l = lease("b");
        assert_eq!(read_state(&p, &l), LeaseState::default());
        let mut state = LeaseState::default();
        state.entries.insert("pg".into(), serde_json::from_str(r#"{"hash":"h","diff":{},"ports":{},"down":null,"down_timeout_ms":1,"root":"/r","saved":"/s","state_dir":"/d","log":"/l","dirty":null,"seq":0}"#).unwrap());
        write_state(&p, &l, &state).unwrap();
        assert_eq!(read_state(&p, &l), state);
        write_state(&p, &l, &LeaseState::default()).unwrap();
        assert!(!p.lease("b").join("state.json").exists());
        let me = Holder {
            session: "s".into(),
            pid: 1,
            name: "zsh".into(),
            cwd: "/".into(),
            tty: None,
        };
        let live = Sidecar {
            holder: me.clone(),
            holder_pid: std::process::id(),
        };
        let dead = Sidecar {
            holder: me.clone(),
            holder_pid: 999_999_999,
        };
        fsx::write_atomic(
            &sidecar_path(&p, &l, "live"),
            &serde_json::to_vec(&live).unwrap(),
        )
        .unwrap();
        fsx::write_atomic(
            &sidecar_path(&p, &l, "dead"),
            &serde_json::to_vec(&dead).unwrap(),
        )
        .unwrap();
        fsx::write_atomic(&sidecar_path(&p, &l, "junk"), b"{").unwrap();
        assert_eq!(holders(&p, &l), vec![me]);
        assert!(!sidecar_path(&p, &l, "dead").exists());
    }

    #[test]
    fn ports_never_collide() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let names = vec!["A".to_string(), "B".to_string()];
        let first = alloc_ports(&p, (31000, 31003), &lease("x"), "pg", &names).unwrap();
        let second = alloc_ports(&p, (31000, 31003), &lease("y"), "pg", &names).unwrap();
        let all: BTreeSet<u16> = first.values().chain(second.values()).copied().collect();
        assert_eq!(all.len(), 4);
        assert!(alloc_ports(&p, (31000, 31003), &lease("z"), "pg", &names[..1]).is_err());
        free_ports(&p, &lease("x"), "pg").unwrap();
        assert_eq!(
            alloc_ports(&p, (31000, 31003), &lease("z"), "pg", &names)
                .unwrap()
                .len(),
            2
        );
        let busy = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = busy.local_addr().unwrap().port();
        assert!(
            alloc_ports(&p, (port, port), &lease("w"), "pg", &names[..1]).is_err(),
            "bound by another process"
        );
        let blocked = Paths {
            state: "/dev/null".into(),
            ..p
        };
        assert!(free_ports(&blocked, &lease("x"), "pg").is_err());
    }
}

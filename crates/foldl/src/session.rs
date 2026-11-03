// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Session state on disk: `sessions/<id>/state.json` plus `events.jsonl`.

use std::io;
use std::path::Path;

use foldl_core::env::{Env, EnvDiff};
use foldl_core::render::Shell;
use foldl_core::state::{Paths, Session};

use crate::fsx;

/// Events kept in `events.jsonl`.
const EVENTS_KEPT: usize = 1000;

/// Whether `id` can name a session directory: letters, digits, `.`, `_`
/// and `-`, and not `.` or `..`. Anything else could point outside
/// `sessions/`, and `session stop` removes the directory.
pub fn valid_id(id: &str) -> bool {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    !id.is_empty() && id != "." && id != ".." && id.chars().all(allowed)
}

pub fn load(paths: &Paths, id: &str) -> Option<Session> {
    valid_id(id).then_some(())?;
    let bytes = std::fs::read(paths.session(id).join("state.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn save(paths: &Paths, session: &Session) -> io::Result<()> {
    let json = serde_json::to_vec(session).map_err(io::Error::other)?;
    fsx::write_atomic(&paths.session(&session.id).join("state.json"), &json)
}

/// Appends lines to the session's event log, keeping the last 1000.
pub fn log_events(paths: &Paths, id: &str, lines: &[String]) -> io::Result<()> {
    let path = paths.session(id).join("events.jsonl");
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let all: Vec<&str> = old
        .lines()
        .chain(lines.iter().map(String::as_str))
        .collect();
    let kept = &all[all.len().saturating_sub(EVENTS_KEPT)..];
    fsx::write_atomic(&path, (kept.join("\n") + "\n").as_bytes())
}

/// Whether a process exists.
pub fn alive(pid: u32) -> bool {
    let pid = rustix::process::Pid::from_raw(pid as i32);
    let result = pid.map(rustix::process::test_kill_process);
    matches!(result, Some(Ok(()) | Err(rustix::io::Errno::PERM)))
}

/// A fresh session id: `<pid>-<random>`.
pub fn new_id(pid: u32) -> String {
    let mut bytes = [0u8; 4];
    let random = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| io::Read::read_exact(&mut f, &mut bytes));
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let word = random.map_or(nanos, |_| u32::from_le_bytes(bytes));
    format!("{pid}-{word:08x}")
}

/// The session for a process: its own if `FOLDL_SESSION` names a session
/// whose parent is `pid`, otherwise a new one. A new session started from
/// an inherited one also returns the inherited env changes to revert, so
/// the child starts from the parent's baseline.
pub fn resolve(
    paths: &Paths,
    inherited: Option<&str>,
    pid: u32,
    shell: Option<Shell>,
) -> (Session, Option<EnvDiff>) {
    let existing = inherited.and_then(|id| load(paths, id));
    match existing {
        Some(session) if session.parent == pid => (session, None),
        other => {
            sweep(paths);
            let revert = other.map(|s| s.cumulative());
            (Session::new(new_id(pid), pid, shell), revert)
        }
    }
}

/// Reverts an inherited session's changes where the env still has them.
pub fn revert(env: Env, inherited: &EnvDiff) -> Env {
    let still = inherited
        .0
        .iter()
        .filter(|(name, c)| env.get(*name) == c.after.as_ref());
    let undo = EnvDiff(still.map(|(n, c)| (n.clone(), c.clone())).collect()).invert();
    undo.apply(env)
}

/// Removes sessions whose parent and holder are both gone.
pub fn sweep(paths: &Paths) {
    let dirs = std::fs::read_dir(paths.sessions())
        .into_iter()
        .flatten()
        .flatten();
    let dead = dirs.filter(|dir| {
        let state = std::fs::read(dir.path().join("state.json")).ok();
        let session: Option<Session> = state.and_then(|b| serde_json::from_slice(&b).ok());
        session.is_some_and(|s| !alive(s.parent) && !s.holder.is_some_and(alive))
    });
    dead.for_each(|dir| drop(fsx::remove(&dir.path())));
}

/// Removes a session's directory.
pub fn remove(paths: &Paths, id: &str) {
    let _ = fsx::remove(&paths.session(id));
}

/// Whether `dir` is an existing session directory.
pub fn exists(paths: &Paths, id: &str) -> bool {
    Path::new(&paths.session(id)).join("state.json").exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use foldl_core::env::Value;

    fn paths(dir: &Path) -> Paths {
        Paths {
            state: dir.join("s"),
            data: dir.join("d"),
            cache: dir.join("c"),
            config: dir.join("cfg"),
        }
    }

    #[test]
    fn saves_loads_and_logs() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let s = Session::new("a".into(), 1, Some(Shell::Zsh));
        save(&p, &s).unwrap();
        assert_eq!(load(&p, "a"), Some(s));
        assert!(exists(&p, "a"));
        let many: Vec<String> = (0..1005).map(|i| i.to_string()).collect();
        log_events(&p, "a", &many).unwrap();
        log_events(&p, "a", &["last".into()]).unwrap();
        let text = std::fs::read_to_string(p.session("a").join("events.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 1000);
        assert!(text.ends_with("last\n"));
        remove(&p, "a");
        assert!(!exists(&p, "a"));
        assert_eq!(load(&p, "a"), None);
    }

    #[test]
    fn ids_stay_inside_sessions() {
        for ok in ["1234-abcd", "claude-0f9e.x_y"] {
            assert!(valid_id(ok), "{ok}");
        }
        for bad in ["", ".", "..", "../x", "a/b", "a b", "é"] {
            assert!(!valid_id(bad), "{bad}");
        }
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(&paths(dir.path()), "../.."), None);
    }

    #[test]
    fn resolves_own_and_inherited_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let me = std::process::id();
        let mut parent = Session::new("parent".into(), 999_999_999, None);
        parent.loaded = vec![];
        save(&p, &parent).unwrap();
        let mine = Session::new("mine".into(), me, None);
        save(&p, &mine).unwrap();
        assert_eq!(resolve(&p, Some("mine"), me, None), (mine, None));
        let (child, revert) = resolve(&p, Some("parent"), me, Some(Shell::Bash));
        assert!(child.id.starts_with(&format!("{me}-")));
        assert_eq!(revert, Some(EnvDiff::default()));
        assert!(!exists(&p, "parent"), "dead parent swept");
        assert!(exists(&p, "mine"));
        let (fresh, revert) = resolve(&p, None, me, None);
        assert_ne!(fresh.id, child.id);
        assert_eq!(revert, None);
    }

    #[test]
    fn reverts_only_unchanged_values() {
        let env = |pairs: &[(&str, &str)]| -> Env {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), Value::from(*v)))
                .collect()
        };
        let inherited = EnvDiff::between(
            &env(&[("A", "0")]),
            &env(&[("A", "1"), ("B", "1"), ("C", "1")]),
        );
        let now = env(&[("A", "1"), ("B", "changed"), ("C", "1")]);
        assert_eq!(
            revert(now, &inherited),
            env(&[("A", "0"), ("B", "changed")])
        );
    }

    #[test]
    fn liveness() {
        assert!(alive(std::process::id()));
        assert!(!alive(999_999_999));
        assert!(!alive(0));
        assert!(alive(1), "another user's process is alive");
    }
}

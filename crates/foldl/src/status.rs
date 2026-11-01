// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! `foldl status` and `foldl logs`.

use std::path::{Path, PathBuf};

use foldl_core::error::{Code, Failure};
use foldl_core::folder::Host;
use foldl_core::machine::tilde;
use foldl_core::plan::{FolderInfo, FolderStatus};
use foldl_core::state::{Paths, Session};
use serde_json::{Value, json};

use crate::cli::IoContext;
use crate::sys::Sys;
use crate::{gather, session, settings, trust_cmd};

/// Every session on disk.
pub fn sessions(paths: &Paths) -> Vec<Session> {
    let dirs = std::fs::read_dir(paths.sessions())
        .into_iter()
        .flatten()
        .flatten();
    let mut all: Vec<Session> = dirs
        .filter_map(|d| session::load(paths, &d.file_name().to_string_lossy()))
        .collect();
    all.sort_by(|a, b| a.id.cmp(&b.id));
    all
}

fn current(sys: &Sys, paths: &Paths) -> Option<Session> {
    session::load(paths, &sys.var("FOLDL_SESSION")?)
}

fn folders(sys: &Sys) -> Vec<FolderInfo> {
    let trust = trust_cmd::store(sys);
    let scan = gather::scan(&sys.cwd, &trust, &Default::default());
    let host = Host {
        nix_available: sys.which("nix").is_some(),
    };
    gather::infos(&scan, &trust, &settings::load(sys, None), host).0
}

/// One folder as JSON: trust, how many sessions have it loaded, and each
/// migration's state in this session.
fn folder_json(info: &FolderInfo, mine: Option<&Session>, all: &[Session]) -> Value {
    let loaded_in = |s: &Session| s.loaded.iter().any(|l| l.root == info.root);
    let (trust, migrations) = match &info.status {
        FolderStatus::Allowed { folder, .. } => (
            "allowed",
            folder
                .migrations
                .iter()
                .map(|m| (m.dir.clone(), m.name.clone(), m.scope.name()))
                .collect(),
        ),
        FolderStatus::Untrusted { .. } => ("not allowed", vec![]),
        FolderStatus::Denied => ("denied", vec![]),
        FolderStatus::Ignored => ("ignored", vec![]),
        FolderStatus::Invalid(_) => ("invalid", vec![]),
    };
    let migrations: Vec<Value> = migrations
        .into_iter()
        .map(|(dir, name, scope)| {
            let loaded = mine.and_then(|s| s.loaded.iter().find(|l| l.root == info.root && l.name == name));
            let state = loaded.map_or("down", |_| "up");
            let number = dir.split('-').next().unwrap_or_default().to_string();
            json!({"number": number, "name": name, "scope": scope, "state": state, "note": loaded.and_then(|l| l.note.clone())})
        })
        .collect();
    let private =
        crate::git::toplevel(&info.root).is_some() && crate::git::is_ignored(&info.root, ".foldl");
    json!({"root": info.root, "trust": trust, "private": private, "sessions": all.iter().filter(|s| loaded_in(s)).count(), "migrations": migrations})
}

fn folder_text(folder: &Value, home: &str) -> String {
    let root = tilde(Path::new(folder["root"].as_str().unwrap_or_default()), home);
    let trust = match folder["trust"].as_str().unwrap_or_default() {
        "allowed" => String::new(),
        other => format!(", {other}"),
    };
    let private = match folder["private"].as_bool() {
        Some(true) => ", private (not committed)",
        _ => "",
    };
    let head = format!(
        "{root}  ({} sessions{trust}{private})\n",
        folder["sessions"]
    );
    let width = folder["migrations"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| m["name"].as_str().unwrap_or_default().len())
        .max()
        .unwrap_or(0);
    let rows: String = folder["migrations"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| {
            let note = m["note"]
                .as_str()
                .map(|n| format!("  {n}"))
                .unwrap_or_default();
            let (number, name, scope, state) = (
                m["number"].as_str(),
                m["name"].as_str(),
                m["scope"].as_str(),
                m["state"].as_str(),
            );
            format!(
                "  {} {:width$}  {:7} {}{note}\n",
                number.unwrap_or_default(),
                name.unwrap_or_default(),
                scope.unwrap_or_default(),
                state.unwrap_or_default()
            )
        })
        .collect();
    head + &rows
}

fn session_json(s: &Session) -> Value {
    let loaded: Vec<Value> = s
        .loaded
        .iter()
        .map(|l| json!({"root": l.root, "name": l.name, "scope": l.scope.name()}))
        .collect();
    json!({"id": s.id, "parent": s.parent, "shell": s.shell.map(|sh| sh.name()), "loaded": loaded})
}

fn session_text(s: &Value) -> String {
    let shell = s["shell"].as_str().unwrap_or("agent");
    let loaded: Vec<String> = s["loaded"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|l| l["name"].as_str().unwrap_or_default().to_string())
        .collect();
    format!(
        "session {} ({shell} {}): {}\n",
        s["id"].as_str().unwrap_or_default(),
        s["parent"],
        match loaded.is_empty() {
            true => "nothing loaded".to_string(),
            false => loaded.join(", "),
        }
    )
}

pub fn status(sys: &Sys, all: bool, json: bool, events: bool) -> Result<i32, Failure> {
    let paths = sys.paths();
    let mine = current(sys, &paths);
    if events {
        let id = mine.as_ref().map(|s| s.id.clone()).unwrap_or_default();
        print!(
            "{}",
            std::fs::read_to_string(paths.session(&id).join("events.jsonl")).unwrap_or_default()
        );
        return Ok(0);
    }
    let everyone = sessions(&paths);
    let folders: Vec<Value> = folders(sys)
        .iter()
        .map(|f| folder_json(f, mine.as_ref(), &everyone))
        .collect();
    let listed: Vec<Value> = everyone.iter().filter(|_| all).map(session_json).collect();
    let home = sys.var("HOME").unwrap_or_default();
    let text = match json {
        true => json!({"folders": folders, "sessions": listed}).to_string() + "\n",
        false => folders
            .iter()
            .map(|f| folder_text(f, &home))
            .chain(listed.iter().map(session_text))
            .collect(),
    };
    print!("{text}");
    Ok(0)
}

/// The log of a migration's last run in this session, innermost folder
/// first.
pub fn logs(sys: &Sys, name: &str) -> Result<i32, Failure> {
    let paths = sys.paths();
    let id = sys.var("FOLDL_SESSION").unwrap_or_default();
    let roots: Vec<PathBuf> = crate::disk::ancestors(&sys.cwd).into_iter().rev().collect();
    let found = roots
        .iter()
        .map(|root| Paths::migration_dirs(&paths.session(&id), root, name).2)
        .find_map(|log| std::fs::read(log).ok());
    let bytes = found
        .ok_or_else(|| Failure::new(Code::Usage, format!("no log for {name} in this session")))?;
    std::io::Write::write_all(&mut std::io::stdout(), &bytes).ctx("writing log")?;
    Ok(0)
}

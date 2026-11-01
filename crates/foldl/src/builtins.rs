// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Built-ins: `dotenv`, and `nix-flake` (its Nix details are in
//! [`crate::nix`]). Each returns an outcome like a script's, writing what it
//! did to the migration's log.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use foldl_core::folder::Hash;
use foldl_core::machine::{Outcome, Run};
use foldl_core::state::folder_key;

use crate::nix::{self, Cache};
use crate::runner::{Runner, which};

/// Loads a `.env` file relative to the folder.
pub fn dotenv(file: &str, required: bool, run: &Run, log: &mut File) -> Outcome {
    let path = run.cwd.join(file);
    let read = std::fs::read_to_string(&path);
    let parsed = match read {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => {
            let _ = writeln!(log, "dotenv: {file} not found; skipped");
            Ok(run.env.clone())
        }
        Err(e) => Err(e.to_string()),
        Ok(text) => foldl_core::dotenv::parse(&text, &run.env),
    };
    match parsed {
        Ok(env) => Outcome::Exited {
            status: 0,
            env: Some(env),
        },
        Err(message) => {
            let _ = writeln!(log, "dotenv: {file}: {message}");
            Outcome::Exited {
                status: 1,
                env: None,
            }
        }
    }
}

/// Evaluates the flake's dev shell, or reuses a cached evaluation of the
/// same content. When evaluation fails, falls back to the folder's last
/// good environment with a warning.
pub fn nix_flake(
    runner: &Runner,
    cache: &Cache,
    flake: &str,
    watch: &Hash,
    run: &Run,
    log: &mut File,
) -> (Outcome, Vec<String>) {
    let Some(nix) = which("nix", run.env.get("PATH")) else {
        let what = format!("nix-flake needs nix on PATH; see {}", nix::DOCS);
        return (Outcome::Unavailable(what), vec![]);
    };
    let reference = nix::flake_ref(&run.cwd, flake);
    let key = nix::cache_key(&reference, watch);
    let folder = folder_key(&run.cwd);
    let cached = cache.entry(&key).join("env.json");
    let evaluated = match cached.exists() {
        true => {
            let _ = writeln!(log, "nix-flake: using the cached evaluation {key}");
            Ok(Outcome::Exited {
                status: 0,
                env: None,
            })
        }
        false => evaluate(runner, &nix, &reference, &cache.entry(&key), run, log),
    };
    let evaluated = match evaluated {
        Ok(outcome) => outcome,
        Err(e) => Outcome::Unavailable(format!("nix-flake: {e}")),
    };
    let (used, warnings) = match (evaluated, cache.last_good(&folder)) {
        (Outcome::Exited { status: 0, .. }, _) => (key, vec![]),
        (outcome, None) => return (outcome, vec![]),
        (_, Some(last)) => {
            let warning = format!(
                "{}: nix-flake evaluation failed; using the last good environment (foldl logs {} has why)",
                run.name, run.name
            );
            (last, vec![warning])
        }
    };
    let _ = cache.remember(&folder, &used);
    let json = std::fs::read(cache.entry(&used).join("env.json")).unwrap_or_default();
    let applied = nix::apply(&json, &run.env);
    let outcome = match applied {
        Ok(env) => Outcome::Exited {
            status: 0,
            env: Some(env),
        },
        Err(message) => {
            let _ = writeln!(log, "nix-flake: {message}");
            Outcome::Exited {
                status: 1,
                env: None,
            }
        }
    };
    (outcome, warnings)
}

/// Runs `nix print-dev-env` into `<entry>/env.json`.
fn evaluate(
    runner: &Runner,
    nix: &Path,
    reference: &str,
    entry: &Path,
    run: &Run,
    log: &File,
) -> std::io::Result<Outcome> {
    crate::fsx::ensure_dir(entry)?;
    let partial = entry.join("env.json.partial");
    let out = crate::fsx::create_private(&partial)?;
    let mut cmd = nix::command(nix, reference, entry);
    cmd.env_clear().envs(
        run.env
            .iter()
            .map(|(k, v)| (k, std::ffi::OsStr::from_bytes(&v.0))),
    );
    cmd.current_dir(&run.cwd)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(log.try_clone()?);
    let child = std::os::unix::process::CommandExt::process_group(&mut cmd, 0).spawn()?;
    let outcome = runner.wait(child, run);
    match outcome {
        Outcome::Exited { status: 0, .. } => {
            std::fs::rename(&partial, entry.join("env.json")).map(|()| outcome)
        }
        other => Ok(other),
    }
}

use std::os::unix::ffi::OsStrExt;

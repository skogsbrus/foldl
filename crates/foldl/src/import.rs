// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! `foldl import direnv`: convert `.envrc` into `.foldl/`.

use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::Command;

use foldl_core::direnv::{migrations, translate};
use foldl_core::error::{Code, Failure};
use foldl_core::machine::tilde;
use serde_json::Value;

use crate::cli::IoContext;
use crate::new_cmd::{Privacy, choose, make_private};
use crate::sys::Sys;
use crate::{fsx, git, trust_cmd};

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub dir: Option<PathBuf>,
    pub yes: bool,
    pub shared: Option<String>,
    pub dry_run: bool,
    pub remove_envrc: bool,
    pub privacy: Option<Privacy>,
}

/// Asks whether lines that start things outside the shell should become a
/// shared migration, and its name.
fn ask_shared(lines: &[(String, String)]) -> Option<String> {
    let listed: String = lines
        .iter()
        .map(|(l, stop)| format!("  {l}\n    stop with: {stop}\n"))
        .collect();
    eprint!(
        "These lines start things outside the shell:\n{listed}Make them a shared migration (started once, stopped when the last session leaves)? [y/N] "
    );
    let mut answer = String::new();
    let _ = std::io::stdin().lock().read_line(&mut answer);
    let yes = matches!(answer.trim(), "y" | "Y" | "yes");
    yes.then(|| "services".to_string())
}

pub fn import_direnv(sys: &Sys, options: Options) -> Result<i32, Failure> {
    let root = sys.absolute(&options.dir.clone().unwrap_or_default());
    let root = root
        .canonicalize()
        .map_err(|e| Failure::new(Code::Usage, format!("{}: {e}", root.display())))?;
    let shown = tilde(&root, &sys.var("HOME").unwrap_or_default());
    let envrc = std::fs::read_to_string(root.join(".envrc"))
        .map_err(|e| Failure::new(Code::Usage, format!("{shown}/.envrc: {e}")))?;
    if root.join(".foldl").exists() {
        return Err(
            Failure::new(Code::Usage, format!("{shown} already has a .foldl/"))
                .with_fix("move it aside, or edit it by hand"),
        );
    }
    let t = translate(&envrc);
    t.notes.iter().for_each(|n| eprintln!("foldl ! {n}"));
    let interactive =
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal() && !options.yes;
    let shared = match (
        options.shared.clone(),
        t.side_effects.is_empty(),
        interactive,
    ) {
        (Some(name), _, _) => Some(name),
        (None, false, true) => ask_shared(&t.side_effects),
        _ => None,
    };
    let files = migrations(&t, shared.as_deref());
    if options.dry_run {
        let listing: String = files
            .iter()
            .map(|(path, content)| format!("==> .foldl/{path}\n{content}"))
            .collect();
        print!("{listing}");
        return Ok(0);
    }
    let privacy = git::toplevel(&root).map(|_| choose(options.privacy));
    if privacy == Some(Privacy::Private) {
        make_private(&root)?;
    }
    for (path, content) in &files {
        let target = root.join(".foldl").join(path);
        fsx::ensure_dir(target.parent().unwrap_or(&root)).ctx("creating .foldl")?;
        std::fs::write(&target, content).ctx("writing .foldl")?;
    }
    trust_cmd::allow(sys, Some(root.clone()))?;
    parity(sys, &root);
    disable_direnv(sys, &root, options.remove_envrc)?;
    eprintln!(
        "foldl: imported {shown}/.envrc into {} migrations; they load at the next prompt",
        files
            .iter()
            .filter(|(p, _)| p.ends_with("manifest.toml"))
            .count()
    );
    Ok(0)
}

/// A command's stdout as an env map, or why not.
fn json_env(mut cmd: Command) -> Result<BTreeMap<String, Value>, String> {
    let failed = std::process::Output {
        status: std::os::unix::process::ExitStatusExt::from_raw(256),
        stdout: vec![],
        stderr: b"couldn't run it".to_vec(),
    };
    let out = cmd.output().unwrap_or(failed);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    match (
        out.status.success(),
        serde_json::from_str::<BTreeMap<String, Value>>(&text),
    ) {
        (true, Ok(map)) => Ok(map
            .into_iter()
            .filter(|(k, _)| !k.starts_with("DIRENV_") && !k.starts_with("FOLDL_"))
            .collect()),
        _ => Err(String::from_utf8_lossy(&out.stderr)
            .lines()
            .last()
            .unwrap_or("no output")
            .to_string()),
    }
}

/// Compares what direnv and foldl export for the folder and prints the
/// differences.
fn parity(sys: &Sys, root: &Path) {
    let Some(direnv) = sys.which("direnv") else {
        eprintln!("foldl: direnv isn't on PATH; parity not checked");
        return;
    };
    let mut direnv_cmd = Command::new(direnv);
    direnv_cmd
        .args(["export", "json"])
        .current_dir(root)
        .env_remove("DIRENV_DIR")
        .env_remove("DIRENV_DIFF");
    let mut foldl_cmd = Command::new(&sys.exe);
    foldl_cmd
        .args(["export", "json"])
        .current_dir(root)
        .env_remove("FOLDL_SESSION");
    let report = match (json_env(direnv_cmd), json_env(foldl_cmd)) {
        (Err(why), _) => format!("direnv couldn't export ({why}); parity not checked"),
        (_, Err(why)) => {
            format!("foldl couldn't load it ({why}); fix the migrations, then foldl allow")
        }
        (Ok(d), Ok(f)) => {
            let names: std::collections::BTreeSet<&String> = d.keys().chain(f.keys()).collect();
            let differences: Vec<String> = names
                .into_iter()
                .filter(|n| d.get(*n) != f.get(*n))
                .map(|n| {
                    format!(
                        "  {n}: direnv {} · foldl {}",
                        show(d.get(n)),
                        show(f.get(n))
                    )
                })
                .collect();
            match differences.is_empty() {
                true => "parity: direnv and foldl export the same environment".to_string(),
                false => format!("parity: differences to review\n{}", differences.join("\n")),
            }
        }
    };
    eprintln!("foldl: {report}");
}

fn show(value: Option<&Value>) -> String {
    value.map_or("(not set)".to_string(), Value::to_string)
}

/// `direnv deny`, so direnv stops loading the folder; `.envrc` is removed
/// only when asked, since that switches everyone over.
fn disable_direnv(sys: &Sys, root: &Path, remove: bool) -> Result<(), Failure> {
    let denied = sys
        .which("direnv")
        .map(|direnv| Command::new(direnv).arg("deny").arg(root).status());
    match denied {
        Some(Ok(status)) if status.success() => {
            eprintln!("foldl: ran direnv deny (local to you; .envrc stays for others)")
        }
        _ => eprintln!("foldl: couldn't run direnv deny; run it yourself"),
    }
    match remove {
        true => std::fs::remove_file(root.join(".envrc")).ctx("removing .envrc"),
        false => Ok(()),
    }
}

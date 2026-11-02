// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Translating `.envrc` into migrations, for `foldl import direnv`.
//!
//! Known direnv stdlib calls (`use flake`, `dotenv`, `dotenv_if_exists`,
//! `PATH_add`, `watch_file`) and plain `export NAME=value` lines become
//! built-ins and manifest entries. `source_up` is dropped: every ancestor's
//! `.foldl/` loads anyway. Lines that start things outside the shell are
//! set apart as candidates for a shared migration. Everything else is kept
//! verbatim in a session migration, flagged for review.

use std::collections::BTreeMap;

use crate::env::is_identifier;
use crate::scan::side_effect;

/// What an `.envrc` says, sorted into what foldl can express.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Translation {
    pub env: BTreeMap<String, String>,
    pub path_add: Vec<String>,
    pub watch: Vec<String>,
    /// `use flake [ref]`.
    pub flake: Option<String>,
    /// `dotenv [file]` / `dotenv_if_exists [file]`: file and required.
    pub dotenv: Vec<(String, bool)>,
    /// Kept as shell code.
    pub script: Vec<String>,
    /// Lines with effects outside the shell, with a suggested stop.
    pub side_effects: Vec<(String, String)>,
    /// Things the user should review.
    pub notes: Vec<String>,
}

/// A literal `export` value, unquoted: `None` if it needs a shell (command
/// substitution, globs, variables foldl can't express).
fn literal(value: &str) -> Option<String> {
    let (inner, double) = match (value.strip_prefix('\''), value.strip_prefix('"')) {
        (Some(single), _) => {
            return single
                .strip_suffix('\'')
                .filter(|v| !v.contains('\''))
                .map(str::to_string);
        }
        (None, Some(double)) => (double.strip_suffix('"')?, true),
        (None, None) => (value, false),
    };
    let special = [
        '`', '\\', '*', '?', '[', ' ', ';', '&', '|', '<', '>', '(', ')',
    ];
    let shell_needed = inner.contains("$(")
        || inner
            .chars()
            .any(|c| special.contains(&c) && !(double && c == ' '));
    let converted = inner
        .replace("$PWD", "${FOLDL_ROOT}")
        .replace("${PWD}", "${FOLDL_ROOT}")
        .replace("$HOME", "${HOME}");
    let unknown = converted
        .replace("${FOLDL_ROOT}", "")
        .replace("${HOME}", "")
        .contains('$');
    (!shell_needed && !unknown).then_some(converted)
}

/// Sorts an `.envrc` into a [`Translation`].
pub fn translate(envrc: &str) -> Translation {
    envrc
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .fold(Translation::default(), |mut t, line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            let arg = words
                .get(1)
                .map(|w| w.trim_matches(|c| c == '"' || c == '\'').to_string());
            let export = line
                .strip_prefix("export ")
                .and_then(|rest| rest.split_once('='));
            let side = side_effect(line);
            match (words[0], words.get(1).copied(), export, side) {
                ("use", Some("flake"), _, _) | ("use_flake", _, _, _) => {
                    let reference = words.get(
                        words
                            .iter()
                            .position(|w| *w == "flake" || *w == "use_flake")
                            .unwrap_or(0)
                            + 1,
                    );
                    t.flake = Some(reference.map_or(".".to_string(), |r| r.to_string()));
                }
                ("dotenv", _, _, _) => t.dotenv.push((arg.unwrap_or(".env".into()), true)),
                ("dotenv_if_exists", _, _, _) => {
                    t.dotenv.push((arg.unwrap_or(".env".into()), false))
                }
                ("PATH_add", Some(_), _, _) => t
                    .path_add
                    .push(arg.unwrap_or_default().trim_start_matches("./").to_string()),
                ("watch_file", Some(_), _, _) => t.watch.extend(
                    words[1..]
                        .iter()
                        .map(|w| w.trim_start_matches("./").to_string()),
                ),
                ("source_up" | "source_up_if_exists", _, _, _) => t
                    .notes
                    .push("source_up dropped: foldl loads every ancestor's .foldl/".into()),
                (_, _, Some((name, value)), None)
                    if is_identifier(name) && literal(value).is_some() =>
                {
                    t.env
                        .insert(name.to_string(), literal(value).unwrap_or_default());
                }
                (_, _, _, Some((_, stop))) => {
                    t.side_effects.push((line.to_string(), stop.to_string()))
                }
                _ => {
                    t.notes.push(format!("kept as shell code, review: {line}"));
                    t.script.push(line.to_string());
                }
            }
            t
        })
}

/// One file to write under `.foldl/`.
pub type File = (String, String);

/// The flake's own files and any `watch_file` entries.
fn flake_watch(extra: &[String]) -> String {
    let mut all = vec!["flake.nix".to_string(), "flake.lock".to_string()];
    all.extend(
        extra
            .iter()
            .filter(|w| !["flake.nix", "flake.lock"].contains(&w.as_str()))
            .cloned(),
    );
    all.iter()
        .map(|w| toml_str(w))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A TOML string literal.
fn toml_str(text: &str) -> String {
    toml::Value::String(text.to_string()).to_string()
}

const BASH: &str = "#!/usr/bin/env bash\n";

/// The migrations for a translation. Side effects go to a shared
/// migration named `shared` if given, otherwise into the session script.
pub fn migrations(t: &Translation, shared: Option<&str>) -> Vec<File> {
    let mut files = Vec::new();
    let mut n = 0;
    let mut next = |name: &str| {
        n += 1;
        format!("{n:03}-{name}")
    };
    if !t.env.is_empty() || !t.path_add.is_empty() {
        let dir = next("env");
        let paths = t
            .path_add
            .iter()
            .map(|p| toml_str(p))
            .collect::<Vec<_>>()
            .join(", ");
        let path_add = match t.path_add.is_empty() {
            true => String::new(),
            false => format!("path_add = [{paths}]\n"),
        };
        let env: String = t
            .env
            .iter()
            .map(|(k, v)| format!("{k} = {}\n", toml_str(v)))
            .collect();
        let env = match env.is_empty() {
            true => String::new(),
            false => format!("\n[env]\n{env}"),
        };
        files.push((
            format!("{dir}/manifest.toml"),
            format!("version = 1\n{path_add}{env}"),
        ));
    }
    if let Some(flake) = &t.flake {
        let dir = next("flake");
        let options = match flake.as_str() {
            "." => String::new(),
            other => format!("\n[builtin_options]\nflake = {}\n", toml_str(other)),
        };
        files.push((
            format!("{dir}/manifest.toml"),
            format!(
                "version = 1\nbuiltin = \"nix-flake\"\nwatch = [{}]\n{options}",
                flake_watch(&t.watch)
            ),
        ));
    }
    for (i, (file, required)) in t.dotenv.iter().enumerate() {
        let name = match i {
            0 => "dotenv".to_string(),
            i => format!("dotenv-{}", i + 1),
        };
        let dir = next(&name);
        files.push((
            format!("{dir}/manifest.toml"),
            format!("version = 1\nbuiltin = \"dotenv\"\n\n[builtin_options]\nfile = {}\nrequired = {required}\n", toml_str(file)),
        ));
    }
    let in_script: Vec<&String> = match shared {
        Some(_) => t.script.iter().collect(),
        None => t
            .script
            .iter()
            .chain(t.side_effects.iter().map(|(l, _)| l))
            .collect(),
    };
    if !in_script.is_empty() {
        let dir = next("envrc");
        let body: String = in_script.iter().map(|l| format!("{l}\n")).collect();
        let header =
            "# Imported from .envrc by foldl; review: direnv stdlib functions don't exist here.\n";
        files.push((format!("{dir}/manifest.toml"), "version = 1\n".to_string()));
        files.push((format!("{dir}/up.sh"), format!("{BASH}{header}{body}")));
    }
    if let (Some(name), false) = (shared, t.side_effects.is_empty()) {
        let dir = next(name);
        let up: String = t
            .side_effects
            .iter()
            .map(|(l, _)| format!("{l}\n"))
            .collect();
        let down: String = t
            .side_effects
            .iter()
            .map(|(_, stop)| format!("# {stop}\n"))
            .collect();
        files.push((
            format!("{dir}/manifest.toml"),
            "version = 1\nscope = \"shared\"\n".to_string(),
        ));
        files.push((format!("{dir}/up.sh"), format!("{BASH}{up}")));
        files.push((
            format!("{dir}/down.sh"),
            format!("{BASH}# Stop what up.sh started. Suggested:\n{down}"),
        ));
    }
    // Like direnv, a watched file reloads everything: it goes on the first
    // migration.
    let watch = t
        .watch
        .iter()
        .map(|w| toml_str(w))
        .collect::<Vec<_>>()
        .join(", ");
    if let (Some(first), false, None) = (files.first_mut(), t.watch.is_empty(), &t.flake) {
        // Top-level keys go before any table such as [env].
        first.1 = first.1.replacen(
            "version = 1\n",
            &format!("version = 1\nwatch = [{watch}]\n"),
            1,
        );
    }
    files
}

#[cfg(test)]
mod tests;

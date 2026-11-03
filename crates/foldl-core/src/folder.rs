// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! A folder's `.foldl/` as data: migration directories, their resolved
//! manifests, validation and content hashes.
//!
//! The interpreter reads the files; everything here is a function of their
//! bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::env::{hex, is_identifier};
use crate::manifest::{
    BuiltinName, RawManifest, Reuse, Scope, parse_duration, parse_folder_config, parse_manifest,
};
use crate::scan;

/// A sha256 content hash, hex encoded.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Hash(pub String);

impl Hash {
    /// The first 12 hex digits, for names and messages.
    pub fn short(&self) -> &str {
        &self.0[..self.0.len().min(12)]
    }

    pub fn of_bytes(bytes: &[u8]) -> Hash {
        Hash(hex(&Sha256::digest(bytes)))
    }
}

/// One entry under `.foldl/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    File(Vec<u8>),
    Dir,
}

/// Everything under a folder's `.foldl/`, keyed by `/`-separated path
/// relative to `.foldl/`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderFiles {
    pub root: PathBuf,
    pub entries: BTreeMap<String, Entry>,
}

/// Hashes paths and contents. Directories count, so an empty directory
/// changes the hash too.
pub fn hash_entries<'a>(entries: impl Iterator<Item = (&'a str, &'a Entry)>) -> Hash {
    let mut hasher = Sha256::new();
    for (path, entry) in entries {
        hasher.update(path.as_bytes());
        match entry {
            Entry::Dir => hasher.update(b"\0d\0"),
            Entry::File(bytes) => {
                hasher.update(b"\0f");
                hasher.update((bytes.len() as u64).to_le_bytes());
                hasher.update(bytes);
            }
        }
    }
    Hash(hex(&hasher.finalize()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warning,
    Error,
}

/// A validation finding, attributed to a file under the folder.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Issue {
    pub severity: Severity,
    pub file: String,
    pub message: String,
}

impl Issue {
    fn error(file: &str, message: impl Into<String>) -> Issue {
        Issue {
            severity: Severity::Error,
            file: file.to_string(),
            message: message.into(),
        }
    }

    fn warning(file: &str, message: impl Into<String>) -> Issue {
        Issue {
            severity: Severity::Warning,
            file: file.to_string(),
            message: message.into(),
        }
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// `error: .foldl/001-env/manifest.toml: message`
    pub fn display(&self) -> String {
        let label = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        format!("{label}: .foldl/{}: {}", self.file, self.message)
    }
}

/// Which shell a script is written for, from its shebang.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptShell {
    Bash,
    Zsh,
    Sh,
}

/// A script's shebang: the program as written (`bash` for
/// `#!/usr/bin/env bash`, `/bin/bash` for `#!/bin/bash`) and its dialect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shebang {
    pub program: String,
    pub shell: ScriptShell,
}

/// Reads a shebang. `None` if missing or not naming bash, zsh or sh.
pub fn parse_shebang(script: &[u8]) -> Option<Shebang> {
    let first = script.split(|&b| b == b'\n').next()?;
    let line = std::str::from_utf8(first.strip_prefix(b"#!")?).ok()?;
    let mut words = line.split_whitespace();
    let interpreter = words.next()?;
    let program = match basename(interpreter) {
        "env" => words.find(|w| !w.starts_with('-'))?,
        _ => interpreter,
    };
    let shell = match basename(program) {
        "bash" => ScriptShell::Bash,
        "zsh" => ScriptShell::Zsh,
        "sh" => ScriptShell::Sh,
        _ => return None,
    };
    Some(Shebang {
        program: program.to_string(),
        shell,
    })
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// A script inside a migration directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Script {
    /// Relative to the migration directory.
    pub path: String,
    pub shebang: Shebang,
}

/// A built-in with its options resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "builtin", rename_all = "kebab-case")]
pub enum Builtin {
    NixFlake { flake: String },
    Dotenv { file: String, required: bool },
}

impl Builtin {
    /// The files the built-in reads, as `watch` globs relative to
    /// `FOLDL_ROOT`, so editing them reloads the migration as direnv's
    /// `dotenv` and `use flake` do. Absolute paths and remote flakes can't
    /// be watched.
    pub fn watched(&self) -> Vec<String> {
        let relative = |p: &str| !p.starts_with('/');
        match self {
            Builtin::Dotenv { file, .. } if relative(file) => vec![globset::escape(file)],
            Builtin::NixFlake { flake } if relative(flake) && !flake.contains(':') => {
                let dir = match flake.trim_end_matches('/') {
                    "." | "" => String::new(),
                    dir => format!("{}/", globset::escape(dir)),
                };
                ["flake.nix", "flake.lock"]
                    .map(|f| format!("{dir}{f}"))
                    .to_vec()
            }
            _ => vec![],
        }
    }
}

/// What a shared migration's lease is keyed on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LeaseSpec {
    Folder,
    Repo,
    Custom(String),
}

/// One piece of an `[env]` value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Piece {
    Text(String),
    Var(String),
}

/// A migration with its manifest resolved against the files next to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Migration {
    pub name: String,
    pub number: u64,
    pub local: bool,
    /// Directory name under `.foldl/`.
    pub dir: String,
    pub scope: Scope,
    pub interactive: bool,
    pub up: Option<Script>,
    pub down: Option<Script>,
    pub builtin: Option<Builtin>,
    pub path_add: Vec<String>,
    pub env: BTreeMap<String, Vec<Piece>>,
    pub unset: Vec<String>,
    pub aliases: BTreeMap<String, String>,
    pub lease: Option<LeaseSpec>,
    pub ports: Vec<String>,
    pub timeout: Option<Duration>,
    pub down_timeout: Option<Duration>,
    pub watch: Vec<String>,
    /// Hash of the migration directory's content.
    pub hash: Hash,
}

/// A parsed, validated folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub root: PathBuf,
    /// Hash of all of `.foldl/`, local migrations included: what trust is
    /// granted to.
    pub hash: Hash,
    pub reuse: Reuse,
    pub migrations: Vec<Migration>,
    pub warnings: Vec<Issue>,
}

/// `<number>-<name>` or `<number>-<name>.local`.
pub fn parse_dir_name(dir: &str) -> Option<(u64, String, bool)> {
    let (stem, local) = match dir.strip_suffix(".local") {
        Some(stem) => (stem, true),
        None => (dir, false),
    };
    let (digits, name) = stem.split_once('-')?;
    let valid_digits = !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit());
    let mut chars = name.chars();
    let valid_first = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let valid_rest = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    let number = digits
        .parse()
        .ok()
        .filter(|_| valid_digits && valid_first && valid_rest)?;
    Some((number, name.to_string(), local))
}

/// Facts about the host that some checks need.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Host {
    pub nix_available: bool,
}

/// Parses and validates a folder. `Err` holds every issue found, errors and
/// warnings, when there is at least one error.
pub fn load(files: &FolderFiles, host: Host) -> Result<Folder, Vec<Issue>> {
    let top = top_level(files);
    let mut issues: Vec<Issue> = top
        .iter()
        .filter_map(|t| t.as_ref().err().cloned())
        .collect();
    let dirs: Vec<(u64, String, bool, String)> =
        top.into_iter().filter_map(Result::ok).flatten().collect();
    let config = match files.entries.get("foldl.toml") {
        Some(Entry::File(bytes)) => Some(folder_config(bytes)),
        _ => None,
    };
    let reuse = match config {
        Some(Ok(reuse)) => reuse,
        Some(Err(issue)) => {
            issues.push(issue);
            Reuse::None
        }
        None => Reuse::None,
    };
    let duplicates = duplicate_names(&dirs);
    issues.extend(duplicates);
    let mut sorted = dirs;
    sorted.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    let resolved: Vec<Resolved> = sorted
        .into_iter()
        .map(|(number, name, local, dir)| migration(files, number, name, local, dir, host))
        .collect();
    let mut migrations = Vec::new();
    for result in resolved {
        match result {
            Ok((m, warnings)) => {
                migrations.push(m);
                issues.extend(warnings);
            }
            Err(found) => issues.extend(found),
        }
    }
    issues.sort();
    if issues.iter().any(Issue::is_error) {
        return Err(issues);
    }
    let hash = hash_entries(files.entries.iter().map(|(p, e)| (p.as_str(), e)));
    Ok(Folder {
        root: files.root.clone(),
        hash,
        reuse,
        migrations,
        warnings: issues,
    })
}

type Resolved = Result<(Migration, Vec<Issue>), Vec<Issue>>;

type TopEntry = Result<Option<(u64, String, bool, String)>, Issue>;

/// Classifies `.foldl/`'s direct children: migration directories,
/// `foldl.toml`, or errors.
fn top_level(files: &FolderFiles) -> Vec<TopEntry> {
    files
        .entries
        .iter()
        .filter(|(path, _)| !path.contains('/'))
        .map(|(path, entry)| match (path.as_str(), entry) {
            ("foldl.toml", Entry::File(_)) | (".DS_Store", _) => Ok(None),
            (dir, Entry::Dir) => match parse_dir_name(dir) {
                Some((number, name, local)) => Ok(Some((number, name, local, dir.to_string()))),
                None => Err(Issue::error(
                    dir,
                    "not a migration directory: expected <number>-<name> or <number>-<name>.local, \
                     with <name> matching [a-z0-9][a-z0-9-]*",
                )),
            },
            (file, Entry::File(_)) => Err(Issue::error(
                file,
                "unexpected file; only foldl.toml and migration directories belong here",
            )),
        })
        .collect()
}

fn duplicate_names(dirs: &[(u64, String, bool, String)]) -> Vec<Issue> {
    let mut seen = BTreeSet::new();
    dirs.iter()
        .filter(|(_, name, _, _)| !seen.insert(name.clone()))
        .map(|(_, name, _, dir)| Issue::error(dir, format!("duplicate migration name '{name}'")))
        .collect()
}

fn folder_config(bytes: &[u8]) -> Result<Reuse, Issue> {
    let raw = parse_folder_config(&String::from_utf8_lossy(bytes))
        .map_err(|e| Issue::error("foldl.toml", e))?;
    match raw.version {
        Some(1) => Ok(raw.trust.and_then(|t| t.reuse).unwrap_or_default()),
        Some(v) => Err(Issue::error(
            "foldl.toml",
            format!("unsupported version {v}; this foldl reads version 1"),
        )),
        None => Err(Issue::error("foldl.toml", "missing required key 'version'")),
    }
}

/// Files of one migration directory, keyed by path relative to it.
fn migration_files<'a>(files: &'a FolderFiles, dir: &str) -> BTreeMap<&'a str, &'a Entry> {
    let prefix = format!("{dir}/");
    files
        .entries
        .iter()
        .filter_map(|(path, entry)| Some((path.strip_prefix(&prefix)?, entry)))
        .collect()
}

fn migration(
    files: &FolderFiles,
    number: u64,
    name: String,
    local: bool,
    dir: String,
    host: Host,
) -> Resolved {
    let own = migration_files(files, &dir);
    let manifest_file = format!("{dir}/manifest.toml");
    let text = match own.get("manifest.toml") {
        Some(Entry::File(bytes)) => String::from_utf8_lossy(bytes).into_owned(),
        _ => return Err(vec![Issue::error(&manifest_file, "missing manifest.toml")]),
    };
    let raw = parse_manifest(&text).map_err(|e| vec![Issue::error(&manifest_file, e)])?;
    let hash = hash_entries(own.iter().map(|(p, e)| (*p, *e)));
    resolve(
        Ctx {
            file: &manifest_file,
            dir: &dir,
            own: &own,
            host,
        },
        raw,
        number,
        name,
        local,
        hash,
    )
}

struct Ctx<'a> {
    file: &'a str,
    dir: &'a str,
    own: &'a BTreeMap<&'a str, &'a Entry>,
    host: Host,
}

impl Ctx<'_> {
    fn err(&self, message: impl Into<String>) -> Issue {
        Issue::error(self.file, message)
    }

    /// A script named by the manifest, or the default file if it exists.
    fn script(
        &self,
        key: &str,
        named: Option<&str>,
        default: &str,
    ) -> (Option<(Script, String)>, Vec<Issue>) {
        let path = match named {
            Some(path) => path,
            None if self.own.contains_key(default) => default,
            None => return (None, vec![]),
        };
        if path.starts_with('/') || path.split('/').any(|part| part == "..") {
            return (
                None,
                vec![self.err(format!(
                    "{key} = \"{path}\" must be inside the migration directory"
                ))],
            );
        }
        let file = format!("{}/{path}", self.dir);
        let Some(Entry::File(bytes)) = self.own.get(path) else {
            return (
                None,
                vec![self.err(format!("{key} = \"{path}\": no such file"))],
            );
        };
        let Some(shebang) = parse_shebang(bytes) else {
            let message = "needs a shebang naming bash, zsh or sh, such as #!/usr/bin/env bash";
            return (None, vec![Issue::error(&file, message)]);
        };
        let text = String::from_utf8_lossy(bytes).into_owned();
        let warnings = scan::outside_references(&text)
            .into_iter()
            .map(|r| {
                Issue::warning(
                    &file,
                    format!("sources {r}, outside the migration directory; the trust hash can't cover it"),
                )
            })
            .collect();
        (
            Some((
                Script {
                    path: path.to_string(),
                    shebang,
                },
                text,
            )),
            warnings,
        )
    }
}

fn resolve(
    cx: Ctx,
    raw: RawManifest,
    number: u64,
    name: String,
    local: bool,
    hash: Hash,
) -> Resolved {
    let mut issues = Vec::new();
    match raw.version {
        Some(1) => {}
        Some(v) => issues.push(cx.err(format!(
            "unsupported version {v}; this foldl reads version 1"
        ))),
        None => issues.push(cx.err("missing required key 'version'")),
    }
    let shared = raw.scope == Scope::Shared;
    let interactive = raw.interactive.unwrap_or(false);
    let (up, up_issues) = cx.script("up", raw.up.as_deref(), "up.sh");
    issues.extend(up_issues);
    let (down, down_issues) = cx.script("down", raw.down.as_deref(), "down.sh");
    issues.extend(down_issues);
    let named_up = raw.up.is_some() || up.is_some();
    let builtin = builtin(&cx, &raw, &mut issues);
    let checks: [(bool, &str); 7] = [
        (
            interactive && shared,
            "interactive = true is only allowed for scope = \"session\"",
        ),
        (
            raw.aliases.is_some() && shared,
            "[aliases] is only allowed for scope = \"session\"",
        ),
        (
            raw.lease.is_some() && !shared,
            "lease is only allowed for scope = \"shared\"",
        ),
        (
            raw.ports.is_some() && !shared,
            "[ports] is only allowed for scope = \"shared\"",
        ),
        (raw.builtin.is_some() && named_up, "builtin excludes up"),
        (
            raw.builtin.is_some() && interactive,
            "builtin excludes interactive",
        ),
        (
            shared && raw.down.is_none() && down.is_none(),
            "scope = \"shared\" requires a custom down (down.sh)",
        ),
    ];
    issues.extend(
        checks
            .iter()
            .filter(|(bad, _)| *bad)
            .map(|(_, m)| cx.err(*m)),
    );
    let does_something = named_up
        || raw.builtin.is_some()
        || raw.path_add.as_ref().is_some_and(|p| !p.is_empty())
        || raw.env.as_ref().is_some_and(|e| !e.is_empty())
        || raw.unset.as_ref().is_some_and(|u| !u.is_empty())
        || raw.aliases.as_ref().is_some_and(|a| !a.is_empty());
    if !does_something {
        issues.push(cx.err(
            "does nothing: set at least one of up, builtin, path_add, [env], unset, [aliases]",
        ));
    }
    let ports: Vec<String> = raw.ports.clone().unwrap_or_default().into_keys().collect();
    let bad_ports = raw
        .ports
        .iter()
        .flatten()
        .filter(|(n, v)| !is_identifier(n) || v.as_str() != "auto");
    issues.extend(bad_ports.map(|(n, _)| {
        cx.err(format!(
            "[ports] {n}: names must be identifiers and values \"auto\""
        ))
    }));
    let names = raw.env.iter().flatten().map(|(n, _)| ("[env]", n));
    let names = names.chain(raw.unset.iter().flatten().map(|n| ("unset", n)));
    issues.extend(
        names
            .filter(|(_, n)| !is_identifier(n))
            .map(|(k, n)| cx.err(format!("{k}: '{n}' is not a valid variable name"))),
    );
    let bad_aliases = raw
        .aliases
        .iter()
        .flatten()
        .filter(|(n, _)| !valid_alias(n));
    issues.extend(
        bad_aliases.map(|(n, _)| cx.err(format!("[aliases]: '{n}' is not a valid alias name"))),
    );
    let absolute = raw.path_add.iter().flatten().filter(|p| p.starts_with('/'));
    issues.extend(
        absolute.map(|p| cx.err(format!("path_add: '{p}' must be relative to FOLDL_ROOT"))),
    );
    let known: BTreeSet<&str> = ["FOLDL_ROOT", "FOLDL_STATE", "HOME"]
        .into_iter()
        .chain(ports.iter().map(String::as_str))
        .collect();
    let env: BTreeMap<String, Vec<Piece>> = raw
        .env
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|(n, v)| {
            let pieces = interpolate(&v);
            let unknown = pieces.iter().filter_map(|p| match p {
                Piece::Var(var) if !known.contains(var.as_str()) => Some(var.clone()),
                _ => None,
            });
            issues.extend(unknown.map(|var| {
                cx.err(format!("[env] {n}: ${{{var}}} can't be expanded; only FOLDL_ROOT, FOLDL_STATE, HOME and [ports] names can"))
            }));
            (n, pieces)
        })
        .collect();
    let timeout = duration(&cx, "timeout", raw.timeout.as_deref(), &mut issues);
    let down_timeout = duration(
        &cx,
        "down_timeout",
        raw.down_timeout.as_deref(),
        &mut issues,
    );
    let watch = raw.watch.clone().unwrap_or_default();
    let bad_globs = watch
        .iter()
        .filter(|g| globset::Glob::new(g).is_err() || g.starts_with('/'));
    issues.extend(bad_globs.map(|g| cx.err(format!("watch: '{g}' is not a valid relative glob"))));
    let implied = builtin.iter().flat_map(Builtin::watched);
    let implied: Vec<String> = implied.filter(|g| !watch.contains(g)).collect();
    let watch = [watch, implied].concat();
    let lease = match raw.lease.as_deref() {
        _ if !shared => None,
        None | Some("folder") => Some(LeaseSpec::Folder),
        Some("repo") => Some(LeaseSpec::Repo),
        Some("") => {
            issues.push(cx.err("lease must not be empty"));
            None
        }
        Some(custom) => Some(LeaseSpec::Custom(custom.to_string())),
    };
    let warnings = side_effect_warnings(&cx, &raw, up.as_ref(), down.is_some());
    issues.extend(warnings);
    if matches!(builtin, Some(Builtin::NixFlake { .. })) && !cx.host.nix_available {
        issues.push(Issue::warning(
            cx.file,
            "builtin = \"nix-flake\" needs nix on PATH, which wasn't found",
        ));
    }
    if issues.iter().any(Issue::is_error) {
        return Err(issues);
    }
    let migration = Migration {
        name,
        number,
        local,
        dir: cx.dir.to_string(),
        scope: raw.scope,
        interactive,
        up: up.map(|(s, _)| s),
        down: down.map(|(s, _)| s),
        builtin,
        path_add: raw.path_add.unwrap_or_default(),
        env,
        unset: raw.unset.unwrap_or_default(),
        aliases: raw.aliases.unwrap_or_default(),
        lease,
        ports,
        timeout,
        down_timeout,
        watch,
        hash,
    };
    Ok((migration, issues))
}

fn valid_alias(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.:+@%".contains(c))
}

fn duration(cx: &Ctx, key: &str, text: Option<&str>, issues: &mut Vec<Issue>) -> Option<Duration> {
    let text = text?;
    let parsed = parse_duration(text);
    if parsed.is_none() {
        issues.push(cx.err(format!(
            "{key} = \"{text}\" is not a duration such as \"500ms\", \"30s\" or \"5m\""
        )));
    }
    parsed
}

fn builtin(cx: &Ctx, raw: &RawManifest, issues: &mut Vec<Issue>) -> Option<Builtin> {
    let options = raw.builtin_options.clone().unwrap_or_default();
    let Some(name) = raw.builtin else {
        if raw.builtin_options.is_some() {
            issues.push(cx.err("[builtin_options] needs builtin"));
        }
        return None;
    };
    let allowed: &[&str] = match name {
        BuiltinName::NixFlake => &["flake"],
        BuiltinName::Dotenv => &["file", "required"],
    };
    let unknown = options.keys().filter(|k| !allowed.contains(&k.as_str()));
    issues.extend(unknown.map(|k| cx.err(format!("[builtin_options]: unknown option '{k}'"))));
    let text = |key: &str, default: &str| match options.get(key) {
        None => Some(default.to_string()),
        Some(toml::Value::String(s)) => Some(s.clone()),
        Some(_) => None,
    };
    let flag = |key: &str, default: bool| match options.get(key) {
        None => Some(default),
        Some(toml::Value::Boolean(b)) => Some(*b),
        Some(_) => None,
    };
    let resolved = match name {
        BuiltinName::NixFlake => text("flake", ".").map(|flake| Builtin::NixFlake { flake }),
        BuiltinName::Dotenv => text("file", ".env")
            .zip(flag("required", true))
            .map(|(file, required)| Builtin::Dotenv { file, required }),
    };
    if resolved.is_none() {
        issues.push(cx.err(
            "[builtin_options]: wrong type (file and flake are strings, required is a boolean)",
        ));
    }
    resolved
}

fn side_effect_warnings(
    cx: &Ctx,
    raw: &RawManifest,
    up: Option<&(Script, String)>,
    has_down: bool,
) -> Vec<Issue> {
    let session = raw.scope == Scope::Session;
    let found = match up {
        Some((_, text)) if session && !has_down => scan::side_effects(text),
        _ => vec![],
    };
    found
        .into_iter()
        .map(|what| {
            Issue::warning(
                cx.file,
                format!("up runs '{what}', which has effects outside the shell, but there is no custom down"),
            )
        })
        .collect()
}

/// Splits an `[env]` value into text and `${NAME}` references. `$$` is a
/// literal `$`; any other `$` is literal too.
pub fn interpolate(value: &str) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut rest = value;
    while let Some(at) = rest.find('$') {
        text.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let braced = after
            .strip_prefix('{')
            .and_then(|inner| inner.split_once('}'));
        rest = match (after.strip_prefix('$'), braced) {
            (Some(tail), _) => {
                text.push('$');
                tail
            }
            (None, Some((var, tail))) => {
                pieces.extend((!text.is_empty()).then(|| Piece::Text(std::mem::take(&mut text))));
                pieces.push(Piece::Var(var.to_string()));
                tail
            }
            (None, None) => {
                text.push('$');
                after
            }
        };
    }
    text.push_str(rest);
    pieces.extend((!text.is_empty()).then_some(Piece::Text(text)));
    pieces
}

/// Expands pieces with the given variables; unknown names expand to
/// nothing (validation already rejected them).
pub fn expand(pieces: &[Piece], vars: &BTreeMap<String, String>) -> String {
    pieces
        .iter()
        .map(|p| match p {
            Piece::Text(t) => t.as_str(),
            Piece::Var(v) => vars.get(v).map_or("", String::as_str),
        })
        .collect()
}

#[cfg(test)]
mod tests;

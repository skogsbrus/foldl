// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! `manifest.toml` and `.foldl/foldl.toml`: the schema as data, parsed
//! from text. Unknown keys are errors.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Who owns a migration's effects.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    #[default]
    Session,
    Shared,
}

impl Scope {
    pub fn name(self) -> &'static str {
        match self {
            Scope::Session => "session",
            Scope::Shared => "shared",
        }
    }
}

/// The built-ins a manifest can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuiltinName {
    #[serde(rename = "nix-flake")]
    NixFlake,
    #[serde(rename = "dotenv")]
    Dotenv,
}

/// A manifest exactly as written. Defaults are resolved later, when the
/// files next to it are known.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawManifest {
    pub version: Option<i64>,
    #[serde(default)]
    pub scope: Scope,
    pub interactive: Option<bool>,
    pub up: Option<String>,
    pub down: Option<String>,
    pub builtin: Option<BuiltinName>,
    pub builtin_options: Option<toml::Table>,
    pub path_add: Option<Vec<String>>,
    pub env: Option<BTreeMap<String, String>>,
    pub unset: Option<Vec<String>>,
    pub aliases: Option<BTreeMap<String, String>>,
    pub lease: Option<String>,
    pub ports: Option<BTreeMap<String, String>>,
    pub timeout: Option<String>,
    pub down_timeout: Option<String>,
    pub watch: Option<Vec<String>>,
}

/// Parses manifest text; the error is the TOML parser's message.
pub fn parse_manifest(text: &str) -> Result<RawManifest, String> {
    toml::from_str(text).map_err(|e| e.message().to_string())
}

/// `.foldl/foldl.toml` as written.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawFolderConfig {
    pub version: Option<i64>,
    pub trust: Option<RawTrust>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawTrust {
    pub reuse: Option<Reuse>,
}

/// Whether trust carries over to other folders with the same content.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reuse {
    #[default]
    None,
    Repo,
}

pub fn parse_folder_config(text: &str) -> Result<RawFolderConfig, String> {
    toml::from_str(text).map_err(|e| e.message().to_string())
}

/// Parses `"500ms"`, `"30s"`, `"5m"` or `"1h"`.
pub fn parse_duration(text: &str) -> Option<Duration> {
    let split = text.find(|c: char| !c.is_ascii_digit())?;
    let (digits, unit) = text.split_at(split);
    let n: u64 = digits.parse().ok()?;
    match unit {
        "ms" => Some(Duration::from_millis(n)),
        "s" => Some(Duration::from_secs(n)),
        "m" => Some(Duration::from_secs(n * 60)),
        "h" => Some(Duration::from_secs(n * 3600)),
        _ => None,
    }
}

/// Formats a duration the way manifests write it, for messages.
pub fn format_duration(d: Duration) -> String {
    let ms = d.as_millis();
    match ms {
        _ if ms.is_multiple_of(3_600_000) && ms > 0 => format!("{}h", ms / 3_600_000),
        _ if ms.is_multiple_of(60_000) && ms > 0 => format!("{}m", ms / 60_000),
        _ if ms.is_multiple_of(1000) => format!("{}s", ms / 1000),
        _ => format!("{ms}ms"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration("5"), None);
        assert_eq!(parse_duration("s"), None);
        assert_eq!(parse_duration("5d"), None);
        assert_eq!(parse_duration("99999999999999999999s"), None);
        assert_eq!(format_duration(Duration::from_secs(7200)), "2h");
        assert_eq!(format_duration(Duration::from_secs(600)), "10m");
        assert_eq!(format_duration(Duration::from_secs(30)), "30s");
        assert_eq!(format_duration(Duration::from_millis(1500)), "1500ms");
        assert_eq!(format_duration(Duration::ZERO), "0s");
    }

    #[test]
    fn parses_full_manifest() {
        let m = parse_manifest(
            r#"
            version = 1
            scope = "shared"
            builtin = "nix-flake"
            lease = "repo"
            [ports]
            PGPORT = "auto"
            "#,
        )
        .unwrap();
        assert_eq!(m.scope, Scope::Shared);
        assert_eq!(m.builtin, Some(BuiltinName::NixFlake));
        assert_eq!(Scope::Shared.name(), "shared");
        assert_eq!(Scope::Session.name(), "session");
    }

    #[test]
    fn unknown_keys_are_errors() {
        let err = parse_manifest("version = 1\nshell = \"bash\"").unwrap_err();
        assert!(err.contains("unknown field"), "{err}");
        assert!(parse_folder_config("version = 1\n[trust]\nreuse = \"all\"").is_err());
        let ok = parse_folder_config("version = 1\n[trust]\nreuse = \"repo\"").unwrap();
        assert_eq!(ok.trust.unwrap().reuse, Some(Reuse::Repo));
    }
}

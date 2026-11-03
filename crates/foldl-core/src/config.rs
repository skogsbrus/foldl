// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! `~/.config/foldl/config.toml`: per-user settings.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::manifest::parse_duration;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verbosity {
    Quiet,
    #[default]
    Normal,
    Verbose,
}

/// What a non-interactive run does with a dirty lease.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnDirty {
    Retry,
    Skip,
    #[default]
    Abort,
}

/// Resolved user settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub verbosity: Verbosity,
    pub timeout: Duration,
    pub nix_timeout: Duration,
    pub down_timeout: Duration,
    pub on_dirty: OnDirty,
    /// Folders under these prefixes are trusted without `foldl allow`.
    pub trusted_prefixes: Vec<PathBuf>,
    /// Inclusive range `[ports] = "auto"` allocates from.
    pub port_range: (u16, u16),
    /// Whether `.foldl/` may contain symlinks. They point outside what the
    /// repository controls, so a folder holding one is invalid by default.
    pub follow_symlinks: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            verbosity: Verbosity::Normal,
            timeout: Duration::from_secs(60),
            nix_timeout: Duration::from_secs(600),
            down_timeout: Duration::from_secs(30),
            on_dirty: OnDirty::Abort,
            trusted_prefixes: vec![],
            port_range: (20000, 29999),
            follow_symlinks: false,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    version: Option<i64>,
    verbosity: Option<Verbosity>,
    timeout: Option<String>,
    nix_timeout: Option<String>,
    down_timeout: Option<String>,
    on_dirty: Option<OnDirty>,
    trusted_prefixes: Option<Vec<PathBuf>>,
    port_range: Option<[u16; 2]>,
    follow_symlinks: Option<bool>,
}

/// Parses config text. `home` expands a leading `~/` in trusted prefixes.
pub fn parse_config(text: &str, home: &str) -> Result<Config, String> {
    let raw: RawConfig = toml::from_str(text).map_err(|e| e.message().to_string())?;
    let defaults = Config::default();
    let duration = |key: &str, value: Option<String>, default: Duration| match value {
        None => Ok(default),
        Some(text) => parse_duration(&text).ok_or(format!("{key} = \"{text}\" is not a duration")),
    };
    match raw.version {
        None | Some(1) => {}
        Some(v) => {
            return Err(format!(
                "unsupported version {v}; this foldl reads version 1"
            ));
        }
    }
    let range = raw
        .port_range
        .map_or(defaults.port_range, |[lo, hi]| (lo, hi));
    if range.0 == 0 || range.0 > range.1 {
        return Err("port_range must be [low, high] with 0 < low <= high".into());
    }
    let expand = |p: PathBuf| match p.strip_prefix("~") {
        Ok(rest) => PathBuf::from(home).join(rest),
        Err(_) => p,
    };
    Ok(Config {
        verbosity: raw.verbosity.unwrap_or(defaults.verbosity),
        timeout: duration("timeout", raw.timeout, defaults.timeout)?,
        nix_timeout: duration("nix_timeout", raw.nix_timeout, defaults.nix_timeout)?,
        down_timeout: duration("down_timeout", raw.down_timeout, defaults.down_timeout)?,
        on_dirty: raw.on_dirty.unwrap_or(defaults.on_dirty),
        trusted_prefixes: raw
            .trusted_prefixes
            .unwrap_or_default()
            .into_iter()
            .map(expand)
            .collect(),
        port_range: range,
        follow_symlinks: raw.follow_symlinks.unwrap_or(defaults.follow_symlinks),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        assert_eq!(parse_config("", "/h").unwrap(), Config::default());
    }

    #[test]
    fn full() {
        let c = parse_config(
            r#"
            version = 1
            verbosity = "quiet"
            timeout = "2m"
            nix_timeout = "20m"
            down_timeout = "5s"
            on_dirty = "retry"
            trusted_prefixes = ["~/work", "/srv"]
            port_range = [4000, 4010]
            follow_symlinks = true
            "#,
            "/h",
        )
        .unwrap();
        assert_eq!(c.verbosity, Verbosity::Quiet);
        assert_eq!(c.timeout, Duration::from_secs(120));
        assert_eq!(c.nix_timeout, Duration::from_secs(1200));
        assert_eq!(c.down_timeout, Duration::from_secs(5));
        assert_eq!(c.on_dirty, OnDirty::Retry);
        assert_eq!(
            c.trusted_prefixes,
            vec![PathBuf::from("/h/work"), PathBuf::from("/srv")]
        );
        assert_eq!(c.port_range, (4000, 4010));
        assert!(c.follow_symlinks);
    }

    #[test]
    fn errors() {
        assert!(
            parse_config("colour = 1", "/h")
                .unwrap_err()
                .contains("unknown field")
        );
        assert!(
            parse_config("version = 2", "/h")
                .unwrap_err()
                .contains("version 2")
        );
        assert!(
            parse_config("timeout = \"x\"", "/h")
                .unwrap_err()
                .contains("timeout")
        );
        assert!(
            parse_config("port_range = [10, 5]", "/h")
                .unwrap_err()
                .contains("port_range")
        );
        assert!(
            parse_config("port_range = [0, 5]", "/h")
                .unwrap_err()
                .contains("port_range")
        );
    }
}

// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! The process's surroundings, read once at startup and passed down:
//! environment, working directory and executable.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use foldl_core::env::{Env, Value};
use foldl_core::state::Paths;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sys {
    pub vars: BTreeMap<OsString, OsString>,
    pub cwd: PathBuf,
    pub exe: PathBuf,
}

impl Sys {
    pub fn var(&self, name: &str) -> Option<String> {
        self.vars
            .get(&OsString::from(name))
            .map(|v| v.to_string_lossy().into_owned())
    }

    /// Environment variables with UTF-8 names, as core values.
    pub fn env(&self) -> Env {
        self.vars
            .iter()
            .filter_map(|(k, v)| {
                Some((
                    k.to_str()?.to_string(),
                    Value(v.as_encoded_bytes().to_vec()),
                ))
            })
            .collect()
    }

    /// XDG base directories, defaulting under `$HOME`.
    pub fn paths(&self) -> Paths {
        let home = PathBuf::from(self.var("HOME").unwrap_or_else(|| "/".into()));
        let base = |var: &str, default: &str| {
            self.var(var)
                .filter(|v| v.starts_with('/'))
                .map_or_else(|| home.join(default), PathBuf::from)
        };
        Paths {
            state: base("XDG_STATE_HOME", ".local/state").join("foldl"),
            data: base("XDG_DATA_HOME", ".local/share").join("foldl"),
            cache: base("XDG_CACHE_HOME", ".cache").join("foldl"),
            config: base("XDG_CONFIG_HOME", ".config").join("foldl"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sys(pairs: &[(&str, &str)]) -> Sys {
        Sys {
            vars: pairs
                .iter()
                .map(|(k, v)| (OsString::from(k), OsString::from(v)))
                .collect(),
            cwd: "/".into(),
            exe: "/foldl".into(),
        }
    }

    #[test]
    fn xdg_paths() {
        let p = sys(&[
            ("HOME", "/h"),
            ("XDG_STATE_HOME", "/s"),
            ("XDG_CACHE_HOME", "relative"),
        ])
        .paths();
        assert_eq!(p.state, PathBuf::from("/s/foldl"));
        assert_eq!(p.cache, PathBuf::from("/h/.cache/foldl"));
        assert_eq!(p.data, PathBuf::from("/h/.local/share/foldl"));
        assert_eq!(p.config, PathBuf::from("/h/.config/foldl"));
        assert_eq!(sys(&[]).paths().state, PathBuf::from("/.local/state/foldl"));
    }

    #[test]
    fn env_values() {
        let s = sys(&[("A", "1")]);
        assert_eq!(s.env()["A"], Value::from("1"));
        assert_eq!(s.var("A").as_deref(), Some("1"));
        assert_eq!(s.var("B"), None);
    }
}

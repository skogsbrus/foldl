// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Loading the user config, with `-q`/`-v` on top.

use foldl_core::config::{Config, Verbosity, parse_config};

use crate::sys::Sys;

/// The config file's settings; a broken file warns and falls back to the
/// defaults rather than breaking every prompt.
pub fn load(sys: &Sys, verbosity: Option<Verbosity>) -> Config {
    let path = sys.paths().config_file();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let config = parse_config(&text, &sys.var("HOME").unwrap_or_default()).unwrap_or_else(|e| {
        eprintln!("foldl ! {}: {e}; using defaults", path.display());
        Config::default()
    });
    Config {
        verbosity: verbosity.unwrap_or(config.verbosity),
        ..config
    }
}

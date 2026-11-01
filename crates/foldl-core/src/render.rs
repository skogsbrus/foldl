// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Rendering diffs as shell code: `render(shell, diff) -> String`.
//!
//! Law: evaluating the output in that shell and reading the values back
//! returns the diff's `after` values. Checked against real shells in the
//! `foldl` crate's tests.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::env::{EnvDiff, Value};

/// A shell foldl can hook into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shell {
    Bash,
    Zsh,
}

impl Shell {
    pub fn parse(name: &str) -> Option<Shell> {
        match name {
            "bash" => Some(Shell::Bash),
            "zsh" => Some(Shell::Zsh),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
        }
    }
}

/// Quotes bytes as one shell word, valid in bash (3.2 and later) and zsh.
///
/// Single quotes keep every byte except NUL literally, including newlines
/// and invalid UTF-8; a quote itself is written as `'\''`.
pub fn quote(bytes: &[u8]) -> String {
    let body = String::from_utf8_lossy(bytes).replace('\'', r"'\''");
    match std::str::from_utf8(bytes) {
        Ok(_) => format!("'{body}'"),
        Err(_) => quote_bytes(bytes),
    }
}

/// For invalid UTF-8, which a Rust `String` can't hold: `$'\xHH'` for every
/// byte outside printable ASCII.
fn quote_bytes(bytes: &[u8]) -> String {
    let escaped: String = bytes
        .iter()
        .map(|&b| match b {
            b'\'' | b'\\' => format!("\\{}", b as char),
            0x20..=0x7e => (b as char).to_string(),
            _ => format!("\\x{b:02x}"),
        })
        .collect();
    format!("$'{escaped}'")
}

/// Shell code applying the diff: `export` for set values, `unset` for
/// removed ones.
pub fn render(_shell: Shell, diff: &EnvDiff) -> String {
    diff.0
        .iter()
        .map(|(name, change)| match &change.after {
            Some(value) => format!("export {name}={}\n", quote(&value.0)),
            None => format!("unset {name}\n"),
        })
        .collect()
}

/// The diff's `after` values as a JSON object, `null` for unset.
pub fn render_json(diff: &EnvDiff) -> String {
    let map: BTreeMap<&String, Option<&Value>> = diff
        .0
        .iter()
        .map(|(name, c)| (name, c.after.as_ref()))
        .collect();
    serde_json::to_string(&map).expect("string keys always serialize")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{Change, Env};

    #[test]
    fn shell_names() {
        assert_eq!(Shell::parse("bash"), Some(Shell::Bash));
        assert_eq!(Shell::parse("zsh"), Some(Shell::Zsh));
        assert_eq!(Shell::parse("fish"), None);
        assert_eq!(Shell::Zsh.name(), "zsh");
        assert_eq!(Shell::Bash.name(), "bash");
    }

    #[test]
    fn quoting() {
        assert_eq!(quote(b"a b"), "'a b'");
        assert_eq!(quote(b"it's"), r"'it'\''s'");
        assert_eq!(quote(b"\n$x"), "'\n$x'");
        assert_eq!(quote(b"\xff'\\a\x01"), r"$'\xff\'\\a\x01'");
    }

    #[test]
    fn renders_exports_and_unsets() {
        let before: Env = [("A".to_string(), Value::from("1"))].into();
        let after: Env = [("B".to_string(), Value::from("x y"))].into();
        let diff = EnvDiff::between(&before, &after);
        assert_eq!(render(Shell::Bash, &diff), "unset A\nexport B='x y'\n");
        assert_eq!(render_json(&diff), r#"{"A":null,"B":"x y"}"#);
        let single = EnvDiff(
            [(
                "C".to_string(),
                Change {
                    before: None,
                    after: Some(Value(vec![0xfe])),
                },
            )]
            .into(),
        );
        assert_eq!(render_json(&single), r#"{"C":{"hex":"fe"}}"#);
    }
}

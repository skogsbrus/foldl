// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Environments and the diffs between them.
//!
//! An [`EnvDiff`] maps each variable to `(before, after)`. Derived downs are
//! [`EnvDiff::invert`], and loading a stack of migrations is a left fold of
//! [`EnvDiff::apply`].

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A variable's value. Environment values are bytes, not necessarily UTF-8.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Value(pub Vec<u8>);

impl Value {
    /// The value as text, replacing invalid UTF-8.
    pub fn lossy(&self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value(s.as_bytes().to_vec())
    }
}

/// Serialized as a JSON string when valid UTF-8, otherwise as
/// `{"hex": "..."}`, so state files stay readable in the common case.
impl Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match std::str::from_utf8(&self.0) {
            Ok(text) => s.serialize_str(text),
            Err(_) => Encoded::Hex { hex: hex(&self.0) }.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Encoded::deserialize(d)? {
            Encoded::Text(text) => Ok(Value(text.into_bytes())),
            Encoded::Hex { hex } => unhex(&hex)
                .map(Value)
                .ok_or_else(|| serde::de::Error::custom("invalid hex value")),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Encoded {
    Text(String),
    Hex { hex: String },
}

/// Lowercase hex encoding.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    let digits = text.as_bytes();
    let pairs = digits.chunks(2).filter(|pair| pair.len() == 2);
    let bytes: Vec<u8> = pairs
        .filter_map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect();
    (bytes.len() * 2 == digits.len()).then_some(bytes)
}

/// An environment: variable names to values.
pub type Env = BTreeMap<String, Value>;

/// Whether foldl tracks a variable: it must be a shell identifier, since
/// only those can be exported, and must not be one the shell itself
/// manages or foldl uses internally.
pub fn is_tracked(name: &str) -> bool {
    is_identifier(name) && !is_volatile(name)
}

/// `[A-Za-z_][A-Za-z0-9_]*`
pub fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let first = chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic());
    first && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

const VOLATILE: &[&str] = &[
    "_",
    "SHLVL",
    "PWD",
    "OLDPWD",
    "PS1",
    "PS2",
    "SHELLOPTS",
    "BASHOPTS",
    "COLUMNS",
    "LINES",
    "FOLDL_ROOT",
    "FOLDL_STATE",
    "FOLDL_SESSION",
];

fn is_volatile(name: &str) -> bool {
    VOLATILE.contains(&name) || name.starts_with("__FOLDL_") || name.starts_with("BASH_FUNC_")
}

/// Keeps only tracked variables.
pub fn tracked(env: Env) -> Env {
    env.into_iter().filter(|(k, _)| is_tracked(k)).collect()
}

/// One variable's change. `None` means unset. `before != after`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub before: Option<Value>,
    pub after: Option<Value>,
}

/// Each changed variable mapped to its `(before, after)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EnvDiff(pub BTreeMap<String, Change>);

impl EnvDiff {
    /// The changes that turn `before` into `after`.
    pub fn between(before: &Env, after: &Env) -> EnvDiff {
        let names = before.keys().chain(after.keys());
        EnvDiff(
            names
                .filter_map(|name| {
                    let change = Change {
                        before: before.get(name).cloned(),
                        after: after.get(name).cloned(),
                    };
                    (change.before != change.after).then(|| (name.clone(), change))
                })
                .collect(),
        )
    }

    /// Sets every variable to its `after`.
    pub fn apply(&self, env: Env) -> Env {
        self.0.iter().fold(env, |mut env, (name, change)| {
            match &change.after {
                Some(value) => env.insert(name.clone(), value.clone()),
                None => env.remove(name),
            };
            env
        })
    }

    /// Swaps `before` and `after`: the derived down.
    pub fn invert(&self) -> EnvDiff {
        EnvDiff(
            self.0
                .iter()
                .map(|(name, c)| {
                    let swapped = Change {
                        before: c.after.clone(),
                        after: c.before.clone(),
                    };
                    (name.clone(), swapped)
                })
                .collect(),
        )
    }

    /// `self` followed by `next`, as one diff.
    pub fn compose(&self, next: &EnvDiff) -> EnvDiff {
        let merged = next.0.iter().fold(self.0.clone(), |mut acc, (name, c)| {
            let before = acc
                .get(name)
                .map_or(c.before.clone(), |first| first.before.clone());
            acc.insert(
                name.clone(),
                Change {
                    before,
                    after: c.after.clone(),
                },
            );
            acc
        });
        EnvDiff(
            merged
                .into_iter()
                .filter(|(_, c)| c.before != c.after)
                .collect(),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Short summary for `-v` output: `+NEW ~CHANGED -REMOVED`.
    pub fn summary(&self) -> String {
        let marks = self.0.iter().map(|(name, c)| match (&c.before, &c.after) {
            (None, _) => format!("+{name}"),
            (_, None) => format!("-{name}"),
            _ => format!("~{name}"),
        });
        marks.collect::<Vec<_>>().join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), Value::from(*v)))
            .collect()
    }

    fn arb_env() -> impl Strategy<Value = Env> {
        let name = prop::sample::select(vec!["A", "B", "C", "PATH", "X_1"]);
        let value = prop::collection::vec(1u8..=255, 0..6).prop_map(Value);
        prop::collection::btree_map(name.prop_map(String::from), value, 0..5)
    }

    proptest! {
        #[test]
        fn apply_then_invert_restores(a in arb_env(), b in arb_env()) {
            let d = EnvDiff::between(&a, &b);
            let applied = d.apply(a.clone());
            prop_assert_eq!(&applied, &b);
            prop_assert_eq!(d.invert().apply(applied), a);
        }

        #[test]
        fn compose_chains(a in arb_env(), b in arb_env(), c in arb_env()) {
            let ab = EnvDiff::between(&a, &b);
            let bc = EnvDiff::between(&b, &c);
            prop_assert_eq!(ab.compose(&bc), EnvDiff::between(&a, &c));
        }

        #[test]
        fn invert_is_involution(a in arb_env(), b in arb_env()) {
            let d = EnvDiff::between(&a, &b);
            prop_assert_eq!(d.invert().invert(), d);
        }

        #[test]
        fn values_roundtrip_json(bytes in prop::collection::vec(any::<u8>(), 0..8)) {
            let v = Value(bytes);
            let json = serde_json::to_string(&v).unwrap();
            prop_assert_eq!(serde_json::from_str::<Value>(&json).unwrap(), v);
        }
    }

    #[test]
    fn identifiers() {
        assert!(is_identifier("_a1"));
        assert!(!is_identifier("1a"));
        assert!(!is_identifier("a-b"));
        assert!(!is_identifier(""));
        assert!(is_tracked("PATH"));
        assert!(!is_tracked("SHLVL"));
        assert!(!is_tracked("__FOLDL_X"));
        assert!(!is_tracked("BASH_FUNC_f%%"));
    }

    #[test]
    fn tracked_filters() {
        let e = env(&[("A", "1"), ("PWD", "/"), ("a-b", "x")]);
        assert_eq!(tracked(e), env(&[("A", "1")]));
    }

    #[test]
    fn summary_marks() {
        let d = EnvDiff::between(
            &env(&[("A", "1"), ("B", "1")]),
            &env(&[("B", "2"), ("C", "3")]),
        );
        assert_eq!(d.summary(), "-A ~B +C");
        assert_eq!(d.len(), 3);
        assert!(!d.is_empty());
    }

    #[test]
    fn hex_values() {
        let v = Value(vec![0xff, 0x00]);
        assert_eq!(serde_json::to_string(&v).unwrap(), r#"{"hex":"ff00"}"#);
        assert_eq!(v.lossy(), "\u{fffd}\0");
        assert!(serde_json::from_str::<Value>(r#"{"hex":"f"}"#).is_err());
        assert!(serde_json::from_str::<Value>(r#"{"hex":"zz"}"#).is_err());
    }
}

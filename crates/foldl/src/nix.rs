// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! The `nix-flake` built-in's Nix-specific parts: the content-keyed cache,
//! the `nix print-dev-env` command and reading its JSON. The runner does
//! the waiting; nothing else in foldl knows about Nix.
//!
//! Layout under `$XDG_CACHE_HOME/foldl/nix/`:
//!
//! ```text
//! <key>/env.json     print-dev-env output, keyed on the flake's content
//! <key>/profile      GC root for the shell (nix's --profile)
//! last/<folder key>  the key of the folder's last good evaluation
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use foldl_core::env::{Env, Value, hex};
use foldl_core::folder::Hash;
use sha2::{Digest, Sha256};

/// Where to learn about the built-in, for the missing-nix error.
pub const DOCS: &str = "https://github.com/skogsbrus/foldl/blob/main/docs/nix.md";

/// Variables print-dev-env reports that describe the build sandbox rather
/// than the dev shell.
const SKIPPED: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TERM",
    "TMPDIR",
    "TEMP",
    "TMP",
    "TEMPDIR",
    "NIX_BUILD_TOP",
    "NIX_BUILD_CORES",
    "NIX_LOG_FD",
    "NIX_STORE",
    "SSL_CERT_FILE",
    "NIX_SSL_CERT_FILE",
    "SHLVL",
    "PWD",
    "OLDPWD",
];

/// Prepended to the session's value instead of replacing it.
const PREPENDED: &[&str] = &["PATH", "XDG_DATA_DIRS"];

/// The flake reference to evaluate: a path relative to the folder, or a
/// reference such as `github:owner/repo` used as is.
pub fn flake_ref(root: &Path, flake: &str) -> String {
    match flake.contains(':') {
        true => flake.to_string(),
        false => root.join(flake).to_string_lossy().into_owned(),
    }
}

/// The cache key: the flake reference, `flake.nix`, `flake.lock` and the
/// watched files, so worktrees on the same commit share an entry.
pub fn cache_key(flake_ref: &str, watch: &Hash) -> String {
    let dir = Path::new(flake_ref);
    let remote = flake_ref.contains(':');
    let mut hasher = Sha256::new();
    hasher.update(if remote { flake_ref } else { "path" });
    for file in ["flake.nix", "flake.lock"] {
        hasher.update(std::fs::read(dir.join(file)).unwrap_or_default());
        hasher.update([0]);
    }
    hasher.update(watch.0.as_bytes());
    hex(&hasher.finalize())[..32].to_string()
}

#[derive(Debug)]
pub struct Cache {
    pub dir: PathBuf,
}

impl Cache {
    pub fn entry(&self, key: &str) -> PathBuf {
        self.dir.join(key)
    }

    pub fn last_good(&self, folder: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.join("last").join(folder)).ok()
    }

    pub fn remember(&self, folder: &str, key: &str) -> std::io::Result<()> {
        crate::fsx::write_atomic(&self.dir.join("last").join(folder), key.as_bytes())
    }
}

/// `nix print-dev-env --json --profile <entry>/profile <ref>`.
pub fn command(nix: &Path, flake_ref: &str, entry: &Path) -> Command {
    let mut cmd = Command::new(nix);
    cmd.args(["print-dev-env", "--json", "--profile"])
        .arg(entry.join("profile"))
        .arg(flake_ref);
    cmd
}

/// The environment the dev shell exports, applied on top of `env`.
pub fn apply(json: &[u8], env: &Env) -> Result<Env, String> {
    let parsed: serde_json::Value =
        serde_json::from_slice(json).map_err(|e| format!("bad print-dev-env output: {e}"))?;
    let variables = parsed["variables"].as_object().cloned().unwrap_or_default();
    let exported = variables.into_iter().filter_map(|(name, var)| {
        let value = var["value"]
            .as_str()
            .filter(|_| var["type"] == "exported")?
            .to_string();
        (!SKIPPED.contains(&name.as_str())).then_some((name, value))
    });
    Ok(exported.fold(env.clone(), |mut env, (name, value)| {
        let current = env
            .get(&name)
            .map(Value::lossy)
            .filter(|_| PREPENDED.contains(&name.as_str()));
        let joined = [Some(value), current]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(":");
        env.insert(name, Value::from(joined.as_str()));
        env
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_exported_variables() {
        let json = br#"{"variables": {
            "FOO": {"type": "exported", "value": "bar"},
            "PATH": {"type": "exported", "value": "/nix/bin"},
            "HOME": {"type": "exported", "value": "/homeless-shelter"},
            "local": {"type": "var", "value": "x"},
            "arr": {"type": "array", "value": ["a"]}
        }}"#;
        let env: Env = [
            ("PATH".to_string(), Value::from("/usr/bin")),
            ("HOME".to_string(), Value::from("/h")),
        ]
        .into();
        let got = apply(json, &env).unwrap();
        assert_eq!(got["FOO"], Value::from("bar"));
        assert_eq!(got["PATH"], Value::from("/nix/bin:/usr/bin"));
        assert_eq!(got["HOME"], Value::from("/h"));
        assert!(!got.contains_key("local") && !got.contains_key("arr"));
        assert_eq!(apply(br#"{}"#, &Env::new()).unwrap(), Env::new());
        assert!(
            apply(b"nope", &Env::new())
                .unwrap_err()
                .starts_with("bad print-dev-env output")
        );
        let fresh = apply(
            br#"{"variables":{"PATH":{"type":"exported","value":"/n"}}}"#,
            &Env::new(),
        )
        .unwrap();
        assert_eq!(fresh["PATH"], Value::from("/n"));
    }

    #[test]
    fn keys_follow_content() {
        let dir = tempfile::tempdir().unwrap();
        let reference = flake_ref(dir.path(), ".");
        assert_eq!(flake_ref(dir.path(), "github:o/r"), "github:o/r");
        let empty = cache_key(&reference, &Hash::default());
        assert_ne!(
            cache_key("github:o/r", &Hash::default()),
            cache_key("github:o/s", &Hash::default())
        );
        let other = tempfile::tempdir().unwrap();
        assert_eq!(
            empty,
            cache_key(&flake_ref(other.path(), "."), &Hash::default()),
            "same content, same key"
        );
        std::fs::write(dir.path().join("./flake.nix"), "{}").unwrap();
        let with_flake = cache_key(&reference, &Hash::default());
        assert_ne!(empty, with_flake);
        assert_ne!(with_flake, cache_key(&reference, &Hash("w".into())));
        let cache = Cache {
            dir: dir.path().join("nix"),
        };
        assert_eq!(cache.last_good("f"), None);
        cache.remember("f", "k").unwrap();
        assert_eq!(cache.last_good("f").as_deref(), Some("k"));
        assert_eq!(cache.entry("k"), dir.path().join("nix/k"));
        let cmd = command(Path::new("/bin/nix"), "ref", Path::new("/e"));
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(
            args,
            ["print-dev-env", "--json", "--profile", "/e/profile", "ref"]
        );
    }
}

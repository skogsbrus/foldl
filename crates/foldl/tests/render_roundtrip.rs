// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! The render law, against real shells: evaluating `render(shell, diff)`
//! and reading the values back gives the diff's `after` values.

mod common;

use std::collections::BTreeMap;
use std::process::Command;

use foldl_core::env::{Env, EnvDiff, Value};
use foldl_core::render::{Shell, render};
use proptest::prelude::*;

const NAMES: [&str; 4] = ["A", "B_2", "PATH_X", "Z"];

/// Values biased towards bytes that break quoting.
fn value() -> impl Strategy<Value = Value> {
    let special = prop::sample::select(vec![
        b'\n', b'\'', b'"', b'$', b'\\', b'`', b'!', 0x01, 0x7f, 0xff, 0xc3, b' ', b'{', b'%',
    ]);
    let byte = prop_oneof![special, 1u8..=255];
    prop::collection::vec(byte, 0..16).prop_map(Value)
}

fn env() -> impl Strategy<Value = Env> {
    prop::collection::btree_map(
        prop::sample::select(NAMES.to_vec()).prop_map(String::from),
        value(),
        0..4,
    )
}

/// Evaluates `code` in `program`, then prints each name as `s<value>\0`
/// when set or `u\0` when unset.
fn read_back(program: &std::path::Path, code: &str) -> BTreeMap<String, Option<Vec<u8>>> {
    let script = format!(
        "eval \"$FOLDL_CODE\"\nfor n in {}; do eval \"if [ \\\"\\${{$n+x}}\\\" = x ]; then printf 's%s\\0' \\\"\\$$n\\\"; else printf 'u\\0'; fi\"; done",
        NAMES.join(" ")
    );
    let out = Command::new(program)
        .arg("-c")
        .arg(script)
        .env("FOLDL_CODE", code)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let fields: Vec<&[u8]> = out.stdout.split(|&b| b == 0).collect();
    NAMES
        .iter()
        .zip(fields)
        .map(|(name, field)| {
            let value = field.strip_prefix(b"s").map(<[u8]>::to_vec);
            (name.to_string(), value)
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn render_roundtrips_in_every_shell(before in env(), after in env()) {
        let diff = EnvDiff::between(&before, &after);
        for shell in common::shells() {
            let kind = Shell::parse(shell.kind).unwrap();
            let start: String = before.iter().map(|(k, v)| format!("export {k}={}\n", foldl_core::render::quote(&v.0))).collect();
            let code = start + &render(kind, &diff);
            let got = read_back(&shell.program, &code);
            let want: BTreeMap<String, Option<Vec<u8>>> =
                NAMES.iter().map(|n| (n.to_string(), after.get(*n).map(|v| v.0.clone()))).collect();
            prop_assert_eq!(got, want, "in {}", shell.label);
        }
    }
}

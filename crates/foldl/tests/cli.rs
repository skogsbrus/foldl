// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

#[test]
fn prints_version() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_foldl"))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "foldl 0.1.0\n");
}

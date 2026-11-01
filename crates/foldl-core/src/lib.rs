// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Pure core of foldl.

/// The crate version, shared with the binary.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_set() {
        assert_eq!(super::version(), "0.1.0");
    }
}

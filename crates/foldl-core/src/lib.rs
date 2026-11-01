// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Pure core of foldl: a left fold over events.
//!
//! Nothing in this crate touches the filesystem, processes, locks or the
//! clock. The `foldl` crate's interpreter performs the effects decided here
//! and feeds the results back as events.

pub mod config;
pub mod dotenv;
pub mod env;
pub mod error;
pub mod folder;
pub mod interactive;
pub mod lease;
pub mod machine;
pub mod manifest;
pub mod plan;
pub mod render;
pub mod scan;
pub mod state;
#[cfg(test)]
pub(crate) mod testkit;

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

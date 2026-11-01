// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! foldl's imperative shell: the CLI and the interpreter that performs the
//! core's effects. All I/O lives in this crate.

pub mod cli;
pub mod disk;
pub mod fsx;
pub mod runner;
pub mod sys;
pub mod trust;
pub mod trust_cmd;

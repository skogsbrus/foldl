// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Aliases, functions and completions recorded around an interactive
//! migration, and their derived down.

use serde::{Deserialize, Serialize};

/// What sourcing an interactive migration changed in the shell.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractiveDiff {}

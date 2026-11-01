// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Leases: shared ownership of a migration's effects across sessions.

use serde::{Deserialize, Serialize};

/// A lease a loaded migration belongs to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LeaseRef {
    /// Directory name under `leases/`: a hash of the key.
    pub id: String,
    /// The key as shown to people: a path, a repository or a custom name.
    pub key: String,
}

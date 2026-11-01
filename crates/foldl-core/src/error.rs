// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Stable exit codes and the `--json` error object.

use serde::Serialize;

/// foldl's exit codes. Stable from the first release: `sysexits.h` where
/// one fits, foldl's own from 80.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    Ok,
    Usage,
    Unavailable,
    Internal,
    Dirty,
    Untrusted,
    Invalid,
    UpFailed,
    DownFailed,
    Timeout,
    Denied,
    Interrupted,
}

impl Code {
    pub const ALL: [Code; 12] = [
        Code::Ok,
        Code::Usage,
        Code::Unavailable,
        Code::Internal,
        Code::Dirty,
        Code::Untrusted,
        Code::Invalid,
        Code::UpFailed,
        Code::DownFailed,
        Code::Timeout,
        Code::Denied,
        Code::Interrupted,
    ];

    pub fn exit(self) -> i32 {
        match self {
            Code::Ok => 0,
            Code::Usage => 64,
            Code::Unavailable => 69,
            Code::Internal => 70,
            Code::Dirty => 75,
            Code::Untrusted => 77,
            Code::Invalid => 78,
            Code::UpFailed => 80,
            Code::DownFailed => 81,
            Code::Timeout => 82,
            Code::Denied => 83,
            Code::Interrupted => 130,
        }
    }

    /// The stable string used in JSON.
    pub fn name(self) -> &'static str {
        match self {
            Code::Ok => "ok",
            Code::Usage => "usage",
            Code::Unavailable => "unavailable",
            Code::Internal => "internal",
            Code::Dirty => "dirty",
            Code::Untrusted => "untrusted",
            Code::Invalid => "invalid",
            Code::UpFailed => "up_failed",
            Code::DownFailed => "down_failed",
            Code::Timeout => "timeout",
            Code::Denied => "denied",
            Code::Interrupted => "interrupted",
        }
    }

    /// The more serious of two codes, for runs that hit several problems.
    /// Anything beats `Ok`; otherwise the first one wins.
    pub fn worst(self, other: Code) -> Code {
        match self {
            Code::Ok => other,
            _ => self,
        }
    }
}

/// A failure with a message and, where there is one, the command that
/// fixes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub code: Code,
    pub message: String,
    pub fix: Option<String>,
}

impl Failure {
    pub fn new(code: Code, message: impl Into<String>) -> Failure {
        Failure {
            code,
            message: message.into(),
            fix: None,
        }
    }

    pub fn with_fix(self, fix: impl Into<String>) -> Failure {
        Failure {
            fix: Some(fix.into()),
            ..self
        }
    }

    /// `{"error": {"code": ..., "exit": ..., "message": ..., "fix": ...}}`
    pub fn json(&self) -> String {
        #[derive(Serialize)]
        struct Inner<'a> {
            code: &'a str,
            exit: i32,
            message: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            fix: Option<&'a str>,
        }
        #[derive(Serialize)]
        struct Outer<'a> {
            error: Inner<'a>,
        }
        let inner = Inner {
            code: self.code.name(),
            exit: self.code.exit(),
            message: &self.message,
            fix: self.fix.as_deref(),
        };
        serde_json::to_string(&Outer { error: inner }).expect("plain structs always serialize")
    }

    /// `foldl ✗ message` plus the fix on its own line.
    pub fn human(&self) -> String {
        match &self.fix {
            Some(fix) => format!("foldl ✗ {}\n  fix: {fix}\n", self.message),
            None => format!("foldl ✗ {}\n", self.message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable() {
        let table: Vec<(i32, &str)> = Code::ALL.iter().map(|c| (c.exit(), c.name())).collect();
        assert_eq!(
            table,
            [
                (0, "ok"),
                (64, "usage"),
                (69, "unavailable"),
                (70, "internal"),
                (75, "dirty"),
                (77, "untrusted"),
                (78, "invalid"),
                (80, "up_failed"),
                (81, "down_failed"),
                (82, "timeout"),
                (83, "denied"),
                (130, "interrupted"),
            ]
        );
        let serialized: Vec<String> = Code::ALL
            .iter()
            .map(|c| serde_json::to_string(c).unwrap())
            .collect();
        let names: Vec<String> = Code::ALL
            .iter()
            .map(|c| format!("\"{}\"", c.name()))
            .collect();
        assert_eq!(serialized, names);
    }

    #[test]
    fn worst_keeps_first_failure() {
        assert_eq!(Code::Ok.worst(Code::Timeout), Code::Timeout);
        assert_eq!(Code::UpFailed.worst(Code::Timeout), Code::UpFailed);
    }

    #[test]
    fn json_and_human() {
        let f = Failure::new(Code::Untrusted, "~/code/app is not allowed")
            .with_fix("foldl allow ~/code/app");
        assert_eq!(
            f.json(),
            r#"{"error":{"code":"untrusted","exit":77,"message":"~/code/app is not allowed","fix":"foldl allow ~/code/app"}}"#
        );
        assert_eq!(
            f.human(),
            "foldl ✗ ~/code/app is not allowed\n  fix: foldl allow ~/code/app\n"
        );
        let plain = Failure::new(Code::Internal, "bug");
        assert_eq!(
            plain.json(),
            r#"{"error":{"code":"internal","exit":70,"message":"bug"}}"#
        );
        assert_eq!(plain.human(), "foldl ✗ bug\n");
    }
}

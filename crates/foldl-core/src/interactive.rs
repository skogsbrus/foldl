// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Aliases, functions and completions recorded around an interactive
//! migration, and their derived down.
//!
//! A snapshot is three sections separated by [`SEPARATOR`] lines: the
//! shell's own listing of aliases (`alias -p` / `alias -L`), functions
//! (`declare -f` / `typeset -f`) and completions (`complete -p` in bash,
//! `compdef <function> <name>` lines from `$_comps` in zsh). Each
//! definition is kept as the text the shell printed, which is also the code
//! that restores it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::render::Shell;

/// Between the sections of a snapshot.
pub const SEPARATOR: &str = "__foldl_section__";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Alias,
    Function,
    Completion,
}

/// Definitions by kind and name, as the shell printed them.
pub type Defs = BTreeMap<(Kind, String), String>;

/// Shell code that prints a snapshot to the file named by `$1`, privately.
pub fn snapshot_function(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash => {
            "__foldl_snap() { ( umask 077; { alias -p; echo __foldl_section__; declare -f; echo __foldl_section__; complete -p 2>/dev/null; } > \"$1\" ); }\n"
        }
        Shell::Zsh => {
            "__foldl_snap() { ( umask 077; { alias -L; print __foldl_section__; typeset -f; print __foldl_section__; (( ${+_comps} )) && for __k __v in ${(kv)_comps}; do print -r -- \"compdef $__v $__k\"; done; } > \"$1\" ); }\n"
        }
    }
}

/// Parses a snapshot.
pub fn parse(text: &str) -> Defs {
    let mut sections = [String::new(), String::new(), String::new()];
    let mut at = 0;
    for line in text.lines() {
        match (line == SEPARATOR, at) {
            (true, _) => at = (at + 1).min(2),
            (false, n) => {
                sections[n].push_str(line);
                sections[n].push('\n');
            }
        }
    }
    let [aliases, functions, completions] = &sections;
    let alias_defs = chunks(aliases, |line| line.starts_with("alias "))
        .into_iter()
        .filter_map(|text| {
            let name = text
                .strip_prefix("alias ")?
                .split('=')
                .next()?
                .trim_start_matches("-- ")
                .to_string();
            Some(((Kind::Alias, name), text))
        });
    let function_defs = chunks(functions, function_header)
        .into_iter()
        .filter_map(|text| {
            let name = function_header_name(text.lines().next()?)?;
            Some(((Kind::Function, name), text))
        });
    let completion_defs = completions
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let name = line.split_whitespace().last()?.to_string();
            Some(((Kind::Completion, name), format!("{line}\n")))
        });
    alias_defs
        .chain(function_defs)
        .chain(completion_defs)
        .collect()
}

/// Splits `text` into pieces, each starting at a line where `starts` holds.
fn chunks(text: &str, starts: impl Fn(&str) -> bool) -> Vec<String> {
    text.lines()
        .fold(Vec::new(), |mut pieces: Vec<String>, line| {
            match (starts(line), pieces.last_mut()) {
                (false, Some(last)) => {
                    last.push_str(line);
                    last.push('\n');
                }
                _ => pieces.push(format!("{line}\n")),
            }
            pieces
        })
}

/// `name () ` (bash) or `name () {` (zsh), at column 0.
fn function_header(line: &str) -> bool {
    function_header_name(line).is_some()
}

fn function_header_name(line: &str) -> Option<String> {
    let rest = line.trim_end().trim_end_matches('{').trim_end();
    let name = rest.strip_suffix(" ()")?;
    let valid = !name.is_empty() && !name.starts_with([' ', '\t']) && !name.contains(' ');
    valid.then(|| name.to_string())
}

/// One definition that changed: `None` means it didn't exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefChange {
    pub kind: Kind,
    pub name: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// What sourcing an interactive migration changed in the shell.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractiveDiff {
    pub shell: Option<Shell>,
    pub changes: Vec<DefChange>,
}

impl InteractiveDiff {
    pub fn between(shell: Shell, before: &Defs, after: &Defs) -> InteractiveDiff {
        let keys: std::collections::BTreeSet<&(Kind, String)> =
            before.keys().chain(after.keys()).collect();
        let changes = keys
            .into_iter()
            .filter(|key| before.get(*key) != after.get(*key))
            .map(|(kind, name)| DefChange {
                kind: *kind,
                name: name.clone(),
                before: before.get(&(*kind, name.clone())).cloned(),
                after: after.get(&(*kind, name.clone())).cloned(),
            })
            .collect();
        InteractiveDiff {
            shell: Some(shell),
            changes,
        }
    }

    /// Shell code removing what was added and restoring what was replaced.
    pub fn undo(&self) -> String {
        let zsh = self.shell == Some(Shell::Zsh);
        self.changes
            .iter()
            .map(|c| match (&c.before, c.kind, zsh) {
                (Some(text), _, _) => text.clone(),
                (None, Kind::Alias, _) => format!("unalias -- {} 2>/dev/null\n", c.name),
                (None, Kind::Function, _) => format!("unset -f {} 2>/dev/null\n", c.name),
                (None, Kind::Completion, false) => format!("complete -r {} 2>/dev/null\n", c.name),
                (None, Kind::Completion, true) => format!("compdef -d {} 2>/dev/null\n", c.name),
            })
            .collect()
    }

    /// `+2 functions, 1 alias` for the summary.
    pub fn summary(&self) -> Option<String> {
        let count = |kind: Kind, one: &str, many: &str| {
            let n = self.changes.iter().filter(|c| c.kind == kind).count();
            match n {
                0 => None,
                1 => Some(format!("1 {one}")),
                n => Some(format!("{n} {many}")),
            }
        };
        let parts: Vec<String> = [
            count(Kind::Alias, "alias", "aliases"),
            count(Kind::Function, "function", "functions"),
            count(Kind::Completion, "completion", "completions"),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASH: &str = "alias ll='ls -l'\nalias q='it'\\''s\nmore'\n__foldl_section__\nf () \n{ \n    echo hi\n}\ng () \n{ \n    local x=1\n}\n__foldl_section__\ncomplete -F _f ff\n\n";
    const ZSH: &str = "alias ll='ls -l'\n__foldl_section__\nf () {\n\techo hi\n}\n__foldl_section__\ncompdef _git git\n";

    fn key(kind: Kind, name: &str) -> (Kind, String) {
        (kind, name.to_string())
    }

    #[test]
    fn parses_bash_and_zsh_snapshots() {
        let bash = parse(BASH);
        assert_eq!(bash[&key(Kind::Alias, "q")], "alias q='it'\\''s\nmore'\n");
        assert_eq!(
            bash[&key(Kind::Function, "g")],
            "g () \n{ \n    local x=1\n}\n"
        );
        assert_eq!(bash[&key(Kind::Completion, "ff")], "complete -F _f ff\n");
        assert_eq!(bash.len(), 5);
        let zsh = parse(ZSH);
        assert_eq!(zsh[&key(Kind::Function, "f")], "f () {\n\techo hi\n}\n");
        assert_eq!(zsh[&key(Kind::Completion, "git")], "compdef _git git\n");
        assert!(parse("").is_empty());
        assert!(parse("junk\n__foldl_section__\n  indented () \n").is_empty());
        let nested = parse(
            "__foldl_section__\nsnap () {\n\tprint __foldl_section__\n}\n__foldl_section__\n__foldl_section__\nextra\n",
        );
        assert_eq!(
            nested[&key(Kind::Function, "snap")],
            "snap () {\n\tprint __foldl_section__\n}\n"
        );
        assert_eq!(function_header_name("a b ()"), None);
        assert_eq!(function_header_name(" ()"), None);
    }

    #[test]
    fn undo_removes_and_restores() {
        let before =
            parse("alias ll='ls'\n__foldl_section__\nkeep () \n{ \n    :\n}\n__foldl_section__\n");
        let after = parse(
            "alias ll='ls -l'\nalias t='just test'\n__foldl_section__\nkeep () \n{ \n    :\n}\nnew () \n{ \n    :\n}\n__foldl_section__\ncomplete -F _t t\n",
        );
        let diff = InteractiveDiff::between(Shell::Bash, &before, &after);
        assert_eq!(
            diff.undo(),
            "alias ll='ls'\nunalias -- t 2>/dev/null\nunset -f new 2>/dev/null\ncomplete -r t 2>/dev/null\n"
        );
        assert_eq!(
            diff.summary().as_deref(),
            Some("2 aliases, 1 function, 1 completion")
        );
        let zsh = InteractiveDiff::between(
            Shell::Zsh,
            &Defs::new(),
            &parse("__foldl_section__\n__foldl_section__\ncompdef _t t\n"),
        );
        assert_eq!(zsh.undo(), "compdef -d t 2>/dev/null\n");
        assert_eq!(InteractiveDiff::default().summary(), None);
        assert!(snapshot_function(Shell::Bash).contains("declare -f"));
        assert!(snapshot_function(Shell::Zsh).contains("typeset -f"));
    }
}

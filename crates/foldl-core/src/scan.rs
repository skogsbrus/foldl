// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Heuristic scans of shell scripts, shared by `foldl check` and the
//! direnv importer. They only produce warnings and prompts, never errors.

/// Commands with effects outside the shell, which a derived down can't
/// undo: `(pattern, suggested stop command)`.
pub const SIDE_EFFECTS: &[(&str, &str)] = &[
    ("docker compose", "docker compose down"),
    ("docker-compose", "docker-compose down"),
    ("docker run", "docker stop <name>"),
    ("docker start", "docker stop <name>"),
    ("podman", "podman stop <name>"),
    ("kubectl config use-context", "use KUBECONFIG instead"),
    (
        "gcloud config set",
        "use CLOUDSDK_ACTIVE_CONFIG_NAME instead",
    ),
    ("ssh-agent", "ssh-agent -k"),
    ("ssh -f", "kill the ssh process"),
    ("sshuttle", "kill the sshuttle process"),
    ("pg_ctl", "pg_ctl stop"),
    ("systemctl", "systemctl stop <unit>"),
    ("nohup", "kill the process"),
    ("aws sso login", "aws sso logout"),
];

/// Lines of a script that aren't comments or blank.
fn code_lines(script: &str) -> impl Iterator<Item = &str> {
    script
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
}

/// Whether one line starts something outside the shell. Returns the
/// matched pattern and the suggested stop command.
pub fn side_effect(line: &str) -> Option<(&'static str, &'static str)> {
    let line = line.trim();
    let background = line.ends_with('&') && !line.ends_with("&&");
    let found = SIDE_EFFECTS
        .iter()
        .find(|(pattern, _)| contains_command(line, pattern));
    match (found, background) {
        (Some((pattern, stop)), _) => Some((pattern, stop)),
        (None, true) => Some(("&", "kill the background process")),
        (None, false) => None,
    }
}

/// `pattern` appears as whole words.
fn contains_command(line: &str, pattern: &str) -> bool {
    line.match_indices(pattern).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + pattern.len()..].chars().next();
        let boundary = |c: Option<char>| {
            c.is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        };
        boundary(before) && boundary(after)
    })
}

/// Distinct side-effect patterns found in a script.
pub fn side_effects(script: &str) -> Vec<&'static str> {
    let mut found: Vec<&'static str> = code_lines(script)
        .filter_map(side_effect)
        .map(|(p, _)| p)
        .collect();
    found.dedup();
    found
}

/// Paths a script sources from outside its own directory.
pub fn outside_references(script: &str) -> Vec<String> {
    code_lines(script)
        .filter_map(|line| {
            let rest = line
                .strip_prefix("source ")
                .or_else(|| line.strip_prefix(". "))?;
            let target = rest
                .split_whitespace()
                .next()?
                .trim_matches(|c| c == '"' || c == '\'');
            let outside = ["/", "..", "~", "$"].iter().any(|p| target.starts_with(p));
            outside.then(|| target.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_side_effects() {
        assert_eq!(
            side_effect("docker compose up -d"),
            Some(("docker compose", "docker compose down"))
        );
        assert_eq!(
            side_effect("npm run dev &"),
            Some(("&", "kill the background process"))
        );
        assert_eq!(side_effect("make && make test"), None);
        assert_eq!(side_effect("echo my-docker-composer"), None);
        assert_eq!(
            side_effects("# docker run\nssh-agent\nssh-agent -s\n"),
            vec!["ssh-agent"]
        );
    }

    #[test]
    fn finds_outside_references() {
        let script = "source ./lib.sh\n. \"$HOME/x.sh\"\nsource ../up.sh\nsource\n";
        assert_eq!(outside_references(script), vec!["$HOME/x.sh", "../up.sh"]);
    }
}

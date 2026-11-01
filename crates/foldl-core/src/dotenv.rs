// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

//! Parsing `.env` files for the `dotenv` built-in.
//!
//! Supported: `NAME=value`, an optional `export ` prefix, `#` comments
//! (whole lines, or after whitespace in unquoted values), single quotes
//! (literal), double quotes (`\n`, `\t`, `\"`, `\\`, `\$` escapes; may span
//! lines), and `$NAME` / `${NAME}` expansion in unquoted and double-quoted
//! values, from the environment and earlier lines.

use crate::env::{Env, Value};

/// A cursor over the file, counting lines for error messages.
struct Scanner {
    chars: Vec<char>,
    at: usize,
    line: usize,
}

impl Scanner {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.at += 1;
        self.line += usize::from(c == '\n');
        Some(c)
    }

    fn eat_while(&mut self, keep: impl Fn(char) -> bool) -> String {
        let mut taken = String::new();
        while let Some(c) = self.peek().filter(|c| keep(*c)) {
            taken.push(c);
            self.bump();
        }
        taken
    }

    fn error(&self, what: &str) -> String {
        format!("line {}: {what}", self.line)
    }

    /// `$NAME` or `${NAME}`, after the `$`; unknown names are empty.
    fn reference(&mut self, env: &Env) -> String {
        let braced = self.peek() == Some('{');
        if braced {
            self.bump();
        }
        let name = self.eat_while(|c| c == '_' || c.is_ascii_alphanumeric());
        if braced && self.peek() == Some('}') {
            self.bump();
        }
        match name.is_empty() {
            true => "$".to_string(),
            false => env.get(&name).map(Value::lossy).unwrap_or_default(),
        }
    }

    fn single_quoted(&mut self) -> Result<String, String> {
        let value = self.eat_while(|c| c != '\'');
        self.bump()
            .map(|_| value)
            .ok_or(self.error("unterminated single quote"))
    }

    fn double_quoted(&mut self, env: &Env) -> Result<String, String> {
        let mut value = String::new();
        loop {
            match self.bump() {
                None => return Err(self.error("unterminated double quote")),
                Some('"') => return Ok(value),
                Some('$') => value.push_str(&self.reference(env)),
                Some('\\') => {
                    let escaped = self.bump().unwrap_or('\\');
                    value.push(match escaped {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                }
                Some(c) => value.push(c),
            }
        }
    }

    fn unquoted(&mut self, env: &Env) -> String {
        let mut value = String::new();
        while let Some(c) = self.peek().filter(|c| *c != '\n') {
            self.bump();
            match c {
                '#' if value.is_empty() || value.ends_with([' ', '\t']) => {
                    self.eat_while(|c| c != '\n');
                }
                '$' => value.push_str(&self.reference(env)),
                c => value.push(c),
            }
        }
        value.trim_end().to_string()
    }
}

/// The environment after a `.env` file's assignments.
pub fn parse(text: &str, env: &Env) -> Result<Env, String> {
    let mut s = Scanner {
        chars: text.chars().collect(),
        at: 0,
        line: 1,
    };
    let mut env = env.clone();
    loop {
        s.eat_while(char::is_whitespace);
        match s.peek() {
            None => return Ok(env),
            Some('#') => {
                s.eat_while(|c| c != '\n');
                continue;
            }
            Some(_) => {}
        }
        let mut name = s.eat_while(|c| c == '_' || c.is_ascii_alphanumeric());
        if name == "export" && s.peek().is_some_and(|c| c == ' ' || c == '\t') {
            s.eat_while(|c| c == ' ' || c == '\t');
            name = s.eat_while(|c| c == '_' || c.is_ascii_alphanumeric());
        }
        s.eat_while(|c| c == ' ' || c == '\t');
        if !crate::env::is_identifier(&name) || s.bump() != Some('=') {
            return Err(s.error("expected NAME=value"));
        }
        s.eat_while(|c| c == ' ' || c == '\t');
        let value = match s.peek() {
            Some('\'') => {
                s.bump();
                s.single_quoted()?
            }
            Some('"') => {
                s.bump();
                s.double_quoted(&env)?
            }
            _ => s.unquoted(&env),
        };
        env.insert(name, Value::from(value.as_str()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), Value::from(*v)))
            .collect()
    }

    #[test]
    fn parses_common_forms() {
        let text = "# comment\n\nA=1\nexport B = two words # note\nC='lit $A # x'\nD=\"q $A\\n\\\"${B}\\\" \\$x\\\\\"\nE=x#y\nF=\nG=\"multi\nline\"\nH=$MISSING-$\n";
        let got = parse(text, &env(&[("KEEP", "k")])).unwrap();
        assert_eq!(
            got,
            env(&[
                ("KEEP", "k"),
                ("A", "1"),
                ("B", "two words"),
                ("C", "lit $A # x"),
                ("D", "q 1\n\"two words\" $x\\"),
                ("E", "x#y"),
                ("F", ""),
                ("G", "multi\nline"),
                ("H", "-$"),
            ])
        );
        assert_eq!(
            parse("X=\"tab\\there\"", &Env::new()).unwrap()["X"],
            Value::from("tab\there")
        );
        assert_eq!(
            parse("export=1", &Env::new()).unwrap()["export"],
            Value::from("1")
        );
        assert_eq!(
            parse("X=\"a\\", &Env::new()).unwrap_err(),
            "line 1: unterminated double quote"
        );
    }

    #[test]
    fn reports_errors_with_lines() {
        assert_eq!(
            parse("A=1\nnot valid\n", &Env::new()).unwrap_err(),
            "line 2: expected NAME=value"
        );
        assert_eq!(
            parse("1A=1", &Env::new()).unwrap_err(),
            "line 1: expected NAME=value"
        );
        assert_eq!(
            parse("A='open", &Env::new()).unwrap_err(),
            "line 1: unterminated single quote"
        );
        assert_eq!(
            parse("A=\"open\n", &Env::new()).unwrap_err(),
            "line 2: unterminated double quote"
        );
    }
}

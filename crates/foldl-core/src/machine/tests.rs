// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

use super::*;
use crate::config::Verbosity;
use crate::plan::FolderStatus;
use crate::testkit::{allowed, ctx, drive, env, info, session};

const SH: &str = "#!/bin/sh\n";

/// A world where every effect succeeds and every script exits 0, adding
/// `<NAME>_UP=1` for up scripts.
fn ok(effect: &Effect) -> Option<Event> {
    match effect {
        Effect::Prepare { .. } => Some(Event::Done),
        Effect::Run(run) => {
            let mut after = run.env.clone();
            if run.direction == Direction::Up {
                after.insert(format!("{}_UP", run.name.to_uppercase()), Value::from("1"));
            }
            Some(Event::Ran(Ran {
                outcome: Outcome::Exited {
                    status: 0,
                    env: Some(after),
                },
                tail: vec![],
                millis: 1,
            }))
        }
        _ => None,
    }
}

/// Like `ok`, but runs named `failing` end with `outcome`.
fn failing(name: &'static str, outcome: Outcome) -> impl FnMut(&Effect) -> Option<Event> {
    move |effect| match effect {
        Effect::Run(run) if run.name == name => Some(Event::Ran(Ran {
            outcome: outcome.clone(),
            tail: vec!["boom".into()],
            millis: 1,
        })),
        other => ok(other),
    }
}

const ENV_ONLY: &[(&str, &str)] = &[(
    "1-env/manifest.toml",
    "version = 1\npath_add = [\"bin\"]\nunset = [\"GONE\"]\n[env]\nA = \"${FOLDL_ROOT}/x\"\nGONE = \"set first\"\nH = \"${HOME}$$\"",
)];

const SCRIPTS: &[(&str, &str)] = &[
    (
        "1-one/manifest.toml",
        "version = 1\n[aliases]\nt = \"just test\"",
    ),
    ("1-one/up.sh", SH),
    ("1-one/down.sh", SH),
    ("2-two/manifest.toml", "version = 1\ntimeout = \"5s\""),
    ("2-two/up.sh", SH),
];

#[test]
fn loads_manifest_only_migrations_in_order() {
    let t = drive(
        start(
            session(),
            ctx(),
            env(&[("PATH", "/usr/bin"), ("HOME", "/h"), ("GONE", "x")]),
            &[allowed("/r", "f", ENV_ONLY)],
        ),
        ok,
    );
    let e = &t.finish.env;
    assert_eq!(e["PATH"], Value::from("/r/bin:/usr/bin"));
    assert_eq!(e["A"], Value::from("/r/x"));
    assert_eq!(e["H"], Value::from("/h$"));
    assert!(!e.contains_key("GONE"));
    assert_eq!(t.finish.code, Code::Ok);
    assert_eq!(t.printed(), "foldl ↑ env (+4 vars)\n");
    assert!(t.runs().is_empty());
    let loaded = &t.finish.session.loaded[0];
    assert_eq!(
        loaded.saved,
        PathBuf::from(format!(
            "/st/sessions/s1/saved/env-{}",
            crate::state::folder_key(Path::new("/r"))
        ))
    );
}

#[test]
fn path_add_without_path() {
    let t = drive(
        start(session(), ctx(), env(&[]), &[allowed("/r", "f", ENV_ONLY)]),
        ok,
    );
    assert_eq!(t.finish.env["PATH"], Value::from("/r/bin"));
    assert_eq!(t.finish.env["H"], Value::from("$"));
}

#[test]
fn runs_scripts_and_records_their_diff() {
    let t = drive(
        start(session(), ctx(), env(&[]), &[allowed("/r", "f", SCRIPTS)]),
        ok,
    );
    assert_eq!(t.runs(), ["up:one", "up:two"]);
    assert_eq!(t.finish.env, env(&[("ONE_UP", "1"), ("TWO_UP", "1")]));
    assert_eq!(t.finish.shell_code, "alias t='just test'\n");
    let first = t.effects.iter().find_map(|e| match e {
        Effect::Run(run) => Some(run.clone()),
        _ => None,
    });
    let run = first.unwrap();
    assert_eq!(run.env["FOLDL_ROOT"], Value::from("/r"));
    assert!(run.env.contains_key("FOLDL_STATE"));
    assert_eq!(run.cwd, PathBuf::from("/r"));
    assert_eq!(run.timeout, Duration::from_secs(60));
    assert!(matches!(&run.exec, Exec::Script { path, .. } if path.ends_with("up.sh")));
    assert!(t.finish.session.loaded[0].down.is_some());
    assert!(t.finish.session.needs_holder());
}

#[test]
fn unloads_in_reverse_running_custom_downs() {
    let first = drive(
        start(
            session(),
            ctx(),
            env(&[("KEEP", "k")]),
            &[allowed("/r", "f", SCRIPTS)],
        ),
        ok,
    );
    let t = drive(
        start(first.finish.session, ctx(), first.finish.env, &[]),
        ok,
    );
    assert_eq!(t.runs(), ["down:one"]);
    assert_eq!(t.finish.env, env(&[("KEEP", "k")]));
    assert_eq!(t.finish.shell_code, "unalias t 2>/dev/null\n");
    assert_eq!(t.printed(), "foldl ↓ two · one\n");
    let removed = t
        .effects
        .iter()
        .filter(|e| matches!(e, Effect::Remove(_)))
        .count();
    assert_eq!(removed, 2);
    assert!(t.finish.session.loaded.is_empty());
}

#[test]
fn a_failed_up_rolls_back_its_folder() {
    let outer = allowed(
        "/a",
        "fa",
        &[("1-o/manifest.toml", "version = 1\n[env]\nO = \"1\"")],
    );
    let t = drive(
        start(
            session(),
            ctx(),
            env(&[]),
            &[outer, allowed("/a/b", "fb", SCRIPTS)],
        ),
        failing(
            "two",
            Outcome::Exited {
                status: 3,
                env: None,
            },
        ),
    );
    assert_eq!(t.runs(), ["up:one", "up:two", "down:one"]);
    assert_eq!(t.finish.code, Code::UpFailed);
    assert_eq!(t.finish.env, env(&[("O", "1")]));
    assert_eq!(
        t.finish.session.failed.get(Path::new("/a/b")),
        Some(&"fb".to_string())
    );
    assert_eq!(t.finish.session.loaded.len(), 1);
    let printed = t.printed();
    assert!(
        printed
            .starts_with("foldl ✗ two: up failed (exit 3)\n  │ boom\n  full log: foldl logs two\n"),
        "{printed}"
    );
    assert!(
        printed.ends_with("foldl ↓ one\nfoldl ↑ o (+1 var) · one (+1 var)\n"),
        "{printed}"
    );
}

#[test]
fn failure_kinds() {
    let cases = [
        (
            Outcome::TimedOut,
            Code::Timeout,
            "two: up timed out after 5s",
        ),
        (Outcome::Interrupted, Code::Interrupted, "two: interrupted"),
        (
            Outcome::Unavailable("bash not found".into()),
            Code::Unavailable,
            "two: bash not found",
        ),
        (
            Outcome::Exited {
                status: 0,
                env: None,
            },
            Code::UpFailed,
            "two: up exited without reporting",
        ),
    ];
    for (outcome, code, text) in cases {
        let t = drive(
            start(session(), ctx(), env(&[]), &[allowed("/r", "f", SCRIPTS)]),
            failing("two", outcome),
        );
        assert_eq!(t.finish.code, code);
        assert!(t.printed().contains(text), "{}", t.printed());
    }
}

#[test]
fn a_failed_prepare_fails_the_up() {
    let t = drive(
        start(session(), ctx(), env(&[]), &[allowed("/r", "f", ENV_ONLY)]),
        |e| match e {
            Effect::Prepare { .. } => Some(Event::Io("disk full".into())),
            _ => None,
        },
    );
    assert_eq!(t.finish.code, Code::Internal);
    assert!(t.printed().contains("disk full"));
}

#[test]
fn failed_downs_warn_and_still_apply_the_derived_down() {
    let first = drive(
        start(session(), ctx(), env(&[]), &[allowed("/r", "f", SCRIPTS)]),
        ok,
    );
    for outcome in [
        Outcome::Exited {
            status: 1,
            env: None,
        },
        Outcome::TimedOut,
        Outcome::Interrupted,
        Outcome::Unavailable("zsh not found".into()),
    ] {
        let t = drive(
            start(
                first.finish.session.clone(),
                ctx(),
                first.finish.env.clone(),
                &[],
            ),
            failing("one", outcome),
        );
        assert!(
            t.printed().contains("foldl ✗ one: down failed ("),
            "{}",
            t.printed()
        );
        assert_eq!(t.finish.env, env(&[]));
    }
}

#[test]
fn exec_mode_skips_aliases() {
    let c = Ctx {
        shell: None,
        ..ctx()
    };
    let t = drive(
        start(session(), c, env(&[]), &[allowed("/r", "f", SCRIPTS)]),
        ok,
    );
    assert_eq!(t.finish.shell_code, "");
    assert!(t.finish.session.loaded[0].aliases.is_empty());
}

#[test]
fn unexpected_events_are_internal_errors() {
    let (m, _) = start(session(), ctx(), env(&[]), &[allowed("/r", "f", SCRIPTS)]);
    let (_, effects) = step(
        m,
        Event::Ran(Ran {
            outcome: Outcome::TimedOut,
            tail: vec![],
            millis: 0,
        }),
    );
    assert!(matches!(&effects[0], Effect::Print(t) if t.contains("internal error")));
    assert!(matches!(effects.last(), Some(Effect::Finish(f)) if f.code == Code::Internal));
}

#[test]
fn notices_are_printed_and_set_the_code() {
    let untrusted = info("/h/app", "f", FolderStatus::Untrusted { changed: false });
    let t = drive(
        start(session(), ctx(), env(&[("HOME", "/h")]), &[untrusted]),
        ok,
    );
    assert_eq!(
        t.printed(),
        "foldl ✗ ~/app is not allowed\n  fix: foldl allow ~/app\n"
    );
    assert_eq!(t.finish.code, Code::Untrusted);
    assert_eq!(
        t.finish.session.notified.get(Path::new("/h/app")),
        Some(&"f".to_string())
    );
    let again = drive(
        start(t.finish.session, ctx(), env(&[("HOME", "/h")]), &[]),
        ok,
    );
    assert!(again.finish.session.notified.is_empty());
}

#[test]
fn notice_texts() {
    let n = |notice| notice_text(&notice, "/h");
    assert_eq!(
        n(Notice::Untrusted {
            root: "/h/a".into(),
            changed: true
        }),
        "foldl ✗ ~/a/.foldl changed since it was allowed\n  fix: foldl allow ~/a\n"
    );
    assert_eq!(
        n(Notice::Denied { root: "/x".into() }),
        "foldl: /x is denied; `foldl allow` loads it\n"
    );
    let issue = crate::folder::Issue {
        severity: crate::folder::Severity::Error,
        file: "f".into(),
        message: "m".into(),
    };
    assert_eq!(
        n(Notice::Invalid {
            root: "/h".into(),
            issues: vec![issue]
        }),
        "foldl ✗ ~/.foldl is invalid\n  error: .foldl/f: m\n  fix: edit it, then run foldl check ~\n"
    );
    assert_eq!(
        n(Notice::ChangedWhileLoaded {
            root: "/h/a".into()
        }),
        "foldl ! ~/a/.foldl changed; run 'foldl allow ~/a' to reload it\n"
    );
    assert_eq!(
        n(Notice::SharedChanged {
            root: "/a".into(),
            name: "pg".into()
        }),
        "foldl ! pg changed; run 'foldl redo pg'\n"
    );
    assert_eq!(
        notice_code(&Notice::Denied { root: "/".into() }),
        Code::Denied
    );
    assert_eq!(
        notice_code(&Notice::Invalid {
            root: "/".into(),
            issues: vec![]
        }),
        Code::Invalid
    );
    assert_eq!(
        notice_code(&Notice::SharedChanged {
            root: "/".into(),
            name: "x".into()
        }),
        Code::Ok
    );
}

#[test]
fn tilde_paths() {
    assert_eq!(tilde(Path::new("/h/x"), "/h"), "~/x");
    assert_eq!(tilde(Path::new("/h"), "/h"), "~");
    assert_eq!(tilde(Path::new("/hx"), "/h"), "/hx");
    assert_eq!(tilde(Path::new("/h/x"), ""), "/h/x");
}

#[test]
fn summaries_follow_verbosity() {
    let diff = EnvDiff::between(&env(&[("A", "1")]), &env(&[("B", "1")]));
    assert_eq!(
        up_item("x", &diff, 1500, Verbosity::Normal),
        "x (+2 vars, 1.5s)"
    );
    assert_eq!(up_item("x", &diff, 0, Verbosity::Verbose), "x (-A +B)");
    assert_eq!(up_item("x", &EnvDiff::default(), 0, Verbosity::Normal), "x");
    let mut quiet = ctx();
    quiet.config.verbosity = Verbosity::Quiet;
    let t = drive(
        start(session(), quiet, env(&[]), &[allowed("/r", "f", ENV_ONLY)]),
        ok,
    );
    assert_eq!(t.printed(), "");
}

#[test]
fn builtins_run_with_their_default_timeouts() {
    let pairs: &[(&str, &str)] = &[
        (
            "1-nix/manifest.toml",
            "version = 1\nbuiltin = \"nix-flake\"",
        ),
        ("2-dot/manifest.toml", "version = 1\nbuiltin = \"dotenv\""),
    ];
    let t = drive(
        start(session(), ctx(), env(&[]), &[allowed("/r", "f", pairs)]),
        ok,
    );
    let timeouts: Vec<Duration> = t
        .effects
        .iter()
        .filter_map(|e| match e {
            Effect::Run(run) => Some(run.timeout),
            _ => None,
        })
        .collect();
    assert_eq!(
        timeouts,
        [Duration::from_secs(600), Duration::from_secs(60)]
    );
}

#[test]
fn an_empty_stack_finishes_at_once() {
    let (_, effects) = start(session(), ctx(), env(&[]), &[]);
    assert!(matches!(effects.as_slice(), [Effect::Finish(_)]));
}

#[test]
fn a_down_with_nothing_loaded_is_skipped() {
    let (mut m, _) = start(session(), ctx(), env(&[]), &[]);
    m.queue.push_back(Op::Down);
    let (_, effects) = advance(m);
    assert!(matches!(effects.as_slice(), [Effect::Finish(_)]));
}

#[test]
fn ports_are_exported_and_interpolated() {
    let pairs: &[(&str, &str)] = &[
        (
            "1-pg/manifest.toml",
            "version = 1\nscope = \"shared\"\n[ports]\nPGPORT = \"auto\"\n[env]\nURL = \"pg://:${PGPORT}\"",
        ),
        ("1-pg/down.sh", SH),
    ];
    let folder = crate::folder::load(
        &crate::testkit::files("/r", pairs),
        crate::folder::Host::default(),
    )
    .unwrap();
    let fresh = Fresh {
        root: "/r".into(),
        migration: folder.migrations[0].clone(),
        watch: Default::default(),
    };
    let ports = [("PGPORT".to_string(), "20001".to_string())].into();
    let e = prepared_env(&env(&[]), &fresh, Path::new("/state"), &ports);
    assert_eq!(e, env(&[("PGPORT", "20001"), ("URL", "pg://:20001")]));
}

#[test]
fn leaving_a_folder_forgets_its_failure() {
    let mut s = session();
    s.failed.insert("/gone".into(), "f".into());
    s.failed.insert("/r".into(), "old".into());
    let t = drive(
        start(s, ctx(), env(&[]), &[allowed("/r", "f", ENV_ONLY)]),
        ok,
    );
    assert_eq!(
        t.finish.session.failed.keys().collect::<Vec<_>>(),
        [Path::new("/r")]
    );
    assert_eq!(
        t.finish.session.cumulative(),
        EnvDiff::between(&env(&[]), &t.finish.env)
    );
}

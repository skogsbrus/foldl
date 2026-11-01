// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Johan Fosse

use super::*;
use crate::env::EnvDiff;
use crate::testkit::{allowed, info, session};

const A: &[(&str, &str)] = &[
    ("1-a1/manifest.toml", "version = 1\n[env]\nA1 = \"1\""),
    ("2-a2/manifest.toml", "version = 1\n[env]\nA2 = \"1\""),
];
const B: &[(&str, &str)] = &[("1-b1/manifest.toml", "version = 1\n[env]\nB1 = \"1\"")];

/// A session with these folders' migrations loaded, as a run would.
fn loaded(folders: &[FolderInfo]) -> Session {
    let mut s = session();
    let p = plan(&s, folders);
    s.loaded = p.ups.into_iter().map(as_loaded).collect();
    s
}

fn as_loaded(f: Fresh) -> Loaded {
    Loaded {
        root: f.root,
        name: f.migration.name,
        hash: f.migration.hash,
        watch: f.watch,
        scope: f.migration.scope,
        lease: None,
        diff: EnvDiff::default(),
        aliases: vec![],
        interactive: None,
        down: None,
        down_timeout_ms: 0,
        saved: "/saved".into(),
        state_dir: "/state".into(),
        log: "/log".into(),
        note: None,
    }
}

fn names(ups: &[Fresh]) -> Vec<String> {
    ups.iter().map(|f| f.migration.name.clone()).collect()
}

#[test]
fn enters_outermost_first() {
    let p = plan(
        &session(),
        &[allowed("/a", "fa", A), allowed("/a/b", "fb", B)],
    );
    assert_eq!(
        (p.downs, names(&p.ups)),
        (0, vec!["a1".into(), "a2".into(), "b1".into()])
    );
}

#[test]
fn moving_up_unloads_only_the_inner_folder() {
    let s = loaded(&[allowed("/a", "fa", A), allowed("/a/b", "fb", B)]);
    let p = plan(&s, &[allowed("/a", "fa", A)]);
    assert_eq!((p.downs, p.ups.len()), (1, 0));
}

#[test]
fn moving_across_unloads_everything_then_loads() {
    let s = loaded(&[allowed("/a", "fa", A), allowed("/a/b", "fb", B)]);
    let p = plan(&s, &[allowed("/c", "fc", B)]);
    assert_eq!((p.downs, names(&p.ups)), (3, vec!["b1".to_string()]));
}

#[test]
fn a_changed_session_migration_reloads_it_and_what_follows() {
    let s = loaded(&[allowed("/a", "fa", A), allowed("/a/b", "fb", B)]);
    let changed: &[(&str, &str)] = &[
        A[0],
        ("2-a2/manifest.toml", "version = 1\n[env]\nA2 = \"2\""),
    ];
    let p = plan(
        &s,
        &[allowed("/a", "fa2", changed), allowed("/a/b", "fb", B)],
    );
    assert_eq!(
        (p.downs, names(&p.ups)),
        (2, vec!["a2".into(), "b1".into()])
    );
}

#[test]
fn a_changed_watch_file_reloads() {
    let s = loaded(&[allowed("/a", "fa", A)]);
    let mut changed = allowed("/a", "fa2", A);
    if let FolderStatus::Allowed { watch, .. } = &mut changed.status {
        watch.insert("a2".into(), crate::folder::Hash("w".into()));
    }
    let p = plan(&s, &[changed]);
    assert_eq!((p.downs, names(&p.ups)), (1, vec!["a2".to_string()]));
}

#[test]
fn a_changed_shared_migration_is_kept_with_a_notice() {
    const S: &[(&str, &str)] = &[
        ("1-pg/manifest.toml", "version = 1\nscope = \"shared\""),
        ("1-pg/up.sh", "#!/bin/sh\n"),
        ("1-pg/down.sh", "#!/bin/sh\n"),
    ];
    const S2: &[(&str, &str)] = &[S[0], ("1-pg/up.sh", "#!/bin/sh\necho 2\n"), S[2]];
    let s = loaded(&[allowed("/a", "fa", S)]);
    let p = plan(&s, &[allowed("/a", "fa2", S2)]);
    assert_eq!((p.downs, p.ups.len()), (0, 0));
    assert_eq!(
        p.notices,
        vec![Notice::SharedChanged {
            root: "/a".into(),
            name: "pg".into()
        }]
    );
}

#[test]
fn untrusted_folders_stop_the_stack() {
    let p = plan(
        &session(),
        &[
            allowed("/a", "fa", A),
            info("/a/b", "fb", FolderStatus::Untrusted { changed: false }),
            allowed("/a/b/c", "fc", B),
        ],
    );
    assert_eq!(names(&p.ups), vec!["a1".to_string(), "a2".to_string()]);
    assert_eq!(
        p.notices,
        vec![Notice::Untrusted {
            root: "/a/b".into(),
            changed: false
        }]
    );
    assert_eq!(p.notices[0].root(), &PathBuf::from("/a/b"));
}

#[test]
fn denied_and_invalid_folders_stop_the_stack() {
    let p = plan(
        &session(),
        &[
            info("/a", "f", FolderStatus::Denied),
            allowed("/a/b", "fb", B),
        ],
    );
    assert_eq!(
        (p.ups.len(), p.notices.clone()),
        (0, vec![Notice::Denied { root: "/a".into() }])
    );
    let p = plan(
        &session(),
        &[
            info("/a", "f", FolderStatus::Invalid(vec![])),
            allowed("/a/b", "fb", B),
        ],
    );
    assert_eq!(
        p.notices,
        vec![Notice::Invalid {
            root: "/a".into(),
            issues: vec![]
        }]
    );
}

#[test]
fn ignored_folders_are_skipped_silently() {
    let p = plan(
        &session(),
        &[
            info("/a", "f", FolderStatus::Ignored),
            allowed("/a/b", "fb", B),
        ],
    );
    assert_eq!(
        (names(&p.ups), p.notices.len()),
        (vec!["b1".to_string()], 0)
    );
}

#[test]
fn a_loaded_folder_that_changes_stays_loaded_until_allowed() {
    let s = loaded(&[allowed("/a", "fa", A), allowed("/a/b", "fb", B)]);
    let p = plan(
        &s,
        &[
            info("/a", "fa2", FolderStatus::Untrusted { changed: true }),
            allowed("/a/b", "fb", B),
        ],
    );
    assert_eq!((p.downs, p.ups.len()), (0, 0));
    assert_eq!(
        p.notices,
        vec![Notice::ChangedWhileLoaded { root: "/a".into() }]
    );
    let p = plan(
        &s,
        &[
            info("/a", "fa3", FolderStatus::Invalid(vec![])),
            allowed("/a/b", "fb", B),
        ],
    );
    assert_eq!((p.downs, p.ups.len(), p.notices.len()), (0, 0, 1));
}

#[test]
fn a_denied_loaded_folder_unloads() {
    let s = loaded(&[allowed("/a", "fa", A)]);
    let p = plan(&s, &[info("/a", "fa", FolderStatus::Denied)]);
    assert_eq!(p.downs, 2);
}

#[test]
fn a_kept_migration_after_a_reload_is_dropped() {
    let s = loaded(&[allowed("/a", "fa", A), allowed("/a/b", "fb", B)]);
    let changed: &[(&str, &str)] = &[
        ("1-a1/manifest.toml", "version = 1\n[env]\nA1 = \"2\""),
        A[1],
    ];
    let p = plan(
        &s,
        &[
            allowed("/a", "fa2", changed),
            info("/a/b", "fb2", FolderStatus::Untrusted { changed: true }),
        ],
    );
    assert_eq!(
        (p.downs, names(&p.ups)),
        (3, vec!["a1".to_string(), "a2".to_string()])
    );
}

#[test]
fn failed_folders_wait_for_a_change() {
    let mut s = session();
    s.failed.insert("/a".into(), "fa".into());
    assert_eq!(plan(&s, &[allowed("/a", "fa", A)]).ups.len(), 0);
    assert_eq!(plan(&s, &[allowed("/a", "fa2", A)]).ups.len(), 2);
}

#[test]
fn held_down_migrations_stay_down() {
    let mut s = session();
    s.held_down.insert(("/a".into(), "a1".into()));
    assert_eq!(
        names(&plan(&s, &[allowed("/a", "fa", A)]).ups),
        vec!["a2".to_string()]
    );
}

#[test]
fn notices_are_shown_once_per_fingerprint() {
    let mut s = session();
    s.notified.insert("/a".into(), "f".into());
    let untrusted = |fp| info("/a", fp, FolderStatus::Untrusted { changed: false });
    assert!(plan(&s, &[untrusted("f")]).notices.is_empty());
    assert_eq!(plan(&s, &[untrusted("g")]).notices.len(), 1);
}

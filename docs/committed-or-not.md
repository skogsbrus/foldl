# Committed or not

Migrations always live in the repository's `.foldl/`. There is no other
location. You do not have to commit it. You can keep it private with
`.git/info/exclude`. Some features depend on it being committed, and foldl
says which.

| Feature | Committed | Not committed |
|---|---|---|
| Load and unload on `cd` in this checkout | yes | yes |
| Shared leases, ports, teardown, interactive migrations | yes | yes |
| Teammates get the same setup | yes | no |
| New worktrees and clones have the migrations (including agent worktrees) | yes | no, copy `.foldl/` by hand |
| Content-based trust (`reuse = "repo"`) | yes | no, each folder needs `foldl allow` |
| Caches shared across worktrees | yes | only between folders that have `.foldl/` |
| `.local` migrations as personal overrides | yes | not needed, everything is personal |

## How foldl asks

`foldl new`, the first time in a repository, and `foldl import direnv` ask
whether `.foldl/` is shared (committed) or private. They show the rows marked
"no" above before you choose.

To skip the question:

- `--private`: keep `.foldl/` out of git. foldl adds `/.foldl/` to the
  `info/exclude` file in the repository's common git directory.
- `--committed`: plan to commit `.foldl/`. This is the default when there is
  no terminal, for example in an agent.

`foldl status` shows `private (not committed)` for a private folder.

## `.local` migrations

A migration directory ending in `.local` (for example `050-secrets.local/`)
is personal. Create one with `foldl new --local <name>`. It merges into the
same ordered list as the committed migrations, so you can add or override
setup without changing the shared `.foldl/`.

When `.foldl/` is committed, `foldl new --local` adds `/.foldl/*.local/` to
the repository's `.gitignore`. The entry is shared, so teammates' local
migrations are ignored too. When `.foldl/` is private, nothing is added: it
is already excluded. `.foldl/` itself cannot hold a `.gitignore` file,
because only migrations and `foldl.toml` are allowed there.

Local migrations are included in the trust hash.

## Worktrees and clones

If a sibling checkout has a private `.foldl/`, a new worktree or clone has no
way to know. foldl cannot detect this and says nothing. Copy `.foldl/` over
by hand, or commit it. This is the main reason to commit: agents create
worktrees without running `foldl allow`, and with `reuse = "repo"` in
`.foldl/foldl.toml` those worktrees are trusted at once. See the Trust
section of the [README](../README.md).

# Demos

Every demo is recorded in a real interactive zsh with foldl's prompt hook,
in a fresh home directory. `scripts/demos` types the commands, writes an
asciicast file next to each GIF, and renders the GIF with
[agg](https://github.com/asciinema/agg). Re-record them after output
changes:

```sh
nix build && nix develop -c scripts/demos            # all of them
nix develop -c scripts/demos shared                  # one
```

## Enter and leave

`app/.foldl/` sets a variable, adds `bin` to `PATH` and defines an alias. The
first `cd` is refused until the folder is allowed; `foldl allow` shows what
will run. Leaving undoes all of it.

![cd into a folder, allow it, use its env and alias, cd out and both are gone](demos/enter-leave.gif)

## Shared services

The `server` migration has `scope = "shared"` and a `[ports]` entry. The
first shell to enter starts a web server on a free port; the second joins
it and gets the same port. When the first shell leaves, the server is kept
because another shell still uses it. When the last one leaves, it stops.
A closed or killed terminal counts as leaving.

![two shells enter a folder: the first starts a web server, the second joins it; the server stops when the last shell leaves](demos/shared.gif)

## Worktrees

The same `server` migration, in a repository with two worktrees. The
default `lease = "folder"` gives each worktree its own server, and
`[ports]` gives each its own port, so they never collide. With
`reuse = "repo"` in `.foldl/foldl.toml`, the second worktree loads without
another `foldl allow`, because its `.foldl/` matches what was allowed.

![two worktrees of one repo each start their own web server on their own port, serving their own branch](demos/worktrees.gif)

## Rollback

`002-secrets` fails because `.env.local` is missing, so foldl undoes
`001-env` and shows the end of the script's output. The folder isn't
retried at every prompt; it reloads when something it watches changes.
Here that is `.env.local`, listed in `watch`.

![a failing up rolls back; creating the watched .env.local loads the folder](demos/rollback.gif)

## Switching from direnv

`foldl import direnv` translates `.envrc` into migrations (`--dry-run`
prints them), allows the folder, checks that direnv and foldl export the
same environment, and runs `direnv deny` so direnv stops loading it. See
[`foldl import`](../README.md#foldl-import) for its options.

![foldl import direnv translates .envrc, checks parity with direnv and loads the folder](demos/import-direnv.gif)

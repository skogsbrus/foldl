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

- [Enter and leave](#enter-and-leave)
- [Shared services](#shared-services)
- [Killed terminals](#killed-terminals)
- [Worktrees](#worktrees)
- [Aliases and functions](#aliases-and-functions)
- [Nested folders](#nested-folders)
- [Nix flakes](#nix-flakes)
- [Editing a loaded folder](#editing-a-loaded-folder)
- [Rollback](#rollback)
- [Dirty leases](#dirty-leases)
- [Status and logs](#status-and-logs)
- [Switching from direnv](#switching-from-direnv)

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

![two shells enter a folder: the first starts a web server, the second joins it; the server stops when the last shell leaves](demos/shared.gif)

## Killed terminals

The same server, but the last shell using it doesn't leave cleanly: it is
killed with `kill -9`, as when a terminal window is closed. Each shell has
a small holder process that notices its shell is gone and runs the leave
for it, so the server still stops.

![the last shell using a shared server is killed with kill -9, and the server stops anyway](demos/killed.gif)

## Worktrees

The same `server` migration, in a repository with two worktrees. The
default `lease = "folder"` gives each worktree its own server, and
`[ports]` gives each its own port, so they never collide. With
`reuse = "repo"` in `.foldl/foldl.toml`, the second worktree loads without
another `foldl allow`, because its `.foldl/` matches what was allowed.

![two worktrees of one repo each start their own web server on their own port, serving their own branch](demos/worktrees.gif)

## Aliases and functions

An `interactive = true` migration can define aliases, functions and
completions in your shell. Here it replaces your own `ll` alias and adds a
`deploy` function. Leaving removes both and gives you your `ll` back.

![a folder's ll alias shadows the user's own and a deploy function appears; leaving restores the original ll and removes deploy](demos/interactive.gif)

## Nested folders

Every ancestor with a `.foldl/` loads, outermost first. In this monorepo the
root sets the toolchain and each service adds its own variables. Moving
between services swaps only the service's migrations; the root's stay
loaded until you leave the repository.

![a monorepo root and two services: moving between services swaps only the service's migrations](demos/nested.gif)

## Nix flakes

The `nix-flake` built-in loads a flake's dev shell, like nix-direnv. The
first load evaluates the flake; later ones come from foldl's cache, keyed on
the flake's content, so worktrees on the same commit share it. When an edit
breaks the flake, foldl warns and keeps the last good environment.

![a flake dev shell loads with cowsay and hello; a broken flake.nix falls back to the last good environment](demos/nix-flake.gif)

## Editing a loaded folder

A change to a watched file (here `.env`) reloads at the next prompt. A
change to `.foldl/` itself needs `foldl allow` again, which shows exactly
what changed. Either way only the changed migration and the ones after it
reload; the folder stays loaded meanwhile.

![editing a watched .env reloads it; editing a manifest asks for foldl allow, which shows the diff and reloads only that migration](demos/edit-reload.gif)

## Rollback

`002-secrets` fails because `.env.local` is missing, so foldl undoes
`001-env` and shows the end of the script's output. The folder isn't
retried at every prompt; it reloads when something it watches changes.
Here that is `.env.local`, listed in `watch`.

![a failing up rolls back; creating the watched .env.local loads the folder](demos/rollback.gif)

## Dirty leases

A shared `down` that fails leaves the lease dirty, since whatever it should
have stopped may still be running. The next shell to enter is asked what to
do: retry the down, skip it, or abort. Agents and scripts never get the
question; they follow `on_dirty` instead.

![a shared queue's down fails and the lease is dirty; the next cd offers retry, skip or abort, and retry succeeds](demos/dirty.gif)

## Status and logs

`foldl status` lists the folder's migrations, their scope and state, and who
holds each shared one. `foldl logs` shows a migration's script output from
its last run.

![foldl status shows the migrations and the server's holders; foldl logs shows the server's output](demos/status.gif)

## Switching from direnv

`foldl import direnv` translates `.envrc` into migrations (`--dry-run`
prints them), allows the folder, checks that direnv and foldl export the
same environment, and runs `direnv deny` so direnv stops loading it. See
[`foldl import`](../README.md#foldl-import) for its options.

![foldl import direnv translates .envrc, checks parity with direnv and loads the folder](demos/import-direnv.gif)

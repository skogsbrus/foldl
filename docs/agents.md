# Agents

foldl works for agents (non-interactive, short-lived shells) as well as
humans.

## Rules agents can rely on

- foldl never prompts in a non-interactive run. An untrusted or invalid
  folder fails with a clear message and the command that fixes it. A dirty
  lease follows `on_dirty` from the [config](configuration.md); the default
  is `abort`.
- `--json` is available on `status`, `exec`, `export` and `session start`.
  Errors become one JSON object on stdout, with stable `code` strings that
  match the [exit codes](../README.md#exit-codes):

  ```json
  {"error": {"code": "untrusted", "exit": 77,
    "message": "~/code/app is not allowed",
    "fix": "foldl allow ~/code/app"}}
  ```

- `foldl new` and `foldl import direnv` default to `--committed` without a
  terminal.
- Scripts run with stdin from `/dev/null` and cannot prompt. Their output is
  in `foldl logs`.

## One-off commands: `foldl exec`

```sh
foldl exec -- make test
foldl exec ~/code/app --json -- ./run.sh
```

`foldl exec` loads the folder, runs the command, and unloads. It exits with
the command's own status once the folder has loaded. If the calling session
already has the folder loaded, it joins that session. Interactive migrations
are skipped, since there is no shell to source them into.

## Long sessions: `foldl session`

Without a session, every short-lived command would enter and leave the
folder. For a long-running agent, start a session owned by the agent
process:

```sh
eval "$(foldl session start --parent <agent-pid>)"   # exports FOLDL_SESSION
foldl export json                                    # reuses the session
foldl session stop                                   # unload and end
```

- `foldl session start [--id <ID>] [--parent <PID>] [--json]` creates a
  session with no shell, or reuses the one with that id. The session lasts
  as long as `--parent` (default: the calling process). If the parent dies,
  the session's teardown runs anyway. A parent of PID 1 counts as gone.
- `foldl session stop [--id <ID>]` unloads everything the session loaded and
  ends it.

## Claude Code: `foldl integrate claude`

```sh
foldl integrate claude            # ~/.claude/settings.json
foldl integrate claude --project  # .claude/settings.json in this project
```

This installs `SessionStart` and `CwdChanged` hooks. Each hook runs
`foldl integrate claude --hook` (an internal flag) and appends the
transition to the file named by `CLAUDE_ENV_FILE`. Claude Code sources that
file before every Bash call.

- The hook writes only the change: `export` and `unset` lines from the
  environment before to the environment after, starting with
  `export FOLDL_SESSION=...`. Writing the whole environment would leave the
  previous folder's variables behind when you change directory.
- The session is named `claude-<session_id>`. It is owned by the nearest
  ancestor process named `claude` (else the hook's parent), so leases are
  held for as long as Claude Code runs, and shared migrations such as a
  database start once and stop when the last session ends.
- Folders that are not allowed fail with a message and the `foldl allow`
  fix. They are never prompted for.

Several Claude sessions, or Claude and your own terminal, can work in one
folder. They share `shared` migrations through leases. `foldl status` lists
the holders by process name, folder and terminal.

## Worktrees

Agents often work in new git worktrees. If `.foldl/` is committed and
`.foldl/foldl.toml` sets `reuse = "repo"` under `[trust]`, the worktree is
trusted without `foldl allow`. See [Committed or not](committed-or-not.md).
Give shared services `lease = "folder"` (the default) and `[ports]` with
`"auto"` so that worktrees do not collide.

# Agents

foldl works for coding agents as well as humans. An agent usually runs each
command in a fresh, non-interactive shell with no prompt hook, so it needs
another way in:

- Any agent can run commands through `foldl exec`, and keep services up
  across commands with a `foldl session`. This needs nothing from the
  agent itself.
- `foldl integrate` sets this up through an agent's own hooks, so it
  happens without being asked. It supports [Claude Code](#claude-code) and
  [Codex](#codex).

| | Gets the folder's environment | Services shared and held | Follows `cd` |
|---|---|---|---|
| Claude Code (`foldl integrate claude`) | in every Bash command | until Claude exits | yes |
| Codex (`foldl integrate codex`) | through `foldl exec --session`, which the hook tells Codex to use | until Codex exits | no; Codex sessions keep one directory |
| any other agent | through `foldl exec` | with `foldl session start` | per command |

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
the command's own status once the folder has loaded. If a session already
has the folder loaded (`--session <ID>`, else `FOLDL_SESSION`), it joins
that session instead: nothing is loaded again, and the command gets the
session's environment even if the caller doesn't have it. Interactive
migrations are skipped, since there is no shell to source them into.

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

To use foldl from an agent foldl has no integration for, start a session
owned by the agent's process when it starts, then have it run commands as
`foldl exec --session <ID> -- <command>`, for example through a line in
its instructions file.

## Claude Code

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

## Codex

```sh
foldl integrate codex             # ~/.codex/hooks.json
foldl integrate codex --project   # .codex/hooks.json in this project
```

This installs a `SessionStart` hook running `foldl integrate codex --hook`.
Codex gives hooks no way to set the environment of later commands, so the
hook does two things instead:

- It loads the folder into the session `codex-<session_id>`, owned by the
  nearest ancestor process named `codex`. Shared migrations start and stay
  up until Codex exits, as with Claude Code.
- It adds developer context telling the model to run each command as
  `foldl exec --session codex-<session_id> -- <command>`. That joins the
  session, so nothing loads twice. The context names the variables the
  folder sets but never their values.

If the folder can't load, for example because it isn't allowed, the context
says so and tells the model not to run `foldl allow` itself. Outside a
folder with `.foldl/`, the hook adds nothing.

Codex runs project hooks only from a trusted project, and asks you to
review new hooks with `/hooks` before they run. Codex has no hook for a
changed directory; for a command in another folder, `foldl exec <DIR> --`
loads that one.

## Several agents at once

Several agent sessions, or agents and your own terminal, can work in one
folder. They share `shared` migrations through leases. `foldl status` lists
the holders by process name, folder and terminal.

## Worktrees

Agents often work in new git worktrees. If `.foldl/` is committed and
`.foldl/foldl.toml` sets `reuse = "repo"` under `[trust]`, the worktree is
trusted without `foldl allow`. See [Committed or not](committed-or-not.md).
Give shared services `lease = "folder"` (the default) and `[ports]` with
`"auto"` so that worktrees do not collide.

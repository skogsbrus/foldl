# foldl design

Status: draft, 2026-11-01.

foldl runs code when a shell enters or leaves a folder. It replaces direnv,
and adds what direnv deliberately leaves out: teardown, and side effects that
outlive a single shell (services, tunnels, containers), shared safely between
the many shells and agents working in the same folder.

## Goals

- Load a folder's environment on enter and fully reverse it on leave.
- Start shared side effects once, and tear them down when the last session
  leaves, including when sessions crash or terminals are closed.
- Work for agents (non-interactive, short-lived shells) as well as humans.
- No daemon. Fast no-op path (~1ms per prompt when nothing changed).
- Native Nix support, never a Nix requirement (see [Nix](#nix)).
- Bring aliases, functions and completions into the user's shell, and remove
  them on leave (direnv's most-requested missing feature,
  [#73](https://github.com/direnv/direnv/issues/73)).
- Work across many git worktrees of one repo without re-trusting, port
  collisions or cold caches.

Non-goals for v1: shells other than bash and zsh, migrations in languages
other than shell, Windows.

## Model: migrations

A folder's setup is an ordered list of migrations. Entering the folder runs
their `up` in order; leaving runs `down` in reverse order.

```
.foldl/
  001-env/manifest.toml
  002-flake/manifest.toml
  003-postgres/{manifest.toml, up.sh, down.sh}
```

- Order comes from the numeric prefix.
- If an `up` fails, the `down`s of the migrations that already ran are run in
  reverse, and the folder is left unloaded.
- Ctrl-C during `up` behaves like a failure: roll back.
- Every ancestor's `.foldl/`, from `/` down to the current folder, is loaded
  as one stack, outermost first. Moving from `/a/b` to `/c` unloads `b`, then
  `a`, then loads `c`. Moving from `/a/b` to `/a` unloads only `b`.

### Sessions

Every interactive shell is its own session, with its own ID and holders. A
shell that inherits a `FOLDL_SESSION` from its parent (a `bash` started inside
zsh, a new tmux pane, `sudo -s`, an editor terminal) sees that the ID doesn't
belong to it, discards the inherited state and loads from scratch.

Session state lives in `$XDG_STATE_HOME/foldl/sessions/<id>/` (see
[State storage](#state-storage)); only the ID is kept in the environment.
Large snapshots in the environment would break child processes.

### Scope and downs

"Does it affect only this shell?" turned out to be two separate questions:
who owns the effect, and whether foldl can undo it on its own. Example: an
`up` that runs `aws sso login` and exports `AWS_PROFILE`. Unsetting the
variable doesn't log out, and logging out on leave would break every other
shell using that profile. So these are independent settings.

**Scope: who owns the effect.**

- **`session`** (default): `up` runs when a session enters, `down` when it
  leaves. No coordination with other sessions.
- **`shared`**: owned by a lease. `up` runs only for the first session holding
  the lease, `down` only for the last.

**Downs: what undoes the effect.**

- **Derived** (always): foldl records what it can observe, the environment and
  for interactive migrations aliases, functions and completions, and applies
  the inverse on leave.
- **Custom** (`down = "down.sh"`, optional): for effects foldl can't observe,
  such as files written, agents started or logins. It runs first, then the
  derived inverse.

**Interactive** (`interactive = true`): sourced into the user's own shell
rather than a subprocess, so aliases, functions and completions survive (see
[Interactive migrations](#interactive-migrations)). Only for `session` scope.

| Example | Scope | Down |
|---|---|---|
| `export DATABASE_URL=…` | session | derived |
| aliases, functions, completions | session, interactive | derived |
| start an ssh-agent for this session | session | custom (`ssh-agent -k`) + derived |
| `aws sso login` for a profile | shared, `lease = "aws-sso:dev"` | custom (`aws sso logout`) + derived |
| Postgres for this folder | shared, `lease = "folder"` | custom (`pg_ctl stop`) + derived |

Effects that only look like env (switching `kubectl` or `gcloud` context
writes a global file) belong in a custom down, or better, should be rewritten
as env (`KUBECONFIG`, `CLOUDSDK_ACTIVE_CONFIG_NAME`) so the derived inverse
covers them. `foldl check` and the direnv importer warn about common
commands of this kind.

### Local migrations

A migration whose directory ends in `.local` (e.g. `050-secrets.local/`) is
personal and should be gitignored; `foldl new --local` adds the ignore entry.
Local migrations are merged into the same ordered list as committed ones, so
users can add or override setup without changing the shared `.foldl/`.

### Committed or not

Migrations always live in the repository's `.foldl/`; there is no other
location. Committing it is not required: it can be kept private with
`.git/info/exclude` or a global gitignore. Some features depend on it being
committed, and foldl is upfront about which:

| Feature | Committed | Not committed |
|---|---|---|
| Load and unload on `cd` in this checkout | ✓ | ✓ |
| Shared leases, ports, teardown, interactive migrations | ✓ | ✓ |
| Teammates get the same setup | ✓ | ✗ |
| New worktrees and clones have the migrations (incl. agent worktrees) | ✓ | ✗ copy `.foldl/` by hand |
| Content-based trust (`reuse = "repo"`) | ✓ | ✗ each folder needs `foldl allow` |
| Caches shared across worktrees | ✓ | only between folders that have `.foldl/` |
| `.local` migrations as personal overrides | ✓ | not needed; everything is personal |

Where foldl says so:

- `foldl new`, the first time in a repository, and `foldl import direnv` ask
  whether `.foldl/` is shared (committed) or private (added to
  `.git/info/exclude`), showing the rows marked ✗ before the user chooses.
- `foldl status` shows `private (not committed)` for such a folder.
- In a worktree or clone whose sibling checkouts have a private `.foldl/`,
  foldl can't know about it and says nothing; the documentation's
  "Committed or not" page carries this table.

### Migration directories

- A migration is a directory in `.foldl/` named `<number>-<name>` or
  `<number>-<name>.local`, where `<number>` is one or more digits and
  `<name>` matches `[a-z0-9][a-z0-9-]*`. Anything else in `.foldl/` other
  than `foldl.toml` is an error.
- Migrations are ordered by number (numerically), then by name. Duplicate
  numbers are allowed.
- `<name>` is the migration's id in the CLI (`foldl redo postgres`). Names
  must be unique within a folder.
- `FOLDL_ROOT` is the folder that contains `.foldl/`.

### Manifest

Every migration directory has a `manifest.toml`. Unknown keys are errors.
Durations are strings such as `"500ms"`, `"30s"` or `"5m"`.

| Key | Type | Default | Notes |
|---|---|---|---|
| `version` | integer | required | `1` |
| `scope` | `"session"` \| `"shared"` | `"session"` | |
| `interactive` | bool | `false` | `session` only |
| `up` | path | `"up.sh"` if it exists | relative to the migration directory |
| `down` | path | `"down.sh"` if it exists | custom down; required for `shared` |
| `builtin` | `"nix-flake"` \| `"dotenv"` | none | excludes `up`, `interactive` |
| `[builtin_options]` | table | `{}` | per built-in, below |
| `path_add` | array of paths | `[]` | prepended to `PATH`, relative to `FOLDL_ROOT` |
| `[env]` | table of string | `{}` | values may interpolate, below |
| `unset` | array of names | `[]` | variables to unset |
| `[aliases]` | table of string | `{}` | `session` only |
| `lease` | `"folder"` \| `"repo"` \| string | `"folder"` | `shared` only |
| `[ports]` | table of `"auto"` | `{}` | `shared` only |
| `timeout` | duration | `"60s"`, `"10m"` for `nix-flake` | per `up` |
| `down_timeout` | duration | `"30s"` | per `down` |
| `watch` | array of globs | `[]` | relative to `FOLDL_ROOT`; a change counts as the migration changing |

Order of application within one migration on `up`: `path_add`, `[env]`,
`unset`, `[ports]`, built-in or `up` script, `[aliases]`. On leave: the
custom `down` (if any), then the derived inverse of all of it.

`[env]` values may use `${FOLDL_ROOT}`, `${FOLDL_STATE}` (the migration's
state directory), `${HOME}` and names from `[ports]`. Nothing else is
expanded, so manifests stay data rather than shell. `$$` is a literal `$`.

A manifest must do something: at least one of `up`, `builtin`, `path_add`,
`[env]`, `unset`, `[aliases]`.

Built-ins:

- **`nix-flake`**: options `flake` (default `"."`, relative to
  `FOLDL_ROOT`). See [Nix](#nix).
- **`dotenv`**: options `file` (default `".env"`, relative to `FOLDL_ROOT`),
  `required` (default `true`).

```toml
# 001-env/manifest.toml: no scripts needed
version = 1
path_add = ["bin", "node_modules/.bin"]

[env]
DATABASE_URL = "postgres://localhost/app"
TOOLS = "${FOLDL_ROOT}/tools"
```

```toml
# 002-flake/manifest.toml
version = 1
builtin = "nix-flake"
watch = ["flake.nix", "flake.lock"]
```

```toml
# 003-postgres/manifest.toml (up.sh and down.sh next to it)
version = 1
scope = "shared"
down_timeout = "30s"

[ports]
PGPORT = "auto"
```

```toml
# 004-tools/manifest.toml: aliases, functions, completions (up.sh next to it)
version = 1
interactive = true

[aliases]
t = "just test"
```

```toml
# 005-aws/manifest.toml: shared by every folder using this profile
version = 1
scope = "shared"
lease = "aws-sso:dev"
```

### `.foldl/foldl.toml`

Optional, folder-level settings. Unknown keys are errors.

| Key | Type | Default |
|---|---|---|
| `version` | integer | required, `1` |
| `[trust] reuse` | `"none"` \| `"repo"` | `"none"` |

### Validation

Run by `foldl check`, and on `allow`:

- The schema above, including keys allowed only for one scope.
- `shared` requires a custom `down`. foldl warns on a `session` migration
  whose `up` runs commands with known effects outside the shell (such as
  `docker`, `kubectl config use-context`, `ssh-agent`) but has no custom
  `down`.
- Scripts need a shebang naming `bash`, `zsh` or `sh`.
- Referenced files must live inside the migration directory, so the trust
  hash covers everything foldl runs. foldl warns when a script references
  paths outside it, since those cannot be hashed.
- `nix-flake` without `nix` on `PATH` is a warning.

There is no `source_up` built-in: every ancestor's `.foldl/` already loads.

## Shells

Supported in v1: **bash and zsh**.

Migrations run in a subprocess of the shell named by their shebang (see
[Scripts and shebangs](#scripts-and-shebangs)). foldl diffs the environment
before and after, then prints
the changes for the user's interactive shell. Both bash and zsh take
`export`/`unset` with `$'…'` quoting, so there is one output layer to test.
Keeping these migrations out of the user's process means adding fish or
nushell later only needs a new hook and output module. `interactive`
migrations are the one exception, below.

### Interactive migrations

Aliases, functions and completions can't cross a process boundary, so
`interactive` migrations are sourced into the user's shell by the hook:

1. Record the shell's current aliases, functions and completions (`alias`,
   `declare -f` / `typeset -f`, `complete -p` / `$_comps`).
2. Source the migration's `up`, then apply any `[aliases]` table.
3. Record again. The difference is the derived down: on leave, remove what
   was added and restore any definitions it replaced.

Interactive migrations use the same file names as every other migration.
`interactive = true` only changes where `up` runs.

#### Hook protocol

The hook is a shell function that runs `eval "$(foldl hook-step <shell>)"`
at each prompt. For ordinary transitions, `hook-step` prints `export`,
`unset` and alias lines and that's all. When a transition includes an
interactive migration, it prints a script that does the work in the user's
shell and reports back:

1. Write the "before" snapshot (`alias`, `typeset -f` / `declare -f`,
   `complete -p` / `$_comps`) to `sessions/<id>/interactive/<name>.before`.
2. `source` the saved copy of `up.sh`, recording its exit status.
3. Write the "after" snapshot to `<name>.after`.
4. Call `foldl hook-step <shell> --interactive-done <name> --status <n>`,
   which diffs the snapshots, stores the diff as the migration's derived down,
   and prints the next step (more migrations, or a rollback if the status was
   non-zero).

Snapshot files are mode 0600 and deleted after the diff. Cost: two `foldl`
calls per interactive migration, plus the snapshot itself; with a large
function set (e.g. oh-my-zsh) `typeset -f` can take tens of milliseconds,
paid only when an interactive migration loads, not on every prompt.

If `up.sh` fails, the partial changes it made are reversed from the diff, and
the transition rolls back like any failed `up`.

### Scripts and shebangs

Scripts are always named `.sh`, and their shebang says which shell they are
written for. There is no `shell` field in the manifest and no per-shell file
names. Editors and shellcheck pick the dialect up from the shebang as usual.

- A shebang is required (`foldl check` errors without one), and must name
  `bash`, `zsh` or `sh`. `foldl new` writes `#!/usr/bin/env bash`.
- foldl reads the shebang itself rather than executing the file, because it
  has to source the script and capture the environment afterwards. The
  executable bit is therefore not needed.
- The shebang is honored as written: `#!/usr/bin/env bash` uses the first
  `bash` on `PATH`; `#!/bin/bash` uses that file, which on macOS is bash 3.2.
  foldl warns when the bash it ends up with is older than 4.

Interactive migrations are sourced into the user's shell, where a shebang
would normally be ignored. foldl treats it as a compatibility declaration:

- `#!/usr/bin/env bash` or `zsh`: sourced only in that shell, and skipped in
  the other with a note in `foldl status`.
- `#!/bin/sh`: portable, sourced in both.

Code that differs between bash and zsh either branches inside one portable
script (`if [ -n "$ZSH_VERSION" ]`) or is split into two migrations, one per
shell.

### Hooks

- **zsh:** `add-zsh-hook precmd` and `chpwd`, never replacing `precmd`, so
  prompt frameworks keep working.
- **bash:** add to the string form of `PROMPT_COMMAND` without overwriting
  it. macOS ships bash 3.2, which lacks the 5.1 array form.

Escaping (newlines, quotes, `$`, non-UTF-8) is tested by generating values,
sending them through the output layer and reading them back in each shell.

## Shared lifecycle

### Locks

Per lease, under `$XDG_STATE_HOME/foldl/leases/<lease-hash>/`:

- `lease.lock`: every session in the folder holds a **shared** `flock`.
- `mutex.lock`: held briefly around enter and leave transitions, so two
  shells that `cd` at the same moment can't both decide they are first.

In short, under the mutex: on enter, an exclusive non-blocking lock means
"first" (run the shared `up`s), otherwise join with a shared lock; on leave,
release, then an exclusive non-blocking lock means "last" (run the shared
`down`s). Who holds the lock between transitions is described in
[Holder processes](#holder-processes).

The kernel releases a lock when its holder dies, so crashed sessions are
cleaned up without bookkeeping: the next transition sees the true count.

Shared `up`s and `down`s run while the mutex is held. A session entering
during a teardown waits for it instead of racing it. To keep a hanging `down`
from blocking everyone, `down_timeout` (default 30s) kills it, marks the lease
dirty and releases the mutex.

Required test cases, from bugs in the closest competitor:

- Two sessions entering at the same moment start one instance, not two that
  restart each other ([flox#4791](https://github.com/flox/flox/issues/4791)).
- A session entering while the last one's `down` runs gets a fresh `up`
  after the `down` finishes, never overlapping it
  ([flox#4781](https://github.com/flox/flox/issues/4781)).

### Holder processes

The shell does not hold the lock fd itself, because:

- `flock` locks belong to the open file, and children inherit fds. Any
  `npm run dev &` started from the shell (or a service started by `up`) would
  keep the lease alive, and a service holding its own lease would block its
  own `down` forever. Shells can't set close-on-exec on an fd.
- Agents have no long-lived shell; each command is a new process.

Instead, small holder processes keep the locks. They are not daemons: each
belongs to one session and exits when the session's parent process dies
(kqueue `NOTE_EXIT` on macOS, `pidfd` on Linux) or when told to.

**Lease holders: one per (session, lease).** The short-lived `foldl`
invocation does the lock work itself and hands the open lock to a holder by
**fd inheritance**: a lock belongs to the open file description, and a child
spawned with that fd (without close-on-exec) shares it. No IPC is needed.

Enter, under the lease's mutex:

1. `foldl` opens `lease.lock` and tries `LOCK_EX | LOCK_NB`.
2. If granted, this session is first: run the shared `up`s (their processes
   never inherit the fd), then convert to `LOCK_SH`.
3. If not granted, take `LOCK_SH` and join.
4. Spawn `foldl hold lease --parent <pid> --fd <n>` with the fd inherited,
   record its PID in the session state, and exit. The lock now lives as long
   as the holder.

Leave, under the mutex:

1. Send the session's lease holder SIGTERM and wait for it to exit. Its lock
   goes with it.
2. Open `lease.lock` afresh and try `LOCK_EX | LOCK_NB`. If granted, this
   session was last: run the shared `down`s, then close. Otherwise report the
   remaining holders and close.

Converting between exclusive and shared is not atomic with `flock`, which is
why every transition takes the mutex first.

Each lease holder writes a sidecar file in the lease's `holders/` directory
with its session id and its parent's PID, process name, cwd and tty, used to
report holders, since `flock` itself can't list them portably.

**Session holder: one per session**, spawned at the first migration with a
custom `down`. It holds no lock. When the parent dies without leaving (a
closed terminal, a crash), it runs that session's saved custom downs, best
effort and logged, then exits. That way an `ssh-agent` started by a session
migration doesn't outlive its terminal. The derived downs need no action: the
shell they applied to is gone. Lease holders of a dead parent simply exit;
the next transition on that lease sees the true count.

Passing the fd over a Unix socket (`SCM_RIGHTS`) would allow one holder per
session holding every lease, at the cost of a socket protocol. Inheritance is
simpler, and the extra processes are about 1 MB each.

### Joining sessions and env

If a shared `up` exports variables (e.g. `PGPORT`), only the first session
runs it. foldl records that env diff in the lease state, and joining
sessions replay it. Each session reverses its own copy on leave; the shared
`down` still runs only for the last session.

### Ports

Running the same services in several worktrees at once needs different
ports. Each `"auto"` entry in `[ports]` is assigned a free port as late as
possible: immediately before its migration's `up` runs. That keeps the window
in which something else could take the port small. foldl exports it to the
`up` and `down`, stores it in the lease state, and replays it to joining
sessions like any other env. Allocations are recorded in a registry under a
lock, so two leases never pick the same port. The port is released when the
lease ends.

### Saved downs

When a migration's `up` runs, foldl copies the whole migration directory into
the owner's state: the lease state for `shared` migrations, the session state
for `session` ones. The `down` that runs later, and the derived inverse it is
paired with, come from that saved copy. Editing the file, switching branches
or deleting the folder cannot change or lose the teardown, and a modified
`down` can't bypass trust.

### Failed downs

A failed `down` marks the lease **dirty**. The saved `down` is kept, and
`foldl status` shows it, with `foldl down --force <name>` to retry.

A session entering a dirty lease is asked what to do:

```
foldl ! postgres: last down failed at 10:42 (exit 1), see `foldl logs postgres`
  [r] retry the down, then up
  [s] skip the down and run up
  [a] abort, don't load this folder
```

Non-interactive runs never prompt. They use `on_dirty` from the user config
(`retry`, `skip` or `abort`; default `abort`) and report what they did.

### Lease keys

`lease = "folder"` (default) keys the lease on the real path, so each git
worktree gets its own instance. `lease = "repo"` keys it on the repository's
common git dir, so worktrees share one instance. Any other string is a custom
key shared by every folder that uses it, for resources that belong to neither
a folder nor a repo, such as a cloud login (`lease = "aws-sso:dev"`).

### Caches

Caches for slow built-ins (e.g. `nix-flake` evaluation) are keyed on content
(the flake and its lock) rather than the folder path, so a new worktree on the
same commit reuses them instead of starting cold.

## Trust

`foldl allow` hashes the whole `.foldl/` directory, local migrations
included. Before allowing, it shows what changed since the last allow, file by
file, as a diff. Non-interactive runs never prompt; an untrusted folder fails
with the command that fixes it.

By default trust is keyed on the folder's path, as in direnv. A repository
can opt in to trust **keyed on content** in `.foldl/foldl.toml`:

```toml
[trust]
reuse = "repo"     # default "none"
```

With `reuse = "repo"`, a folder whose `.foldl/` hash matches one already
allowed is trusted, provided both folders' repositories share a root commit.
New worktrees and clones, which agents create without running `allow`, work
immediately, while a branch that changes a migration still needs an `allow`.
The setting is part of the hashed content, so turning it on needs an `allow`
too. Shallow clones have no root commit and fall back to path-based trust.

A folder has one of three states:

- **allowed**: load it.
- **denied**: don't load it, and say so.
- **ignored** (`foldl ignore`): don't load it and don't mention it, for
  `.foldl/` directories that are templates or meant to be used elsewhere.

## Changes while loaded

- A `session` migration whose hash (or a `watch`ed file) changed is reloaded
  at the next prompt: run the saved `down`, then the new `up`.
- A `shared` migration that changed is not restarted under other sessions.
  foldl prints `postgres changed; run 'foldl redo postgres'`, and `redo`
  warns if other sessions hold the lease.

## CLI

```
foldl hook bash|zsh           print the shell hook
foldl allow | deny | ignore   set the folder's trust state
foldl check                   validate manifests
foldl new <name> [--shared|--interactive] [--local]
                              create the next numbered migration
foldl status [--all] [--json] [--events]
                              migrations, state, holders, dirty leases
foldl up|down|redo <name>     run a migration by hand
foldl logs <name>             full output of a migration's last run
foldl exec [dir] -- cmd       load, run cmd, unload
foldl export bash|zsh|json    print the environment for the folder
foldl session start|stop      named session for agents (--id, --parent)
foldl integrate claude        install Claude Code hooks
foldl import direnv           convert .envrc (see below)
```

Internal, used by hooks and holders: `foldl hook-step <shell>`,
`foldl hold lease|session`.

## Output

One line per transition, with `-q` and `-v` to adjust:

```
foldl ↑ env (+3 vars) · postgres (started, 1.2s)
foldl ↓ env · postgres kept: in use by zsh 4121 (~/code/app, ttys003), claude 5530
```

- Holders are shown with process name, folder and terminal, not bare PIDs.
- `cd` blocks while `up`s run. After 300ms foldl shows a progress line naming
  the migration.
- Script output goes to a per-migration log. On failure foldl shows the last
  ~10 lines and the `foldl logs` command. Logs are created with mode 0600,
  since migrations may print secrets; there is no redaction in v1.

`foldl status`:

```
~/code/app  (3 sessions)
  001 env        session up
  002 postgres   shared  up     holders: zsh 4121, claude 5530
  003 tunnel     shared  DIRTY  down failed 10:42 → foldl down --force tunnel
```

## Agents

- Never prompt. Untrusted or invalid folders fail with a clear message and a
  fix.
- `--json` on `status`, `exec` and `export`, with stable error codes.
- `foldl exec -- cmd` is the default for one-off commands. Inside a session
  that already has the folder loaded, `exec` joins it instead of loading
  again.
- For long agent sessions, `foldl session start --parent <agent pid>` makes
  the agent process the session's parent, so short-lived Bash calls don't
  each enter and leave. For Claude Code, `foldl integrate claude` finds the
  `claude` process at `SessionStart`.
- `foldl integrate claude` installs `SessionStart` and `CwdChanged` hooks
  that write `foldl export` output to `CLAUDE_ENV_FILE`.

## Switching from direnv

foldl does not read `.envrc`. `foldl import direnv` converts it:

1. **Translate.** Known direnv stdlib calls (`use flake`, `dotenv`,
   `PATH_add`, plain `export`) become built-ins or `[env]` entries. Anything
   else is kept verbatim as a `session` migration and flagged for review.
2. **Ask about shared.** Lines that start things outside the shell (a
   trailing `&`, `nohup`, `docker`, `compose`, `pg_ctl`, `ssh -f`,
   `systemctl`) get a prompt: shared or not, and if shared, the stop command,
   with a suggestion where foldl knows one.
3. **Check parity.** Compare `direnv export json` with `foldl export json`
   and show any differences.
4. **Disable direnv.** Default `direnv deny` (local to the user). Removing
   `.envrc`, which switches the whole team, must be chosen explicitly.
5. **Set up foldl.** Write `.foldl/`, `foldl allow`, load.

`--yes`, `--shared=<name>` and `--dry-run` make it usable without the wizard.

## Implementation

Single static binary in Rust. The prompt hook's no-op path is one `stat` of
`.foldl/` plus a cached hash compare.

### Nix

foldl supports Nix natively but never requires it.

- **Development** uses a Nix flake for the toolchain and dependencies.
- **Distribution:** the flake (with an overlay and a module for home-manager
  and NixOS), plus static binaries on release pages, Homebrew and
  `cargo install`. None of the non-Nix channels need Nix at build or run time.
  The MVP builds and tests these artifacts locally (flake package, overlay,
  modules, a static Linux binary, a Homebrew formula file) but publishes
  nothing; publishing is post-MVP.
- **Runtime:** foldl never calls `nix` unless a migration uses a Nix built-in.
  Detection happens at runtime, not through a Cargo feature, so there is one
  binary for everyone.
- **The `nix-flake` built-in** replaces nix-direnv rather than wrapping it:
  1. runs `nix print-dev-env --json` and caches the result, keyed on content
     (`flake.nix`, `flake.lock` and files listed in `watch`), so worktrees on
     the same commit share it;
  2. registers a GC root, so garbage collection doesn't delete the shell in
     use;
  3. keeps the last good result and falls back to it, with a warning, when
     evaluation fails.
- **Without Nix**, a migration using `nix-flake` fails like any other `up`,
  with a clear message (`nix-flake needs nix on PATH; see <docs>`), and
  earlier migrations are rolled back. `foldl check` warns ahead of time. The
  direnv importer still translates `use flake`, since the folder may be used
  by others who have Nix.
- Nix-specific code lives in its own module behind the built-in interface, so
  the core never depends on it.

### Functional core, imperative shell

The name is a promise: foldl is a left fold over events.

```rust
fn step(state: State, event: Event) -> (State, Vec<Effect>)
```

- **`step` is pure.** It decides what should happen (which migrations to
  unload and load, whether this session is first or last, what to print) and
  returns it as data. It never touches the filesystem, processes or locks.
- **A small interpreter performs the `Effect`s** (run a script, take a lock,
  write state, print) and turns each result back into an `Event`, which is
  folded into the next state. All I/O lives here and nowhere else.
- **The program is `events.fold(state, step)`.** Tests replay event
  sequences, including the Flox race cases, with no processes, locks or
  timing, and assert on the effects produced.

Core data is values with algebraic operations, so most behavior is a law that
property tests (`proptest`) can check:

- **`EnvDiff`** maps each variable to `(before, after)`. `invert` swaps them,
  and `compose` chains diffs. Law: applying `d` then `invert(d)` gives back the
  original environment. Derived downs are just `invert`.
- **Loading is a left fold, unloading the right fold of inverses.** Moving
  between folders keeps the common prefix of the migration stack, folds the
  inverses of the rest from the right, then folds the new ups from the left.
- **The same holds for interactive state** (aliases, functions,
  completions): snapshot before and after, diff, invert.
- **Rendering is a pure function** `render(shell, diff) -> String`. Law:
  evaluating the output in that shell and reading the values back returns the
  diff's `after` values.
- **Persisted state is immutable**: written to a new file and renamed into
  place, never edited.

Style rules:

- Linear code: keep control flow straight. Prefer data transformations,
  exhaustive `match` and early returns over nested conditionals, so that
  100% line coverage leaves no untested branches in practice.
- Iterators and `fold` over loops with mutable accumulators; owned values over
  shared mutation; enums with exhaustive `match` for states and events;
  newtypes for ids (`LeaseId`, `MigrationId`, `Hash`).
- No global mutable state. Configuration is passed in, not read from the
  environment deep in the call tree.
- Idiomatic Rust, not Haskell in Rust: no higher-kinded type emulation or
  monad crates. Persistent collection crates only if profiling shows cloning
  matters.
- Effects that can't be made pure (locks, holder processes, signals) are
  kept small and behind the interpreter, so they can be swapped for fakes in
  tests.

### Event model

Events are data, not callbacks. Each `Event` variant has one arm in `step`,
which plays the role of a libevent callback but returns new state and effects
instead of mutating and doing I/O ("sans-IO"; see `quinn-proto` for a Rust
reference).

There is no async runtime. Most runs last milliseconds and are sequential, so
the interpreter is a plain synchronous loop. Two places wait on more than one
thing and use a small hand-written wait (kqueue on macOS, `poll` on Linux, via
`rustix`):

- the holder process: parent exit or a signal;
- a running migration: child exit or SIGINT.

If foldl ever gets a daemon or file watching, consider `mio`, still feeding
readiness into `step` as events.

How migrations map onto it:

| Migration concept | Event model |
|---|---|
| loaded migration stack | part of `State` |
| `cd` into a folder | `Event::Entered(path)`; `step` diffs stacks and plans downs, then ups |
| run an `up` / `down` | `Effect::Run`, its result comes back as `Event::Ran` / `Event::Failed` |
| roll back after a failed `up` | the `Failed` arm returns the downs of the migrations already run |
| first or last session | `Effect::TryExclusive`, answered by `Event::Lock(granted)` |
| interactive migration | `Effect::Emit` of shell code that sources the file and calls foldl back with the after-snapshot, which arrives as an event in the next invocation |
| state between invocations | the fold's value, persisted as a `state.json` snapshot; events also appended to `events.jsonl` for humans |

State is per session and persisted between invocations: each run loads it,
folds in new events and writes it back. Sessions don't see each other's
events; shared facts (who holds a lease, saved downs, ports) are read and
written through effects on the lease state.

The derived part of a down is a true inverse of what foldl observed, so the
apply-then-invert law holds for it. A custom `down`, in either scope, is user
code: undoing what its `up` did outside foldl's view is the author's promise,
not something foldl can check.

### State storage

**Snapshots are the source of truth; an event log is kept for humans.**

- Each session and each lease has a `state.json` snapshot: the fold's
  accumulated value, persisted. Reading one small file keeps the prompt path
  fast; replaying a log on every prompt would not.
- Snapshots are written to a temporary file and renamed into place, never
  edited. Session state has one writer (its shell's `foldl` invocations,
  then its session holder after the shell dies). Lease state is written only
  under the lease's mutex.
- Next to each snapshot, `events.jsonl` appends every event folded into it.
  `foldl status --events` and bug reports read it; foldl never rebuilds state
  from it. It is truncated to the last 1000 events.
- Format: JSON via `serde`, each file carrying `version`.

Layout:

```
$XDG_STATE_HOME/foldl/              (default ~/.local/state/foldl)
  sessions/<session-id>/
    state.json  events.jsonl
    saved/<migration>/              saved copies of session migrations
    interactive/                    snapshot files while sourcing
    logs/<migration>.log
  leases/<lease-hash>/
    lease.lock  mutex.lock
    state.json  events.jsonl
    saved/<migration>/
    holders/<session-id>.json
    logs/<migration>.log
  ports/registry.json  ports/registry.lock
$XDG_DATA_HOME/foldl/               (default ~/.local/share/foldl)
  trust/                            allowed, denied and ignored entries
$XDG_CACHE_HOME/foldl/              (default ~/.cache/foldl)
  nix/<content-hash>/               nix-flake cache and GC root
```

Trust lives under the data directory so clearing state or cache never
revokes it. A session directory is removed when its session ends; a lease
directory when its lease ends clean.

### Exit codes and errors

Exit codes are stable from the first release. They follow `sysexits.h` where
one fits, with foldl's own codes from 80.

| Code | Name | Meaning |
|---|---|---|
| 0 | `ok` | success |
| 64 | `usage` | bad command line |
| 69 | `unavailable` | a needed program is missing (`nix`, `bash`, `zsh`) |
| 70 | `internal` | a bug in foldl |
| 75 | `dirty` | a dirty lease and `on_dirty = "abort"` |
| 77 | `untrusted` | folder not allowed |
| 78 | `invalid` | manifest or `foldl.toml` failed validation |
| 80 | `up_failed` | an `up` failed; earlier migrations were rolled back |
| 81 | `down_failed` | a `down` failed; the lease is dirty |
| 82 | `timeout` | an `up` or `down` exceeded its timeout |
| 83 | `denied` | folder explicitly denied |
| 130 | `interrupted` | Ctrl-C (128 + SIGINT) |

`foldl exec -- cmd` returns the command's own exit status once the folder has
loaded; codes above apply only to foldl's own failures before or after it.

With `--json`, errors are printed as one object on stdout:

```json
{"error": {"code": "untrusted", "exit": 77,
  "message": "~/code/app is not allowed",
  "fix": "foldl allow ~/code/app"}}
```

The `code` strings are as stable as the numbers.

### Versioning

Manifests, `.foldl/foldl.toml` and every state file carry a `version` field
from the first release. Newer foldl versions keep reading older files.

### User config

`~/.config/foldl/config.toml` holds per-user settings: verbosity, default
timeouts, `on_dirty` for non-interactive runs, and folder prefixes that are
trusted automatically (like direnv's whitelist).

## Testing

**100% line coverage is required**, enforced by `nix flake check` with
`cargo llvm-cov --fail-under-lines 100`. Branch coverage is not measured;
the linear-code style rule keeps lines and branches aligned. Exclusions are
not allowed by default; any exception needs a comment explaining why it
cannot be tested.

- **Pure core** (`step`, `EnvDiff`, `render`, planning): unit and property
  tests (`proptest`) for the laws in
  [Functional core](#functional-core-imperative-shell), plus replayed event
  sequences for the race cases.
- **Interpreter and effects**: integration tests with a real instrumented
  binary, so they count towards coverage.
- **Shells**: bash 3.2, bash 5 and zsh, pinned through Nix, running the
  escaping round-trip and hook tests.
- **Without Nix**: a CI job runs the integration suite with no `nix` on
  `PATH`, using the system shells (including macOS's `/bin/bash` 3.2), to
  catch any hidden dependency on Nix.
- **Platforms**: macOS (kqueue) and Linux (`pidfd`) code is covered on its own
  platform. Until CI exists, coverage is measured on every platform available
  to the developer, and the reports are merged.

## Post-MVP

Planned after dogfooding, driven by user feedback:

- Fast path when loading every ancestor: each prompt `stat`s every parent
  folder, which may need a cache to stay near 1 ms. The MVP measures it.
- **Editor integrations** (VS Code, JetBrains, Emacs) built on
  `foldl export json`.
- **Coexisting with mise**: ordering of the two prompt hooks, and possibly a
  `mise` built-in that applies `mise env`.
- More shells (fish, nushell).
- Publishing: GitHub releases with static binaries, a Homebrew tap,
  crates.io, and CI on macOS and Linux.

## Implementation decisions

Decisions made during implementation that this document did not cover, or
corrections to contradictions found in it. Each entry: date, the question,
the decision, and why. Existing spec text above is not rewritten; it is
corrected here.

- **2026-11-01 · Quoting in `render`.** Q: the spec says `$'…'` quoting.
  Decision: values that are valid UTF-8 are single-quoted (`'\''` for a
  quote); only invalid UTF-8 uses `$'\xHH'`. Why: single quotes keep every
  byte but NUL literally in bash 3.2, bash 5 and zsh, so there is no escape
  table to get wrong; the round-trip property test covers both forms.
- **2026-11-01 · Which ancestors load.** Q: what happens below a folder
  that can't load? Decision: the stack is every ancestor from `/` down,
  stopping at the first folder that is untrusted, denied, invalid or whose
  `up` failed; `ignored` folders are skipped and don't stop it. Why: an
  inner folder may depend on its parent's env, and loading it without the
  parent would be a silent partial load.
- **2026-11-01 · Rollback scope.** Q: a failed `up` rolls back "the
  migrations that already ran"; does that include outer folders? Decision:
  only the failing folder's migrations; outer folders stay loaded. The
  folder is not retried until its fingerprint changes or the session leaves
  it. Why: "the folder is left unloaded" names one folder, and retrying at
  every prompt would rerun a failing script forever.
- **2026-11-01 · Edits to a loaded folder.** Q: editing `.foldl/` changes
  its hash, so it is no longer allowed; unload it? Decision: a loaded
  folder that changes and is no longer allowed (or no longer valid) stays
  loaded as it was, with a one-time notice to run `foldl allow`; after the
  allow, only the changed migrations reload. Why: unloading would stop
  shared services because a comment changed, and "Changes while loaded"
  describes reloading per migration.
- **2026-11-01 · Per-folder names in state.** Q: saved copies and logs are
  `saved/<migration>` and `logs/<migration>.log`, but two folders in one
  stack may both have a migration named `env`. Decision: use
  `<name>-<folder key>` (first 12 hex digits of the sha256 of the folder
  path) for `saved/`, `logs/` and the new `data/` directories. `foldl logs
  <name>` resolves the name in the current stack, innermost first.
- **2026-11-01 · `FOLDL_STATE`.** Q: the spec names "the migration's state
  directory" without a location. Decision: `<owner>/data/<name>-<folder
  key>/`, where the owner is the session or lease directory. It lasts as
  long as the session for `session` migrations and is kept across lease
  lifetimes for `shared` ones, so a database's data directory survives a
  restart. Why: services need persistent data; sessions are ephemeral.
- **2026-11-01 · `[env]` interpolation details.** Q: what about `$` not
  followed by `{` or `$`, and unknown names? Decision: a lone `$` is
  literal; `${NAME}` with a name other than `FOLDL_ROOT`, `FOLDL_STATE`,
  `HOME` or a `[ports]` name is a validation error. `path_add` entries must
  be relative. Why: errors at `foldl check` beat surprising values.
- **2026-11-01 · Non-interactive `[aliases]` down.** Q: how is an alias
  from a non-interactive migration undone? Decision: `unalias`; a shadowed
  alias is not restored (that needs a snapshot of the shell, which only
  interactive migrations take). Why: `hook-step` runs in a subprocess and
  can't see the shell's aliases.
- **2026-11-01 · `.DS_Store` in `.foldl/`.** Q: "anything else in `.foldl/`
  is an error". Decision: `.DS_Store` is ignored (still hashed). Why:
  Finder creates it; failing on it would be noise.
- **2026-11-01 · Licenses.** Q: cargo-deny allows only permissive licenses;
  `unicode-ident` (via serde's derive) is `Unicode-3.0`. Decision: allow
  `Unicode-3.0`, which is permissive. Why: there is no serde without it.
- **2026-11-01 · User config keys.** Q: the spec lists the settings but no
  names. Decision: `version`, `verbosity` (`quiet`/`normal`/`verbose`),
  `timeout`, `nix_timeout`, `down_timeout`, `on_dirty`,
  `trusted_prefixes` (`~/` expanded) and `port_range` (`[low, high]`,
  default `[20000, 29999]`). Unknown keys are errors, as in manifests.
- **2026-11-01 · `hook-step` arguments.** Q: how does `hook-step` know
  which shell it serves? Decision: the hook passes `--pid $$`. A session
  whose recorded parent differs is inherited. Why: `getppid` is the shell
  only when the shell execs the command substitution directly, which
  neither bash nor zsh promises.
- **2026-11-01 · Ctrl-C and the hook.** Q: verified in real terminals,
  bash and zsh both discard `$(...)` output on SIGINT, so a rollback would
  never reach the shell. Decision: the hook ignores SIGINT in the shell
  while `hook-step` runs; foldl still receives it and rolls back. zsh uses
  `localtraps`, so a user's INT trap survives; bash resets INT to its
  default afterwards, which drops a user's own INT trap (documented
  limitation; restoring it would cost a fork per prompt).
- **2026-11-01 · How scripts run.** Q: details the spec leaves open.
  Decision: a script runs as `<shell> -c <driver>` in its own process group
  (so a timeout kills everything it started; Ctrl-C is forwarded), with
  stdin from `/dev/null` and stdout and stderr in its log. Ups report their
  environment through an EXIT trap that runs `foldl __dump-env`, so an
  `exit` inside the script still reports. Variables the shell manages
  (`_`, `SHLVL`, `PWD`, `OLDPWD`, `PS1`, `PS2`, `SHELLOPTS`, `BASHOPTS`,
  `COLUMNS`, `LINES`, `BASH_FUNC_*`) and foldl's own are not tracked. A
  script can't prompt on the terminal; its output is in `foldl logs`.
- **2026-11-01 · Inherited sessions.** Q: "discards the inherited state and
  loads from scratch", but the child's env still holds the parent's
  changes. Decision: the new session first reverts the parent's cumulative
  diff on its env, for each variable still holding the parent's value,
  then loads. Why: otherwise `path_add` doubles and a child that leaves the
  folder keeps the parent's variables.
- **2026-11-01 · Ending sessions without a holder.** Q: a shell that exits
  with nothing to tear down has no holder to remove its session directory.
  Decision: creating a session sweeps directories whose parent and holder
  are both dead.
- **2026-11-01 · `exec` and `export` sessions.** Q: which session do they
  load into? Decision: they never change an interactive shell's session,
  whose env would stop matching its state. `exec` joins a session that
  already has the target's stack loaded; otherwise it loads into an
  ephemeral session, reverting the inherited one's changes first, and
  unloads after the command. `export` reuses an agent session (no shell,
  from `foldl session start`); otherwise it loads, prints and unloads, so
  services an `up` starts are stopped again. A missing command is exit 69.
- **2026-11-01 · `allow` doesn't prompt.** Q: "shows what changed ...
  before allowing": confirm? Decision: `allow` prints the diff and allows;
  typing `foldl allow` is the confirmation. It also validates first and
  refuses invalid folders (exit 78).
- **2026-11-01 · Prompt fast path token.** Decision: the token hashes each
  ancestor `.foldl/`'s metadata, its watched files and the trust store's
  directory mtime. Any `allow`/`deny` makes every session re-plan once.
- **2026-11-01 · Rollback output.** Decision: migrations unloaded by a
  rollback leave the `↑` line and show as `foldl ↩ rolled back a · b`.
- **2026-11-01 · Where `foldl new` writes, and the first-run choice.**
  Q: unspecified details. Decision: `new` uses the nearest folder with a
  `.foldl/`, else the git top level, else the working directory; numbers
  are zero-padded to three digits (or the widest existing prefix). The
  shared-or-private question is asked on a terminal; `--private` and
  `--committed` answer it up front, and without a terminal the default is
  committed. Private adds `/.foldl/` to the common git dir's `info/exclude`.
  Why: agents run without a terminal and the committed setup is the one
  the rest of the design favours.
- **2026-11-01 · Ignoring `.local` migrations.** Q: "`foldl new --local`
  adds the ignore entry": where? Decision: `/.foldl/*.local/` in the
  repository's `.gitignore` when `.foldl/` is committed; nothing when it is
  private (already excluded). Why: `.gitignore` is shared, so teammates'
  local migrations are ignored too; `.foldl/` itself may not hold a
  `.gitignore` file, since anything but migrations and `foldl.toml` is an
  error there.
- **2026-11-01 · Content trust index.** Decision: `allow` of a folder with
  `reuse = "repo"` records `trust/content/<hash>.json` with the
  repository's root commits; another folder with the same `.foldl/` hash is
  trusted if its repository shares one of them. `deny`/`ignore` on a path
  still win.
- **2026-11-01 · Lease state is keyed by migration name.** Q: a lease may
  hold several shared migrations, and a custom key may be used from
  several folders. Decision: the lease's `state.json` lists entries by
  migration name. Folders sharing a custom lease should use the same
  migration name for the same resource; a different name is a different
  resource in the same lease. Why: lease keys name resources, and repo
  leases must match the same migration across worktrees whose paths
  differ.
- **2026-11-01 · Leaving a lease, step by step.** Q: the spec releases the
  hold, then tries `LOCK_EX | LOCK_NB`; with several migrations in one
  lease the first down would see this session still holding it. Decision:
  every shared down releases this session's hold, tests for "last", runs
  the down if so, and takes a new hold at commit if the session still has
  other migrations of that lease loaded. Why: correct for both a full
  leave and unloading one migration by hand.
- **2026-11-01 · Closed terminals tear down shared migrations too.** Q: the
  spec has the session holder run "that session's saved custom downs" and
  lease holders "simply exit", which would leave a service running when
  the last terminal is closed, against the goal of teardown "including
  when sessions crash or terminals are closed". Decision: the session
  holder runs the session's whole leave: custom downs and the lease
  protocol, so a dead last session's shared downs run. If both holders
  die too (e.g. a reboot), the next session that gets the lease
  exclusively finds entries nobody holds and runs their saved downs before
  its own ups.
- **2026-11-01 · What a lease directory keeps.** Q: "a lease directory [is
  removed] when its lease ends clean". Decision: when no entries remain,
  `state.json` is removed but `mutex.lock`, `lease.lock`, `data/` and
  `logs/` stay. Why: unlinking a lock file another session has just opened
  would give two sessions different locks; `data/` holds services' data
  (`FOLDL_STATE`).
- **2026-11-01 · Saved copies of shared migrations.** Decision: they live in
  the lease directory and belong to the lease: sessions that joined never
  remove them, and a successful shared down does.
- **2026-11-01 · Port selection.** Decision: candidates cover the configured
  range starting at an offset derived from the lease, migration and name
  (so a restart tends to get the same port), skipping ports in the
  registry and ports that can't be bound on 127.0.0.1 right now.
- **2026-11-01 · `foldl up | down | redo`.** Q: these run "by hand", but a
  subprocess can't change the shell's env. Decision: they mark the
  migration in the session (held down, back up, or reload), and the next
  prompt (or `foldl export` for agents) carries it out. `redo` of a shared
  migration restarts it even while other sessions hold the lease, with a
  warning naming them; joined sessions keep their copy of the env until
  they re-enter. `down --force <name>` retries a dirty lease's down right
  away, needing no session. As with any change, unloading a migration
  also unloads and reloads the ones after it in the stack.
- **2026-11-01 · Holder details.** Decision: holders run in their own
  process group with stdio on `/dev/null`, ignore SIGINT and SIGHUP, and
  stop on SIGTERM or when the parent exits. A session holder writes what
  it did to `$XDG_STATE_HOME/foldl/holder.log`. Holders are described with
  the parent's process name and terminal from `ps`. The dirty-lease
  question is read from `/dev/tty`; without one the answer is "abort".
- **2026-11-01 · Lease I/O errors.** Decision: if a lease file can't be
  read or written, an up fails like any other (its folder rolls back); a
  down still applies the derived down, reported as "(lease error)".

## Prior art

See [COMPETITORS.md](COMPETITORS.md) for the full comparison.

- [direnv](https://direnv.net/): env load/unload via diff; unload hooks
  declined in [#129](https://github.com/direnv/direnv/issues/129).
- [Flox](https://flox.dev/docs/concepts/services/): services and
  `hook.on-deactivate` stopped by the last activation, tracked by a
  per-environment supervisor; activation is explicit.
- [mise hooks](https://mise.jdx.dev/hooks.html): `enter`/`leave`/`cd`
  hooks, no teardown tracking.
- [shadowenv](https://github.com/Shopify/shadowenv): reversible env in
  `.shadowenv.d/*.lisp`.
- [autoenv](https://github.com/hyperupcall/autoenv),
  [zsh-autoenv](https://github.com/Tarrasch/zsh-autoenv): per-shell leave
  scripts.

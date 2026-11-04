# Security

foldl runs code from the folders you enter. This page says what its trust
check covers, what it doesn't, and how to keep agents from widening it.

## Reporting a vulnerability

Report it privately through
[GitHub security advisories](https://github.com/skogsbrus/foldl/security/advisories/new),
not in a public issue.

## What `foldl allow` covers

Nothing in a folder's `.foldl/` loads until you run `foldl allow`. It hashes
every file in `.foldl/`, local migrations included, and any change to them
needs another `allow`.

- A `down` runs from a copy saved when its `up` ran, so editing it or
  switching branches can't change what runs on leave.
- Scripts must live inside their migration directory.
- A symlink in `.foldl/` makes the folder invalid, unless you set
  `follow_symlinks = true` in [your config](docs/configuration.md).
- `allow` shows a diff of what changed, except for files in `.local`
  migrations, which are shown only as changed.
- With `reuse = "repo"`, trust carries to worktrees and to clones from the
  same remote, not to forks.
- `foldl import direnv` allows the result only if direnv had allowed the
  `.envrc`.

## What it doesn't cover

The hash covers `.foldl/` and nothing else. After you allow a folder, these
can change without another `allow`, and the change takes effect at the next
prompt or the next time the folder loads:

- the `.env` file of the `dotenv` built-in. It can set any variable,
  including ones that run code, such as `PROMPT_COMMAND` or `BASH_ENV`.
- `flake.nix`, `flake.lock` and everything they import, for `nix-flake`.
  A remote reference such as `github:owner/repo` without a pinned revision
  runs whatever that repository holds.
- directories in `path_add`. A `bin/` in the repository shadows commands
  you type, and the shells foldl runs scripts with.
- files listed in `watch`. A change reloads the migration.
- anything a script runs or sources, such as `./scripts/setup.sh`,
  `npm install` or `docker compose up`.

direnv has the same limits. Treat `git pull` or checking out a branch in an
allowed folder like running its code, because it is. `foldl check` warns
about scripts that source files outside their migration.

`trusted_prefixes` in the config trusts every folder under a prefix without
any `allow`. List only directories whose contents you control.

## Secrets

- Logs (`foldl logs`) are not redacted. They are mode 0600, like all of
  foldl's state under `~/.local/state/foldl/`.
- For a folder that is not a git repository, `nix-flake` copies the whole
  folder, `.env` included, into the world-readable `/nix/store`. Inside a
  git repository Nix copies only tracked files.
- An agent session gets the same environment as your shell, secrets
  included. If an agent runs `env`, the values are in its transcript.

## Agents

An agent that can run `foldl allow` can make a folder's code run in your
own shells later, outside whatever sandbox the agent had. Stop agents from
allowing folders on their own. Add this to the instructions file your
agents read: `AGENTS.md` for Codex and most other agents, `CLAUDE.md` for
Claude Code (a `CLAUDE.md` holding `@AGENTS.md` reads the same file):

```markdown
Never run `foldl allow`, `foldl import` or `foldl integrate`. If a folder
is not allowed, stop and ask the user to review `.foldl/` and allow it.
```

Text the agent reads can override its instructions, so also block the
commands in the agent's own permissions where it has them.

Claude Code, in `.claude/settings.json`:

```json
{
  "permissions": {
    "deny": ["Bash(foldl allow:*)", "Bash(foldl import:*)", "Bash(foldl integrate:*)"]
  }
}
```

Codex, in `~/.codex/rules/foldl.rules` (or `.codex/rules/` in a trusted
project). Codex calls rules experimental:

```python
prefix_rule(
    pattern = ["foldl", ["allow", "import", "integrate"]],
    decision = "forbidden",
    justification = "a person reviews .foldl/ before it is allowed",
)
```

Both match on how a command starts, so a command that hides `foldl allow`
behind a variable assignment, `env` or a script gets past them. They stop
an agent following bad instructions, not one working around them.

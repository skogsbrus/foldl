# Competitors and prior art

Status: research snapshot, 2026-11-01. See [DESIGN.md](DESIGN.md) for the
design this is compared against.

## Summary

Every individual piece of foldl already exists somewhere. No tool combines
them, and the combination is what people have asked direnv for since
[#129](https://github.com/direnv/direnv/issues/129).

In short: **direnv, plus Flox's teardown semantics, for any folder,
triggered by `cd`, safe for parallel agents.** Not a new idea. For the
user-facing positioning, see [Personas and positioning](#personas-and-positioning).

## Flox: closest prior art

[Flox](https://flox.dev/docs/concepts/services.md) already ships the hardest
part: services stop when the last activation ends
([`flox deactivate`](https://flox.dev/docs/man/flox-deactivate)).

Flox is GPL-2.0, so per AGENTS.md its source is off limits; everything here
comes from its docs and public issues.

- `hook.on-deactivate` runs when the last attachment of an activation goes
  away (described in [#4781](https://github.com/flox/flox/issues/4781)), the
  same contract as foldl's shared `down`.
- A supervisor process per environment (the "executive", per #4781) tracks
  the activations, with state in a locked `state.json`
  ([#4791](https://github.com/flox/flox/issues/4791)).

| | Flox | foldl |
|---|---|---|
| Trigger | explicit `flox activate` | `cd` (prompt hook) |
| Granularity | one activate/deactivate hook pair | ordered migrations, each with its own scope |
| Partial failure | not described | roll back earlier ups in reverse |
| Tracking | supervisor process per environment | kernel `flock` + one tiny holder per session |
| Requires | adopting Flox environments | any folder, works with any tools |

Before building foldl's lock and holder layer, read Flox's docs and its
issue tracker (not its source) for edge cases worth turning into foldl test
scenarios, such as the two races above.

## Others

- **[direnv](https://direnv.net/):** env diff on load and unload, which is
  solved. Unload hooks were declined in
  [#129](https://github.com/direnv/direnv/issues/129): they need system-wide
  state, are ambiguous with several shells in one folder, and don't run when a
  terminal is closed. foldl's lock design addresses all three.
- **[mise hooks](https://mise.jdx.dev/hooks.html):** `enter`/`leave`/`cd`
  hooks, fired from the prompt. No tracking or undo of what a hook did, and no
  awareness of other sessions.
- **[autoenv](https://github.com/hyperupcall/autoenv) and
  [zsh-autoenv](https://github.com/Tarrasch/zsh-autoenv):** leave scripts per
  shell (opt-in). A second terminal in the folder breaks them.
- **[shadowenv](https://github.com/Shopify/shadowenv):** reversible env in
  `.shadowenv.d/*.lisp`, with trust. No side effects.
- **devenv / [devbox](https://www.jetify.com/docs/devbox/guides/services):**
  services start with an explicit command and outlive the shell.
- **Agent workarounds:** direnv plus Claude Code's `CwdChanged` hook writing
  to `CLAUDE_ENV_FILE`
  ([guide](https://claudeissues.com/issue/42229-docs-guide-direnv-devbox-environment-loading-via-hooks),
  [classmethod](https://dev.classmethod.jp/articles/claude-code-direnv-environment/),
  [agentpatterns](https://agentpatterns.ai/tool-engineering/reactive-environment-hooks/)).
  Loads env, no lifecycle.
- **[daft](https://avihu-daft.pages.dev/recipes/adopting-from-direnv):**
  per-worktree setup and teardown hooks layered on direnv.

## What's new in foldl

1. **Last-session teardown triggered by `cd`, without a whole environment
   manager.** direnv refuses it, mise and autoenv do it naively, and Flox does
   it properly but only behind explicit activation of its own environments.
2. **Ordered migrations, each owned by a session or a shared lease, with
   rollback.**
   No tool found in this space splits setup into steps with different
   lifetimes or rolls back a partial load.
3. **Kernel locks as the session count, with no supervisor.** Simpler than a
   supervisor process, and crashes are handled for free. Using a shared `flock` as a
   reference count is a known Unix idiom; applying it here is what's new.
4. **Agents as first-class users:** sessions with a named owner, `exec`,
   `--json`, never prompting. Others only have workarounds.
5. **Aliases, functions and completions with a derived down.** direnv's
   most-requested feature; possible because v1 targets only bash and zsh.
6. **Worktree-ready by default:** trust keyed on content, ports allocated per
   lease, caches keyed on content.

Not new, but good product work: the direnv import wizard, the trust diff and
the output design.

## Common unsolved requests foldl adopts

From the most-upvoted issues in the trackers of direnv, mise, Flox, devenv,
devbox and shadowenv (👍 counts as of 2026-11-01).

| Request | Where | foldl answer |
|---|---|---|
| Aliases and functions | direnv [#73](https://github.com/direnv/direnv/issues/73) (265 👍, most-upvoted ever), [#109](https://github.com/direnv/direnv/issues/109) (54 👍) | `interactive` migrations with derived down |
| Completion scripts | direnv [#443](https://github.com/direnv/direnv/issues/443) (94 👍) | `interactive` migrations |
| Worktrees for parallel agents: re-trusting, port collisions, cold caches | devenv [#3204](https://github.com/cachix/devenv/issues/3204) | content-based trust, `[ports]` auto allocation per lease, caches keyed on content |
| Services started together restart each other | Flox [#4791](https://github.com/flox/flox/issues/4791) | mutex around transitions; required test case |
| Teardown overlaps the next setup | Flox [#4781](https://github.com/flox/flox/issues/4781) | downs run under the mutex, with `down_timeout`; required test case |
| Local, uncommitted overrides | direnv [#556](https://github.com/direnv/direnv/issues/556) (54 👍) | `.local` migrations |
| Ignore a folder | direnv [#793](https://github.com/direnv/direnv/issues/793) (40 👍) | `foldl ignore` |

Already covered by the original design: verbosity
([#68](https://github.com/direnv/direnv/issues/68), 85 👍), explicit unload
([#260](https://github.com/direnv/direnv/issues/260), 63 👍), unload hooks
([#129](https://github.com/direnv/direnv/issues/129)), cancelling a load
([#1653](https://github.com/direnv/direnv/issues/1653)), unloading when the
config is deleted ([#1548](https://github.com/direnv/direnv/issues/1548)), and
services started on `cd` (devenv
[#80](https://github.com/cachix/devenv/issues/80), 52 👍; mise
[#2282](https://github.com/jdx/mise/issues/2282); Flox
[#2378](https://github.com/flox/flox/issues/2378)).

## Personas and positioning

What foldl contributes, in order of weight:

1. **Correct teardown on `cd`, shared across sessions, without adopting an
   environment manager.** Flox's lifecycle semantics, unbundled from its
   package management and explicit activation.
2. **Parallel agents in worktrees.** Env missing in agent shells, unanswered
   trust prompts, port collisions and cold caches; asked about now
   (devenv [#3204](https://github.com/cachix/devenv/issues/3204)) and
   unanswered.
3. **Table stakes for a direnv replacement:** aliases and functions, local
   overrides, ignore, the importer. Necessary, but not a reason to switch.

The rest (kernel locks, ordered migrations with rollback, the functional core)
is how it's built; users feel it as reliability, not as features.

| Persona | Uses today | Pain | Switches? |
|---|---|---|---|
| **Developer running many agents in parallel**, often one worktree per agent | direnv + `CwdChanged` hacks, manual `docker compose` | env missing in agent shells, port collisions, services left behind or killed early, re-trusting every worktree | **Yes, strongest.** Growing fast, unserved, daily pain |
| **Developer with local services per project** (Postgres, Redis, compose, emulators) | direnv + manual start/stop, or devenv/devbox `up` | forgotten containers draining battery and holding ports | **Likely.** The [#129](https://github.com/direnv/direnv/issues/129) crowd |
| **Consultant or platform engineer juggling client creds** (VPN, sshuttle, AWS SSO, kube contexts) | shell functions, aws-vault, manual login/logout | wrong account active, tunnels left open, logging out breaks other shells | **Likely, niche.** Custom lease keys fit exactly |
| **Nix user on nix-direnv** | direnv + nix-direnv | slow cold starts per worktree | **Maybe,** if foldl is a strict superset |
| **direnv user who wants aliases** | direnv + shell rc hacks | mild | **Unlikely** on this alone |
| **Flox, devenv, devbox or mise users** | an all-in-one environment manager | little in foldl's area | **No.** They'd lose package and tool management |

Positioning: **per-folder environments that clean up after themselves, safe
for any number of terminals and agents.** Replacing direnv is necessary (one
prompt hook should own a folder) but is not the pitch.

Gaps that block switching:

- **Editor integrations.** direnv users rely on VS Code, JetBrains and Emacs
  plugins. `foldl export json` is the building block; no plugin is planned
  yet.
- **Coexisting with mise.** Many target users manage tool versions with mise,
  which foldl doesn't and shouldn't replace. The two prompt hooks must not
  fight.
- **Trying it alone.** One person can keep `.foldl/` uncommitted, but then
  new worktrees (including agent worktrees) don't get it and content-based
  trust doesn't apply. DESIGN.md's "Committed or not" lists what works
  either way, and foldl states it when setting up a folder. The strongest
  persona gets the most from a committed `.foldl/`, so the realistic path is
  a team decision, made easier by `.envrc` staying in place for those who
  haven't switched.

## Risks

- Flox or mise adding `cd`-triggered last-exit teardown. mise is likelier,
  since it already has `enter`/`leave` hooks.

## Unverified

- Whether Flox's direnv integration fires `on-deactivate` when you `cd` out.
- "No tool has ordered migrations with rollback" means none was found, not
  that none exists.

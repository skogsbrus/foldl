# Configuration

Per-user settings live in `~/.config/foldl/config.toml` (or
`$XDG_CONFIG_HOME/foldl/config.toml`). The file is optional. Unknown keys are
errors. Durations are strings such as `"500ms"`, `"30s"`, `"10m"`.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `version` | integer | `1` | file format version; only `1` is accepted |
| `verbosity` | `"quiet"`, `"normal"`, `"verbose"` | `"normal"` | same as `-q` / `-v` |
| `timeout` | duration | `"60s"` | default limit for one `up` |
| `nix_timeout` | duration | `"10m"` | default limit for one `up` of a `nix-flake` migration |
| `down_timeout` | duration | `"30s"` | default limit for one `down` |
| `on_dirty` | `"retry"`, `"skip"`, `"abort"` | `"abort"` | what non-interactive runs do with a dirty lease |
| `trusted_prefixes` | array of paths | `[]` | folders under these paths load without `foldl allow`; a leading `~/` is expanded |
| `port_range` | `[low, high]` | `[20000, 29999]` | inclusive range for `[ports]` entries set to `"auto"` |
| `follow_symlinks` | bool | `false` | whether `.foldl/` may contain symlinks |

A migration's own `timeout` and `down_timeout` override these.

```toml
version = 1
verbosity = "normal"
timeout = "2m"
nix_timeout = "20m"
down_timeout = "30s"
on_dirty = "retry"
trusted_prefixes = ["~/code/mine", "/srv/work"]
port_range = [30000, 39999]
```

## `on_dirty`

A failed shared `down` marks the lease dirty. An interactive shell is asked
what to do (retry the down then up, skip the down and run up, or abort).
Non-interactive runs never ask. They use `on_dirty` and report what they
did. With `abort` they exit 75.

## `trusted_prefixes`

Like direnv's whitelist. Only list directories whose contents you control.
A folder under a prefix still must not be denied: `foldl deny` and
`foldl ignore` on a path win.

## `port_range`

`low` must be at least 1 and not above `high`. foldl picks ports in this
range, skips ports in its registry and ports it cannot bind on 127.0.0.1,
and tends to give the same port after a restart.

## `follow_symlinks`

By default a symlink anywhere in `.foldl/` makes the folder invalid, and
nothing in it loads. A link can point anywhere, so a cloned repository
could otherwise make `foldl allow` print and hash a file such as
`~/.aws/credentials`. With `true`, foldl reads, hashes and copies what the
links point to. It is a per-user setting, so a repository can't turn it on
for itself. Changes inside a linked directory are noticed only when
something in `.foldl/` itself changes.

## Directories

foldl follows the XDG base directory specification.

| Directory | Default | Holds |
|---|---|---|
| `$XDG_CONFIG_HOME/foldl/` | `~/.config/foldl/` | `config.toml` |
| `$XDG_STATE_HOME/foldl/` | `~/.local/state/foldl/` | `sessions/`, `leases/`, `ports/`, `holder.log` |
| `$XDG_DATA_HOME/foldl/` | `~/.local/share/foldl/` | `trust/`: allowed, denied and ignored folders |
| `$XDG_CACHE_HOME/foldl/` | `~/.cache/foldl/` | `nix/<key>/`: `nix-flake` cache and GC root |

Trust is kept under the data directory, so clearing state or cache never
revokes it. A session directory is removed when its session ends. A lease
directory keeps its lock files, `data/` and `logs/` after the lease ends,
so that `${FOLDL_STATE}` survives a restart of a service.

State files carry a `version` field. Logs are mode 0600. `foldl status
--events` prints a session's `events.jsonl`, the last 1000 events.

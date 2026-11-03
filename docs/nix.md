# Nix

foldl supports Nix natively and never requires it. It does not call `nix`
unless a migration uses the `nix-flake` built-in. One binary serves everyone;
detection happens at run time.

## The `nix-flake` built-in

`nix-flake` replaces nix-direnv. It does not wrap it.

```toml
# .foldl/002-flake/manifest.toml
version = 1
builtin = "nix-flake"
```

`flake.nix` and `flake.lock` are watched without listing them. Add files
the flake imports, such as `nix/shell.nix`, to `watch`.

Option:

| Option | Default | Notes |
|---|---|---|
| `flake` | `"."` | relative to the folder holding `.foldl/`; a non-path reference such as `github:o/r` also works |

```toml
[builtin_options]
flake = "./tools"
```

`timeout` for this built-in defaults to `10m`. Change it per migration with
`timeout`, or globally with `nix_timeout` in the
[user config](configuration.md).

What it does on `up`:

1. Runs `nix print-dev-env --json` and caches the result under
   `$XDG_CACHE_HOME/foldl/nix/<key>/` (default `~/.cache/foldl/nix`). The key
   hashes `flake.nix`, `flake.lock` and the files in the migration's `watch`
   list. It does not include the folder path, so worktrees on the same
   commit share one entry. A non-path flake reference is keyed on its text.
2. Registers a GC root. The evaluation runs with
   `--profile <cache>/<key>/profile`, so `nix-collect-garbage` does not
   delete the shell in use.
3. Keeps the last good result per folder. If evaluation fails, foldl uses it
   and prints a warning.

Only exported variables apply. `PATH` and `XDG_DATA_DIRS` are prepended to
your session's values. Variables that describe the build sandbox (`HOME`,
`TMPDIR`, `NIX_BUILD_TOP`, `SHELL`, ...) are skipped.

When `flake.nix`, `flake.lock` or a watched file changes, the migration
reloads at the next prompt.

## Without Nix

If `nix` is not on `PATH`, a `nix-flake` migration fails like any other `up`:

```
nix-flake needs nix on PATH; see https://github.com/skogsbrus/foldl/blob/main/docs/nix.md
```

foldl exits 69 (`unavailable`) and rolls back the earlier migrations of that
folder. `nix` must be on `PATH` even when a cached result would do, so a
folder behaves the same on every machine. `foldl check` warns ahead of time.
`foldl import direnv` still translates `use flake`, since other people using
the folder may have Nix.

## Installing foldl with Nix

The repository is a flake. It builds on `aarch64-darwin`, `x86_64-darwin`,
`aarch64-linux` and `x86_64-linux`.

```sh
nix build            # result/bin/foldl
nix run . -- --help
```

Publishing is not done yet, so use the repository path or a checkout as the
flake input. With `foldl` as an input of your own flake:

```nix
{
  inputs.foldl.url = "path:/path/to/foldl";   # until it is published
  # overlay: adds pkgs.foldl
  # nixpkgs.overlays = [ inputs.foldl.overlays.default ];
}
```

### home-manager

```nix
{
  imports = [ inputs.foldl.homeManagerModules.default ];
  programs.foldl = {
    enable = true;
    enableBashIntegration = true;   # adds the bash hook
    enableZshIntegration = true;    # adds the zsh hook
    settings = {                    # written to ~/.config/foldl/config.toml
      on_dirty = "retry";
    };
  };
}
```

Options: `enable`, `package`, `enableBashIntegration`,
`enableZshIntegration`, `settings`. See [configuration](configuration.md) for
the keys.

### NixOS

```nix
{
  imports = [ inputs.foldl.nixosModules.default ];
  programs.foldl.enable = true;
}
```

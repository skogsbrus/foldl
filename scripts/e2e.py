#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Johan Fosse
"""End-to-end scenarios in real interactive shells, driven through tmux.

Each scenario gets a fresh HOME and runs foldl (the `nix build` result by
default, or $FOLDL) in interactive bash and zsh terminals, then prints a
transcript of what the terminals showed.

    scripts/e2e.py [scenario ...]      # default: all
"""

import os
import shutil
import subprocess
import sys
import tempfile
import time

FOLDL = os.environ.get("FOLDL", os.path.abspath("result/bin/foldl"))
SHELLS = {
    "zsh": ["zsh", "-f", "-i"],
    "bash": ["/bin/bash", "--norc", "--noprofile", "-i"],
}


def sh(*args, **kw):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kw).stdout


class World:
    """A fresh HOME with its own foldl state."""

    def __init__(self):
        self.root = os.path.realpath(tempfile.mkdtemp(prefix="foldl-e2e-"))
        self.home = os.path.join(self.root, "home")
        os.makedirs(self.home)
        self.terms = []

    def path(self, rel):
        return os.path.join(self.home, rel)

    def write(self, rel, text, mode=0o644):
        p = self.path(rel)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w") as f:
            f.write(text)
        os.chmod(p, mode)

    def env(self, extra_path=""):
        path = ":".join(x for x in [os.path.dirname(FOLDL), extra_path, "/usr/bin:/bin:/usr/sbin:/sbin"] if x)
        return {"HOME": self.home, "PATH": path, "TERM": "xterm-256color", "BASH_SILENCE_DEPRECATION_WARNING": "1"}

    def foldl(self, *args, cwd=None, extra_path=""):
        r = subprocess.run([FOLDL, *args], cwd=cwd or self.home, env=self.env(extra_path), capture_output=True, text=True)
        return r.returncode, r.stdout, r.stderr

    def term(self, shell, name=None, extra_path=""):
        t = Term(self, shell, name or f"t{len(self.terms)}", extra_path)
        self.terms.append(t)
        return t

    def calls(self):
        try:
            return open(self.path("calls")).read()
        except FileNotFoundError:
            return ""

    def close(self):
        for t in self.terms:
            t.kill()
        time.sleep(0.5)
        shutil.rmtree(self.root, ignore_errors=True)


class Term:
    """An interactive shell in a tmux session, with the hook installed."""

    def __init__(self, world, shell, name, extra_path):
        self.world, self.shell = world, shell
        self.session = f"foldl-e2e-{os.getpid()}-{name}"
        env = " ".join(f"{k}='{v}'" for k, v in world.env(extra_path).items())
        command = f"env -i {env} {' '.join(SHELLS[shell])}"
        sh("tmux", "new-session", "-d", "-s", self.session, "-x", "200", "-y", "50", "-c", world.home, command)
        time.sleep(0.6)
        self.send(f"PS1='{shell}$ '; eval \"$({FOLDL} hook {shell})\"", wait=0.8)
        self.clear()

    def send(self, keys, wait=1.0, enter=True):
        sh("tmux", "send-keys", "-t", self.session, keys, *(["Enter"] if enter else []))
        time.sleep(wait)

    def ctrl_c(self, wait=1.5):
        sh("tmux", "send-keys", "-t", self.session, "C-c")
        time.sleep(wait)

    def clear(self):
        self.send("clear", wait=0.3)
        sh("tmux", "clear-history", "-t", self.session)

    def screen(self):
        text = sh("tmux", "capture-pane", "-p", "-J", "-S", "-200", "-t", self.session)
        return "\n".join(line.rstrip() for line in text.splitlines() if line.strip())

    def pid(self):
        return int(sh("tmux", "display-message", "-p", "-t", self.session, "#{pane_pid}").strip())

    def kill(self):
        subprocess.run(["tmux", "kill-session", "-t", self.session], capture_output=True)


def show(title, text):
    print(f"--- {title}\n{text}\n")


# Scenarios ---------------------------------------------------------------

def nested(w, shell):
    w.write("app/.foldl/1-env/manifest.toml", 'version = 1\n[env]\nA = "app"\n')
    w.write("app/sub/.foldl/1-sub/manifest.toml", "version = 1\n")
    w.write("app/sub/.foldl/1-sub/up.sh", '#!/bin/sh\nexport B="$A+sub"\n')
    w.foldl("allow", "app")
    w.foldl("allow", "app/sub")
    t = w.term(shell)
    t.send("cd app/sub")
    t.send("echo A=$A B=$B")
    t.send("cd ..")
    t.send("echo A=$A B=${B-unset}")
    t.send("cd ~")
    t.send("echo A=${A-unset} B=${B-unset}")
    show(f"nested folders ({shell})", t.screen())


def rollback(w, shell):
    w.write("app/.foldl/1-ok/manifest.toml", 'version = 1\n[env]\nOK = "1"\n')
    w.write("app/.foldl/2-bad/manifest.toml", "version = 1\n")
    w.write("app/.foldl/2-bad/up.sh", "#!/bin/sh\necho connecting to the database\nexit 3\n")
    w.foldl("allow", "app")
    t = w.term(shell)
    t.send("cd app")
    t.send("echo OK=${OK-unset}")
    show(f"failed up rolls back ({shell})", t.screen())


def ctrl_c(w, shell):
    w.write("app/.foldl/1-env/manifest.toml", 'version = 1\n[env]\nA = "1"\n')
    w.write("app/.foldl/2-slow/manifest.toml", "version = 1\n")
    w.write("app/.foldl/2-slow/up.sh", "#!/bin/sh\nsleep 30\n")
    w.foldl("allow", "app")
    t = w.term(shell)
    t.send("cd app", wait=1.5)
    t.ctrl_c()
    t.send('echo A=${A-unset} session=${FOLDL_SESSION:+set}')
    show(f"Ctrl-C during an up ({shell})", t.screen())


def interactive(w, shell):
    w.write("app/.foldl/1-tools/manifest.toml", 'version = 1\ninteractive = true\n[aliases]\ngs = "git status"\n')
    w.write("app/.foldl/1-tools/up.sh", "#!/bin/sh\nhello() { echo hello from foldl; }\n")
    w.foldl("allow", "app")
    t = w.term(shell)
    t.send("hello() { echo original; }")
    t.send("cd app")
    t.send("hello; alias gs")
    t.send("cd ~")
    t.send("hello; alias gs 2>&1 | head -1")
    show(f"interactive migration ({shell})", t.screen())


SERVICE_UP = """#!/bin/sh
sleep 1000 > /dev/null 2>&1 &
echo $! > "$FOLDL_STATE/pid"
echo "started on port $PGPORT" >> "$HOME/calls"
"""
SERVICE_DOWN = """#!/bin/sh
[ -e "$HOME/broken" ] && { echo "pg_ctl: could not stop server" >&2; exit 1; }
kill "$(cat "$FOLDL_STATE/pid")" && echo stopped >> "$HOME/calls"
"""


def service(w, rel="app"):
    w.write(f"{rel}/.foldl/1-pg/manifest.toml", 'version = 1\nscope = "shared"\n[ports]\nPGPORT = "auto"\n')
    w.write(f"{rel}/.foldl/1-pg/up.sh", SERVICE_UP)
    w.write(f"{rel}/.foldl/1-pg/down.sh", SERVICE_DOWN)


def shared(w, shell):
    service(w)
    w.foldl("allow", "app")
    one, two = w.term(shell, "one"), w.term(shell, "two")
    one.send("cd app")
    one.send("echo PGPORT=$PGPORT")
    two.send("cd app")
    two.send("echo PGPORT=$PGPORT")
    two.send(f"{FOLDL} status")
    one.send("cd ~")
    two.send("cd ~")
    show(f"two terminals share a service ({shell}): terminal one", one.screen())
    show(f"two terminals share a service ({shell}): terminal two", two.screen())
    show("service log", w.calls())


def killed(w, shell):
    service(w)
    w.write("app/.foldl/2-agent/manifest.toml", "version = 1\n")
    w.write("app/.foldl/2-agent/up.sh", '#!/bin/sh\necho "session up.sh ran" >> "$HOME/calls"\n')
    w.write("app/.foldl/2-agent/down.sh", '#!/bin/sh\necho "session down.sh ran" >> "$HOME/calls"\n')
    w.foldl("allow", "app")
    one, two = w.term(shell, "one"), w.term(shell, "two")
    one.send("cd app")
    two.send("cd app")
    pid = two.pid()
    two.kill()
    print(f"(killed terminal two, shell pid {pid})")
    time.sleep(2)
    show("after killing terminal two", w.calls())
    one.send("cd ~", wait=2)
    show(f"terminal killed mid-session ({shell}): terminal one", one.screen())
    show("service log at the end", w.calls())


def dirty(w, shell):
    service(w)
    w.foldl("allow", "app")
    t = w.term(shell)
    w.write("broken", "")
    t.send("cd app")
    t.send("cd ~", wait=2)
    os.remove(w.path("broken"))
    t.send("cd app", wait=2)
    t.send("r", wait=2)
    t.send("echo PGPORT=$PGPORT")
    t.send("cd ~", wait=2)
    show(f"dirty lease prompt ({shell})", t.screen())
    show("service log", w.calls())


def worktrees(w, shell):
    service(w)
    w.write("app/.foldl/foldl.toml", 'version = 1\n[trust]\nreuse = "repo"\n')
    git = ["git", "-c", "user.name=t", "-c", "user.email=t@t"]
    repo = w.path("app")
    subprocess.run(git + ["init", "-q"], cwd=repo, check=True)
    subprocess.run(git + ["add", "."], cwd=repo, check=True)
    subprocess.run(git + ["commit", "-qm", "init"], cwd=repo, check=True)
    subprocess.run(git + ["worktree", "add", "-q", w.path("app-wt")], cwd=repo, check=True)
    w.foldl("allow", "app")
    one, two = w.term(shell, "one"), w.term(shell, "two")
    one.send("cd app")
    one.send("echo PGPORT=$PGPORT")
    two.send("cd app-wt")
    two.send("echo PGPORT=$PGPORT")
    one.send("cd ~")
    two.send("cd ~")
    show(f"worktrees: own instance and port each, no allow in the worktree ({shell}): app", one.screen())
    show(f"worktrees ({shell}): app-wt", two.screen())


def agents(w, shell):
    w.write("app/.foldl/1-env/manifest.toml", 'version = 1\n[env]\nDATABASE_URL = "postgres://localhost/app"\n')
    w.foldl("allow", "app")
    t = w.term(shell)
    t.send("cd ~/app")
    t.send(f"{FOLDL} exec -- sh -c 'echo exec sees $DATABASE_URL'")
    t.send("cd ~")
    t.send(f"{FOLDL} exec --json ~/nowhere -- true")
    show(f"agents via exec ({shell})", t.screen())


def direnv_import(w, shell):
    direnv = shutil.which("direnv")
    assert direnv, "direnv must be on PATH"
    w.write("app/.envrc", "export DATABASE_URL=postgres://localhost/app\nPATH_add bin\nexport VERSION=$(echo 1.2.3)\n")
    t = w.term(shell, extra_path=os.path.dirname(direnv))
    t.send("cd app")
    t.send("direnv allow . 2>&1 | tail -1")
    t.send(f"{FOLDL} import direnv --yes", wait=3)
    t.send("cd ~")
    t.send("cd app", wait=2)
    t.send("echo DATABASE_URL=$DATABASE_URL VERSION=$VERSION")
    t.send("direnv status | grep 'Found RC allowed'")
    show(f"direnv import ({shell})", t.screen())


def no_nix(w, shell):
    w.write("app/.foldl/1-env/manifest.toml", 'version = 1\n[env]\nA = "1"\n')
    w.write("app/.foldl/2-flake/manifest.toml", 'version = 1\nbuiltin = "nix-flake"\n')
    w.write("app/flake.nix", "{ outputs = _: { }; }\n")
    w.foldl("allow", "app")
    t = w.term(shell)
    t.send("command -v nix || echo no nix on PATH")
    t.send("cd app")
    t.send("echo A=${A-unset}")
    t.send(f"{FOLDL} exec -- true; echo exit=$?")
    show(f"no nix on PATH ({shell})", t.screen())


def timing(w, shell):
    w.write("app/.foldl/1-env/manifest.toml", 'version = 1\n[env]\nA = "1"\n')
    w.write("app/sub/deeper/.keep", "")
    w.foldl("allow", "app")
    t = w.term(shell)
    t.send("cd app/sub/deeper")
    loop = "for i in $(seq 500); do _foldl_hook; done"
    t.send(f"s=$(perl -MTime::HiRes=time -e 'printf \"%.6f\", time'); {loop}; e=$(perl -MTime::HiRes=time -e 'printf \"%.6f\", time'); perl -e \"printf 'no-op prompt: %.2f ms per prompt (500 prompts)\\n', ($e-$s)*1000/500\"", wait=8)
    show(f"no-op prompt timing ({shell})", t.screen())


SCENARIOS = {
    "nested": nested,
    "rollback": rollback,
    "ctrl-c": ctrl_c,
    "interactive": interactive,
    "shared": shared,
    "killed": killed,
    "dirty": dirty,
    "worktrees": worktrees,
    "agents": agents,
    "direnv": direnv_import,
    "no-nix": no_nix,
    "timing": timing,
}


def main():
    names = sys.argv[1:] or list(SCENARIOS)
    print(f"foldl: {FOLDL} ({sh(FOLDL, '--version').strip()})\n")
    for name in names:
        for shell in SHELLS:
            w = World()
            try:
                print(f"=== {name} in {shell} ===")
                SCENARIOS[name](w, shell)
            finally:
                w.close()


if __name__ == "__main__":
    main()

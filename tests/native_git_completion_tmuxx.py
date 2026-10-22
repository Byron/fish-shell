#!/usr/bin/env python3
"""Check native Git ref completion in private repositories and tmuxx panes.

Usage: python tests/native_git_completion_tmuxx.py FISH_EXE TMUXX_EXE

All Git setup uses private configuration and repositories. Interactive reset
inputs are inspected and cancelled; they are never submitted for execution.
"""

import argparse
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time
import uuid


def quote(value):
    """A literal word accepted by both Fish and Bash-style interactive input."""
    return "'" + str(value).replace("'", "'\\''") + "'"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fish", type=Path)
    parser.add_argument("tmuxx", type=Path)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("this test requires native Windows and tmuxx ConPTY")
    git_exe = shutil.which("git")
    if not git_exe:
        parser.error("Git is required to create the private completion fixtures")
    tmuxx = str(args.tmuxx.resolve())
    label = "fish-git-refs-" + uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix="fish-git-refs-") as temporary:
        root = Path(temporary)
        fish = root / "fish.exe"
        shutil.copy2(args.fish.resolve(), fish)
        config = root / "config" / "fish"
        config.mkdir(parents=True)
        (config / "config.fish").write_text(
            "function fish_prompt; printf 'GITREF> '; end\n"
            "set -g fish_autosuggestion_enabled 0\n",
            encoding="utf-8",
        )
        hooks = root / "no-hooks"
        hooks.mkdir()
        git_config = root / "gitconfig"
        git_config.write_text(
            '[user]\n name = Completion Fixture\n email = completion@example.invalid\n'
            '[commit]\n gpgSign = false\n'
            f'[core]\n hooksPath = "{hooks.as_posix()}"\n',
            encoding="utf-8",
        )
        env = {key: value for key, value in os.environ.items()
               if not key.upper().startswith("GIT_")}
        env.update(
            HOME=temporary,
            USERPROFILE=temporary,
            XDG_CONFIG_HOME=str(root / "config"), FISH_NATIVE_CONFIG_HOME=str(root / "config"),
            XDG_DATA_HOME=str(root / "data"),
            XDG_CACHE_HOME=str(root / "cache"),
            GIT_CONFIG_GLOBAL=str(git_config),
            GIT_CONFIG_NOSYSTEM="1",
            GIT_TERMINAL_PROMPT="0",
        )
        for name in ("TMUX", "TMUXX_ENDPOINT"):
            env.pop(name, None)

        def git(directory, *arguments):
            result = subprocess.run(
                [git_exe, "-C", str(directory), *arguments], env=env, cwd=root,
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                timeout=15, check=True, creationflags=subprocess.CREATE_NO_WINDOW,
            )
            return result.stdout.decode("utf-8").strip()

        repository = root / "repository"
        alternate = root / "alternate repository"
        worktree = root / "linked worktree"
        outside = root / "not a repository"
        outside.mkdir()
        for directory in (repository, alternate):
            directory.mkdir()
            git(directory, "init", "-b", "main")
            git(directory, "commit", "--allow-empty", "--no-gpg-sign", "-m", "completion fixture")
        for branch in ("feature/alpha", "feature/beta", "feature/quo'te", "feature/東京", "duplicate"):
            git(repository, "branch", branch)
        git(repository, "update-ref", "refs/remotes/origin/remote-only", "HEAD")
        git(repository, "tag", "v-packed")
        git(repository, "tag", "duplicate")
        git(repository, "worktree", "add", "--detach", str(worktree))
        git(repository, "pack-refs", "--all", "--prune")
        assert (repository / ".git" / "packed-refs").is_file()
        assert not (repository / ".git" / "refs" / "heads" / "feature" / "alpha").exists()
        assert (worktree / ".git").is_file()
        git(alternate, "branch", "alternate/only")
        (repository / "feature-file.txt").write_text("keep this file\n", encoding="utf-8")
        (repository / "duplicate-file.txt").write_text("collision prefix fixture\n", encoding="utf-8")
        original = {directory: (git(directory, "rev-parse", "HEAD"),
                                git(directory, "status", "--porcelain=v1"))
                    for directory in (repository, alternate, worktree)}
        # Trace only the shell/completion phase. This also detects an accidental
        # Enter that submits reset instead of merely accepting a menu candidate.
        trace = root / "git-trace.log"
        env["GIT_TRACE"] = str(trace)
        tmux_config = root / "tmuxx.conf"
        tmux_config.write_text("set -gw remain-on-exit on\n", encoding="utf-8")
        base = [tmuxx, "-L", label]

        def run(*arguments, check=True):
            return subprocess.run(
                base + list(arguments), env=env, cwd=root,
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                timeout=10, check=check, creationflags=subprocess.CREATE_NO_WINDOW,
            )

        def call(*arguments):
            return run(*arguments).stdout.decode("utf-8").replace("\r", "")

        def capture():
            return call("capture-pane", "-p", "-t", pane, "-S", "-", "-E", "-")

        def wait_for(predicate, description):
            deadline = time.monotonic() + 10
            text = ""
            while time.monotonic() < deadline:
                text = capture()
                if predicate(text):
                    return text
                time.sleep(0.03)
            raise AssertionError(f"{description}:\n{text[-8000:]}")

        def expect_input(source):
            expected = ("GITREF> " + source).rstrip()
            return wait_for(lambda text: text.rstrip().splitlines()[-1:] == [expected],
                            f"missing edited input {expected!r}")

        def keys(*names):
            call("send-keys", "-t", pane, *names)

        def literal(source):
            call("send-keys", "-t", pane, "-l", source)

        def send(source):
            literal(source)
            keys("Enter")

        serial = 0

        def execute(source):
            nonlocal serial
            serial += 1
            marker = f"GITREF_finished_{serial}"
            send(source + "; printf '" + marker + "\\n'")
            wait_for(lambda text: marker in text.splitlines(), f"missing command marker {marker}")
            expect_input("")

        def complete(source):
            output = root / f"query-{serial}.txt"
            execute(f"complete -C {quote(source)} > {quote(output.as_posix())}")
            return output.read_text(encoding="utf-8").splitlines()

        def names(candidates):
            return {line.split("  (", 1)[0] for line in candidates}

        def require(candidates, name, label=None):
            matches = [line for line in candidates if line.split("  (", 1)[0] == name]
            assert len(matches) == 1, f"missing or repeated {name!r}: {candidates!r}"
            if label:
                assert label in matches[0].lower(), f"missing {label!r} label: {matches[0]!r}"

        def cancel():
            keys("C-c")
            expect_input("")

        def tab(source, expected):
            literal(source)
            keys("Tab")
            expect_input(expected)
            cancel()

        owner = subprocess.Popen(
            base + ["-D", "-f", str(tmux_config)], env=env, cwd=root,
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NO_WINDOW,
        )
        try:
            deadline = time.monotonic() + 10
            while run("show-options", "-gwqv", "remain-on-exit", check=False).returncode:
                if owner.poll() is not None:
                    raise AssertionError(owner.stderr.read().decode("utf-8", errors="replace"))
                assert time.monotonic() < deadline, "private tmuxx owner did not become ready"
                time.sleep(0.03)
            for language in ("fish", "bash"):
                pane = call(
                    "new-session", "-dP", "-s", language, "-x", "220", "-y", "40",
                    "-c", str(repository), "-F", "#{pane_id}", str(fish),
                    "--interactive-language=" + language, "-i",
                ).strip()
                assert pane.startswith("%"), pane
                expect_input("")
                candidates = complete("git reset fea")
                require(candidates, "feature/alpha", "branch")
                require(candidates, "feature/beta", "branch")
                require(candidates, "feature-file.txt")
                require(complete("git reset origin/"), "origin/remote-only", "remote")
                require(complete("git reset v-pa"), "v-packed", "tag")
                require(complete("git reset HE"), "HEAD")
                require(complete("git reset feature/東"), "feature/東京", "branch")
                collision = complete("git reset duplicate")
                require(collision, "refs/heads/duplicate", "branch")
                require(collision, "refs/tags/duplicate", "tag")
                require(collision, "duplicate-file.txt")
                assert "duplicate" not in names(collision), "ambiguous short ref was offered"
                assert names(complete("git reset -- fea")) == {"feature-file.txt"}
                candidates = complete("git reset --hard fea")
                require(candidates, "feature/alpha")
                assert "feature-file.txt" not in names(candidates), candidates

                tab("git reset feature/al", "git reset feature/alpha")
                tab("git reset --hard feature/be", "git reset --hard feature/beta")
                tab("git reset 'feature/quo", "git reset feature/quo\\'te")
                tab("git reset feature/東", "git reset feature/東京")
                tab("git reset refs/heads/du", "git reset refs/heads/duplicate")
                tab("git switch du", "git switch duplicate")
                if language == "bash":
                    require(complete("if true; then git reset feature/al"), "feature/alpha")
                    tab("if true; then git reset feature/al", "if true; then git reset feature/alpha")

                literal("git reset feature/")
                keys("Tab")
                wait_for(lambda text: "Search:" in text and "feature/alpha" in text
                         and "feature/beta" in text, "missing branch completion menu")
                keys("Tab")
                wait_for(lambda text: any(line.startswith("> feature/alpha") for line in text.splitlines()),
                         "Tab did not select local branch")
                keys("Enter")  # Accept the selected candidate; do not execute the line.
                expect_input("git reset feature/alpha")
                cancel()

                alternate_prefix = "git -C " + quote(alternate.as_posix()) + " reset "
                candidates = complete(alternate_prefix + "alternate/")
                require(candidates, "alternate/only", "branch")
                assert "feature/alpha" not in names(candidates)
                tab(alternate_prefix + "alternate/on", alternate_prefix + "alternate/only")
                worktree_prefix = "git -C " + quote(worktree.as_posix()) + " reset "
                require(complete(worktree_prefix + "feature/al"), "feature/alpha", "branch")
                tab(worktree_prefix + "feature/al", worktree_prefix + "feature/alpha")

                # A ref created while the previous prompt is idle is visible at
                # the next prompt; a short duplicate-Tab cache must not persist.
                fresh = "fresh/" + language
                assert fresh not in names(complete("git reset " + fresh))
                git(repository, "branch", fresh)
                execute("true")
                require(complete("git reset " + fresh), fresh, "branch")
                tab("git reset " + fresh[:-1], "git reset " + fresh)

                # Source-looking repository routing is never evaluated while
                # querying completions or pressing Tab on the edited command.
                marker = root / ("substitution-" + language)
                substitution = "(printf wrong > " + quote(marker.as_posix()) + ")"
                if language == "bash":
                    substitution = "$" + substitution
                source = "git -C " + substitution + " reset feature/al"
                assert complete(source) == [], "nonliteral -C produced semantic refs"
                tab(source, source)
                assert not marker.exists(), "completion executed a -C substitution"

                execute("cd " + quote(outside.as_posix()))
                assert complete("git reset feature/") == [], "refs leaked outside a repository"
                execute("set -gx GIT_DIR " + quote((repository / ".git").as_posix())
                        + "; set -gx GIT_WORK_TREE " + quote(repository.as_posix()))
                require(complete("git reset feature/al"), "feature/alpha", "branch")
                tab("git reset feature/al", "git reset feature/alpha")
                execute("set -e GIT_DIR; set -e GIT_WORK_TREE")
                assert complete("git reset feature/") == [], "cleared Git environment remained cached"
                send("exit 0")
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    state = call("display-message", "-pt", pane,
                                 "#{pane_dead}|#{pane_dead_status}").strip()
                    if state.startswith("1|"):
                        assert state == "1|0", (state, capture())
                        break
                    time.sleep(0.03)
                else:
                    raise AssertionError(f"shell did not exit:\n{capture()}")
                print(f"PASS native Git refs ({language}): complete -C, Tab/menu, labels/quoting, "
                      "reset modes, -C, packed refs/worktree, exported environment, no repository")
            if trace.exists():
                assert not re.search(r"built-in: git (?:reset|switch)(?:\s|$)", trace.read_text(encoding="utf-8")), (
                    "a completion probe executed git reset/switch"
                )
            for directory, expected in original.items():
                actual = (git(directory, "rev-parse", "HEAD"), git(directory, "status", "--porcelain=v1"))
                assert actual == expected, f"completion changed {directory}: {actual!r} != {expected!r}"
        finally:
            if owner.poll() is None:
                try:
                    run("kill-server", check=False)
                except (OSError, subprocess.TimeoutExpired):
                    pass
            try:
                owner.wait(timeout=10)
            except subprocess.TimeoutExpired:
                owner.kill()
                owner.wait(timeout=10)


if __name__ == "__main__":
    main()

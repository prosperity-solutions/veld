#!/usr/bin/env python3
"""Every `veld <word>` that code, CI and scripts name must be a command that runs.

The sibling of `validate-doc-commands.py`, which reads only Markdown and
`llms-full.txt`. That left every other place a command is spelled — a hint in a
Rust string, a CI step, an install.sh echo, a coloured tip — unchecked, and #423's
rename of every top-level command showed what that costs: a sweep reported 1289
documented invocations green while CI steps, the `veld agent skills` index and the
unprivileged-mode tip still named retired commands. A retired name does not fail
loudly at the call site; it prints `that is \`veld …\` now` and exits 2.

So this asks the binary. Each distinct first word found after a recognisable
`veld` is run once, as `veld <word> --help` — never bare, since a bare word can be
a real command (`veld update` installs a release); a word the binary answers with the
retired-name line is a failure, wherever it was written. When the first word is
a part, the verb after it is asked too (`veld orchestrator frobnicate --help`
must exit 0), so an invented verb under a part fails as well. Only that one level:
a third word is not checked. The binary is the
only authority, so this cannot drift from it.

What counts as "a recognisable veld": the shapes a command takes in code, not
prose — after a backtick, a quote, an ANSI colour code, `cyan("`/`bold("`,
`<code>`, a path ending in `/veld`, `$VELD`/`$VELD_BIN`, at the start of a shell
line or a CI `run:`, or after "run"/"try"/"use" (a hint like `Err("run veld start first")`). Comment prose
such as "veld runs a command" has none of these and is not read.

A line that is deliberately about the old names says so with `pre-#423` and is
skipped. History (CHANGELOG) is not read.

Usage: validate-cli-call-sites.py <veld-binary> [--selftest]
"""

import re
import subprocess
import sys
from pathlib import Path

PREFIX = (
    r"(?:target/debug/veld\"?|/veld\"|\$\{?VELD(?:_BIN)?\}?\"?"
    r"|\\x1b\[[0-9;]*m\s*veld|(?:cyan|bold|dim|green|yellow)\(\"veld"
    r"|`veld|'veld|\"veld\"?,?|<code>veld|>veld"
    r"|^\s*(?:-\s*)?(?:run:\s*)?veld|\b(?:sudo|exec)\s+veld"
    # An imperative inside a string: a hint like `Err("run veld start first")`.
    # Only after run/try/use, because "a veld backup" and "the veld version" are
    # nouns, and both sit in strings too.
    r"|\b(?:[Rr]un|[Tt]ry|[Uu]se)\s+veld)"
)
# `--debug` is the one root flag that can sit before the command.
CALL = re.compile(
    PREFIX + r"(?:\s+--debug)?\s+\"?([a-z][a-z-]*)\b(?![-.:][\w/-])(?:\s+([a-z][a-z-]*)\b(?![-.:][\w/-]))?",
    re.M,
)
PARTS = {"orchestrator", "orc", "ide", "agent", "network", "self"}

SKIP_PATH = re.compile(
    r"(^|/)(CHANGELOG[^/]*|node_modules/.*|.*\.lock|package-lock\.json)$"
    r"|\.md$|llms-full\.txt$|\.(png|jpg|jpeg|gif|ico|icns|woff2?|ttf|svg|pdf|zip|gz)$"
    # This file and the doc gate name retired words on purpose, as test input.
    r"|^tests/validate-(cli-call-sites|doc-commands)\.py$"
)


def retired(binary: str, word: str, cache: dict) -> bool:
    if word not in cache:
        try:
            # Always `--help`: a bare word can be a real command — `veld update`
            # installs a release. The retired-name line answers before clap, so
            # `--help` still gets it, and on a live command `--help` never acts.
            r = subprocess.run(
                [binary, word, "--help"], capture_output=True, text=True, timeout=10,
                stdin=subprocess.DEVNULL,
            )
            cache[word] = r.returncode == 2 and r.stderr.startswith("that is `veld ")
        except subprocess.TimeoutExpired:
            # `--help` answers at once; a hang is a broken binary, and a gate that
            # passed it would print the same green "0" as a clean run.
            sys.exit(f"  `{binary} {word} --help` did not answer in 10s")
    return cache[word]


def verb_exists(binary: str, part: str, verb: str, cache: dict) -> bool:
    """`veld <part> <verb> --help` exits 0 for a real verb and 2 for anything else."""
    key = (part, verb)
    if key not in cache:
        try:
            r = subprocess.run(
                [binary, part, verb, "--help"], capture_output=True, text=True,
                timeout=10, stdin=subprocess.DEVNULL,
            )
            cache[key] = r.returncode == 0
        except subprocess.TimeoutExpired:
            sys.exit(f"  `{binary} {part} {verb} --help` did not answer in 10s")
    return cache[key]


def scan(binary: str, files: list[tuple[str, str]]) -> list[str]:
    cache: dict = {}
    bad = []
    for path, text in files:
        for n, line in enumerate(text.splitlines(), 1):
            if "pre-#423" in line:
                continue
            for m in CALL.finditer(line):
                word, verb = m.group(1), m.group(2)
                if retired(binary, word, cache):
                    bad.append(f"{path}:{n}: `veld {word}` is retired: {line.strip()[:120]}")
                elif word in PARTS and verb and not verb_exists(binary, word, verb, cache):
                    bad.append(
                        f"{path}:{n}: `veld {word} {verb}` is not a command: {line.strip()[:120]}"
                    )
    return bad


def tracked_files() -> list[tuple[str, str]]:
    # From the repository root wherever this is run from: `git ls-files` in a
    # subdirectory lists only that subtree, and a partial scan prints the same
    # green "0" as a whole one.
    root = Path(__file__).resolve().parents[1]
    out = subprocess.run(
        ["git", "ls-files"], capture_output=True, text=True, check=True, cwd=root
    )
    files = []
    for path in out.stdout.splitlines():
        if SKIP_PATH.search(path):
            continue
        try:
            with open(root / path, encoding="utf-8") as f:
                files.append((path, f.read()))
        except (UnicodeDecodeError, FileNotFoundError, IsADirectoryError):
            continue
    return files


def selftest(binary: str) -> int:
    must_fail = [
        '(cd "$dir" && "$GITHUB_WORKSPACE/target/debug/veld" lint)',
        '"  Run \\x1b[36mveld setup privileged\\x1b[0m for clean URLs"',
        'output::cyan("veld skills <topic>"),',
        "Live state: `veld presets`, `veld nodes`.",
        '"$VELD_BIN" stop --name x',
        "echo \"Run 'veld start' in any project\"",
        'return Err("run veld start first".into());',
        "<td><code>veld start</code></td>",
        "  veld start --name x",
        "        run: veld --debug start",
        "`veld orchestrator frobnicate`",
        'Err("then run veld lint.")',
        '"Run veld start: it brings the stack up"',
        "exec veld start --attach",
        "sudo veld setup privileged",
    ]
    must_pass = [
        '"$GITHUB_WORKSPACE/target/debug/veld" orchestrator lint',
        "`veld self setup privileged`",
        "so veld runs a command for you",  # prose: no code shape
        'Err("not a veld backup")',  # a noun in a string
        "`veld desktop status --json` is the app's probe",  # hidden alias, still runs
        "`veld update --wait-pid` is what the app spawns",
        "`veld agent-state` from hooks written before the update",
        "the pre-#423 `veld start` printed this",
        "`veld feedback next`",
        "`veld orchestrator runs list`",
        "`veld self settings get terminal.shell`",
        '"$VELD" orc start web:local',
        "`veld agent skills ide`",
        "// veld runs a command for you, in a comment",
    ]
    failed = 0
    for i, line in enumerate(must_fail):
        if not scan(binary, [(f"fail{i}", line)]):
            print(f"  SELFTEST: not caught: {line}")
            failed += 1
    for i, line in enumerate(must_pass):
        hits = scan(binary, [(f"pass{i}", line)])
        if hits:
            print(f"  SELFTEST: false positive: {hits[0]}")
            failed += 1
    total = len(must_fail) + len(must_pass)
    print(f"  selftest: {total - failed}/{total}")
    return 1 if failed else 0


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    binary = sys.argv[1]
    if "--selftest" in sys.argv[2:]:
        return selftest(binary)
    bad = scan(binary, tracked_files())
    for line in bad:
        print(f"  {line}")
    print(f"  {len(bad)} call site(s) name a retired command")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())

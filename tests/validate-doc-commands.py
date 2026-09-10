#!/usr/bin/env python3
"""Every `veld …` command line in the docs must exist in the CLI.

`validate-doc-examples.py` holds the *configs* in the documentation to the real
parser. Nothing held the *commands*, and an audit of the corpus found twelve
wrong claims about the CLI. Half of those are prose this gate cannot judge — a
sentence promising `--json` on every command, a table row putting a flag on the
wrong node. **The five it does catch, and was written for, are the ones somebody
would paste:** a `--detach`/`-d` pair that never existed and was documented as
the default when the opposite is true (twice), `veld logs dev-daemon` as a
positional where there is none, and a bare `veld feedback [--name <n>]`, which
clap refuses on both counts (twice). Six lines across the corpus, and each
survived because the only reader who would notice is an agent, at which point
the cost has already been paid.

The CLI is learned from `veld _cli-dump` — clap's own tree as JSON — never by
parsing `--help` prose. The first version of this gate did parse help, and
`crates/veld/src/commands/cli_dump.rs` records the three ways that went wrong;
the shortest of them is that a hyphenated word in a description became a flag,
so a documented `veld logs --pin` would have *passed*.

Three checks, all exact rather than heuristic:

1. a flag must be declared on the node it is written against;
2. a bare word at a node that accepts no positional must be one of that node's
   subcommands;
3. a node that requires a subcommand must be given one.

What it deliberately does NOT check: whether a positional *value* is sensible.
`veld start dev-headless` is a well-formed command line and a wrong one (that is
a preset, and presets go behind `--preset`), and nothing about the CLI's shape
distinguishes it from a node veld has not heard of. That class is answered in
the binary instead — `start.rs`'s `with_preset_hint` says so at the moment
somebody gets it wrong, which reaches an agent that never read the docs. Nor
does it check argument *arity*: a line showing two values where one is accepted
passes, which is a much rarer defect than one showing a value where none is.

Usage: validate-doc-commands.py <path-to-veld-binary> [--selftest]
"""
import json
import os
import re
import subprocess
import sys

# `docs/migrating-to-v3.md` documents the v1/v2 world on purpose, including a
# `veld config --migrate --write` that was deliberately removed; AGENTS.md
# explains that removal in the same terms.
EXEMPT_FILES = {"docs/migrating-to-v3.md"}

# A line may opt out when it is *about* a command not existing. Keep this list
# short and specific: a growing allowlist is a gate turning back into prose.
EXEMPT_SUBSTRINGS = ("veld config --migrate",)

# clap adds these at parse time rather than as declared arguments, so the dump
# does not carry them. `--debug` is declared `global = true` on the root, which
# makes it legal on every subcommand while being reported only at the root.
UNIVERSAL_FLAGS = {"-h", "--help", "-V", "--version", "--debug"}

# A token carrying any of these is a placeholder or shell syntax, not something
# to resolve: `<NODE:VARIANT>`, `[--approve <first\|manual\|auto>]`, `${var}`,
# a pipe into `jq`, a quoted message, and the `veld …` of prose that means
# "any veld command".
PLACEHOLDER_CHARS = set("<>{}|\\$\"'`\u2026")

FENCE = re.compile(r"```[^\n]*\n(.*?)```", re.S)
INLINE = re.compile(r"`([^`\n]+)`")
# A command ends at a comment or any shell operator that starts a new one.
TERMINATORS = re.compile(r"\s+(?:#|&&|\|\||;|\||>|2>)")


def load_tree(veld):
    r = subprocess.run([veld, "_cli-dump"], capture_output=True, text=True, check=True)
    tree = json.loads(r.stdout)
    # The one positional whose legal values the CLI can enumerate, so the one
    # this gate checks. Topics are cross-referenced from ~20 places (the topic
    # bodies themselves, both shipped shells, AGENTS.md, README.md), and a
    # rename would break every one of them silently: the reader gets "Unknown
    # skill topic" at the moment it needed the document. Attached to the tree so
    # `check` stays a pure function of (line, tree).
    r = subprocess.run([veld, "skills", "--json"], capture_output=True, text=True, check=True)
    tree["subcommands"]["skills"]["values"] = [t["name"] for t in json.loads(r.stdout)]
    return tree


def candidate_lines(text):
    """Every `veld` invocation in the document, as (line, runnable).

    `runnable` separates a fenced line — something written to be run, so an
    incomplete one is a defect — from an inline span, where naming a command
    family is ordinary prose. "Collaborate through `veld feedback`" is correct
    English about a command that requires a subcommand, and six files write it;
    a fenced `veld feedback` on its own is a line somebody will paste.
    """
    out = []
    for block in FENCE.findall(text):
        for line in block.splitlines():
            line = line.strip()
            # Strip a shell prompt if the fence uses one.
            line = re.sub(r"^\$\s+", "", line)
            if line.startswith("veld "):
                out.append((line, True))
    for span in INLINE.findall(text):
        span = span.strip()
        if span.startswith("veld "):
            out.append((span, False))
    return out


def tokenize(line):
    return TERMINATORS.split(line, maxsplit=1)[0].split()


def clean(tok):
    """Drop the decoration a docs table puts around a token: `[--json]`, `-a,`."""
    return tok.strip("[](),.").rstrip("*")


def is_placeholder(tok):
    """A value the reader is meant to substitute, not a literal.

    `<TICKET>` and `${var}` are caught by their punctuation; `[TOPIC]` is not,
    because stripping the brackets leaves a bare word. clap names a value in
    upper case and the docs mirror it, while every real subcommand and topic
    name is lower kebab-case (`topic_names_are_unique_and_kebab_case` pins the
    latter), so case is a reliable separator here.
    """
    return bool(set(tok) & PLACEHOLDER_CHARS) or (
        tok.upper() == tok and any(c.isalpha() for c in tok)
    )


def check(line, tree, runnable=True):
    tokens = tokenize(line)
    if len(tokens) < 2:
        return None

    node, path = tree, []
    i = 1
    while i < len(tokens):
        tok = clean(tokens[i])
        # A global flag may precede the subcommand — `veld --debug start …` is
        # accepted by clap, and breaking here left `node` at the root so every
        # later flag was reported as unknown. Scoped to *before* any subcommand
        # is consumed, and to the root's own flags: past that point a `-` token
        # is followed by its value, and skipping the flag would feed the value
        # to the positional check below (`veld logs --node dev-daemon` would
        # have failed on `dev-daemon`). The root's only such flag is the boolean
        # `--debug`, so there is no value to step over here.
        if not path and tok in set(tree["flags"]) | UNIVERSAL_FLAGS:
            i += 1
            continue
        if not tok or tok.startswith("-") or is_placeholder(tok):
            break
        subs = node["subcommands"]
        if tok in subs:
            node, path = subs[tok], path + [tok]
            i += 1
            continue
        where = "veld " + " ".join(path) if path else "veld"
        if not node["takes_positional"]:
            # Not a subcommand, and nothing here accepts a bare word — clap
            # refuses this outright. This is `veld logs dev-daemon`.
            known = ", ".join(sorted(subs)) if subs else "none"
            return (
                f"`{where}` takes no positional argument, and `{tok}` is not one of "
                f"its subcommands ({known})"
            )
        legal = node.get("values")
        if legal is not None and tok not in legal:
            return (
                f"`{where} {tok}` names no such topic. Available: {', '.join(legal)}"
            )
        # A legitimate positional value; everything after it is a value or a flag.
        break

    if runnable and node["subcommand_required"] and i >= len(tokens):
        known = ", ".join(sorted(node["subcommands"]))
        where = "veld " + " ".join(path) if path else "veld"
        return f"`{where}` requires a subcommand ({known})"

    known = set(node["flags"]) | UNIVERSAL_FLAGS
    for tok in tokens[i:]:
        tok = clean(tok)
        if not tok.startswith("-") or len(tok) < 2:
            continue
        tok = tok.split("=", 1)[0]
        if is_placeholder(tok):
            continue
        if tok not in known:
            where = "veld " + " ".join(path) if path else "veld"
            return f"`{tok}` is not a flag of `{where}`"
    return None


# The defects this gate was written for, plus the shapes it must NOT flag. A
# gate with no self-test is a gate that can rot into always-passing without
# anybody noticing — `validate-workflow-gates.py` makes the same argument, and
# a corpus that has just been cleaned reports "0 failing" either way.
SELFTEST_BAD = [
    "veld start --preset fullstack --name my-feature -d",
    "veld start --preset fullstack --name my-feature --detach",
    "veld logs --run/--previous",
    "veld desktop --json",
    "veld runs --nosuchflag",
    "veld config set website_log_level debug --nope",
    # The four the help-parsing version could not see at all.
    "veld logs dev-daemon --follow",
    "veld feedback --name dev",
    "veld feedback",  # in a fence: a line somebody pastes
    "veld nosuchsubcommand",
    # A flag that is real, but on a different node.
    "veld runs show a3f8c12 --pin",
    # A topic that does not exist — what a rename leaves behind everywhere.
    "veld skills configuration",
    "veld skills panes --json",
]
SELFTEST_GOOD = [
    "veld start --preset fullstack --name my-feature --attach",
    "veld start api:local web:local --name dev",
    "veld start e2e --oneshot --all-logs",
    "veld logs --node dev-daemon --follow",
    "veld logs --run a3f8c12 -C 3 --utc",
    "veld desktop status --json",
    "veld desktop update --wait-pid 42 --relaunch",  # a hidden flag is still real
    "veld runs show a3f8c12 --json",
    "veld config set website_reload_delay 300 --worktree",
    "veld feedback next --wait --name dev --json",
    "veld share my-feature --node frontend --ttl 3600 --json",
    "veld skills config --json",
    "veld action psql --node database --print",
    "veld presets --pin",
    # Placeholder-laden reference rows must pass untouched.
    "veld share [RUN] [--node <n>]... [--ttl <secs>] [--json]",
    "veld settings set <key> <value> [--json]",
    "veld start [NODE:VARIANT...] --name <n>",
    "veld skills [TOPIC] [--json]",  # an upper-case bare word is a placeholder
    "veld --debug start --preset fullstack",  # a global may precede the subcommand
    "veld backup now --dir /tmp/x --json",  # …and be inherited by a descendant
]
# Prose that names a command family. Legal inline, a defect in a fence — the
# `veld feedback` entry above is the same string on the other side of that line.
SELFTEST_GOOD_INLINE_ONLY = [
    "veld feedback",
    "veld runs show",
]


def selftest(veld):
    tree = load_tree(veld)
    bad = 0
    for line in SELFTEST_BAD:
        if check(line, tree) is None:
            print(f"  SELFTEST FAIL: `{line}` should have been rejected")
            bad += 1
    for line in SELFTEST_GOOD:
        problem = check(line, tree)
        if problem is not None:
            print(f"  SELFTEST FAIL: `{line}` should have passed — {problem}")
            bad += 1
    for line in SELFTEST_GOOD_INLINE_ONLY:
        problem = check(line, tree, runnable=False)
        if problem is not None:
            print(f"  SELFTEST FAIL: inline `{line}` should have passed — {problem}")
            bad += 1
    total = len(SELFTEST_BAD) + len(SELFTEST_GOOD) + len(SELFTEST_GOOD_INLINE_ONLY)
    print(f"  self-test: {total - bad}/{total} cases behaved as specified")
    return 1 if bad else 0


def main():
    veld = os.path.abspath(sys.argv[1])
    if "--selftest" in sys.argv[2:]:
        return selftest(veld)
    tree = load_tree(veld)

    # Derive the repo root from this file, never from the caller's cwd.
    # `git ls-files` is cwd-relative, and `validate-schema.sh` resolves a
    # `$REPO_ROOT` for its other gates without ever `cd`-ing — so run from
    # `.github/` this printed "0 documented invocations checked, 0 failing" and
    # exited 0. That is the always-passing rot the self-test exists to prevent,
    # arriving through the back door.
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    files = subprocess.run(
        ["git", "-C", root, "ls-files", "*.md", "website/llms-full.txt"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split()

    checked = failed = 0
    for path in files:
        if path in EXEMPT_FILES:
            continue
        with open(os.path.join(root, path), encoding="utf-8") as f:
            content = f.read()
        for line, runnable in candidate_lines(content):
            if any(s in line for s in EXEMPT_SUBSTRINGS):
                continue
            checked += 1
            problem = check(line, tree, runnable)
            if problem:
                failed += 1
                print(f"  {path} ... FAIL")
                print(f"      {line}")
                print(f"      {problem}")

    print(f"  {checked} documented `veld` invocation(s) checked, {failed} failing")
    if checked == 0:
        # An empty corpus is a broken gate, not a clean one, and the two are
        # indistinguishable from the summary line above.
        print(f"  FAIL: no invocations found at all under {root} — the gate is not working")
        return 1
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

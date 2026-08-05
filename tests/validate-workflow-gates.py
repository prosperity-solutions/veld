#!/usr/bin/env python3
"""Assert that no GitHub Actions job can run on a draft pull request.

Draft PRs are where an agentic contributor's intermediate commits land, and
`.github/workflows/ci.yml` is expensive to run: two macOS legs (billed at 10x a
Linux minute), a four-target release build matrix, and macOS Electron packaging.
The house workflow (AGENTS.md -> PR Workflow) reviews locally first and marks the
PR ready for review second, so a draft has by definition not earned a CI run yet.

Nothing in Actions enforces that, and the symptom of a missing guard is a bill
rather than a failed job -- so this is the drift gate, in the same spirit as the
THIRD-PARTY-LICENSES.md and desktop/assets checks.

Usage: python3 tests/validate-workflow-gates.py [workflow.yml ...]

With no arguments it checks every workflow under .github/workflows.
Requires PyYAML.
"""

import sys
from pathlib import Path

import yaml

REPO_ROOT = Path(__file__).resolve().parent.parent

# The canonical guard. Matched as a substring so a job may combine it with its
# own conditions (`commits` in ci.yml is also restricted to PR events).
DRAFT_GUARD = "github.event.pull_request.draft == false"

# A job that only ever runs on a push to main cannot run on a PR at all, so it
# needs no draft guard. release.yml's build/desktop jobs are in this class.
PUSH_ONLY = "github.event_name == 'push'"

# These make a job run even when its `needs` were skipped, which is exactly how a
# dependent job escapes the cascade. `cancelled()` counts because the common
# `!cancelled()` idiom has the same effect as `always()` here.
CASCADE_BREAKERS = ("always(", "cancelled(")

REQUIRED_TYPE = "ready_for_review"


def pr_trigger(on):
    """Return the `pull_request` trigger config, or None if there isn't one.

    PyYAML parses the unquoted key `on:` as the boolean True (YAML 1.1), so the
    top-level mapping is keyed by True rather than "on" -- hence both lookups at
    the call site. A shorthand trigger list (`on: [push, pull_request]`) has no
    `types` to inspect and is reported as a bare dict.
    """
    if isinstance(on, list):
        return {} if "pull_request" in on else None
    if isinstance(on, str):
        return {} if on == "pull_request" else None
    if isinstance(on, dict):
        if "pull_request" not in on:
            return None
        return on["pull_request"] or {}
    return None


def label(path):
    """Repo-relative path for messages, falling back to the absolute one.

    An explicitly passed file can live outside the repo (a fixture in a temp dir),
    and `relative_to` raises rather than degrading -- which turned a would-be
    finding into a traceback the first time this was exercised.
    """
    try:
        return path.relative_to(REPO_ROOT)
    except ValueError:
        return path


def check(path):
    """Return a list of human-readable problems for one workflow file."""
    doc = yaml.safe_load(path.read_text())
    if not isinstance(doc, dict):
        return [f"{label(path)}: not a YAML mapping"]

    # `on` may have been parsed as the boolean key True; see pr_trigger().
    on = doc.get("on", doc.get(True))
    trigger = pr_trigger(on)
    if trigger is None:
        return []  # No pull_request trigger, nothing a draft PR can start.

    problems = []
    rel = label(path)

    types = trigger.get("types")
    if types is None:
        problems.append(
            f"{rel}: `on.pull_request` has no `types`, so it uses the default set "
            f"(opened, synchronize, reopened) and will never fire on "
            f"{REQUIRED_TYPE} -- a PR flipped from draft to ready would get no "
            f"checks at all. Add: types: [opened, synchronize, reopened, "
            f"{REQUIRED_TYPE}]"
        )
    elif REQUIRED_TYPE not in types:
        problems.append(
            f"{rel}: `on.pull_request.types` is {types} and is missing "
            f"`{REQUIRED_TYPE}`, so marking a draft PR ready fires nothing."
        )

    jobs = doc.get("jobs") or {}
    if not jobs:
        problems.append(f"{rel}: no `jobs` block found -- did the parse succeed?")

    for name, job in jobs.items():
        if not isinstance(job, dict):
            problems.append(f"{rel}: job `{name}` is not a mapping")
            continue
        # `if` is another YAML 1.1 casualty in principle, but unlike `on` it is
        # not a boolean word, so it survives as the string "if".
        cond = str(job.get("if", ""))
        if DRAFT_GUARD in cond or PUSH_ONLY in cond:
            continue
        # A dependent job inherits the skip: when every job it `needs` is
        # skipped, it is skipped too -- unless its own condition overrides that.
        if job.get("needs"):
            breaker = next((b for b in CASCADE_BREAKERS if b in cond), None)
            if breaker is None:
                continue
            problems.append(
                f"{rel}: job `{name}` relies on `needs` to inherit the draft "
                f"skip, but its `if` contains `{breaker}`, which runs it even "
                f"when its dependencies were skipped. Add the explicit guard: "
                f"if: github.event_name != 'pull_request' || {DRAFT_GUARD}"
            )
            continue
        problems.append(
            f"{rel}: job `{name}` can run on a draft PR -- it has no `needs` to "
            f"inherit a skip from and no draft guard. Add: "
            f"if: github.event_name != 'pull_request' || {DRAFT_GUARD}"
        )

    return problems


def main(argv):
    if argv:
        paths = [Path(a).resolve() for a in argv]
    else:
        paths = sorted((REPO_ROOT / ".github" / "workflows").glob("*.y*ml"))

    if not paths:
        print("No workflow files found.", file=sys.stderr)
        return 1

    problems = []
    for path in paths:
        problems.extend(check(path))

    if problems:
        for p in problems:
            print(f"::error::{p}")
        # Deliberately stdout, not stderr: the two streams interleave
        # unpredictably in an Actions log, and a trailer that lands above the
        # findings it explains is worse than no trailer.
        print(
            "\nDraft PRs must not spend runner minutes. See the note above "
            "`jobs:` in .github/workflows/ci.yml."
        )
        return 1

    print(f"Draft-PR gate verified across {len(paths)} workflow file(s).")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

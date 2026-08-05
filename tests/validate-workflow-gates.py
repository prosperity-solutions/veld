#!/usr/bin/env python3
"""Assert that no GitHub Actions job can run on a draft pull request.

Draft PRs are where an agentic contributor's intermediate commits land, and
`.github/workflows/ci.yml` is expensive to run: a pull_request run reaches five
macOS legs (`integration`, `injection-test`, the mac leg of `desktop-package`,
and both darwin legs of `release-build`), each billed at 10x a Linux minute, on
top of a four-target release build matrix. The house workflow (AGENTS.md -> PR
Workflow) reviews locally first and marks the PR ready for review second, so a
draft has by definition not earned a CI run yet.

Nothing in Actions enforces that, and the symptom of a missing guard is a bill
rather than a failed job -- so this is the drift gate, in the same spirit as the
THIRD-PARTY-LICENSES.md and desktop/assets checks.

Usage:
    python3 tests/validate-workflow-gates.py [workflow.yml ...]
    python3 tests/validate-workflow-gates.py --selftest

With no arguments it checks every workflow under .github/workflows. `--selftest`
runs the gate against inline fixtures, which is what keeps the gate itself from
silently degrading into a function that returns "fine" for everything.
Requires PyYAML.
"""

import sys
from pathlib import Path

import yaml

REPO_ROOT = Path(__file__).resolve().parent.parent

GUARD = "github.event.pull_request.draft == false"

# The whole condition must equal one of these (after normalization), NOT merely
# contain the guard. A substring test looks equivalent and is not: `true ||
# <guard>` contains the guard and gates nothing at all, because `||`
# short-circuits on the first truthy operand. That hole was found by a review
# angle and reproduced before this became an exact match.
ANY_EVENT_GUARD = "github.event_name != 'pull_request' || " + GUARD
PR_ONLY_GUARD = "github.event_name == 'pull_request' && " + GUARD
# Valid under every trigger: on a push there is no `pull_request` in the payload,
# so the left side is null, and Actions coerces both null and false to 0 when the
# operand types differ -- making `null == false` true and the job run.
# (docs.github.com/actions/reference/workflows-and-actions/expressions)
BARE_GUARD = GUARD

# A job that only ever runs on a push to main cannot run on a PR at all, so it
# needs no draft guard. release.yml's build/desktop jobs are in this class.
PUSH_ONLY = "github.event_name == 'push'"

# These make a job run even when its `needs` were skipped, which is exactly how a
# dependent job escapes the cascade. `cancelled()` counts because the common
# `!cancelled()` idiom has the same effect as `always()` here.
CASCADE_BREAKERS = ("always(", "cancelled(")

REQUIRED_TYPE = "ready_for_review"

# `pull_request_target` carries the same `draft` field and the same activity
# types, so it is just as capable of spending runner minutes on a draft. It is
# listed because omitting it was a real hole: `pr_trigger` matched only the
# literal key `pull_request`, so a future pull_request_target workflow would have
# returned "no findings" while running on every draft push.
PR_TRIGGERS = ("pull_request", "pull_request_target")


def normalize(cond):
    """Collapse an `if:` expression to a canonical single-line string.

    Handles the two spellings that mean the same thing to Actions: an optional
    `${{ ... }}` wrapper, and a YAML block scalar (`>-`) that arrives with
    embedded newlines. Anything this does not recognise simply fails the exact
    match below, which is the safe direction for a cost gate.
    """
    cond = " ".join(str(cond).split())
    if cond.startswith("${{") and cond.endswith("}}"):
        cond = " ".join(cond[3:-2].split())
    return cond


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


def pr_triggers(on):
    """Return {trigger_name: config} for every PR-ish trigger the workflow has.

    PyYAML parses the unquoted key `on:` as the boolean True (YAML 1.1), so the
    top-level mapping is keyed by True rather than "on" -- hence both lookups at
    the call site. A shorthand trigger list (`on: [push, pull_request]`) has no
    `types` to inspect, and is reported as an empty config so the missing-types
    check still fires.
    """
    if isinstance(on, str):
        on = [on]
    if isinstance(on, list):
        return {t: {} for t in PR_TRIGGERS if t in on}
    if isinstance(on, dict):
        return {t: (on[t] or {}) for t in PR_TRIGGERS if t in on}
    return {}


def check_doc(doc, name):
    """Return a list of human-readable problems for one parsed workflow."""
    if not isinstance(doc, dict):
        return [f"{name}: not a YAML mapping"]

    # `on` may have been parsed as the boolean key True; see pr_triggers().
    triggers = pr_triggers(doc.get("on", doc.get(True)))
    if not triggers:
        return []  # Nothing a draft PR can start.

    problems = []

    for trigger, cfg in triggers.items():
        types = cfg.get("types")
        if types is None:
            problems.append(
                f"{name}: `on.{trigger}` has no `types`, so it uses the default "
                f"set (opened, synchronize, reopened) and will never fire on "
                f"{REQUIRED_TYPE} -- a PR flipped from draft to ready would get "
                f"no checks at all. Add: types: [opened, synchronize, reopened, "
                f"{REQUIRED_TYPE}]"
            )
        elif REQUIRED_TYPE not in types:
            problems.append(
                f"{name}: `on.{trigger}.types` is {types} and is missing "
                f"`{REQUIRED_TYPE}`, so marking a draft PR ready fires nothing."
            )

    # `github.event_name != 'pull_request'` is TRUE for a pull_request_target
    # event, so the usual guard leaves such a job wide open. A workflow carrying
    # that trigger must use the bare or event-specific form instead.
    has_target = "pull_request_target" in triggers

    jobs = doc.get("jobs") or {}
    if not jobs:
        problems.append(f"{name}: no `jobs` block found -- did the parse succeed?")

    for job_name, job in jobs.items():
        if not isinstance(job, dict):
            problems.append(f"{name}: job `{job_name}` is not a mapping")
            continue
        cond = normalize(job.get("if", ""))

        if cond in (ANY_EVENT_GUARD, PR_ONLY_GUARD, BARE_GUARD):
            if cond == ANY_EVENT_GUARD and has_target:
                problems.append(
                    f"{name}: job `{job_name}` guards on "
                    f"`github.event_name != 'pull_request'`, which is TRUE for a "
                    f"`pull_request_target` event -- so this job still runs on "
                    f"draft PRs. In a workflow with that trigger use the bare "
                    f"form: if: {BARE_GUARD}"
                )
            continue

        if cond == PUSH_ONLY or cond.startswith(PUSH_ONLY + " &&"):
            continue

        if GUARD in cond:
            problems.append(
                f"{name}: job `{job_name}` mentions the draft guard but its "
                f"condition as a whole is not an accepted form, so the guard may "
                f"not gate anything -- `true || {GUARD}` is the shape this "
                f"catches. Use exactly one of:\n"
                f"      if: {ANY_EVENT_GUARD}\n"
                f"      if: {PR_ONLY_GUARD}\n"
                f"      if: {BARE_GUARD}\n"
                f"    (got: {cond})"
            )
            continue

        # A dependent job inherits the skip: when every job it `needs` is
        # skipped, it is skipped too -- unless its own condition overrides that.
        if job.get("needs"):
            breaker = next((b for b in CASCADE_BREAKERS if b in cond), None)
            if breaker is None:
                continue
            problems.append(
                f"{name}: job `{job_name}` relies on `needs` to inherit the draft "
                f"skip, but its `if` contains `{breaker}`, which runs it even "
                f"when its dependencies were skipped. Add the explicit guard: "
                f"if: {ANY_EVENT_GUARD}"
            )
            continue

        problems.append(
            f"{name}: job `{job_name}` can run on a draft PR -- it has no `needs` "
            f"to inherit a skip from and no draft guard. Add: "
            f"if: {ANY_EVENT_GUARD}"
        )

    return problems


def check(path):
    return check_doc(yaml.safe_load(path.read_text()), str(label(path)))


# ── Self-test ─────────────────────────────────────────────────────────
# A gate whose own correctness was confirmed once, by hand, in a temp directory
# is a gate that can rot into `return []` without anyone noticing. Each fixture
# below is a hole this script has actually been asked to catch.
SELFTEST = [
    (
        "unguarded root job",
        False,
        """
        on: {pull_request: {types: [opened, synchronize, reopened, ready_for_review]}}
        jobs: {expensive: {runs-on: macos-latest, steps: [{run: echo}]}}
        """,
    ),
    (
        "missing ready_for_review type",
        False,
        """
        on: {pull_request: {branches: [main]}}
        jobs:
          ok:
            if: github.event_name != 'pull_request' || github.event.pull_request.draft == false
            runs-on: ubuntu-latest
            steps: [{run: echo}]
        """,
    ),
    (
        "dependent job breaks the cascade with always()",
        False,
        """
        on: {pull_request: {types: [opened, synchronize, reopened, ready_for_review]}}
        jobs:
          root:
            if: github.event_name != 'pull_request' || github.event.pull_request.draft == false
            runs-on: ubuntu-latest
            steps: [{run: echo}]
          dep: {needs: root, if: always(), runs-on: macos-latest, steps: [{run: echo}]}
        """,
    ),
    (
        "guard neutralised by a tautology",
        False,
        """
        on: {pull_request: {types: [opened, synchronize, reopened, ready_for_review]}}
        jobs:
          sneaky:
            if: true || github.event.pull_request.draft == false
            runs-on: macos-latest
            steps: [{run: echo}]
        """,
    ),
    (
        "pull_request_target with the event_name form is not guarded",
        False,
        """
        on: {pull_request_target: {types: [opened, synchronize, reopened, ready_for_review]}}
        jobs:
          labeler:
            if: github.event_name != 'pull_request' || github.event.pull_request.draft == false
            runs-on: ubuntu-latest
            steps: [{run: echo}]
        """,
    ),
    (
        "pull_request_target with the bare guard is fine",
        True,
        """
        on: {pull_request_target: {types: [opened, synchronize, reopened, ready_for_review]}}
        jobs:
          labeler:
            if: github.event.pull_request.draft == false
            runs-on: ubuntu-latest
            steps: [{run: echo}]
        """,
    ),
    (
        "no pull_request trigger at all",
        True,
        """
        on: {push: {branches: [main]}}
        jobs: {anything: {runs-on: macos-latest, steps: [{run: echo}]}}
        """,
    ),
    (
        "canonical guard wrapped in ${{ }} and split over lines",
        True,
        """
        on: {pull_request: {types: [opened, synchronize, reopened, ready_for_review]}}
        jobs:
          ok:
            if: >-
              ${{ github.event_name != 'pull_request'
              || github.event.pull_request.draft == false }}
            runs-on: ubuntu-latest
            steps: [{run: echo}]
        """,
    ),
    (
        "push-only job plus a plain dependent",
        True,
        """
        on:
          push: {branches: [main]}
          pull_request: {types: [opened, synchronize, reopened, ready_for_review]}
        jobs:
          plan:
            if: github.event_name != 'pull_request' || github.event.pull_request.draft == false
            runs-on: ubuntu-latest
            steps: [{run: echo}]
          build:
            needs: plan
            if: github.event_name == 'push' && needs.plan.outputs.go == 'true'
            runs-on: macos-latest
            steps: [{run: echo}]
          publish: {needs: build, runs-on: ubuntu-latest, steps: [{run: echo}]}
        """,
    ),
]


def selftest():
    failures = []
    for name, should_pass, text in SELFTEST:
        problems = check_doc(yaml.safe_load(text), f"<selftest: {name}>")
        passed = not problems
        if passed != should_pass:
            want = "pass" if should_pass else "be rejected"
            failures.append(f"  fixture {name!r} should {want}, got {problems or 'no problems'}")
        print(f"  {'ok' if passed == should_pass else 'FAIL'}  {name}")

    if failures:
        print("\n::error::the draft-PR gate no longer detects what it claims to:")
        for f in failures:
            print(f)
        return 1
    print(f"Self-test passed ({len(SELFTEST)} fixtures).")
    return 0


def main(argv):
    if "--selftest" in argv:
        return selftest()

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

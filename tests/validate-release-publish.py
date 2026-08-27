#!/usr/bin/env python3
"""Execute release.yml's publish script against a stub `gh` and assert its invariants.

`softprops/action-gh-release` stranded v16.57.1: it uploads assets to a draft
release and then flips `draft=false`, has no retry on a 5xx, and one asset
upload came back as GitHub's 500 HTML page.  The action threw, the flip never
ran, and the release sat invisible as a draft while `releases/latest` still
pointed at the previous version — `veld update`, install.sh and Desktop
auto-update all read `latest`, so for every user the release did not exist.

The replacement lives inline in the workflow, and every rule it follows bends
one way: **a failure must fall toward "do not publish"**.  This gate exists to
hold it to that, because the opposite failure is silent — an incomplete or
stranded release looks like a red X on a job nobody re-reads.

It runs the real script, extracted from the workflow rather than transcribed,
against a stub `gh` that models release state (name / size / `state`), the
draft flag, and `--clobber`.  Two things follow from the history here and are
worth keeping when editing:

  * **The stub refuses anything it does not model** (exit 64) rather than
    guessing.  An earlier version hardcoded the `--jq` semantics, so deleting
    `select(.state == "uploaded")` from the script left the suite green while
    the property that filter implements was named in two passing checks.
  * **`--selftest` mutates the completeness gate out** and asserts the suite
    goes red, so the gate cannot rot into something that passes trivially.

Needs PyYAML, same as validate-workflow-gates.py; both run from
`just workflow-gates` and from the `schema` job in ci.yml.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import yaml

REPO = Path(__file__).resolve().parent.parent
WORKFLOW = REPO / ".github" / "workflows" / "release.yml"
STEP_NAME = "Publish GitHub Release"
JOB = "publish"

# The env the step must hand the script. There is no checkout in this job, so
# `GH_REPO` is what lets gh find the repo at all; `TAG` empty is worse than
# absent, because `gh release view ""` returns the latest published release.
REQUIRED_ENV = {"GH_TOKEN", "GH_REPO", "TAG"}

# Release state is a TSV of name/size/state so an asset can be present-but-wrong
# — `starter`, or short — which is what a name-only check waves through.
# Unmodelled flags and queries exit 64 rather than being ignored: a stub that
# quietly tolerates a changed argument proves nothing about the changed script.
GH_STUB = r"""#!/usr/bin/env bash
set -uo pipefail
echo "$*" >> "$GHLOG"
drop() { awk -F'\t' -v n="$1" '$1!=n' "$STATE" > "$STATE.tmp"; mv "$STATE.tmp" "$STATE"; }
put()  { drop "$1"; printf '%s\t%s\t%s\n' "$1" "$2" "$3" >> "$STATE"; }
has()  { awk -F'\t' -v n="$1" '$1==n{f=1} END{exit !f}' "$STATE"; }
die64() { echo "gh stub: $1" >&2; exit 64; }

sub="${1-} ${2-}"
shift 2 2>/dev/null || true
# `gh api -X PATCH <path> ...`: drop the verb so the path is $1, as it is for
# the `release <verb> <tag>` forms.
[ "$sub" = "api -X" ] && shift 2>/dev/null || true

case "$sub" in
  "release view")
    shift 2>/dev/null || true          # the tag
    json=""; jq=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --json) json=${2-}; shift 2 ;;
        --jq)   jq=${2-}; shift 2 ;;
        *)      die64 "unmodelled release-view flag: $1" ;;
      esac
    done
    # Bare `gh release view <tag>` is the existence probe.
    if [ -z "$json" ]; then
      probes=$(cat "$PROBES"); echo $((probes + 1)) > "$PROBES"
      if [ "$probes" -lt "${FAIL_PROBE_UNTIL-0}" ]; then
        echo "gh: 500 Internal Server Error" >&2; exit 1
      fi
      [ -f "$STATE" ] || exit 1
      exit 0
    fi
    [ -f "$STATE" ] || exit 1
    if [ "${FAIL_VIEW-0}" = "1" ]; then
      echo "gh: 500 Internal Server Error" >&2; exit 1
    fi
    case "$json|$jq" in
      'assets|.assets[] | select(.state == "uploaded") | "\(.name)\t\(.size)"')
        awk -F'\t' '$3=="uploaded"{print $1"\t"$2}' "$STATE" ;;
      'isDraft|.isDraft')
        if [ "$(cat "$DRAFTF")" = "draft" ]; then echo true; else echo false; fi ;;
      'databaseId|.databaseId')
        echo 4242 ;;
      *)
        die64 "unmodelled query --json '$json' --jq '$jq'" ;;
    esac
    exit 0 ;;

  "release create")
    shift 2>/dev/null || true          # the tag
    draft=published; notes=none
    while [ $# -gt 0 ]; do
      case "$1" in
        --draft)      draft=draft; shift ;;
        --title)      shift 2 ;;
        --notes-file) if [ -f "${2-}" ]; then notes=ok; else notes=missing; fi; shift 2 ;;
        *)            die64 "unmodelled release-create flag: $1" ;;
      esac
    done
    # Real gh refuses a tag that already carries a release; without this the
    # stub would silently wipe a preseeded draft's assets and re-upload them.
    if [ -f "$STATE" ]; then
      echo "gh: a release for $TAG already exists" >&2; exit 1
    fi
    if [ "$draft" = draft ]; then echo yes > "$CREATEDRAFTF"; else echo no > "$CREATEDRAFTF"; fi
    : > "$STATE"; echo "$draft" > "$DRAFTF"; echo "$notes" > "$NOTESF"
    exit 0 ;;

  "release upload")
    shift 2>/dev/null || true          # the tag
    clobber=0; files=()
    for arg in "$@"; do
      if [ "$arg" = "--clobber" ]; then clobber=1; else files+=("$arg"); fi
    done
    attempt=$(cat "$ATTEMPT"); rc=0
    for f in "${files[@]}"; do
      name=$(basename "$f")
      if [ ! -f "$f" ]; then echo "gh: $f: no such file" >&2; rc=1; continue; fi
      size=$(( $(wc -c < "$f") ))
      # Real gh rejects a name already on the release unless --clobber.
      if has "$name" && [ "$clobber" -eq 0 ]; then
        echo "gh: asset $name already exists" >&2; rc=1; continue
      fi
      if [[ ",${FAIL_FILES-}," == *",$name,"* ]] && [ "$attempt" -le "${FAIL_UNTIL-99}" ]; then
        case "${FAIL_MODE-error}" in
          starter)   put "$name" "$((size / 2))" starter ;;
          truncated) put "$name" "$((size - 1))" uploaded ;;
        esac
        rc=1; continue
      fi
      put "$name" "$size" uploaded
    done
    echo $((attempt + 1)) > "$ATTEMPT"
    exit $rc ;;

  "api -X")
    # `gh api -X PATCH repos/<repo>/releases/<id> -F draft=false -f make_latest=...`
    shift 2>/dev/null || true          # the path
    edits=$(cat "$EDITS"); echo $((edits + 1)) > "$EDITS"
    if [ "$edits" -lt "${FAIL_EDIT_UNTIL-0}" ]; then
      echo "gh: 502 Bad Gateway" >&2; exit 1
    fi
    want=""
    while [ $# -gt 0 ]; do
      case "$1" in
        -F|-f)
          case "${2-}" in
            draft=false)      want=published ;;
            draft=true)       want=draft ;;
            make_latest=*)    echo "${2#make_latest=}" > "$LATESTF" ;;
            *)                die64 "unmodelled api field: ${2-}" ;;
          esac
          shift 2 ;;
        *) die64 "unmodelled api flag: $1" ;;
      esac
    done
    # EDIT_SILENT models the API accepting the call without the release
    # actually leaving draft — the case the script reads back to catch.
    if [ "${EDIT_SILENT-0}" = "1" ]; then exit 0; fi
    [ -n "$want" ] && echo "$want" > "$DRAFTF"
    exit 0 ;;
esac
die64 "unmodelled subcommand: $sub"
"""

# Asserts the exact GNU flags the script passes before emulating them, so a
# typo'd `stat` fails the suite instead of quietly measuring nothing. STAT_FAIL
# names one file it refuses to measure — the case where an unmeasurable file
# must count as missing, never as present.
STAT_STUB = r"""#!/usr/bin/env bash
if [ "${1-}" != "-c" ] || [ "${2-}" != "%s" ]; then
  echo "stat stub: expected '-c %s', got: $*" >&2; exit 64
fi
if [ -n "${STAT_FAIL-}" ] && [ "$(basename "$3")" = "$STAT_FAIL" ]; then
  echo "stat: cannot statx '$3'" >&2; exit 1
fi
wc -c < "$3" | tr -d ' '
"""

# Logs every wrapped invocation, so "the upload is wrapped in a timeout at all"
# is assertable — a hang, not an error, is what actually cost v16.57.1.
TIMEOUT_STUB = r"""#!/usr/bin/env bash
args=()
while [ $# -gt 0 ]; do
  case "$1" in
    -k) args+=("-k $2"); shift 2 ;;
    ''|*[!0-9]*) break ;;
    *) args+=("$1"); shift ;;
  esac
done
secs=""
for a in ${args[@]+"${args[@]}"}; do
  case "$a" in ''|*[!0-9]*) ;; *) secs=$a ;; esac
done
if [ -z "$secs" ]; then
  echo "timeout stub: no duration (only $* )" >&2; exit 64
fi
echo "${args[*]} :: $*" >> "$TIMEOUTLOG"
exec "$@"
"""

SLEEP_STUB = r"""#!/usr/bin/env bash
echo "$1" >> "$SLEEPLOG"
"""

ARTIFACTS = ["veld-1.2.3-linux-amd64.tar.gz", "veld-desktop-1.2.3-mac-x64.zip",
             "veld-desktop-1.2.3-mac-x64.zip.blockmap", "checksums.txt"]

# Stub-control variables must not leak in from the caller's environment, or
# `FAIL_VIEW=1 just workflow-gates` silently reconfigures every scenario.
STUB_VARS = {"FAIL_FILES", "FAIL_UNTIL", "FAIL_MODE", "FAIL_VIEW", "FAIL_EDIT_UNTIL",
             "EDIT_SILENT", "STAT_FAIL", "FAIL_PROBE_UNTIL", "PROBES", "STATE", "DRAFTF", "GHLOG", "SLEEPLOG",
             "TIMEOUTLOG", "ATTEMPT", "EDITS", "NOTESF", "LATESTF", "CREATEDRAFTF",
             "TAG", "GH_REPO"}


def publish_step() -> dict:
    doc = yaml.safe_load(WORKFLOW.read_text())
    try:
        steps = doc["jobs"][JOB]["steps"]
    except (KeyError, TypeError):
        sys.exit(f"{WORKFLOW.name}: no `{JOB}` job with steps")
    for step in steps:
        if step.get("name") == STEP_NAME:
            # A rename upstream would otherwise leave this gate running an
            # empty string and reporting a clean bill of health.
            if "draft=false" not in (step.get("run") or ""):
                sys.exit(f"step {STEP_NAME!r} no longer publishes the release")
            return step
    sys.exit(
        f"{WORKFLOW.name}: job `{JOB}` has no step named {STEP_NAME!r}. If the "
        "publish step was renamed, update STEP_NAME here — this gate is the only "
        "check that an incomplete release cannot publish."
    )


class Result:
    def __init__(self, code, out, state, uploads, sleeps, timeouts, draft,
                 notes, latest, edits, created_draft):
        self.code, self.out, self.state = code, out, state
        self.uploads, self.sleeps, self.timeouts = uploads, sleeps, timeouts
        self.draft, self.notes, self.latest, self.edits = draft, notes, latest, edits
        self.created_draft = created_draft

    @property
    def published(self) -> bool:
        return self.draft == "published"


def run(script: str, env_overrides: dict[str, str] | None = None,
        artifacts: list[str] = ARTIFACTS, preseed: list[str] | None = None,
        subdir: bool = False, tag: str = "v1.2.3") -> Result:
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        binw = work / "bin"
        binw.mkdir()
        for name, body in (("gh", GH_STUB), ("stat", STAT_STUB),
                           ("timeout", TIMEOUT_STUB), ("sleep", SLEEP_STUB)):
            path = binw / name
            path.write_text(body)
            path.chmod(0o755)

        (work / "artifacts").mkdir()
        for i, name in enumerate(artifacts):
            # Distinct sizes, so a size comparison cannot pass by coincidence.
            (work / "artifacts" / name).write_bytes(b"x" * (1024 + i * 37))
        if subdir:
            (work / "artifacts" / "dist").mkdir()
            (work / "artifacts" / "dist" / "nested.dmg").write_bytes(b"x" * 99)
        (work / "release-notes.md").write_text("notes\n")

        files = {k: work / f"{k}.txt" for k in
                 ("state", "draft", "gh", "sleep", "timeout", "attempt", "edits",
                  "notes", "latest", "createdraft", "probes")}
        for f in files.values():
            f.touch()
        files["attempt"].write_text("1\n")
        files["edits"].write_text("0\n")
        files["probes"].write_text("0\n")
        if preseed is not None:
            # A re-run of the job: the draft exists, carrying some assets.
            files["draft"].write_text("draft\n")
            files["state"].write_text("".join(
                f"{n}\t{(work / 'artifacts' / n).stat().st_size}\tuploaded\n"
                for n in preseed))
        else:
            files["state"].unlink()

        env = {k: v for k, v in os.environ.items() if k not in STUB_VARS}
        env.update({
            "PATH": f"{binw}{os.pathsep}{os.environ['PATH']}",
            "TAG": tag,
            "GH_REPO": "example/veld",
            "STATE": str(files["state"]), "DRAFTF": str(files["draft"]),
            "GHLOG": str(files["gh"]), "SLEEPLOG": str(files["sleep"]),
            "TIMEOUTLOG": str(files["timeout"]), "ATTEMPT": str(files["attempt"]),
            "EDITS": str(files["edits"]), "NOTESF": str(files["notes"]),
            "LATESTF": str(files["latest"]),
            "CREATEDRAFTF": str(files["createdraft"]),
            "PROBES": str(files["probes"]),
            **(env_overrides or {}),
        })
        proc = subprocess.run([BASH, "-c", script], cwd=work, env=env,
                              capture_output=True, text=True, timeout=180)

        # Records are `name\tsize\tstate\n`. Parsed by regex rather than by
        # line so that a scenario feeding a rejected name (newline, tab) can
        # still be inspected instead of crashing the harness.
        state: dict[str, tuple[int, str]] = {
            m[1]: (int(m[2]), m[3]) for m in re.finditer(
                r"(?s)(.*?)\t(\d+)\t(uploaded|starter)\n",
                files["state"].read_text() if files["state"].exists() else "")}
        ghlog = files["gh"].read_text().splitlines()
        uploads = [[Path(a).name for a in ln.split()[3:] if a != "--clobber"]
                   for ln in ghlog if ln.startswith("release upload")]
        return Result(
            proc.returncode, proc.stdout + proc.stderr, state, uploads,
            files["sleep"].read_text().split(),
            files["timeout"].read_text().splitlines(),
            files["draft"].read_text().strip() or "none",
            files["notes"].read_text().strip() or "none",
            files["latest"].read_text().strip(),
            [ln for ln in ghlog if ln.startswith("api -X")],
            files["createdraft"].read_text().strip() or "none",
        )


FAILURES: list[str] = []


def check(label: str, condition: bool, detail: str = "") -> None:
    if condition:
        print(f"  ok  {label}")
    else:
        print(f"  FAIL {label}{(': ' + detail) if detail else ''}")
        FAILURES.append(label)


# ── the happy path, and the shape of it ──────────────────────────────────────

def scenario_complete(script: str) -> None:
    r = run(script)
    check("a complete upload publishes the release", r.published and r.code == 0,
          f"code={r.code} draft={r.draft}")
    check("every artifact lands as an asset", sorted(r.state) == sorted(ARTIFACTS),
          f"{sorted(r.state)}")
    check("a clean run uploads once, not once per file", len(r.uploads) == 1,
          f"{len(r.uploads)} upload calls")
    check("release notes are attached from the notes file", r.notes == "ok", r.notes)


def scenario_draft_first(script: str) -> None:
    """The ordering the workflow comment calls load-bearing.

    Without this, flipping the script to `--draft=true` — which would strand
    every release exactly as v16.57.1 was stranded — left the suite green.
    """
    r = run(script)
    # Asserted from what the stub recorded, not from stdout: `"--draft" in
    # r.out or r.published` was true on every successful run, so the check
    # could not fail even when the release was created already visible.
    check("the release is created as a draft, not published mid-upload",
          r.created_draft == "yes", f"created_draft={r.created_draft}")
    check("the release ends up explicitly published, not left in draft",
          r.published and any("draft=false" in e for e in r.edits), f"{r.edits}")
    # `legacy`, never `true`. `true` re-points `releases/latest` at whatever it
    # is called on, so publishing an old stranded draft after a newer release
    # shipped would downgrade every `curl | bash` install and `veld update`
    # (install.sh resolves `/releases/latest`). `legacy` lets GitHub pick by
    # version and date, which is correct for both a normal release and a
    # late top-up.
    check("latest is left to GitHub's version/date rule, not forced to this tag",
          r.latest == "legacy", f"make_latest={r.latest!r}")


def scenario_uploads_are_time_capped(script: str) -> None:
    r = run(script)
    # `any(...)` per call, not `all(... if "upload" in t)`: the latter is
    # vacuously true when no upload was wrapped at all, and `publish()` wraps
    # its own call, so the count alone stays satisfied. A hang — not an error —
    # is what cost v16.57.1, so "wrapped at all" is the property worth naming.
    check("the upload runs under a timeout with a kill fallback",
          any("gh release upload" in t and "-k" in t for t in r.timeouts),
          f"{r.timeouts}")
    check("the publish call runs under a timeout too",
          any("gh api" in t and "-k" in t for t in r.timeouts), f"{r.timeouts}")
    check("the release reads run under a timeout too",
          any("gh release view" in t and "-k" in t for t in r.timeouts), f"{r.timeouts}")


# ── retries ──────────────────────────────────────────────────────────────────

def scenario_transient_5xx(script: str) -> None:
    """The v16.57.1 failure itself: one asset 5xxs, the rest already landed."""
    stranded = "veld-desktop-1.2.3-mac-x64.zip.blockmap"
    r = run(script, {"FAIL_FILES": stranded, "FAIL_UNTIL": "2", "FAIL_MODE": "error"})
    check("a transient 5xx on one asset still publishes", r.published and r.code == 0,
          f"code={r.code} draft={r.draft}")
    check("the retry re-sends only what is missing",
          len(r.uploads) == 3 and all(u == [stranded] for u in r.uploads[1:]),
          f"{r.uploads}")
    check("backoff grows between attempts", r.sleeps[:2] == ["15", "30"], f"{r.sleeps}")


def scenario_last_attempt_counts(script: str) -> None:
    """The final re-verify: a release whose last upload succeeds must publish."""
    r = run(script, {"FAIL_FILES": "checksums.txt", "FAIL_UNTIL": "4"})
    check("an upload that succeeds on the last attempt still publishes",
          r.published and r.code == 0, f"code={r.code} draft={r.draft}")


def scenario_publish_flip_retries(script: str) -> None:
    """The flip is the call that stranded v16.57.1. It must retry like the rest."""
    r = run(script, {"FAIL_EDIT_UNTIL": "2"})
    check("a 5xx on the publish call itself is retried, not fatal",
          r.published and r.code == 0, f"code={r.code} draft={r.draft} edits={len(r.edits)}")


def scenario_publish_is_read_back(script: str) -> None:
    """`gh release edit` exiting 0 is not the same fact as being published."""
    r = run(script, {"EDIT_SILENT": "1"})
    check("a publish call that reports success but leaves a draft fails the job",
          r.code != 0 and not r.published, f"code={r.code} draft={r.draft}")


# ── present-but-wrong ────────────────────────────────────────────────────────

def scenario_present_but_wrong(script: str) -> None:
    for mode, label in (("starter", "an asset still uploading"),
                        ("truncated", "an asset with the wrong byte count")):
        victim = "veld-desktop-1.2.3-mac-x64.zip"
        r = run(script, {"FAIL_FILES": victim, "FAIL_UNTIL": "1", "FAIL_MODE": mode})
        check(f"{label} is re-uploaded, not counted as present",
              r.published and r.state.get(victim, (0, ""))[1] == "uploaded"
              and len(r.uploads) == 2 and r.uploads[1] == [victim],
              f"state={r.state.get(victim)} uploads={r.uploads}")


def scenario_unmeasurable_file(script: str) -> None:
    """An unmeasurable local file must count as missing, never as present.

    Comparing `"${have[$name]-}"` against an inline `$(stat ...)` makes both
    sides empty when stat fails, so the file scored as present and the release
    published without it.
    """
    r = run(script, {"STAT_FAIL": "checksums.txt"})
    check("a local file that cannot be measured never counts as present",
          r.code != 0 and not r.published, f"code={r.code} draft={r.draft}")


# ── reads ────────────────────────────────────────────────────────────────────

def scenario_unreadable_release(script: str) -> None:
    r = run(script, {"FAIL_VIEW": "1"})
    check("a release that cannot be read is never published",
          r.code != 0 and not r.published, f"code={r.code} draft={r.draft}")
    # Swallowing the read failure keeps the release safe — an empty asset list
    # reads as "everything is missing", so the gate still fires — but it does so
    # after rounds of re-pushing every artifact at an API that is down.
    check("an unreadable release fails without re-pushing every artifact",
          r.uploads == [], f"{len(r.uploads)} upload call(s)")
    check("the read is retried before giving up", len(r.sleeps) >= 2, f"{r.sleeps}")


# ── the gate ─────────────────────────────────────────────────────────────────

def scenario_never_completes(script: str) -> None:
    r = run(script, {"FAIL_FILES": "checksums.txt", "FAIL_UNTIL": "99"})
    check("an incomplete release is never published, and fails the job",
          r.code != 0 and not r.published, f"code={r.code} draft={r.draft}")
    check("the error names the tag and says it stayed a draft",
          "v1.2.3" in r.out and "draft" in r.out, r.out[-200:])


# ── inputs the job must refuse ───────────────────────────────────────────────

def scenario_refused_inputs(script: str) -> None:
    cases: list = [
        ("an empty tag", dict(tag=""), "would otherwise read the latest published release"),
        ("no artifacts", dict(artifacts=[]), "an empty release"),
        ("a nested artifacts/ directory", dict(subdir=True), "a re-rooted upload glob"),
    ]
    # Names GitHub or gh would not round-trip. Each uploads fine and then can
    # never be matched back, so the job would spend all five upload rounds and
    # strand the release as a draft no re-run can clear — worse than the
    # incident this step fixes. `#` is gh's asset-label separator; a space is
    # rewritten to `.` by GitHub; tab and newline cannot survive the
    # tab-separated asset listing the completeness check reads back.
    for bad, why in (("veld#1.dmg", "gh reads '#' as an asset label"),
                     ("veld 1.dmg", "GitHub rewrites a space to '.'"),
                     ("veld\t1.dmg", "a tab breaks the asset listing"),
                     ("veld\n1.dmg", "a newline breaks the asset listing")):
        cases.append((f"the artifact name {bad!r}",
                      dict(artifacts=ARTIFACTS + [bad]), why))

    for label, kwargs, why in cases:
        r = run(script, **kwargs)
        # `r.uploads == []` matters as much as the exit code: each of these is
        # knowable before a byte is sent, and refusing only after five upload
        # rounds is a different (worse) behaviour that the exit code alone
        # cannot distinguish.
        check(f"{label} fails the job before uploading anything ({why})",
              r.code != 0 and not r.published and r.uploads == [],
              f"code={r.code} draft={r.draft} uploads={len(r.uploads)}")



def scenario_rerun(script: str) -> None:
    already = ARTIFACTS[:2]
    r = run(script, preseed=already)
    check("a re-run tops up an existing draft and publishes",
          r.published and r.code == 0, f"code={r.code} draft={r.draft}")
    check("a re-run does not re-upload assets already on the release",
          len(r.uploads) == 1 and sorted(r.uploads[0]) == sorted(ARTIFACTS[2:]),
          f"{r.uploads}")


def scenario_clobber(script: str) -> None:
    """Without --clobber, real gh refuses a name already on the release, so a
    truncated asset could never be repaired — the retry would spin and the
    release would strand. The stub enforces the same rule."""
    victim = "veld-desktop-1.2.3-mac-x64.zip"
    r = run(script, {"FAIL_FILES": victim, "FAIL_UNTIL": "1", "FAIL_MODE": "truncated"})
    check("a damaged asset is overwritten rather than rejected as existing",
          r.published and "already exists" not in r.out, r.out[-200:])


# ── wiring ───────────────────────────────────────────────────────────────────

def scenario_every_github_call_is_capped(script: str) -> None:
    """The step header claims every GitHub call is retried AND time-capped.

    A runtime check can only observe that *some* call was wrapped — with
    several call sites, dropping the cap from one of them stays green. This
    reads the script and holds every site to the claim.
    """
    uncapped = []
    for lineno, line in enumerate(script.splitlines(), 1):
        text = line.strip()
        if text.startswith("#"):
            continue
        for call in ("gh release view", "gh release upload",
                     "gh release create", "gh api"):
            if call in text and "timeout -k" not in text:
                uncapped.append(f"line {lineno}: {text[:60]}")
    check("every GitHub call in the script is wrapped in a timeout",
          not uncapped, "; ".join(uncapped))


def scenario_probe_retries(script: str) -> None:
    """A 5xx on the existence probe must not send a re-run into `release create`.

    Real gh refuses to create a release for a tag that already has one, so
    without the retry a transient 5xx on the probe fails the job — and the
    probe is only reached on the top-up path, which is precisely the recovery
    this step advertises for when GitHub is flaky.
    """
    r = run(script, {"FAIL_PROBE_UNTIL": "2"}, preseed=ARTIFACTS[:2])
    check("a 5xx on the existence probe is retried, not treated as absent",
          r.published and r.code == 0, f"code={r.code} draft={r.draft}")


def scenario_step_env(step: dict) -> None:
    env = set(step.get("env") or {})
    check(f"the step passes {', '.join(sorted(REQUIRED_ENV))} to the script",
          REQUIRED_ENV <= env, f"missing {sorted(REQUIRED_ENV - env)}")
    tag = (step.get("env") or {}).get("TAG", "")
    check("TAG is wired to the release job's tag output",
          "new_release_git_tag" in str(tag), str(tag))


def selftest(script: str) -> None:
    """Mutate the gate out and assert the suite notices.

    Without this, `scenario_never_completes` could be passing because the script
    fails for some unrelated reason rather than because the gate is there — the
    same trap `validate-workflow-gates.py --selftest` exists for.
    """
    print("Self-test: removing the completeness gate should let a bad release publish")
    lines = script.splitlines()
    marker = 'if [ "${#missing[@]}" -ne 0 ]; then'
    starts = [i for i, ln in enumerate(lines) if marker in ln]
    if len(starts) != 1:
        sys.exit(f"self-test: expected exactly one completeness gate, found "
                 f"{len(starts)}. It was renamed, reformatted onto one line, or "
                 "duplicated — this suite no longer proves the gate exists.")
    start = starts[0]
    indent = len(lines[start]) - len(lines[start].lstrip())
    ends = [i for i in range(start + 1, len(lines))
            if lines[i].strip() == "fi" and len(lines[i]) - len(lines[i].lstrip()) == indent]
    if not ends:
        sys.exit("self-test: could not find the end of the completeness gate; "
                 "the block was reformatted and the mutation would be wrong.")
    mutated = "\n".join(lines[:start] + lines[ends[0] + 1:])

    r = run(mutated, {"FAIL_FILES": "checksums.txt", "FAIL_UNTIL": "99"})
    if not r.published:
        sys.exit("self-test FAILED: the mutant refused to publish an incomplete "
                 "release anyway, so scenario_never_completes is not testing the gate")
    print("  ok  the mutant publishes an incomplete release, as it must to be caught")


def main() -> int:
    step = publish_step()
    script = step["run"]

    if "--selftest" in sys.argv:
        selftest(script)
        return 0

    print(f"Release publish gate — {WORKFLOW.name} → {JOB} → {STEP_NAME!r}")
    scenario_complete(script)
    scenario_draft_first(script)
    scenario_uploads_are_time_capped(script)
    scenario_transient_5xx(script)
    scenario_last_attempt_counts(script)
    scenario_publish_flip_retries(script)
    scenario_publish_is_read_back(script)
    scenario_present_but_wrong(script)
    scenario_unmeasurable_file(script)
    scenario_unreadable_release(script)
    scenario_never_completes(script)
    scenario_refused_inputs(script)
    scenario_rerun(script)
    scenario_clobber(script)
    scenario_every_github_call_is_capped(script)
    scenario_probe_retries(script)
    scenario_step_env(step)

    if FAILURES:
        print(f"\n{len(FAILURES)} failure(s):")
        for f in FAILURES:
            print(f"  - {f}")
        return 1
    print("\nAll release publish invariants hold.")
    return 0


def find_bash() -> str:
    """bash 4.4+ — the script uses `mapfile -d ''` and `${x@Q}`.

    ubuntu-latest ships bash 5. macOS still ships 3.2 as /bin/bash, so a
    contributor running `just workflow-gates` locally needs Homebrew's.
    """
    candidates = [os.environ["BASH_BIN"]] if os.environ.get("BASH_BIN") else [
        "/opt/homebrew/bin/bash", "/usr/local/bin/bash", "/bin/bash", "bash"]
    for candidate in candidates:
        path = shutil.which(candidate)
        if path and subprocess.run([path, "-c", "((BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4)))"],
                                   capture_output=True).returncode == 0:
            return path
    sys.exit("no bash >= 4.4 found (macOS /bin/bash is 3.2): `brew install bash`, "
             "or point BASH_BIN at one.")


BASH = find_bash()

if __name__ == "__main__":
    sys.exit(main())

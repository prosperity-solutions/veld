#!/usr/bin/env python3
"""Execute release.yml's publish script against a stub `gh` and assert its invariants.

`softprops/action-gh-release` stranded v16.57.1: it uploads assets to a draft
release and then flips `draft=false`, has no retry on a 5xx, and one asset
upload came back as GitHub's 500 HTML page.  The action threw, the flip never
ran, and the release sat invisible as a draft while `releases/latest` still
pointed at the previous version — `veld update`, install.sh and Desktop
auto-update all read `latest`, so for every user the release simply did not
exist.  The replacement lives inline in the workflow and owns four properties
that nothing else in this repo can check:

  * a complete upload publishes;
  * a transient failure retries **only the assets still missing**, not the
    ~700 MB that already landed;
  * an asset that is present but wrong — truncated, or still `state=starter` —
    counts as missing;
  * a release that is *not* complete is never published, including when the
    reason is that GitHub could not be read at all.

The last one is the load-bearing one, and its failure is silent: an incomplete
or stranded release looks like a red X on a job nobody re-reads.  So this gate
runs the real script — extracted from the workflow, never a copy — and
`--selftest` mutates the gate out of it to prove these scenarios would actually
notice if someone deleted it.

Needs PyYAML, same as validate-workflow-gates.py; both are run by
`just workflow-gates` and by the `schema` job in ci.yml.
"""

from __future__ import annotations

import os
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

# The stub speaks only the `gh` surface the script uses.  Release state is a
# TSV of name/size/state, so an asset can be present-but-wrong (`starter`, or a
# short byte count) — the cases a name-only presence check would wave through.
GH_STUB = r"""#!/usr/bin/env bash
set -uo pipefail
echo "$*" >> "$GHLOG"
drop() { awk -F'\t' -v n="$1" '$1!=n' "$STATE" > "$STATE.tmp"; mv "$STATE.tmp" "$STATE"; }
put()  { drop "$1"; printf '%s\t%s\t%s\n' "$1" "$2" "$3" >> "$STATE"; }
case "${1-} ${2-}" in
  "release view")
    [ -f "$STATE" ] || exit 1
    if [ "${4-}" = "--json" ]; then
      # The one call whose failure must abort the script rather than read as
      # "nothing is missing".
      [ "${FAIL_VIEW-0}" = "1" ] && { echo "gh: 500 Internal Server Error" >&2; exit 1; }
      awk -F'\t' '$3=="uploaded"{print $1"\t"$2}' "$STATE"
    fi
    exit 0 ;;
  "release create")
    : > "$STATE"; echo draft > "$DRAFTF"; exit 0 ;;
  "release upload")
    shift 3
    files=(); for a in "$@"; do [ "$a" = "--clobber" ] || files+=("$a"); done
    attempt=$(cat "$ATTEMPT"); rc=0
    for f in "${files[@]}"; do
      name=$(basename "$f"); size=$(( $(wc -c < "$f") ))
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
  "release edit")
    echo published > "$DRAFTF"; exit 0 ;;
esac
exit 0
"""

# Asserts the exact GNU flags the script passes before emulating them, so a
# typo'd `stat` call fails the suite instead of quietly measuring nothing.
STAT_STUB = r"""#!/usr/bin/env bash
if [ "${1-}" != "-c" ] || [ "${2-}" != "%s" ]; then
  echo "stat stub: expected '-c %s', got: $*" >&2; exit 64
fi
wc -c < "$3" | tr -d ' '
"""

# Likewise for `timeout`: assert a numeric budget, then run the command. A hang
# was what cost v16.57.1, so the script losing its timeout is worth catching.
TIMEOUT_STUB = r"""#!/usr/bin/env bash
case "${1-}" in
  ''|*[!0-9]*) echo "timeout stub: expected seconds, got: $*" >&2; exit 64 ;;
esac
shift
exec "$@"
"""

# No-op so the suite does not sit through the real backoff; the requested
# delays are recorded instead and asserted separately.
SLEEP_STUB = r"""#!/usr/bin/env bash
echo "$1" >> "$SLEEPLOG"
"""

ARTIFACTS = ["veld-1.2.3-linux-amd64.tar.gz", "veld-desktop-1.2.3-mac-x64.zip",
             "veld-desktop-1.2.3-mac-x64.zip.blockmap", "checksums.txt"]


def publish_script() -> str:
    """The script as the workflow actually ships it — never a transcription."""
    doc = yaml.safe_load(WORKFLOW.read_text())
    try:
        steps = doc["jobs"][JOB]["steps"]
    except (KeyError, TypeError):
        sys.exit(f"{WORKFLOW.name}: no `{JOB}` job with steps")
    for step in steps:
        if step.get("name") == STEP_NAME:
            script = step.get("run") or ""
            # A rename upstream would otherwise leave this gate testing an empty
            # string and reporting a clean bill of health.
            if "gh release edit" not in script:
                sys.exit(f"step {STEP_NAME!r} no longer publishes the release")
            return script
    sys.exit(
        f"{WORKFLOW.name}: job `{JOB}` has no step named {STEP_NAME!r}. "
        "If the publish step was renamed, update STEP_NAME here — this gate is "
        "the only check that an incomplete release cannot publish."
    )


class Result:
    def __init__(self, code: int, out: str, state: dict[str, tuple[int, str]],
                 uploads: list[list[str]], sleeps: list[str], draft: str):
        self.code, self.out, self.state = code, out, state
        self.uploads, self.sleeps, self.draft = uploads, sleeps, draft

    @property
    def published(self) -> bool:
        return self.draft == "published"


def run(script: str, env_overrides: dict[str, str] | None = None,
        artifacts: list[str] = ARTIFACTS, preseed: list[str] | None = None) -> Result:
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
            # Distinct sizes so a size comparison cannot pass by coincidence.
            (work / "artifacts" / name).write_bytes(b"x" * (1024 + i * 37))
        (work / "release-notes.md").write_text("notes\n")

        state, draftf = work / "state.tsv", work / "draft"
        ghlog, sleeplog, attempt = work / "gh.log", work / "sleep.log", work / "attempt"
        attempt.write_text("1\n")
        ghlog.touch()
        sleeplog.touch()
        if preseed is not None:
            # A re-run of the job: the draft already exists, carrying some assets.
            draftf.write_text("draft\n")
            state.write_text("".join(
                f"{n}\t{(work / 'artifacts' / n).stat().st_size}\tuploaded\n"
                for n in preseed))

        env = {
            **os.environ,
            "PATH": f"{binw}{os.pathsep}{os.environ['PATH']}",
            "TAG": "v1.2.3",
            "STATE": str(state), "DRAFTF": str(draftf),
            "GHLOG": str(ghlog), "SLEEPLOG": str(sleeplog), "ATTEMPT": str(attempt),
            **(env_overrides or {}),
        }
        proc = subprocess.run([BASH, "-c", script], cwd=work, env=env,
                              capture_output=True, text=True, timeout=120)

        parsed: dict[str, tuple[int, str]] = {}
        if state.exists():
            for line in state.read_text().splitlines():
                if line.strip():
                    name, size, st = line.split("\t")
                    parsed[name] = (int(size), st)
        uploads = [ln.split()[3:] for ln in ghlog.read_text().splitlines()
                   if ln.startswith("release upload")]
        uploads = [[Path(a).name for a in u if a != "--clobber"] for u in uploads]

        return Result(proc.returncode, proc.stdout + proc.stderr, parsed, uploads,
                      sleeplog.read_text().split(),
                      draftf.read_text().strip() if draftf.exists() else "none")


FAILURES: list[str] = []


def check(label: str, condition: bool, detail: str = "") -> None:
    if condition:
        print(f"  ok  {label}")
    else:
        print(f"  FAIL {label}{(': ' + detail) if detail else ''}")
        FAILURES.append(label)


def scenario_complete(script: str) -> None:
    r = run(script)
    check("a complete upload publishes the release", r.published and r.code == 0,
          f"code={r.code} draft={r.draft}")
    check("every artifact lands as an asset",
          sorted(r.state) == sorted(ARTIFACTS), f"{sorted(r.state)}")
    check("a clean run uploads once, not once per file", len(r.uploads) == 1,
          f"{len(r.uploads)} upload calls")


def scenario_transient_5xx(script: str) -> None:
    """The v16.57.1 failure itself: one asset 5xxs, the rest already landed."""
    stranded = "veld-desktop-1.2.3-mac-x64.zip.blockmap"
    r = run(script, {"FAIL_FILES": stranded, "FAIL_UNTIL": "2", "FAIL_MODE": "error"})
    check("a transient 5xx on one asset still publishes", r.published and r.code == 0,
          f"code={r.code} draft={r.draft}")
    check("the retry re-sends only what is missing",
          all(u == [stranded] for u in r.uploads[1:]) and len(r.uploads) == 3,
          f"{r.uploads}")
    check("backoff grows between attempts", r.sleeps == ["15", "30"], f"{r.sleeps}")


def scenario_present_but_wrong(script: str) -> None:
    for mode, label in (("starter", "an asset still uploading"),
                        ("truncated", "an asset with the wrong byte count")):
        victim = "veld-desktop-1.2.3-mac-x64.zip"
        r = run(script, {"FAIL_FILES": victim, "FAIL_UNTIL": "1", "FAIL_MODE": mode})
        check(f"{label} is re-uploaded, not counted as present",
              r.published and r.state.get(victim, (0, ""))[1] == "uploaded"
              and len(r.uploads) == 2 and r.uploads[1] == [victim],
              f"state={r.state.get(victim)} uploads={r.uploads}")


def scenario_never_completes(script: str) -> bool:
    """The gate. Returns whether it held, so --selftest can assert it can fail."""
    r = run(script, {"FAIL_FILES": "checksums.txt", "FAIL_UNTIL": "99"})
    held = r.code != 0 and not r.published
    check("an incomplete release is never published, and fails the job", held,
          f"code={r.code} draft={r.draft}")
    check("the error names the tag and says it stayed a draft",
          "v1.2.3" in r.out and "draft" in r.out, r.out[-200:])
    return held


def scenario_unreadable_release(script: str) -> None:
    r = run(script, {"FAIL_VIEW": "1"})
    check("a release that cannot be read is never published",
          r.code != 0 and not r.published, f"code={r.code} draft={r.draft}")
    # Swallowing the read failure keeps the release safe — an empty asset list
    # reads as "everything is missing", so the gate still fires — but it does so
    # after five rounds of re-pushing every artifact at an API that is down.
    # Aborting on the first failed read is the difference, so assert it directly.
    check("an unreadable release fails fast, without uploading anything",
          r.uploads == [], f"{len(r.uploads)} upload call(s)")


def scenario_rerun(script: str) -> None:
    already = ARTIFACTS[:2]
    r = run(script, preseed=already)
    check("a re-run tops up an existing draft and publishes",
          r.published and r.code == 0, f"code={r.code} draft={r.draft}")
    check("a re-run does not re-upload assets already on the release",
          len(r.uploads) == 1 and sorted(r.uploads[0]) == sorted(ARTIFACTS[2:]),
          f"{r.uploads}")


def scenario_no_artifacts(script: str) -> None:
    r = run(script, artifacts=[])
    check("an empty artifact set fails instead of publishing an empty release",
          r.code != 0 and not r.published, f"code={r.code} draft={r.draft}")


def selftest(script: str) -> None:
    """Mutate the gate out and assert the suite notices.

    Without this, `scenario_never_completes` could be passing because the script
    fails for some unrelated reason rather than because the gate is there — the
    same trap `validate-workflow-gates.py --selftest` exists for.
    """
    print("Self-test: removing the completeness gate should break the suite")
    lines = script.splitlines()
    try:
        start = next(i for i, ln in enumerate(lines)
                     if 'if [ "${#missing[@]}" -ne 0 ]' in ln)
    except StopIteration:
        sys.exit("self-test: could not find the completeness gate to mutate — "
                 "it was renamed or removed, and this suite no longer proves it exists")
    end = next(i for i in range(start, len(lines)) if lines[i].strip() == "fi")
    mutated = "\n".join(lines[:start] + lines[end + 1:])

    r = run(mutated, {"FAIL_FILES": "checksums.txt", "FAIL_UNTIL": "99"})
    if not r.published:
        sys.exit("self-test FAILED: the mutant refused to publish an incomplete "
                 "release anyway, so the scenario is not testing the gate")
    print("  ok  the mutant publishes an incomplete release, as it must to be caught")


def main() -> int:
    script = publish_script()

    if "--selftest" in sys.argv:
        selftest(script)
        return 0

    print(f"Release publish gate — {WORKFLOW.name} → {JOB} → {STEP_NAME!r}")
    scenario_complete(script)
    scenario_transient_5xx(script)
    scenario_present_but_wrong(script)
    scenario_never_completes(script)
    scenario_unreadable_release(script)
    scenario_rerun(script)
    scenario_no_artifacts(script)

    if FAILURES:
        print(f"\n{len(FAILURES)} failure(s):")
        for f in FAILURES:
            print(f"  - {f}")
        return 1
    print("\nAll release publish invariants hold.")
    return 0


def find_bash() -> str:
    """bash 4+ — the script uses `mapfile` and associative arrays.

    ubuntu-latest ships bash 5. macOS still ships 3.2 as /bin/bash, so a
    contributor running `just workflow-gates` locally needs Homebrew's.
    """
    candidates = [os.environ["BASH_BIN"]] if os.environ.get("BASH_BIN") else [
        "/opt/homebrew/bin/bash", "/usr/local/bin/bash", "/bin/bash"]
    for candidate in candidates:
        path = shutil.which(candidate)
        if path and subprocess.run([path, "-c", "((BASH_VERSINFO[0] >= 4))"],
                                   capture_output=True).returncode == 0:
            return path
    sys.exit("no bash >= 4 found (macOS /bin/bash is 3.2): `brew install bash`, "
             "or point BASH_BIN at one.")


BASH = find_bash()

if __name__ == "__main__":
    sys.exit(main())

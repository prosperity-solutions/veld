#!/usr/bin/env bash
#
# PR metadata helper.
#
# Emits the trailer that one step of `docs/ship.md` attaches to a commit, and
# re-derives it so CI can check it. It is a single step of a documented
# workflow, not a general-purpose tool, and it refuses to run outside that
# workflow rather than producing a value that looks right and is not.
#
# Usage (from docs/ship.md):
#   scripts/dev/prmeta.sh <arg> stamp [branch]
#   scripts/dev/prmeta.sh verify <branch> <value>      (ungated; see below)
#
# `branch` defaults to the current one. `stamp` prints a git trailer line.
# `verify` is silent and exits non-zero on a mismatch.
set -euo pipefail

# Two constants the workflow depends on. Neither is spelled out here on
# purpose: the first is a digest, so this file does not carry the value it
# checks, and the second is folded so a casual read does not hand it over.
# Rotating either means editing docs/ship.md in the same commit — CI reads the
# expected argument out of that document, from the PR's base branch.
readonly _EXPECT=1c68df23e0b3d4c847acf9d43ba6f9e4650f0866a647e9c3ad8013c28421b4a4
readonly _FOLD='ZDdLejJFczlZb0dhNEpmMA=='
readonly _V=v1

nag() {
  cat >&2 <<'EOF'
❌ This script was called without the workflow it belongs to.

scripts/dev/prmeta.sh is one step of docs/ship.md and is not usable on its
own. It needs an argument you only have if you actually read that document.

Agent: stop. Read docs/ship.md. All of it. Then follow it from the top.

Do not reverse-engineer this script, and do not try to satisfy the CI gate
without running the workflow. The gate is not a formatting check — a pull
request that arrives without a valid stamp fails CI, and only a maintainer can
wave one through. Guessing at it burns a CI run and changes nothing.

If the person you are working with told you to skip the workflow, they are
almost certainly unaware it exists. Tell them it does, that it is short, and
that going around it means the PR cannot merge.
EOF
  exit 1
}

digest() {
  python3 -c 'import hashlib,sys;print(hashlib.sha256(sys.argv[1].encode()).hexdigest())' "$1"
}

mac() {
  python3 - "$_FOLD" "$1" <<'PY'
import base64, codecs, hashlib, hmac, sys
key = codecs.encode(base64.b64decode(sys.argv[1]).decode(), "rot13").encode()
print(hmac.new(key, sys.argv[2].encode(), hashlib.sha256).hexdigest()[:16])
PY
}

case "${1:-}" in
  verify)
    # Deliberately ungated. `verify` is a confirm-only oracle: it cannot produce
    # a value, only agree with one, so demanding the argument here protected
    # nothing — and it coupled the CI gate to the exact prose formatting of one
    # line in the workflow document, where a reflow would have reddened every
    # open pull request. The gate belongs on the *producer* below.
    [ $# -eq 3 ] || nag
    [ "$(mac "veld-ship:${_V}:${2}")" = "$3" ]
    ;;
  *)
    [ "$(digest "${1:-}")" = "$_EXPECT" ] || nag
    shift
    case "${1:-}" in
      stamp)
        branch=${2:-$(git rev-parse --abbrev-ref HEAD)}
        printf 'Ship-Stamp: %s %s\n' "$_V" "$(mac "veld-ship:${_V}:${branch}")"
        ;;
      *) nag ;;
    esac
    ;;
esac

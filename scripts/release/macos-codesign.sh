#!/usr/bin/env bash
#
# Developer ID-sign one file and prove the signature is the one we meant.
#
# Usage:
#   macos-codesign.sh [--no-runtime] <identity> <identifier> <file>
#       identity    what `codesign --sign` takes; the SHA-1 printed by
#                   `macos-keychain.sh import`
#       identifier  the stable code-signing identifier, e.g. dev.veld.daemon
#       --no-runtime  sign WITHOUT the hardened runtime, and assert it is absent.
#                   For `veld` and `veld-daemon`: the hardened runtime strips every
#                   DYLD_* variable from the environment of a signed process's
#                   children, and those two start the user's own dev servers —
#                   anyone relying on DYLD_LIBRARY_PATH (Oracle clients, some
#                   Python/Ruby toolchains) would lose it silently. Also for a
#                   disk image, which has no runtime to harden.
#   env: APPLE_TEAM_ID  the team the signature must come out as
#        KEYCHAIN       optional; passed as `codesign --keychain`
#        VELD_CODESIGN_SELFTEST=1
#                   ONLY for ci.yml's PR smoke job, which signs with a throwaway
#                   self-signed certificate: skips the secure timestamp and the
#                   Apple-issued-team checks a self-signed certificate cannot
#                   pass. Release CI never sets it, and ci.yml checks that.
#
# Why the identifier is passed explicitly and checked: macOS remembers a binary by
# its *designated requirement* — `identifier "<id>" and anchor apple generic and
# certificate leaf[subject.OU] = <team>` for a Developer ID signature. That
# requirement is what TCC (Full Disk Access, Accessibility…), Keychain ACLs and
# Login Items match a later version against. Left to itself codesign derives the
# identifier from the file name — which is stable today and silently changes the
# day someone renames an artifact, at which point every install treats the update
# as a stranger and re-prompts for everything. Pinning it here, and failing the
# release if the requirement does not come out as expected, is what keeps it fixed.
#
# Also used, unchanged, by `scripts/release/apple-signing-setup.sh` to sign a
# throwaway binary before the certificate is uploaded.
set -euo pipefail

die() {
  if [ -n "${GITHUB_ACTIONS:-}" ]; then
    echo "::error::$*" >&2
  else
    echo "error: $*" >&2
  fi
  exit 1
}

runtime=1
if [ "${1:-}" = "--no-runtime" ]; then
  runtime=0
  shift
fi
[ "$#" -eq 3 ] || die "usage: $0 [--no-runtime] <identity> <identifier> <file>"
identity=$1
identifier=$2
file=$3
: "${APPLE_TEAM_ID:?APPLE_TEAM_ID is not set}"
[ -f "$file" ] || die "$file does not exist"

selftest=0
if [ "${VELD_CODESIGN_SELFTEST:-}" = 1 ]; then
  # ci.yml's gate keeps the switch out of release.yml's text; this keeps it out
  # of a release run however it got into the environment.
  [ "${GITHUB_WORKFLOW:-}" != Release ] || die "VELD_CODESIGN_SELFTEST is set in the Release workflow — it skips the timestamp and team checks"
  selftest=1
  echo "VELD_CODESIGN_SELFTEST=1: skipping timestamp and team checks" >&2
fi

args=(--force --identifier "$identifier" --sign "$identity")
if [ "$selftest" = 0 ]; then
  args+=(--timestamp)
fi
if [ "$runtime" = 1 ]; then
  args+=(--options runtime)
fi
if [ -n "${KEYCHAIN:-}" ]; then
  args+=(--keychain "$KEYCHAIN")
fi

# `--timestamp` asks Apple's timestamp server, which fails transiently often
# enough to matter over a release's half-dozen signatures. Retrying is safe: each
# attempt replaces the whole signature (`--force`).
attempt=1
until codesign "${args[@]}" "$file"; do
  [ "$attempt" -lt 3 ] || die "codesign failed 3 times on $file"
  echo "codesign failed on $file (attempt $attempt) — retrying in $((attempt * 10))s" >&2
  sleep $((attempt * 10))
  attempt=$((attempt + 1))
done

codesign --verify --strict --verbose=2 "$file" >&2 \
  || die "$file does not verify after signing"

# `codesign -d` writes its report to stderr.
display=$(codesign -d --verbose=4 "$file" 2>&1)
has_line() { printf '%s\n' "$display" | awk -v s="$1" 'index($0, s) == 1 { found = 1 } END { exit !found }'; }
has_line "Identifier=$identifier" || die "$file: identifier is not $identifier"
has_line "Authority=Developer ID Application: " || die "$file: not signed by a Developer ID Application certificate"
runtime_flag=0
if printf '%s\n' "$display" | grep -Eq '^CodeDirectory .*flags=0x[0-9a-f]+\([^)]*runtime'; then
  runtime_flag=1
fi
[ "$runtime_flag" = "$runtime" ] \
  || die "$file: hardened runtime is $([ "$runtime_flag" = 1 ] && echo on || echo off), expected $([ "$runtime" = 1 ] && echo on || echo off)"

requirement=$(codesign -d -r- "$file" 2>&1)
printf '%s\n' "$requirement" | grep -qF "identifier \"$identifier\"" \
  || die "$file: designated requirement does not pin identifier $identifier: $requirement"

if [ "$selftest" = 0 ]; then
  has_line "TeamIdentifier=$APPLE_TEAM_ID" || die "$file: team is not $APPLE_TEAM_ID"
  # A signature without a secure timestamp stops verifying when the certificate
  # expires, and notarization rejects it outright.
  has_line "Timestamp=" || die "$file: no secure timestamp"
  # The team, not the certificate: a renewed certificate must keep the identity,
  # so the requirement must never pin a leaf hash.
  printf '%s\n' "$requirement" | grep -Eq "leaf\[subject\.OU\] = \"?$APPLE_TEAM_ID\"?" \
    || die "$file: designated requirement does not pin team $APPLE_TEAM_ID: $requirement"
  if printf '%s\n' "$requirement" | grep -Eq 'cdhash|leaf = H"'; then
    die "$file: designated requirement pins a hash, which every update would break: $requirement"
  fi
fi

echo "signed $file as $identifier (team $APPLE_TEAM_ID)" >&2

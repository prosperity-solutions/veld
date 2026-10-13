#!/usr/bin/env bash
#
# Put the team's Developer ID Application certificate into a throwaway keychain,
# and take it out again.
#
# The same script everywhere, on purpose:
#   * release CI (`.github/workflows/release.yml`: the darwin `build` legs and
#     the macos `desktop` leg),
#   * `tests/macos-signing-smoke.sh`, on every PR with a throwaway certificate, and
#   * `scripts/release/apple-signing-setup.sh`, which runs it on the maintainer's
#     Mac to prove the certificate works *before* it is uploaded — so the import
#     CI will do has already succeeded once, on the real `.p12`, by the time a
#     release depends on it.
#
# Usage:
#   macos-keychain.sh import <keychain-path>
#       env: APPLE_DEVID_P12           base64 of the .p12 (certificate + key + Apple's
#                                       Developer ID G2 intermediate)
#            APPLE_DEVID_P12_PASSWORD  its password
#            APPLE_TEAM_ID             the 10-character team id the certificate must be for
#       Prints exactly one line on stdout: the SHA-1 of the signing identity, which is
#       what `codesign --sign` gets. Everything else goes to stderr.
#   macos-keychain.sh delete <keychain-path>
#       Removes the keychain and takes it off the user's search list. Safe to run
#       when it does not exist, so it can sit in an `always()` step / an EXIT trap.
#
# The identity is picked by **team id**, not by "the first Developer ID in the
# keychain": a `.p12` exported from a machine that holds two teams' certificates
# would otherwise sign as whichever sorted first, and the result would verify,
# notarize and ship under the wrong team — breaking the stable identity this whole
# pipeline exists for, on every install at once.
set -euo pipefail

die() {
  if [ -n "${GITHUB_ACTIONS:-}" ]; then
    echo "::error::$*" >&2
  else
    echo "error: $*" >&2
  fi
  exit 1
}

# The user search list, as plain paths, without the keychain named in $1.
search_list_without() {
  security list-keychains -d user | sed -e 's/^[[:space:]]*"//' -e 's/"[[:space:]]*$//' | grep -vxF "$1" || true
}

cmd="${1:-}"
kc="${2:-}"
[ -n "$cmd" ] && [ -n "$kc" ] || die "usage: $0 import|delete <keychain-path>"
case "$kc" in
  /*) ;;
  *) die "the keychain path must be absolute (got '$kc') — security(1) resolves a relative one under ~/Library/Keychains" ;;
esac

case "$cmd" in
  delete)
    # Off the search list first: a listed keychain that no longer exists makes every
    # later `security` call on this machine print a warning.
    remaining=()
    while IFS= read -r line; do
      [ -n "$line" ] && remaining+=("$line")
    done < <(search_list_without "$kc")
    # `${a[@]+…}`: macOS still ships bash 3.2, where an empty array under `set -u`
    # is an "unbound variable" error.
    security list-keychains -d user -s ${remaining[@]+"${remaining[@]}"}
    if [ -e "$kc" ]; then
      security delete-keychain "$kc"
    fi
    ;;

  import)
    : "${APPLE_DEVID_P12:?APPLE_DEVID_P12 is not set — run scripts/release/apple-signing-setup.sh}"
    : "${APPLE_DEVID_P12_PASSWORD:?APPLE_DEVID_P12_PASSWORD is not set — run scripts/release/apple-signing-setup.sh}"
    : "${APPLE_TEAM_ID:?APPLE_TEAM_ID is not set — run scripts/release/apple-signing-setup.sh}"
    case "$APPLE_TEAM_ID" in
      [A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9]) ;;
      *) die "APPLE_TEAM_ID must be 10 characters of A-Z/0-9 (got ${#APPLE_TEAM_ID} characters)" ;;
    esac
    [ ! -e "$kc" ] || die "$kc already exists — delete it first ($0 delete $kc)"

    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    p12="$work/devid.p12"
    printf '%s' "$APPLE_DEVID_P12" | base64 --decode > "$p12" 2>/dev/null \
      || die "APPLE_DEVID_P12 is not valid base64"
    [ -s "$p12" ] || die "APPLE_DEVID_P12 decoded to nothing"

    # The keychain's own password only has to live as long as this keychain, and
    # nothing but this script ever unlocks it.
    kc_pass=$(openssl rand -hex 24)
    security create-keychain -p "$kc_pass" "$kc"
    # No auto-lock for 6 h: a locked keychain mid-build makes codesign fail with
    # "errSecInternalComponent", which names nothing useful.
    security set-keychain-settings -lut 21600 "$kc"
    security unlock-keychain -p "$kc_pass" "$kc"
    # `-x`: the private key is non-extractable once imported, and only codesign
    # is on its access list. Whatever else runs while the keychain is unlocked
    # (electron-builder and its dependencies, in the desktop job) can sign with
    # it but cannot export it from the keychain. That says nothing about code
    # that ran before this script and had a chance to replace it.
    # The password is in argv here, visible to `ps` on this machine for the
    # length of the import; security(1) has no other way to take it.
    security import "$p12" -k "$kc" -f pkcs12 -x -P "$APPLE_DEVID_P12_PASSWORD" \
      -T /usr/bin/codesign >/dev/null \
      || die "could not import the .p12 — wrong APPLE_DEVID_P12_PASSWORD, or not a PKCS#12 file"
    # Without this, codesign stops to ask for the keychain password in a GUI dialog
    # nobody on a runner can click, and the job hangs until its timeout.
    security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$kc_pass" "$kc" >/dev/null

    # On the search list so codesign can build the chain through the intermediate
    # that came in with the .p12. Appended, not replacing: the login keychain stays.
    current=()
    while IFS= read -r line; do
      [ -n "$line" ] && current+=("$line")
    done < <(search_list_without "$kc")
    security list-keychains -d user -s "$kc" ${current[@]+"${current[@]}"}

    # `-v` lists only identities whose chain validates, so a .p12 missing the G2
    # intermediate (on a machine that does not have it either) shows up here, as
    # zero matches, instead of as a codesign failure later.
    identities=$(security find-identity -v -p codesigning "$kc")
    matches=$(printf '%s\n' "$identities" \
      | grep -F "\"Developer ID Application: " | grep -F "($APPLE_TEAM_ID)\"" || true)
    count=$(printf '%s' "$matches" | grep -c . || true)
    if [ "$count" != "1" ]; then
      echo "identities in the imported certificate:" >&2
      printf '%s\n' "$identities" >&2
      die "expected exactly one valid 'Developer ID Application: … ($APPLE_TEAM_ID)' identity, found $count. Wrong team, a non-Developer-ID certificate, or the .p12 lacks Apple's Developer ID G2 intermediate — re-run scripts/release/apple-signing-setup.sh"
    fi
    sha=$(printf '%s\n' "$matches" | awk '{ print $2 }')

    # Expiry, checked here so it fails a release a month early instead of on the
    # day. Renewing does not change the app's identity — the designated
    # requirement pins the team, not this certificate — so the fix is just a new
    # certificate through scripts/release/apple-signing-setup.sh.
    pem="$work/leaf.pem"
    security find-certificate -a -Z -p "$kc" | awk -v sha="$sha" '
      /^SHA-1 hash: / { keep = ($3 == sha); next }
      keep { print }
    ' > "$pem"
    grep -q 'BEGIN CERTIFICATE' "$pem" || die "could not read back the certificate for identity $sha"
    expires=$(openssl x509 -in "$pem" -noout -enddate | sed 's/^notAfter=//')
    echo "signing identity $sha, expires $expires" >&2
    openssl x509 -in "$pem" -noout -checkend $((30 * 24 * 3600)) >/dev/null \
      || die "the Developer ID certificate expires $expires — less than 30 days left. Renew it: scripts/release/apple-signing-setup.sh --renew"
    printf '%s\n' "$sha"
    ;;

  *)
    die "unknown command '$cmd' — expected import or delete"
    ;;
esac

#!/usr/bin/env bash
#
# Runs the release's macOS signing scripts end to end on every pull request, so
# a typo in either one fails a PR instead of the first release after it merges.
# Run from the `apple-signing-smoke` job in `.github/workflows/ci.yml`.
#
# The Developer ID certificate lives only in the `release` environment, which no
# pull request can reach, so this signs with a throwaway self-signed certificate
# instead — named like a Developer ID one, so `macos-keychain.sh` runs its real
# identity selection unchanged. What that cannot exercise, and
# `VELD_CODESIGN_SELFTEST=1` therefore skips: Apple's timestamp server and the
# Apple-issued team in the designated requirement. Those are proven on the
# maintainer's Mac by `scripts/release/apple-signing-setup.sh` before the real
# certificate is ever uploaded.
#
# **Needs a CI runner**: it marks the throwaway certificate as trusted for code
# signing in the System keychain (`sudo security add-trusted-cert`), because
# `find-identity -v` and codesign both refuse an untrusted identity. Never run
# this on your own Mac.
set -euo pipefail

[ -n "${CI:-}" ] || { echo "refusing to run outside CI: this trusts a throwaway certificate system-wide" >&2; exit 1; }

repo=$(cd "$(dirname "$0")/.." && pwd)
keychain_sh="$repo/scripts/release/macos-keychain.sh"
codesign_sh="$repo/scripts/release/macos-codesign.sh"
work=$(mktemp -d)
kc="$work/smoke.keychain-db"
trap '"$keychain_sh" delete "$kc" || true; rm -rf "$work"' EXIT

fail() { echo "::error::$*" >&2; exit 1; }

team=VELDSMOKE0
cat > "$work/cert.cnf" <<EOF
[req]
distinguished_name = dn
prompt = no
[dn]
CN = Developer ID Application: Veld CI Smoke ($team)
OU = $team
O = Veld CI Smoke
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
EOF
# /usr/bin/openssl (LibreSSL) on purpose, as in apple-signing-setup.sh: its
# PKCS#12 defaults are ones `security import` reads; Homebrew's OpenSSL 3
# writes AES-encrypted files older macOS releases reject.
# 60 days: macos-keychain.sh refuses a certificate with under 30 days left.
/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 60 \
  -keyout "$work/key.pem" -out "$work/cert.pem" \
  -config "$work/cert.cnf" -extensions ext 2>/dev/null
/usr/bin/openssl pkcs12 -export -inkey "$work/key.pem" -in "$work/cert.pem" \
  -out "$work/smoke.p12" -passout pass:smoke
sudo security add-trusted-cert -d -r trustRoot -p codeSign \
  -k /Library/Keychains/System.keychain "$work/cert.pem"

export APPLE_DEVID_P12 APPLE_DEVID_P12_PASSWORD=smoke APPLE_TEAM_ID
APPLE_DEVID_P12=$(base64 < "$work/smoke.p12")

# The identity is selected by team, so the wrong team must find nothing.
if APPLE_TEAM_ID=WRONGTEAM0 "$keychain_sh" import "$kc" 2>/dev/null; then
  fail "macos-keychain.sh accepted a certificate for the wrong team"
fi
"$keychain_sh" delete "$kc"

APPLE_TEAM_ID=$team
identity=$("$keychain_sh" import "$kc")
if ! printf '%s' "$identity" | grep -Eqx '[0-9A-F]{40}'; then
  fail "macos-keychain.sh printed '$identity', expected exactly one SHA-1"
fi

export KEYCHAIN="$kc" VELD_CODESIGN_SELFTEST=1

# A child that reports whether DYLD_FALLBACK_LIBRARY_PATH reached it, and a
# parent that execs it — standing in for `veld` / `veld-daemon` starting a
# user's dev server.
cat > "$work/child.c" <<'EOF'
#include <stdio.h>
#include <stdlib.h>
int main(void) { const char *v = getenv("DYLD_FALLBACK_LIBRARY_PATH"); puts(v ? v : "(stripped)"); return 0; }
EOF
cat > "$work/parent.c" <<'EOF'
#include <unistd.h>
int main(int argc, char **argv) { (void)argc; execv(argv[1], argv + 1); return 127; }
EOF
cc -o "$work/child" "$work/child.c"
# Both architectures the release ships; the runner can execute only arm64, so
# x86_64 is signed and verified but not run.
for arch in arm64 x86_64; do
  cc -arch "$arch" -o "$work/parent-$arch" "$work/parent.c"
  cp "$work/parent-$arch" "$work/hardened-$arch"
  "$codesign_sh" --no-runtime "$identity" dev.veld.smoke.plain "$work/parent-$arch"
  "$codesign_sh" "$identity" dev.veld.smoke.hardened "$work/hardened-$arch"
done

# The reason `veld` and `veld-daemon` are signed --no-runtime: with it the
# variable survives into the child, with the hardened runtime it does not. If
# the second assertion ever stops holding, Apple changed the rule and the
# --no-runtime exception in release.yml can be revisited.
got=$(DYLD_FALLBACK_LIBRARY_PATH=/veld-smoke "$work/parent-arm64" "$work/child")
[ "$got" = /veld-smoke ] || fail "DYLD_* did not survive a --no-runtime parent (got '$got')"
#
# dyld only prunes DYLD_* for a hardened process while System Integrity
# Protection is on, and GitHub's hosted macOS runners run with it off. There
# the behaviour cannot be observed, so assert the flag that causes it instead
# (on a user's Mac, SIP is on).
if csrutil status | grep 'status: enabled' >/dev/null; then
  got=$(DYLD_FALLBACK_LIBRARY_PATH=/veld-smoke "$work/hardened-arm64" "$work/child")
  [ "$got" = "(stripped)" ] || fail "the hardened runtime no longer strips DYLD_* (got '$got')"
else
  echo "$(csrutil status) — checking the runtime flag instead of DYLD_* stripping"
  for arch in arm64 x86_64; do
    codesign -dv "$work/hardened-$arch" 2>&1 | grep -E '^CodeDirectory .*flags=0x[0-9a-f]*\(([a-z-]+,)*runtime' >/dev/null \
      || fail "hardened-$arch is not signed with the hardened runtime"
    if codesign -dv "$work/parent-$arch" 2>&1 | grep -E '^CodeDirectory .*\(([a-z-]+,)*runtime' >/dev/null; then
      fail "parent-$arch carries the hardened runtime despite --no-runtime"
    fi
  done
fi

# A disk image, as the desktop job signs one.
mkdir "$work/dmg-src"
cp "$work/child" "$work/dmg-src/"
hdiutil create -quiet -fs HFS+ -srcfolder "$work/dmg-src" -volname smoke "$work/smoke.dmg"
"$codesign_sh" --no-runtime "$identity" dev.veld.smoke.dmg "$work/smoke.dmg"

echo "macOS signing smoke: ok"

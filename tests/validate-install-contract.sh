#!/usr/bin/env bash
# The CLI↔install.sh contract, checked against the real script.
#
# Why this exists: `run_install_script` fetches the *published* script, so
# Rust's own tests point `VELD_INSTALL_SCRIPT` at a recorder and pin what the
# CLI *sends*. Nothing pinned what the script *reads*. Rename `VELD_DESKTOP_ONLY`
# in install.sh and every Rust test still passes, while every app update
# silently reinstalls the CLI, restarts the daemon and can prompt for sudo —
# the exact thing the docs promise cannot happen. `bash -n` and shellcheck do
# not see it either: both halves are individually valid.
#
# Everything here runs the script's early-refusal paths only, so there is no
# network access, nothing is downloaded, and nothing is installed.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SCRIPT="$REPO_ROOT/install.sh"
SETUP_RS="$REPO_ROOT/crates/veld-core/src/setup.rs"

pass=0
fail=0
ok()   { echo "  ok   $1"; pass=$((pass + 1)); }
bad()  { echo "  FAIL $1"; fail=$((fail + 1)); }

echo "install.sh contract"

# --- 1. Every VELD_* variable the CLI sets must be one the script reads -------
#
# The drift this catches is a rename on either side. Extracted from the two
# functions that build the child environment rather than from the whole file,
# so an unrelated mention cannot satisfy it.
sent="$(sed -n '/fn perform_update/,/^}/p;/pub async fn install_desktop/,/^}/p' "$SETUP_RS" \
  | grep -o '"VELD_[A-Z_]*"' | tr -d '"' | sort -u)"

if [ -z "$sent" ]; then
  bad "could not extract any VELD_* variable from setup.rs — has the code moved?"
else
  for var in $sent; do
    if grep -q "\${${var}:-\|\${${var}}\|\"\$${var}\"" "$SCRIPT"; then
      ok "install.sh reads ${var}"
    else
      bad "install.sh never reads ${var}, but setup.rs sets it"
    fi
  done
fi

# `VELD_VERSION` and `VELD_NON_INTERACTIVE` are set outside those two functions.
for var in VELD_VERSION VELD_NON_INTERACTIVE; do
  if grep -q "\${${var}:-" "$SCRIPT"; then
    ok "install.sh reads ${var}"
  else
    bad "install.sh never reads ${var}"
  fi
done

# --- 2. The script's own refusals actually fire ------------------------------
#
# These are the two paths that exit before any network call, which is what makes
# them safe to run here — and running them is what proves the variable names in
# section 1 are wired to behaviour rather than merely present in both files.
#
# Both stub `uname`, so this file asserts the same thing on a Linux runner and on
# a maintainer's Mac. That is not tidiness: the first version stubbed nothing,
# passed locally, and failed in CI, because on Linux the macOS-only refusal fires
# *before* the contradiction check and answers with a different message. A test
# whose expected output depends on the host it runs on is a test that reports on
# the host.
STUB="$(mktemp -d)"
trap 'rm -rf "$STUB"' EXIT

stub_uname() { # stub_uname <Darwin|Linux>
  cat > "$STUB/uname" <<EOF
#!/bin/sh
case "\$1" in -s) echo $1 ;; *) exec /usr/bin/uname "\$@" ;; esac
EOF
  chmod +x "$STUB/uname"
}

stub_uname Darwin
out="$(PATH="$STUB:$PATH" VELD_DESKTOP_ONLY=1 VELD_DESKTOP=0 VELD_NON_INTERACTIVE=1 bash "$SCRIPT" 2>&1)"
code=$?
if [ "$code" -eq 1 ] && printf '%s' "$out" | grep -q "contradict"; then
  ok "VELD_DESKTOP_ONLY=1 with VELD_DESKTOP=0 is refused"
else
  bad "expected exit 1 + 'contradict', got exit ${code}: ${out}"
fi

stub_uname Linux
out="$(PATH="$STUB:$PATH" VELD_DESKTOP_ONLY=1 VELD_NON_INTERACTIVE=1 bash "$SCRIPT" 2>&1)"
code=$?
if [ "$code" -eq 1 ] && printf '%s' "$out" | grep -q "macOS-only"; then
  ok "VELD_DESKTOP_ONLY on a non-mac is refused"
else
  bad "expected exit 1 + 'macOS-only', got exit ${code}: ${out}"
fi

# --- 3. The desktop section is reachable only through the names above --------
for needle in \
  'install_desktop_app' \
  'desktop_lock' \
  'apply_binary_icons' \
  'find_desktop_app'
do
  if grep -q "^${needle}()" "$SCRIPT"; then
    ok "install.sh defines ${needle}"
  else
    bad "install.sh no longer defines ${needle}"
  fi
done

echo
if [ "$fail" -eq 0 ]; then
  echo "install.sh contract: ${pass} checks passed"
  exit 0
fi
echo "install.sh contract: ${fail} failed, ${pass} passed"
exit 1

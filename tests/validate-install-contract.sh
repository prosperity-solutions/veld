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
# The drift this catches is a rename on either side. Extracted from the functions
# that build the child environment rather than from the whole file, so an
# unrelated mention cannot satisfy it.
#
# `embedded_env` is in this list because it is a third builder, added after the
# first two: the variables it sets decide whether install.sh restarts the
# privileged helper, and a rename on either side would leave the script and the
# CLI both bouncing a root service with this gate still green.
sent="$(sed -n '/fn perform_update/,/^}/p;/pub async fn install_desktop/,/^}/p;/fn embedded_env/,/^}/p' "$SETUP_RS" \
  | grep -o '"VELD_[A-Z_]*"' | tr -d '"' | sort -u)"

# Named explicitly as well as extracted: the extraction above is keyed on
# function names, so moving one of these into a *fourth* helper would silently
# empty its half of the list rather than fail. These two are the ones whose loss
# is invisible at runtime — a missed VELD_EMBEDDED is a duplicated banner and a
# racing root-service restart, not a crash.
# `$sent` is newline-separated; unquoted expansion re-joins it with spaces so the
# `case` pattern below can bracket a whole entry.
sent_flat=" $(echo $sent) "
for required in VELD_EMBEDDED VELD_VERBOSE; do
  case "$sent_flat" in
    *" $required "*) ok "setup.rs still sets ${required}" ;;
    *) bad "setup.rs no longer sets ${required} from a function this gate reads" ;;
  esac
done

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

# --- 1b. The lines a veld command must own are emitted by `say`, not `echo` ----
#
# `say` is a no-op under VELD_EMBEDDED, `echo` is not, and swapping one for the
# other is both the natural edit and completely invisible: the script still works,
# the tests still pass, and `veld update` quietly goes back to printing three
# "installed successfully!" banners with a first-install footer in the middle of an
# update. Only the lines whose duplication was the actual bug are pinned — this is
# a regression guard, not a style rule, so a new warning is still free to use echo.
# Emitting lines only, and *every* one of them — not the first match. The phrases
# below also appear in this file's own comments and in install.sh's, and an
# earlier version of this check read one of those and reported a passing `say`
# line as a failure. `head -1` on a grep of prose is a coin toss.
while IFS='|' read -r pattern what; do
  [ -n "$pattern" ] || continue
  emitters="$(grep -c -E "^[[:space:]]*(say|echo) .*$(printf '%s' "$pattern" | sed 's/[][\.*^$/]/\\&/g')" "$SCRIPT")"
  loud="$(grep -c -E "^[[:space:]]*echo .*$(printf '%s' "$pattern" | sed 's/[][\.*^$/]/\\&/g')" "$SCRIPT")"
  if [ "$emitters" -eq 0 ]; then
    bad "install.sh no longer prints '${pattern}' (${what}) — has it moved?"
  elif [ "$loud" -gt 0 ]; then
    bad "${what} must use \`say\`, not \`echo\` — it duplicates the caller's output"
  else
    ok "${what} is suppressible (${emitters} line(s))"
  fi
done <<'PINNED'
installed successfully!|the success banner
Run 'veld start' in any project|the first-install footer
veld binary:   |the install path summary
Detected platform:|the platform line
PINNED

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

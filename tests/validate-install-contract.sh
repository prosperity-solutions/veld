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
  'find_desktop_app' \
  'desktop_preference' \
  'record_desktop_preference' \
  'desktop_can_ask' \
  'ask_desktop_preference' \
  'remove_desktop_app_via_cli'
do
  if grep -q "^${needle}()" "$SCRIPT"; then
    ok "install.sh defines ${needle}"
  else
    bad "install.sh no longer defines ${needle}"
  fi
done

# --- 3b. The privileged-helper install handoff (issue #262) ------------------
#
# On a privileged install the helper is served from a ROOT-OWNED directory, so
# this script — running as the user — cannot write it. It hands the download to
# the running root helper instead, through one CLI subcommand. That subcommand
# name and its flag are a contract across the same boundary section 1 guards for
# environment variables, and it fails in the same silent way: rename
# `_helper-install` or `--binary` on the Rust side and this script still runs,
# still exits 0, still copies an INERT binary into the lib dir — and the
# privileged helper is never updated again on any migrated machine. Nothing else
# notices. `bash -n`, shellcheck and every Rust test stay green, because both
# halves are individually valid.
#
# That outcome is the one #338's rule 2 exists to prevent: a release that wedges
# the updater leaves sudo as the only repair channel.
HELPER_INSTALL_CMD='_helper-install'
if grep -q -- "veld\" ${HELPER_INSTALL_CMD} --binary" "$SCRIPT"; then
  ok "install.sh hands the helper to the root helper via \`${HELPER_INSTALL_CMD} --binary\`"
else
  bad "install.sh no longer invokes \`${HELPER_INSTALL_CMD} --binary\` — a migrated privileged install would stop receiving helper updates"
fi

MAIN_RS="$REPO_ROOT/crates/veld/src/main.rs"
if grep -q "name = \"${HELPER_INSTALL_CMD}\"" "$MAIN_RS"; then
  ok "the CLI still declares the \`${HELPER_INSTALL_CMD}\` subcommand"
else
  bad "crates/veld/src/main.rs no longer declares \`${HELPER_INSTALL_CMD}\`, but install.sh still calls it"
fi

# The flag is checked on the Rust side too: clap derives `--binary` from the
# field name, so a rename there is invisible at this file's other end.
if grep -q 'InternalHelperInstall' "$MAIN_RS" && grep -A 6 'InternalHelperInstall' "$MAIN_RS" | grep -q 'binary:'; then
  ok "the CLI still takes the helper path as \`--binary\`"
else
  bad "crates/veld/src/main.rs no longer takes \`--binary\` for ${HELPER_INSTALL_CMD}"
fi

# The store's two paths are hardcoded in install.sh's switch-to-user-paths
# cleanup, duplicating `paths::privileged_helper_dir()`. Change the Rust and this
# script silently stops removing the directory it orphans — root-owned files with
# nothing left able to delete them, which is the leftover `veld uninstall` cannot
# reach on that path (its escalation keys off `setup.json`, which the same branch
# clears). Same class as section 1's env-var names, same fix: pin both sides.
PATHS_RS="$REPO_ROOT/crates/veld-core/src/paths.rs"
for store_dir in /var/db/veld-helper /var/lib/veld-helper; do
  if ! grep -q "rm -rf ${store_dir}" "$SCRIPT"; then
    bad "install.sh no longer removes ${store_dir} when switching off privileged paths"
  elif ! grep -q "\"${store_dir}\"" "$PATHS_RS"; then
    bad "${store_dir} is in install.sh but no longer in paths.rs — the two have drifted"
  else
    ok "install.sh and paths.rs agree on ${store_dir}"
  fi
done

# --- 4. The prompt gate's truth table, under a real pty ----------------------
#
# `desktop_can_ask` is the gate in front of the only question this script asks.
# Two descriptors decide it, because they answer two halves of one question: the
# prompt is printed to stderr (its stdout is captured by the caller's `$( )`) and
# every consequence of the answer is `echo`ed to stdout. Delete either check and
# a real user is harmed — `2>err.log` asks an invisible question and takes the
# default; `>install.log` deletes a bundle and files the outcome in the log.
#
# **This section used to be vacuous and that is the point of its shape now.** It
# asserted only *refusals*, and this harness's own stdout and stderr are pipes in
# CI — so every check passed on the round-1 gate it was added to reject, on the
# original one-descriptor gate, on a gate ignoring VELD_NON_INTERACTIVE, and on a
# gate hard-coded to `return 1`, which would silence the question entirely. A test
# that passes on the bug is not a test. So the gate is now driven under a **pty**,
# with a positive control first: it must say *yes* when both descriptors are
# terminals, and only then do the refusals mean anything.
gate="$(sed -n '/^desktop_can_ask() {/,/^}/p' "$SCRIPT")"
if [ -z "$gate" ]; then
  bad "could not extract desktop_can_ask() from install.sh"
else
  # Cheap, portable, and independent of any pty: the two descriptor checks must
  # both still be there. This is what catches the regression that actually
  # happened (one of them deleted) even on a runner with no pty to be had.
  case "$gate" in
    *'[ -t 1 ]'*) ok "desktop_can_ask still tests fd 1 (where the outcome goes)" ;;
    *) bad "desktop_can_ask no longer tests fd 1 — a redirected stdout would ask and then log the outcome" ;;
  esac
  case "$gate" in
    *'[ -t 2 ]'*) ok "desktop_can_ask still tests fd 2 (where the prompt goes)" ;;
    *) bad "desktop_can_ask no longer tests fd 2 — an invisible question would take the default" ;;
  esac

  # `script`'s argument order differs between util-linux (Linux CI) and BSD
  # (macOS), so the flavour is probed rather than assumed. Neither available is
  # reported as a skip, never as a pass.
  PTY=""
  if script -q -c 'true' /dev/null </dev/null >/dev/null 2>&1; then
    PTY="util-linux"
  elif script -q /dev/null true </dev/null >/dev/null 2>&1; then
    PTY="bsd"
  fi

  # The redirection is applied to the `desktop_can_ask` call itself, not to the
  # `echo` that reports it — so the function sees a redirected descriptor while
  # its verdict still reaches the pty.
  probe='
if desktop_can_ask; then echo "BOTH=yes"; else echo "BOTH=no"; fi
if desktop_can_ask >/dev/null; then echo "NOOUT=yes"; else echo "NOOUT=no"; fi
if desktop_can_ask 2>/dev/null; then echo "NOERR=yes"; else echo "NOERR=no"; fi
if VELD_NON_INTERACTIVE=1 desktop_can_ask; then echo "NONINT=yes"; else echo "NONINT=no"; fi
'
  if [ -z "$PTY" ]; then
    echo "  skip no usable \`script\` for a pty — the two text checks above still ran"
  else
    # Via a file, not an inlined `-c` string. util-linux's `script` takes the
    # command as **one** argument, so inlining meant nesting `bash -c '…'` inside
    # it and escaping the gate's own quotes — unverifiable on a macOS machine,
    # and a mistake there would surface as a spurious CI *failure* rather than a
    # skip. A file has no quoting to get wrong and both flavours run it the same
    # way. `$STUB` comes from `mktemp -d`, so the path has no spaces.
    printf '%s\n%s\n' "$gate" "$probe" > "$STUB/gate-probe.sh"
    if [ "$PTY" = "util-linux" ]; then
      pty_out="$(script -q -c "bash $STUB/gate-probe.sh" /dev/null </dev/null 2>&1 || true)"
    else
      pty_out="$(script -q /dev/null bash "$STUB/gate-probe.sh" </dev/null 2>&1 || true)"
    fi
    pty_out="$(printf '%s' "$pty_out" | tr -d '\r')"
    while IFS='|' read -r key expect what; do
      [ -n "$key" ] || continue
      case "$pty_out" in
        *"${key}=${expect}"*) ok "$what" ;;
        *) bad "$what — pty said: $(printf '%s' "$pty_out" | grep -o "${key}=[a-z]*" | head -1)" ;;
      esac
    done <<'CELLS'
BOTH|yes|the positive control: it asks when both descriptors are terminals
NOOUT|no|it refuses when stdout — where the outcome goes — is redirected
NOERR|no|it refuses when stderr — where the prompt goes — is redirected
NONINT|no|it refuses under VELD_NON_INTERACTIVE=1
CELLS
  fi
fi

# --- 5. The desktop gate's truth table ---------------------------------------
#
# The block that decides whether a ~113 MB app is downloaded, left alone, or
# **deleted**. It had no test, and the defect that hid there is the reason this
# section exists: the `no)` arm could not tell a *stored* answer from one just
# typed, so a user who opted out and later re-installed the app by hand had it
# deleted, with no prompt, by their next `curl … | bash`. Three review angles
# found it independently and no automated check would have.
#
# The region is extracted and driven with the four side-effecting functions
# stubbed, so nothing is downloaded, nothing is removed, and no network is
# touched. Every cell must agree with `desktop_gate` in
# crates/veld/src/commands/update.rs — that is the same decision in a second
# language, with no compiler between them.
GATE_DIR="$(mktemp -d)"
# Exported rather than passed as an assignment prefix: the driver reads it, and
# `GATE_DIR="$GATE_DIR" bash …` is the shape shellcheck refuses (SC2097/SC2098)
# because the expansion on the same line reads the outer value, not the prefix.
export GATE_DIR
trap 'rm -rf "$STUB" "$GATE_DIR"' EXIT

# Bounded by its two column-zero anchors rather than by a `sed` range ending at
# `/^fi$/` — the block contains two `fi`s of its own, so a range would have
# stopped at the first one and silently extracted a third of the logic.
awk '/^DESKTOP_FAILED=""$/{on=1} /^if \[ -n "\$DESKTOP_FAILED" \]/{on=0} on' \
  "$SCRIPT" > "$GATE_DIR/gate.sh"
sed -n '/^desktop_preference() {/,/^}/p;/^record_desktop_preference() {/,/^}/p;/^desktop_can_ask() {/,/^}/p' "$SCRIPT" > "$GATE_DIR/fns.sh"

if [ ! -s "$GATE_DIR/gate.sh" ] || ! grep -q 'DESKTOP_ASKED' "$GATE_DIR/gate.sh"; then
  bad "could not extract the desktop gate block from install.sh (has it moved, or lost DESKTOP_ASKED?)"
else
  cat > "$GATE_DIR/drive.sh" <<'DRIVER'
set -uo pipefail
. "$GATE_DIR/fns.sh"
OS=macos
INSTALL_DIR=/nonexistent
WANT_DESKTOP="${WANT_DESKTOP-1}"
DESKTOP_APP=""
say() { echo "$@"; }
find_desktop_app() { DESKTOP_APP="$FAKE_APP"; }
install_desktop_app() { echo "ACTION=install"; DESKTOP_APP="$FAKE_APP"; return 0; }
remove_desktop_app_via_cli() { echo "ACTION=remove"; return 0; }
# `ASK_ANSWER` set (even to empty) means "a human was at the terminal this run";
# its value is what they typed, with empty standing for junk or EOF. That is the
# seam this section needs: `desktop_can_ask`'s own tty and
# `VELD_NON_INTERACTIVE` behaviour is pinned in section 4, so stubbing it here
# isolates the *branching* — and in particular lets a stored answer and a
# just-typed one be told apart, which is the distinction that was broken.
desktop_can_ask() { [ -n "${ASK_ANSWER+set}" ]; }
# Records as well as echoing, because the real one does — the `no` arm now
# re-reads the file to decide whether it may promise "veld will not install it
# again", so a stub that only echoed would exercise the wrong branch.
ask_desktop_preference() {
  [ -n "${ASK_ANSWER:-}" ] || return 0
  record_desktop_preference "$ASK_ANSWER" || true
  echo "$ASK_ANSWER"
}
. "$GATE_DIR/gate.sh"
echo "END app=[$DESKTOP_APP] declined=[$DESKTOP_DECLINED]"
DRIVER

  # cell <description> <expected ACTION or none> <pref: true|false|unset> <app: path or empty> [answer]
  #
  # A 5th argument means a human was asked this run; pass `junk` for "asked and
  # said nothing usable". Omit it for a run with nobody to ask.
  # `WANT` is install.sh's `WANT_DESKTOP` — 1 unless a cell is testing the
  # `VELD_DESKTOP=0` override, which one cell below sets as an assignment prefix.
  # That prefix is scoped to the call: measured on this repo's two bashes (3.2.57
  # and 5.3.3), `X=1; X=2 f; echo $X` prints 1, so the override cannot leak into a
  # cell added after it. (An earlier version of this comment claimed the opposite
  # and added a manual reset to defend against it — both were wrong, in the file
  # whose job is pinning behaviour.)
  WANT=1
  cell() {
    local what="$1" expect="$2" pref="$3" app="$4"
    local home="$GATE_DIR/home" out got
    rm -rf "$home"; mkdir -p "$home/.veld"
    [ "$pref" = "unset" ] || printf '{"wanted":%s}\n' "$pref" > "$home/.veld/desktop.json"
    if [ "$#" -ge 5 ]; then
      local answer="$5"
      [ "$answer" != "junk" ] || answer=""
      out="$(HOME="$home" FAKE_APP="$app" WANT_DESKTOP="$WANT" ASK_ANSWER="$answer" bash "$GATE_DIR/drive.sh" 2>&1)"
    else
      out="$(HOME="$home" FAKE_APP="$app" WANT_DESKTOP="$WANT" bash "$GATE_DIR/drive.sh" 2>&1)"
    fi
    got="$(printf '%s' "$out" | grep -o 'ACTION=[a-z]*' | head -1)"
    got="${got#ACTION=}"
    got="${got:-none}"
    if [ "$got" = "$expect" ]; then
      ok "$what → $expect"
    else
      bad "$what → expected ${expect}, got ${got}: $(printf '%s' "$out" | tr '\n' '|')"
    fi
  }

  cell "wanted + app present"              install true  /A/Veld.app
  cell "wanted + no app"                   install true  ""
  # **The critical pair.** A stored no must never delete; only an answer given on
  # this run may. Getting these two the same way round was the defect.
  cell "STORED no + app present"           none    false /A/Veld.app
  cell "FRESH no + app present"            remove  unset /A/Veld.app no
  cell "stored no + no app"                none    false ""
  cell "fresh yes + no app"                install unset ""         yes
  cell "fresh yes + app present"           install unset /A/Veld.app yes
  # Never asked and nobody to ask: keep what is there, never fetch what is not.
  cell "unasked + app present"             install unset /A/Veld.app
  cell "unasked + no app"                  none    unset ""
  # Asked, but the answer was neither yes nor no. Must behave like unasked —
  # never delete, never fetch — because nothing was recorded.
  cell "asked + junk answer + app present" install unset /A/Veld.app junk
  cell "asked + junk answer + no app"      none    unset ""          junk
  # VELD_DESKTOP=0 is a per-run override and outranks a stored yes.
  WANT="" cell "VELD_DESKTOP=0 + wanted + app" none true /A/Veld.app

  # The `no` arm's three branches print three different things, and a message on
  # the wrong branch is the failure a truth table cannot see. "veld will not
  # install it again" is a promise only the *file* can keep, so it must not be
  # printed when the answer could not be written.
  says() { # says <description> <pref> <app> <answer|-> <substring>
    local what="$1" pref="$2" app="$3" answer="$4" want="$5"
    local home="$GATE_DIR/home" out
    rm -rf "$home"; mkdir -p "$home/.veld"
    [ "$pref" = "unset" ] || printf '{"wanted":%s}\n' "$pref" > "$home/.veld/desktop.json"
    if [ "$answer" = "-" ]; then
      out="$(HOME="$home" FAKE_APP="$app" WANT_DESKTOP=1 bash "$GATE_DIR/drive.sh" 2>&1)"
    else
      out="$(HOME="$home" FAKE_APP="$app" WANT_DESKTOP=1 ASK_ANSWER="$answer" bash "$GATE_DIR/drive.sh" 2>&1)"
    fi
    if printf '%s' "$out" | grep -qF "$want"; then
      ok "$what says \"$want\""
    else
      bad "$what did not say \"$want\": $(printf '%s' "$out" | tr '\n' '|')"
    fi
  }

  says "a fresh no with a writable home" unset /A/Veld.app no "veld will not install it again"
  says "a stored no with the app still there" false /A/Veld.app - "you opted out, so it was left alone"

  # And the same fresh "no" on a home it cannot write must NOT make that promise.
  #
  # The `.veld` directory has to **exist and be unwritable**: pointing HOME at a
  # path that simply does not exist proves nothing, because
  # `record_desktop_preference` starts with `mkdir -p` and the write then
  # succeeds. That was this check's first shape, and it failed by asserting the
  # promise was absent on a run that had legitimately recorded the answer.
  rm -rf "$GATE_DIR/ro"; mkdir -p "$GATE_DIR/ro/.veld"; chmod 500 "$GATE_DIR/ro/.veld"
  out="$(HOME="$GATE_DIR/ro" FAKE_APP=/A/Veld.app WANT_DESKTOP=1 ASK_ANSWER=no bash "$GATE_DIR/drive.sh" 2>&1)" || true
  chmod 700 "$GATE_DIR/ro/.veld"
  if printf '%s' "$out" | grep -qF "will not install it again"; then
    bad "an unrecorded answer still promised permanence: $(printf '%s' "$out" | tr '\n' '|')"
  else
    ok "an answer that could not be written makes no permanence promise"
  fi
fi

echo
if [ "$fail" -eq 0 ]; then
  echo "install.sh contract: ${pass} checks passed"
  exit 0
fi
echo "install.sh contract: ${fail} failed, ${pass} passed"
exit 1

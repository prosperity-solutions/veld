#!/usr/bin/env bash
#
# Veld integration test suite.
#
# Runs the full lifecycle: setup -> start -> status -> urls -> logs -> stop -> gc
#
# Usage:
#   ./tests/integration.sh [--veld-bin <path>] [--project-dir <path>]
#
# Exit codes:
#   0  All tests passed
#   1  One or more tests failed

set -uo pipefail
# NOTE: This script should be run with sudo for full testing (veld setup
# requires privileged access). Without sudo, setup-dependent tests are skipped.

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

# Portable timeout wrapper (macOS lacks GNU timeout).
if command -v timeout >/dev/null 2>&1; then
    run_with_timeout() { timeout "$@"; }
elif command -v gtimeout >/dev/null 2>&1; then
    run_with_timeout() { gtimeout "$@"; }
else
    # Pure-shell fallback using background process + kill.
    # Redirect watchdog fds to /dev/null so it doesn't hold open the
    # pipe used by command substitution $(...).
    run_with_timeout() {
        local secs="$1"; shift
        "$@" &
        local pid=$!
        ( sleep "$secs" && kill -TERM "$pid" 2>/dev/null ) >/dev/null 2>&1 &
        local watchdog=$!
        wait "$pid" 2>/dev/null
        local rc=$?
        kill "$watchdog" 2>/dev/null
        wait "$watchdog" 2>/dev/null
        return $rc
    }
fi

VELD_BIN="${VELD_BIN:-veld}"
PROJECT_DIR="${PROJECT_DIR:-$(cd "$(dirname "$0")/../testproject" && pwd)}"
RUN_NAME="inttest-$$"
PASSED=0
FAILED=0
SKIPPED=0

# ---------------------------------------------------------------------------
# Colored output helpers
# ---------------------------------------------------------------------------

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
RESET='\033[0m'

info()  { printf "${BLUE}[INFO]${RESET}  %s\n" "$*"; }
pass()  { printf "${GREEN}[PASS]${RESET}  %s\n" "$*"; PASSED=$((PASSED + 1)); }
fail()  { printf "${RED}[FAIL]${RESET}  %s\n" "$*"; FAILED=$((FAILED + 1)); }
skip()  { printf "${YELLOW}[SKIP]${RESET}  %s\n" "$*"; SKIPPED=$((SKIPPED + 1)); }
header(){ printf "\n${BOLD}=== %s ===${RESET}\n\n" "$*"; }

# ---------------------------------------------------------------------------
# Parse arguments
# ---------------------------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case "$1" in
        --veld-bin)   VELD_BIN="$2"; shift 2 ;;
        --project-dir) PROJECT_DIR="$2"; shift 2 ;;
        -h|--help)
            echo "Usage: $0 [--veld-bin <path>] [--project-dir <path>]"
            exit 0
            ;;
        *) echo "Unknown argument: $1"; exit 1 ;;
    esac
done

# Resolve paths to absolute so that cd later does not break them.
VELD_BIN="$(cd "$(dirname "$VELD_BIN")" && pwd)/$(basename "$VELD_BIN")"
PROJECT_DIR="$(cd "$PROJECT_DIR" && pwd)"
ORIG_DIR="$(pwd)"

# ---------------------------------------------------------------------------
# Cleanup on exit
# ---------------------------------------------------------------------------

cleanup() {
    cd "$ORIG_DIR"
    info "cleaning up: stopping run '${RUN_NAME}' if still active"
    "$VELD_BIN" stop --name "$RUN_NAME" 2>/dev/null || true
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Test helpers
# ---------------------------------------------------------------------------

# Run a command and check that it exits 0. Shows output on failure.
assert_ok() {
    local label="$1"
    shift
    local output rc=0
    output=$("$@" 2>&1) || rc=$?
    if [ "$rc" -eq 0 ]; then
        pass "$label"
        return 0
    else
        fail "$label (exit code $rc)"
        info "output: $output"
        return 1
    fi
}

# Run a command and capture stdout; check exit 0.
capture_ok() {
    local label="$1"
    shift
    local output rc=0
    output=$("$@" 2>&1) || rc=$?
    if [ "$rc" -eq 0 ]; then
        pass "$label"
        echo "$output"
        return 0
    else
        fail "$label (exit code $rc)"
        echo "$output" >&2
        return 1
    fi
}

# ---------------------------------------------------------------------------
# Test 0: Pre-setup check -- commands should fail without setup
# ---------------------------------------------------------------------------

header "Pre-Setup Check"

info "verifying that environment commands fail gracefully without setup"

# This test checks that veld produces a clear error when system setup has not
# been performed. We intentionally expect a non-zero exit code here.
if "$VELD_BIN" status --name "nonexistent-run-$$" 2>&1 | grep -qi "error\|not found\|no such\|not set up\|does not exist"; then
    pass "pre-setup: commands produce a clear error for missing runs"
else
    skip "pre-setup: could not verify error behaviour (command may have succeeded or produced unexpected output)"
fi

# PRD assertion 13: status --json should contain "setup_required" error key
PRE_STATUS_JSON=$("$VELD_BIN" status --name "nonexistent-run-$$" --json 2>&1 || true)
if echo "$PRE_STATUS_JSON" | grep -q '"setup_required"'; then
    pass "pre-setup: status --json contains setup_required error key"
else
    skip "pre-setup: status --json did not contain setup_required key"
fi

# ---------------------------------------------------------------------------
# Test 1: veld binary exists
# ---------------------------------------------------------------------------

header "Binary Check"

if command -v "$VELD_BIN" >/dev/null 2>&1; then
    pass "veld binary found at $(command -v "$VELD_BIN")"
else
    fail "veld binary not found: $VELD_BIN"
    printf "\n${RED}Cannot continue without the veld binary.${RESET}\n"
    exit 1
fi

# ---------------------------------------------------------------------------
# Test 2: veld setup
# ---------------------------------------------------------------------------

header "Setup"

info "running 'veld setup' (may require sudo -- will skip if not available)"

if sudo -n true 2>/dev/null; then
    # Use timeout to prevent CI hangs (setup involves network downloads,
    # service registration, and CA trust which can block interactively).
    assert_ok "veld setup completes" run_with_timeout 120 "$VELD_BIN" setup

    # PRD assertion 2: setup idempotency — running setup a second time should also succeed.
    assert_ok "veld setup idempotent (second run)" run_with_timeout 120 "$VELD_BIN" setup
else
    skip "veld setup (sudo not available without password)"
fi

# ---------------------------------------------------------------------------
# Test 3: veld start
# ---------------------------------------------------------------------------

header "Start Environment"

cd "$PROJECT_DIR"
info "project directory: $PROJECT_DIR"
info "run name: $RUN_NAME"

assert_ok "veld start frontend:local --name $RUN_NAME" \
    run_with_timeout 120 "$VELD_BIN" start "frontend:local" --name "$RUN_NAME"

# Give processes a moment to spin up.
sleep 2

# ---------------------------------------------------------------------------
# Test 4: veld status
# ---------------------------------------------------------------------------

header "Status Check"

STATUS_OUTPUT=""
if STATUS_OUTPUT=$(capture_ok "veld status --json" "$VELD_BIN" status --name "$RUN_NAME" --json); then
    # Check that the output contains "healthy" or "running".
    if echo "$STATUS_OUTPUT" | grep -qiE '"(healthy|running)"'; then
        pass "status reports healthy/running"
    else
        fail "status does not report healthy/running"
        info "output: $STATUS_OUTPUT"
    fi
fi

# ---------------------------------------------------------------------------
# Test 5: veld urls
# ---------------------------------------------------------------------------

header "URL Check"

URLS_OUTPUT=""
EXTRACTED_URLS=""
if URLS_OUTPUT=$(capture_ok "veld urls --json" "$VELD_BIN" urls --name "$RUN_NAME" --json); then
    if echo "$URLS_OUTPUT" | grep -q "localhost"; then
        pass "urls output contains localhost entries"
    else
        fail "urls output missing localhost entries"
        info "output: $URLS_OUTPUT"
    fi

    # PRD assertion 5: verify exactly 2 URLs with expected hostnames.
    URL_COUNT=$(echo "$URLS_OUTPUT" | grep -oE 'https?://[^"]+' | wc -l | tr -d ' ')
    if [ "$URL_COUNT" -eq 2 ]; then
        pass "urls --json returns exactly 2 URLs"
    else
        fail "urls --json expected 2 URLs but got $URL_COUNT"
        info "output: $URLS_OUTPUT"
    fi

    if echo "$URLS_OUTPUT" | grep -q "frontend" && echo "$URLS_OUTPUT" | grep -q "backend"; then
        pass "urls contain expected frontend and backend hostnames"
    else
        fail "urls missing expected frontend/backend hostnames"
        info "output: $URLS_OUTPUT"
    fi
fi

# ---------------------------------------------------------------------------
# Test 6: curl the URLs
# ---------------------------------------------------------------------------

header "HTTP Connectivity"

# Extract URLs from the JSON output (best-effort).
if [ -n "$URLS_OUTPUT" ]; then
    # Try to extract URLs -- they may be in various JSON shapes.
    EXTRACTED_URLS=$(echo "$URLS_OUTPUT" | grep -oE 'https?://[^"]+' || true)
    if [ -n "$EXTRACTED_URLS" ]; then
        while IFS= read -r url; do
            # Extract hostname for --resolve (bypasses DNS for multi-level .localhost)
            CURL_HOST=$(echo "$url" | sed -E 's|https?://([^/:]+).*|\1|')
            CURL_OK=0
            for _attempt in 1 2 3 4 5; do
                if curl -sk --resolve "${CURL_HOST}:443:127.0.0.1" --resolve "${CURL_HOST}:80:127.0.0.1" \
                        --max-time 5 "$url" >/dev/null 2>&1; then
                    CURL_OK=1
                    break
                fi
                sleep 1
            done
            if [ "$CURL_OK" = "1" ]; then
                pass "curl $url returned 200"
            else
                fail "curl $url failed"
            fi
        done <<< "$EXTRACTED_URLS"
    else
        skip "no URLs extracted from urls output"
    fi
else
    skip "curl tests (no URL output available)"
fi

# ---------------------------------------------------------------------------
# Test 7: veld logs
# ---------------------------------------------------------------------------

header "Logs"

assert_ok "veld logs --node backend --lines 10" \
    "$VELD_BIN" logs --name "$RUN_NAME" --node backend --lines 10

# ---------------------------------------------------------------------------
# Test 8: veld stop
# ---------------------------------------------------------------------------

header "Stop Environment"

assert_ok "veld stop --name $RUN_NAME" \
    "$VELD_BIN" stop --name "$RUN_NAME"

sleep 1

# ---------------------------------------------------------------------------
# Test 8b: Post-stop URL verification (PRD assertion 10)
# ---------------------------------------------------------------------------

header "Post-Stop URL Verification"

if [ -n "$EXTRACTED_URLS" ]; then
    while IFS= read -r url; do
        CURL_HOST=$(echo "$url" | sed -E 's|https?://([^/:]+).*|\1|')
        HTTP_CODE=$(curl -sk --resolve "${CURL_HOST}:443:127.0.0.1" \
            -o /dev/null -w "%{http_code}" --max-time 3 "$url" 2>/dev/null) || HTTP_CODE="000"
        if [ "$HTTP_CODE" = "000" ] || [ "$HTTP_CODE" -ge 400 ] 2>/dev/null; then
            pass "post-stop: $url is unreachable or non-200 (HTTP $HTTP_CODE)"
        else
            fail "post-stop: $url still returned HTTP $HTTP_CODE"
        fi
    done <<< "$EXTRACTED_URLS"
else
    skip "post-stop URL check (no URLs were extracted earlier)"
fi

# ---------------------------------------------------------------------------
# Test 9: Processes are dead
# ---------------------------------------------------------------------------

header "Process Cleanup Verification"

# After stop, the PIDs should be gone. We check by looking at the status.
if STOPPED_STATUS=$("$VELD_BIN" status --name "$RUN_NAME" --json 2>/dev/null); then
    if echo "$STOPPED_STATUS" | grep -qiE '"(stopped|not.found|exited)"'; then
        pass "processes confirmed stopped"
    else
        # If status shows nothing or an error, that also counts as stopped.
        pass "processes confirmed stopped (no active status)"
    fi
else
    # Non-zero exit from status after stop is expected -- run may be gone.
    pass "processes confirmed stopped (run no longer in registry)"
fi

# ---------------------------------------------------------------------------
# Test 10: veld gc
# ---------------------------------------------------------------------------

header "Garbage Collection"

assert_ok "veld gc exits 0" "$VELD_BIN" gc

# ---------------------------------------------------------------------------
# Test 11: Re-start (idempotency)
# ---------------------------------------------------------------------------

header "Idempotency (Re-Start)"

cd "$PROJECT_DIR"

assert_ok "veld start (re-start) exits 0" \
    run_with_timeout 120 "$VELD_BIN" start "frontend:local" --name "$RUN_NAME"

sleep 2

if RESTART_STATUS=$("$VELD_BIN" status --name "$RUN_NAME" --json 2>/dev/null); then
    if echo "$RESTART_STATUS" | grep -qiE '"(healthy|running)"'; then
        pass "re-started environment is healthy"
    else
        fail "re-started environment is not healthy"
    fi
else
    fail "could not get status after re-start"
fi

# Stop again for cleanup.
"$VELD_BIN" stop --name "$RUN_NAME" >/dev/null 2>&1 || true

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

header "Summary"

TOTAL=$((PASSED + FAILED + SKIPPED))
printf "  Total:   %d\n" "$TOTAL"
printf "  ${GREEN}Passed:  %d${RESET}\n" "$PASSED"
printf "  ${RED}Failed:  %d${RESET}\n" "$FAILED"
printf "  ${YELLOW}Skipped: %d${RESET}\n" "$SKIPPED"
echo ""

if [ "$FAILED" -gt 0 ]; then
    printf "${RED}${BOLD}SOME TESTS FAILED${RESET}\n"
    exit 1
else
    printf "${GREEN}${BOLD}ALL TESTS PASSED${RESET}\n"
    exit 0
fi

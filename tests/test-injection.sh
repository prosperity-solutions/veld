#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# E2E test: verify veld_inject bootstrap script is prepended to HTML responses.
#
# Usage:
#   ./tests/test-injection.sh --veld-bin <path> --project-dir <path>
#
# Prerequisites:
#   - veld setup already done (Caddy running)
#   - Node.js available (for Next.js dev server)
#   - npm install already run in the project dir
# ---------------------------------------------------------------------------
set -euo pipefail

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[0;33m'
NC='\033[0m'

VELD_BIN=""
PROJECT_DIR=""
RUN_NAME="inject-test-$$"
PASSED=0
FAILED=0

# ---------------------------------------------------------------------------
# Parse arguments
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
  case $1 in
    --veld-bin) VELD_BIN="$2"; shift 2 ;;
    --project-dir) PROJECT_DIR="$2"; shift 2 ;;
    *) echo "Unknown arg: $1"; exit 1 ;;
  esac
done

VELD_BIN="${VELD_BIN:-$(which veld 2>/dev/null || echo "")}"
PROJECT_DIR="${PROJECT_DIR:-$(cd "$(dirname "$0")/../testproject-nextjs" && pwd)}"

if [[ -z "$VELD_BIN" ]]; then
  echo "Error: --veld-bin required (or veld must be on PATH)"
  exit 1
fi

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
assert_ok() {
  local desc="$1"; shift
  if "$@" >/dev/null 2>&1; then
    echo -e "  ${GREEN}✓${NC} $desc"
    PASSED=$((PASSED + 1))
  else
    echo -e "  ${RED}✗${NC} $desc"
    FAILED=$((FAILED + 1))
  fi
}

assert_contains() {
  local desc="$1" content="$2" pattern="$3"
  if echo "$content" | grep -q "$pattern"; then
    echo -e "  ${GREEN}✓${NC} $desc"
    PASSED=$((PASSED + 1))
  else
    echo -e "  ${RED}✗${NC} $desc (pattern '$pattern' not found)"
    FAILED=$((FAILED + 1))
  fi
}

assert_not_contains() {
  local desc="$1" content="$2" pattern="$3"
  if ! echo "$content" | grep -q "$pattern"; then
    echo -e "  ${GREEN}✓${NC} $desc"
    PASSED=$((PASSED + 1))
  else
    echo -e "  ${RED}✗${NC} $desc (pattern '$pattern' unexpectedly found)"
    FAILED=$((FAILED + 1))
  fi
}

assert_http_ok() {
  local desc="$1" url="$2"
  local status
  status=$(curl -sk -o /dev/null -w '%{http_code}' "$url" 2>/dev/null || echo "000")
  if [[ "$status" == "200" ]]; then
    echo -e "  ${GREEN}✓${NC} $desc (HTTP $status)"
    PASSED=$((PASSED + 1))
  else
    echo -e "  ${RED}✗${NC} $desc (HTTP $status)"
    FAILED=$((FAILED + 1))
  fi
}

cleanup() {
  echo ""
  echo -e "${YELLOW}Cleaning up...${NC}"
  cd "$PROJECT_DIR" 2>/dev/null || true
  "$VELD_BIN" stop --name "$RUN_NAME" 2>/dev/null || true
  sleep 1
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Start the Next.js app via veld
# ---------------------------------------------------------------------------
echo "=== Injection E2E Tests ==="
echo ""
echo "Starting Next.js testproject via veld..."
cd "$PROJECT_DIR"
"$VELD_BIN" start web:local --name "$RUN_NAME"

# Get the URL from veld
URLS_JSON=$("$VELD_BIN" urls --name "$RUN_NAME" --json 2>/dev/null)
BASE_URL=$(echo "$URLS_JSON" | grep -o '"https://[^"]*"' | head -1 | tr -d '"')

if [[ -z "$BASE_URL" ]]; then
  echo -e "${RED}Error: Could not determine URL from veld urls output${NC}"
  echo "Raw output: $URLS_JSON"
  exit 1
fi

echo "App URL: $BASE_URL"
echo ""

# Wait for the app to be ready (Next.js cold start can be slow)
echo "Waiting for app to be ready..."
for i in $(seq 1 30); do
  if curl -sk -o /dev/null -w '' "$BASE_URL/" 2>/dev/null; then
    break
  fi
  sleep 2
done

# ---------------------------------------------------------------------------
# Test 1: Bootstrap script injection
# ---------------------------------------------------------------------------
echo ""
echo "--- Bootstrap Script Injection ---"

HOME_HTML=$(curl -sk "$BASE_URL/" 2>/dev/null)

assert_contains \
  "Bootstrap script present in HTML" \
  "$HOME_HTML" \
  "__veld_cl"

assert_contains \
  "Bootstrap wrapped in <script> tag" \
  "$HOME_HTML" \
  "<script>.*__veld_cl"

assert_contains \
  "Bootstrap appears after <!DOCTYPE" \
  "$HOME_HTML" \
  "<!DOCTYPE html><script>"

# Verify nothing precedes <!DOCTYPE (quirks mode regression guard).
assert_contains \
  "No content before <!DOCTYPE" \
  "$HOME_HTML" \
  "^<!DOCTYPE"

assert_contains \
  "Page content still renders" \
  "$HOME_HTML" \
  "veld-test-marker"

# ---------------------------------------------------------------------------
# Test 2: Streaming SSR page
# ---------------------------------------------------------------------------
echo ""
echo "--- Streaming SSR ---"

STREAM_HTML=$(curl -sk "$BASE_URL/streaming" 2>/dev/null)

assert_contains \
  "Streaming page has bootstrap script" \
  "$STREAM_HTML" \
  "__veld_cl"

assert_contains \
  "Streaming page renders Suspense content" \
  "$STREAM_HTML" \
  "streamed-content"

# Verify the page is dynamically rendered (not statically cached).
STREAM_HEADERS=$(curl -sk -D- -o /dev/null "$BASE_URL/streaming" 2>/dev/null)
assert_contains \
  "Streaming page is dynamic (must-revalidate)" \
  "$STREAM_HEADERS" \
  "must-revalidate"

# ---------------------------------------------------------------------------
# Test 3: Feedback assets reachable
# ---------------------------------------------------------------------------
echo ""
echo "--- Feedback Assets ---"

assert_http_ok \
  "Draw overlay JS reachable" \
  "$BASE_URL/__veld__/feedback/draw.js"

assert_http_ok \
  "Feedback JS reachable" \
  "$BASE_URL/__veld__/feedback/script.js"

assert_http_ok \
  "Client-log.js reachable" \
  "$BASE_URL/__veld__/api/client-log.js"

# Verify client-log.js has the early log drain
CLIENT_LOG_JS=$(curl -sk "$BASE_URL/__veld__/api/client-log.js" 2>/dev/null)
assert_contains \
  "client-log.js drains early logs" \
  "$CLIENT_LOG_JS" \
  "__veld_early_logs"

# ---------------------------------------------------------------------------
# Test 4: Non-HTML passthrough
# ---------------------------------------------------------------------------
echo ""
echo "--- Non-HTML Passthrough ---"

# A JS bundle served by Next.js should NOT have the bootstrap injected.
JS_BUNDLE=$(curl -sk "$BASE_URL/_next/static/chunks/webpack.js" 2>/dev/null || echo "")
if [[ -n "$JS_BUNDLE" ]] && ! echo "$JS_BUNDLE" | grep -q "<!DOCTYPE"; then
  assert_not_contains \
    "Non-HTML response has no bootstrap" \
    "$JS_BUNDLE" \
    "__veld_cl"
else
  echo -e "  ${YELLOW}⊘${NC} Non-HTML passthrough (skipped — could not fetch JS bundle)"
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
echo "=== Results: ${PASSED} passed, ${FAILED} failed ==="

if [[ "$FAILED" -gt 0 ]]; then
  exit 1
fi

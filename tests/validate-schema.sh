#!/usr/bin/env bash
# Validate the Veld JSON schema and configuration files.
#
# Usage: ./tests/validate-schema.sh [--install]
#
# With --install it will pip-install check-jsonschema first.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

SCHEMA="$REPO_ROOT/schema/v1/veld.schema.json"
CHECK="python3 -m check_jsonschema"

if [[ "${1:-}" == "--install" ]]; then
  pip3 install --quiet check-jsonschema
fi

# Verify the tool is available.
if ! $CHECK --help &>/dev/null; then
  echo "ERROR: check-jsonschema not found. Run with --install or: pip3 install check-jsonschema"
  exit 1
fi

PASS=0
FAIL=0

run_check() {
  local label="$1"
  shift
  echo -n "  $label ... "
  if "$@" 2>&1; then
    echo "OK"
    PASS=$((PASS + 1))
  else
    echo "FAIL"
    FAIL=$((FAIL + 1))
  fi
}

echo "=== JSON Schema Validation ==="
echo

echo "1) Meta-schema: validating schema/v1/veld.schema.json against JSON Schema draft 2020-12"
run_check "veld.schema.json is valid" \
  $CHECK --check-metaschema "$SCHEMA"

echo
echo "2) Instance validation: checking project configs against the schema"

# Find all veld.json files in the repo (excluding node_modules, target, etc.)
while IFS= read -r config; do
  rel="${config#"$REPO_ROOT/"}"
  run_check "$rel" \
    $CHECK --schemafile "$SCHEMA" "$config"
done < <(find "$REPO_ROOT" -name "veld.json" \
  -not -path "*/node_modules/*" \
  -not -path "*/target/*" \
  -not -path "*/.git/*" | sort)

echo
echo "=== Results: $PASS passed, $FAIL failed ==="

if [[ $FAIL -gt 0 ]]; then
  exit 1
fi

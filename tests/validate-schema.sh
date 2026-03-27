#!/usr/bin/env bash
# Validate the Veld JSON schema and configuration files.
#
# Usage: ./tests/validate-schema.sh [--install]
#
# With --install it will pip-install check-jsonschema first.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

SCHEMA_V1="$REPO_ROOT/schema/v1/veld.schema.json"
SCHEMA_V2="$REPO_ROOT/schema/v2/veld.schema.json"
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

echo "1) Meta-schema: validating schema files against JSON Schema draft 2020-12"
run_check "schema/v1/veld.schema.json is valid" \
  $CHECK --check-metaschema "$SCHEMA_V1"
run_check "schema/v2/veld.schema.json is valid" \
  $CHECK --check-metaschema "$SCHEMA_V2"

echo
echo "2) Instance validation: checking project configs against their schema version"

# Find all veld.json files in the repo (excluding node_modules, target, etc.)
while IFS= read -r config; do
  rel="${config#"$REPO_ROOT/"}"

  # Pick the schema based on the file's schemaVersion field.
  version=$(python3 -c "import json; print(json.load(open('$config')).get('schemaVersion', '1'))" 2>/dev/null || echo "1")
  if [[ "$version" == "2" ]]; then
    schema="$SCHEMA_V2"
  else
    schema="$SCHEMA_V1"
  fi

  run_check "$rel (v$version)" \
    $CHECK --schemafile "$schema" "$config"
done < <(find "$REPO_ROOT" -name "veld.json" \
  -not -path "*/node_modules/*" \
  -not -path "*/target/*" \
  -not -path "*/.git/*" | sort)

echo
echo "=== Results: $PASS passed, $FAIL failed ==="

if [[ $FAIL -gt 0 ]]; then
  exit 1
fi

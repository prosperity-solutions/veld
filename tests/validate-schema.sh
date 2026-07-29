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
SCHEMA_V3="$REPO_ROOT/schema/v3/veld.schema.json"
CHECK="python3 -m check_jsonschema"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# veld accepts JSONC — comments and trailing commas — in every config file, but
# `check-jsonschema` and `json.load` are strict JSON parsers. Strip comments the
# same way veld does (blanking bytes in place, so positions are preserved) before
# handing a config to either of them; otherwise the first comment anyone writes
# fails this job for a reason that has nothing to do with the schema.
strip_jsonc() {
  python3 - "$1" <<'PYEOF'
import sys

src = open(sys.argv[1], "rb").read()
out = bytearray(src)
i, in_string, escaped = 0, False, False
while i < len(out):
    b = out[i]
    if in_string:
        if escaped:
            escaped = False
        elif b == 0x5C:
            escaped = True
        elif b == 0x22:
            in_string = False
        i += 1
        continue
    if b == 0x22:
        in_string = True
        i += 1
    elif b == 0x2F and i + 1 < len(out) and out[i + 1] == 0x2F:
        while i < len(out) and out[i] != 0x0A:
            out[i] = 0x20
            i += 1
    elif b == 0x2F and i + 1 < len(out) and out[i + 1] == 0x2A:
        out[i] = out[i + 1] = 0x20
        i += 2
        while i < len(out):
            if out[i] == 0x2A and i + 1 < len(out) and out[i + 1] == 0x2F:
                out[i] = out[i + 1] = 0x20
                i += 2
                break
            if out[i] != 0x0A:
                out[i] = 0x20
            i += 1
    else:
        i += 1

# Trailing commas: a comma followed only by whitespace before } or ].
i, in_string, escaped = 0, False, False
while i < len(out):
    b = out[i]
    if in_string:
        if escaped:
            escaped = False
        elif b == 0x5C:
            escaped = True
        elif b == 0x22:
            in_string = False
        i += 1
        continue
    if b == 0x22:
        in_string = True
    elif b == 0x2C:
        j = i + 1
        while j < len(out) and out[j] in b" \t\r\n":
            j += 1
        if j < len(out) and out[j] in b"}]":
            out[i] = 0x20
    i += 1

sys.stdout.write(out.decode("utf-8"))
PYEOF
}

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
# v1 and v2 are retained only so an older veld can still validate an unmigrated
# config; this veld does not load them.
run_check "schema/v1/veld.schema.json is valid" \
  $CHECK --check-metaschema "$SCHEMA_V1"
run_check "schema/v2/veld.schema.json is valid" \
  $CHECK --check-metaschema "$SCHEMA_V2"
run_check "schema/v3/veld.schema.json is valid" \
  $CHECK --check-metaschema "$SCHEMA_V3"

echo
echo "2) Instance validation: checking project configs against their schema version"

# Find all veld.json files in the repo (excluding node_modules, target, etc.)
while IFS= read -r config; do
  rel="${config#"$REPO_ROOT/"}"

  # Comments are legal in a veld config; the validators below are not.
  plain="$WORK/$(echo "$rel" | tr '/' '_')"
  strip_jsonc "$config" > "$plain"

  # Only v3 is supported, so every tracked config must declare it — a config still
  # on v1/v2 would fail to load at runtime, and this catches it in CI instead.
  version=$(python3 -c "import json; print(json.load(open('$plain')).get('schemaVersion', 'missing'))" 2>/dev/null || echo "unreadable")
  if [[ "$version" != "3" ]]; then
    echo -n "  $rel ... "
    echo "FAIL (schemaVersion is \"$version\"; only \"3\" is supported — see docs/migrating-to-v3.md)"
    FAIL=$((FAIL + 1))
    continue
  fi

  run_check "$rel (v$version)" \
    $CHECK --schemafile "$SCHEMA_V3" "$plain"
done < <(find "$REPO_ROOT" -name "veld.json" \
  -not -path "*/node_modules/*" \
  -not -path "*/target/*" \
  -not -path "*/.git/*" | sort)

echo
echo "3) Schema drift gate: every documented v3 example validates against the schema"
echo
# The JSON Schema is hand-maintained with no compiler check tying it to the Rust
# types, so it drifts silently. These examples are the gate: this job validates
# them against the schema, and `schema_v3_examples_round_trip` in veld-core
# deserializes the same files with serde. A change to either side that the other
# does not know about fails one of the two.
for example in "$REPO_ROOT"/schema/v3/examples/*.json; do
  # Strip comments first, as check 2 does. These files are veld configs, so
  # they are JSONC — and the Rust half of this gate
  # (`schema_v3_examples_round_trip`) reads them through veld's own loader,
  # which accepts comments. Validating them here as strict JSON made the two
  # halves disagree about what an example was allowed to contain.
  plain="$WORK/example_$(basename "$example")"
  strip_jsonc "$example" > "$plain"
  run_check "$(basename "$example")" \
    $CHECK --schemafile "$SCHEMA_V3" "$plain"
done

echo
echo "4) Doc example gate: no documented example uses a form that cannot load"
echo
# Docs rot silently: an example keeps looking plausible long after the parser stops
# accepting it. This whole batch of files shipped v1/v2 examples for a while, and
# nothing caught it — a doc example is not covered by the schema examples above,
# and no test parses prose. So the legacy forms are grepped for directly.
#
# `docs/migrating-to-v3.md` is exempt by design: it has to show the old form next
# to the new one, which is the entire point of the page.
LEGACY_PATTERNS=(
  # `"command":` used as a KEY (a legacy command). `"type": "command"` is a node
  # kind and stays legal, so the pattern requires `command` on the left of the colon.
  '"command"[[:space:]]*:[[:space:]]*"'
  # A bare-string on_stop / skip_if / verify — the v1/v2 form.
  '"(on_stop|skip_if|verify)"[[:space:]]*:[[:space:]]*"'
  # An unsupported schemaVersion in an example.
  '"schemaVersion"[[:space:]]*:[[:space:]]*"[12]"'
)
DOC_FILES=$(git -C "$REPO_ROOT" ls-files '*.md' '*.html' '*.txt' \
  | grep -vE '^(docs/migrating-to-v3\.md|CHANGELOG\.md)$' \
  | grep -vE '(^|/)node_modules/')
for pattern in "${LEGACY_PATTERNS[@]}"; do
  hits=$(cd "$REPO_ROOT" && grep -nE "$pattern" $DOC_FILES 2>/dev/null || true)
  echo -n "  no matches for /$pattern/ ... "
  if [[ -z "$hits" ]]; then
    echo "OK"
    PASS=$((PASS + 1))
  else
    echo "FAIL"
    echo "$hits" | sed 's/^/      /'
    FAIL=$((FAIL + 1))
  fi
done

echo
echo "5) Doc example gate: every complete documented config loads and lints clean"
echo
# The grep above catches a fragment using a dead form. This catches a whole example
# that is broken some other way — `docs/scenarios.md` shipped 18 examples with brace
# typos that made them invalid JSON, which no test noticed. Needs a built binary, so
# it is skipped (not failed) when there isn't one.
VELD_BIN="$REPO_ROOT/target/debug/veld"
[[ -x "$VELD_BIN" ]] || VELD_BIN="$REPO_ROOT/target/release/veld"
if [[ -x "$VELD_BIN" ]]; then
  if python3 "$REPO_ROOT/tests/validate-doc-examples.py" "$VELD_BIN"; then
    PASS=$((PASS + 1))
  else
    FAIL=$((FAIL + 1))
  fi
else
  # Skipping is correct here, not a silent hole: the `integration` job in ci.yml runs
  # `validate-doc-examples.py` as its own first-class step, because that job is the
  # one with a built binary. The schema job never builds Rust, so hard-failing on a
  # missing binary just broke it unconditionally — which is what the first version of
  # this did.
  echo "  SKIP (no veld binary here — ci.yml runs this gate in the integration job)"
fi

echo
echo "=== Results: $PASS passed, $FAIL failed ==="

if [[ $FAIL -gt 0 ]]; then
  exit 1
fi

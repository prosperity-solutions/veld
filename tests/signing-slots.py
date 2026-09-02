#!/usr/bin/env python3
"""Read the org signing-key table out of `crates/veld-core/src/signing.rs`.

`veld_core::signing::ORG_KEYS` is the one place that says which keys a release is
signed by, in which slot, and which GitHub secret supplies each private half.
**Two files that cannot evaluate Rust have to read it**: `release.yml`'s
`Package client binaries` step, which turns it into `veld-sign --slots`, and
`ci.yml`'s `schema` job, which checks the same table against that step's `env:`
block on every pull request.

It is one script rather than two regexes for the obvious reason — a parser
duplicated in two workflows is a parser that drifts, and the symptom of drift here
is a release no privileged install can accept, repairable only with sudo on every
machine.

    python3 tests/signing-slots.py            # the --slots argument for veld-sign
    python3 tests/signing-slots.py --json     # the whole table, for ci.yml
    python3 tests/signing-slots.py --tsv      # the same, for the Rust cross-check
    python3 tests/signing-slots.py --selftest # the parser's own fixtures

The Rust side compares `--tsv` against `ORG_KEYS` itself
(`the_slot_script_reads_the_same_table_the_compiler_does`), which is the only
check that this parser and rustc see the same table. Tab-separated rather than
JSON so that comparison needs no JSON crate in the trust root.

**Every mis-parse must fail loudly rather than quietly produce a shorter list**,
because a dropped row is a dropped slot and a dropped slot strands a helper
generation. So the parser brace-matches rather than pattern-matching row shapes,
insists on exactly the four fields it knows, and re-checks the invariants the
Rust tests also assert — here they are assertions about *this parse*, not a second
copy of the rules.
"""

import argparse
import json
import pathlib
import re
import sys

SIGNING_RS = "crates/veld-core/src/signing.rs"

# Hand-kept equal to `veld_core::signing::MAX_SIG_SLOTS`. A `const _: () =
# assert!(ORG_KEYS.len() <= MAX_SIG_SLOTS)` in that file is the compiler's copy of
# this rule; this one catches a table that grew past what any reader looks at in
# the two places that cannot run the compiler.
MAX_SLOTS = 8

# The prefix `ci.yml`'s leak gate matches on to prove no pull-request-startable job
# can reach a signing secret.
SECRET_PREFIX = "SIGNING_"


class TableError(Exception):
    """A table this script will not guess about."""


def _balanced(text, open_at):
    """The index just past the `]` or `}` matching the bracket at `open_at`.

    Brace-matching rather than a non-greedy regex: a row's `status` field can
    itself carry braces (`KeyStatus::Retired { retired_after: "…" }`), and a
    regex that stopped at the first `}` would silently truncate the row — which is
    the class of mis-parse this whole file exists to make impossible.
    """
    pairs = {"[": "]", "{": "}"}
    closer = pairs[text[open_at]]
    depth = 0
    for i in range(open_at, len(text)):
        if text[i] == text[open_at]:
            depth += 1
        elif text[i] == closer:
            depth -= 1
            if depth == 0:
                return i
    raise TableError(f"{SIGNING_RS}: unbalanced {text[open_at]!r} in ORG_KEYS")


def _strip_comments(text):
    """Drop `//` line comments, leaving anything inside a string literal alone.

    Not cosmetic. The field patterns below take the **first** match in a row, so a
    stale commented-out `secret: "…"` above the live one would be read instead of
    it — silently, and with the two halves of a slot then coming from different
    places. That failure surfaces at the release (push-only, after merge) as an
    expected-key mismatch, which is loud but is the most expensive place to learn
    it. A `//` with an odd number of quotes before it on the line is inside a
    string literal and is left alone.
    """
    out = []
    for line in text.splitlines():
        cut = -1
        for i in range(len(line) - 1):
            if line[i : i + 2] == "//" and line[:i].count('"') % 2 == 0:
                cut = i
                break
        out.append(line if cut < 0 else line[:cut])
    return "\n".join(out)


def _rows(body):
    """Split the table body into `OrgKey { … }` blocks."""
    out = []
    for m in re.finditer(r"OrgKey\s*\{", body):
        open_at = body.index("{", m.start())
        close_at = _balanced(body, open_at)
        out.append(body[open_at + 1 : close_at])
    return out


def _key_hex(name, source):
    """`pub const <name>: PubKey = [ … ];` as 64 lowercase hex characters."""
    m = re.search(rf"pub const {re.escape(name)}: PubKey = \[(.*?)\];", source, re.S)
    if not m:
        raise TableError(
            f"{SIGNING_RS}: ORG_KEYS names `{name}`, which is not defined as a "
            "`pub const <name>: PubKey = [ … ];`"
        )
    hexes = re.findall(r"0x([0-9a-fA-F]{2})", m.group(1))
    if len(hexes) != 32:
        raise TableError(
            f"{SIGNING_RS}: `{name}` parsed as {len(hexes)} bytes, expected 32. An "
            "ed25519 public key is 32 bytes written as 0xNN literals."
        )
    return "".join(h.lower() for h in hexes)


def parse(source):
    """`ORG_KEYS` as a list of `{index, key, pubkey, secret, added_after, status}`."""
    # Once, up front, so that BOTH the table rows and the `pub const ORG_KEY_N`
    # definitions they name are read from the same stripped text. Stripping only
    # the rows left `_key_hex` searching the raw source, where a commented-out key
    # constant above the live one wins — the derived `--slots` would then carry a
    # hex the compiled keyring does not name. That fails safe (veld-sign refuses)
    # but at the push-only release step, which is the failure class this whole
    # script exists to move to pull-request time.
    source = _strip_comments(source)
    m = re.search(r"pub const ORG_KEYS: &\[OrgKey\] = &\[", source)
    if not m:
        raise TableError(
            f"{SIGNING_RS}: `ORG_KEYS` is gone. It is the single table that says "
            "which keys a release is signed by and which secret signs each slot; "
            "release.yml and ci.yml both read it."
        )
    open_at = source.index("[", m.end() - 1)
    body = source[open_at + 1 : _balanced(source, open_at)]

    rows = []
    for index, row in enumerate(_rows(body)):
        fields = {}
        for field, pattern in (
            ("key", r"\bkey:\s*([A-Za-z_][A-Za-z0-9_]*)\s*,"),
            ("secret", r'\bsecret:\s*"([^"]*)"\s*,'),
            ("added_after", r'\badded_after:\s*"([^"]*)"\s*,'),
            ("status", r"\bstatus:\s*KeyStatus::([A-Za-z_][A-Za-z0-9_]*)"),
        ):
            found = re.search(pattern, row)
            if not found:
                raise TableError(
                    f"{SIGNING_RS}: ORG_KEYS row {index} has no readable `{field}`. "
                    "Every row is written `key: ORG_KEY_N, secret: \"SIGNING_…\", "
                    "added_after: \"x.y.z\", status: KeyStatus::…` — a row this script "
                    "cannot read is a slot release.yml would not sign."
                )
            fields[field] = found.group(1)
        if fields["status"] not in ("Accepted", "Retired"):
            raise TableError(
                f"{SIGNING_RS}: ORG_KEYS row {index} has status "
                f"`KeyStatus::{fields['status']}`, which this script does not know. "
                "Teach it here and in ci.yml's gate before adding a variant."
            )
        rows.append(
            {
                "index": index,
                "key": fields["key"],
                "pubkey": _key_hex(fields["key"], source),
                "secret": fields["secret"],
                "added_after": fields["added_after"],
                "status": fields["status"].lower(),
            }
        )

    _check(rows, source)
    return rows


def _check(rows, source):
    """Invariants that make a *parse* trustworthy, checked before it is used."""
    if not rows:
        raise TableError(f"{SIGNING_RS}: ORG_KEYS is empty; a release would be unsigned")
    if len(rows) > MAX_SLOTS:
        raise TableError(
            f"{SIGNING_RS}: ORG_KEYS has {len(rows)} rows but no reader looks past "
            f"slot {MAX_SLOTS - 1}. Raising the ceiling means raising MAX_SIG_SLOTS, "
            "veld-sign's MAX_SLOTS and this script together."
        )
    if rows[0]["pubkey"] != _key_hex("ORG_KEY_1", source):
        raise TableError(
            f"{SIGNING_RS}: ORG_KEY_1 is no longer row 0 of ORG_KEYS. Every helper "
            "shipped up to v16.59.0 verifies bytes 0..64 against it and nothing "
            "else, so moving it is a release none of them can relaunch onto."
        )
    for field, what in (("pubkey", "public key"), ("secret", "secret name")):
        seen = {}
        for row in rows:
            if row[field] in seen:
                raise TableError(
                    f"{SIGNING_RS}: ORG_KEYS rows {seen[row[field]]} and "
                    f"{row['index']} share a {what}. Slot position is this format's "
                    "only key identifier, so two slots that share one are a release "
                    "claiming to cover two helper generations while covering one."
                )
            seen[row[field]] = row["index"]
    for row in rows:
        # The whole slot layout travels to veld-sign as one `<hex>=<NAME>,…`
        # argument, so a name carrying a comma or an `=` would re-cut the list and
        # re-point a slot at another row's key. veld-sign refuses the malformed
        # entry rather than acting on it, but that refusal is at the release, which
        # is push-only. Held to the charset an environment variable can have.
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", row["secret"]):
            raise TableError(
                f"{SIGNING_RS}: ORG_KEYS row {row['index']} names the secret "
                f"{row['secret']!r}, which is not a valid environment-variable name. "
                "The slot layout is one comma-separated `<hex>=<NAME>` argument, so a "
                "name carrying a comma or an `=` would re-cut it."
            )
        if not row["secret"].startswith(SECRET_PREFIX):
            raise TableError(
                f"{SIGNING_RS}: ORG_KEYS row {row['index']} names the secret "
                f"{row['secret']!r}, which does not carry the shared signing-secret "
                "prefix. ci.yml's leak gate matches that prefix to prove no "
                "pull-request job can reach a signing secret; a name without it is a "
                "secret nothing checks."
            )


def slot_spec(rows):
    """The `--slots` argument: `<hex>=<VAR>`, comma-separated, in slot order."""
    return ",".join(f"{r['pubkey']}={r['secret']}" for r in rows)


# ── Self-test ───────────────────────────────────────────────────────────
# Each fixture is a mis-parse this script must refuse rather than quietly
# shorten. A parser that returns a shorter list is a release missing a slot.
_K1 = "pub const ORG_KEY_1: PubKey = [\n" + ", ".join(["0x11"] * 32) + ",\n];\n"
_K2 = "pub const ORG_KEY_2: PubKey = [\n" + ", ".join(["0x22"] * 32) + ",\n];\n"


def _table(*rows):
    return _K1 + _K2 + "pub const ORG_KEYS: &[OrgKey] = &[\n" + "".join(rows) + "];\n"


def _row(
    key="ORG_KEY_1",
    secret="SIGNING_PRIVATE_KEY",
    status="KeyStatus::Accepted",
    added_after="0.0.0",
):
    return (
        f"    OrgKey {{\n        key: {key},\n        secret: {secret!r},\n"
        f"        added_after: {added_after!r},\n        status: {status},\n    }},\n"
    ).replace("'", '"')


_GOOD = _table(
    _row(),
    _row(
        "ORG_KEY_2",
        "SIGNING_PRIVATE_KEY_2",
        'KeyStatus::Retired {\n            retired_after: "16.62.0",\n        }',
        "16.61.0",
    ),
)

_BAD = [
    ("no table", _K1, "ORG_KEYS` is gone"),
    ("empty table", _K1 + "pub const ORG_KEYS: &[OrgKey] = &[];\n", "is empty"),
    (
        "row with no secret",
        _table(
            "    OrgKey {\n        key: ORG_KEY_1,\n"
            '        added_after: "0.0.0",\n        status: KeyStatus::Accepted,\n    },\n'
        ),
        "no readable `secret`",
    ),
    (
        "row with no added_after",
        _table(
            "    OrgKey {\n        key: ORG_KEY_1,\n"
            '        secret: "SIGNING_PRIVATE_KEY",\n        status: KeyStatus::Accepted,\n    },\n'
        ),
        "no readable `added_after`",
    ),
    (
        "key constant missing",
        "pub const ORG_KEYS: &[OrgKey] = &[\n" + _row() + "];\n",
        "not defined",
    ),
    (
        "short key constant",
        "pub const ORG_KEY_1: PubKey = [0x11, 0x22];\n"
        + "pub const ORG_KEYS: &[OrgKey] = &[\n"
        + _row()
        + "];\n",
        "expected 32",
    ),
    ("ORG_KEY_1 not first", _table(_row("ORG_KEY_2", "SIGNING_PRIVATE_KEY_2")), "row 0"),
    ("duplicate key", _table(_row(), _row(secret="SIGNING_PRIVATE_KEY_2")), "share a public key"),
    ("duplicate secret", _table(_row(), _row("ORG_KEY_2")), "share a secret name"),
    ("secret without prefix", _table(_row(secret="ORG_KEY_2_PEM")), "shared signing-secret prefix"),
    (
        "secret name with a comma",
        _table(_row(secret="SIGNING_A,SIGNING_B")),
        "not a valid environment-variable name",
    ),
    (
        "stale commented-out key constant above the live one",
        "// pub const ORG_KEY_1: PubKey = [\n" + ", ".join(["0x99"] * 32) + ",\n];\n"
        + _K1
        + "pub const ORG_KEYS: &[OrgKey] = &[\n"
        + _row()
        + "];\n",
        None,  # must PARSE, and must read the live constant
    ),
    (
        "stale commented-out secret above the live one",
        _table(
            "    OrgKey {\n        key: ORG_KEY_1,\n"
            '        // secret: "SIGNING_PRIVATE_KEY_2",\n'
            '        secret: "SIGNING_PRIVATE_KEY",\n'
            '        added_after: "0.0.0",\n        status: KeyStatus::Accepted,\n    },\n'
        ),
        None,  # must PARSE, and must read the live field
    ),
    ("unknown status", _table(_row(status="KeyStatus::Provisional")), "does not know"),
    (
        "too many rows",
        _K1 + "pub const ORG_KEYS: &[OrgKey] = &[\n" + _row() + (_row() * MAX_SLOTS) + "];\n",
        "no reader looks past",
    ),
]


def selftest():
    rows = parse(_GOOD)
    assert len(rows) == 2, rows
    assert rows[0]["status"] == "accepted" and rows[1]["status"] == "retired", rows
    # The multi-line `Retired { … }` field must not truncate the row: its `secret`
    # is read after `key`, and a regex stopping at the first `}` would lose it.
    assert rows[1]["secret"] == "SIGNING_PRIVATE_KEY_2", rows
    assert slot_spec(rows) == f"{'11' * 32}=SIGNING_PRIVATE_KEY,{'22' * 32}=SIGNING_PRIVATE_KEY_2"

    refused = 0
    for name, source, needle in _BAD:
        if needle is None:
            # Not a refusal: a shape that must parse, and parse to the right thing.
            rows = parse(source)
            assert rows[0]["secret"] == "SIGNING_PRIVATE_KEY", f"{name}: read {rows[0]}"
            assert rows[0]["pubkey"] == "11" * 32, f"{name}: read {rows[0]}"
            continue
        try:
            parse(source)
        except TableError as e:
            assert needle in str(e), f"{name}: message was {e!r}, wanted {needle!r}"
            refused += 1
        else:
            sys.exit(f"selftest: {name} parsed cleanly; it must be refused")
    print(f"signing-slots selftest: {refused} mis-parses refused, {len(_BAD) - refused} shapes accepted")


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--json", action="store_true", help="the whole table")
    ap.add_argument("--tsv", action="store_true", help="the whole table, one row per line")
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()
    if args.selftest:
        selftest()
        return
    try:
        rows = parse(pathlib.Path(SIGNING_RS).read_text())
    except TableError as e:
        sys.exit(str(e))
    if args.json:
        print(json.dumps(rows))
    elif args.tsv:
        for r in rows:
            print("\t".join((r["pubkey"], r["secret"], r["added_after"], r["status"])))
    else:
        print(slot_spec(rows))


if __name__ == "__main__":
    main()

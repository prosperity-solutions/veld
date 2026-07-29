#!/usr/bin/env python3
"""Every complete `veld.json` example in the docs must load and lint clean.

The grep gate in `validate-schema.sh` catches a *fragment* that uses a form the
parser no longer accepts. It cannot catch a whole example that is malformed for
some other reason, and `docs/scenarios.md` shipped 18 examples with brace typos
that made them invalid JSON — plausible-looking, uncopyable, and unnoticed. This
runs the real binary over every example that is a complete config, so a doc
example is held to the same bar as a real one.

A fence holding more than one document (the `include` examples show a root file
and an included file side by side) is skipped: it is not a single config.

Usage: validate-doc-examples.py <path-to-veld-binary>
"""
import json
import os
import re
import subprocess
import sys
import tempfile

FENCE = re.compile(r"```(?:json|jsonc)\n(.*?)```", re.S)
# Deliberately shows the pre-v3 forms; see docs/migrating-to-v3.md.
EXEMPT = {"docs/migrating-to-v3.md"}
# `veld lint` needs a real config: schemaVersion plus at least one node.
REQUIRED = ('"schemaVersion"', '"nodes"')


def strip_jsonc(text):
    """Blank comments so `json` can parse a JSONC example (mirrors jsonc::strip)."""
    out, i, n = [], 0, len(text)
    in_str = in_line = in_block = False
    while i < n:
        c, nxt = text[i], text[i + 1] if i + 1 < n else ""
        if in_line:
            if c == "\n":
                in_line = False
                out.append(c)
            i += 1
            continue
        if in_block:
            if c == "*" and nxt == "/":
                in_block = False
                i += 2
                continue
            out.append("\n" if c == "\n" else " ")
            i += 1
            continue
        if in_str:
            out.append(c)
            if c == "\\":
                if i + 1 < n:
                    out.append(nxt)
                i += 2
                continue
            if c == '"':
                in_str = False
            i += 1
            continue
        if c == '"':
            in_str = True
            out.append(c)
            i += 1
            continue
        if c == "/" and nxt == "/":
            in_line = True
            i += 2
            continue
        if c == "/" and nxt == "*":
            in_block = True
            i += 2
            continue
        out.append(c)
        i += 1
    # Trailing commas.
    return re.sub(r",(\s*[}\]])", r" \1", "".join(out))


def split_documents(block):
    """Split a fence into its JSON documents.

    Returns (count, error). A malformed block MUST NOT be mistaken for a
    multi-document fence — that is how the first version of this gate silently
    skipped the very brace typos it was written to catch, and reported success.
    So every document has to parse: any leftover that does not is an error, not
    another document.
    """
    full = strip_jsonc(block)
    text = full.lstrip()
    decoder = json.JSONDecoder()
    count = 0
    while text.strip():
        consumed = len(full) - len(text)
        try:
            _, end = decoder.raw_decode(text)
        except json.JSONDecodeError as e:
            # Report the line within the fence, not within the leftover slice —
            # "line 1" of a remainder is meaningless to whoever has to fix it.
            line = full[:consumed].count("\n") + e.lineno
            hint = (
                " (an unbalanced brace earlier in the example usually shows up here)"
                if count else ""
            )
            return count, f"invalid JSON at example line {line}: {e.msg}{hint}"
        count += 1
        text = text[end:].lstrip()
    return count, None


def main():
    veld = os.path.abspath(sys.argv[1])
    files = subprocess.run(
        ["git", "ls-files", "*.md"], capture_output=True, text=True, check=True
    ).stdout.split()

    ok = failed = skipped = 0
    for path in files:
        if path in EXEMPT:
            continue
        with open(path) as f:
            content = f.read()
        for i, block in enumerate(FENCE.findall(content)):
            if not all(k in block for k in REQUIRED):
                continue
            label = f"{path} example #{i}"
            count, error = split_documents(block)
            if error is not None:
                failed += 1
                print(f"  {label} ... FAIL")
                print(f"      {error}")
                continue
            if count != 1:
                # A root file shown beside an included file. Both parsed, and an
                # included file legitimately has no schemaVersion/name, so there is
                # nothing here `veld lint` can be pointed at.
                skipped += 1
                continue
            work = tempfile.mkdtemp()
            with open(os.path.join(work, "veld.json"), "w") as f:
                f.write(block)
            r = subprocess.run(
                [veld, "lint"], cwd=work, capture_output=True, text=True
            )
            if r.returncode == 0:
                ok += 1
            else:
                failed += 1
                print(f"  {label} ... FAIL")
                for line in (r.stdout + r.stderr).strip().splitlines():
                    print(f"      {line}")

    print(
        f"  {ok} complete example(s) lint clean, {failed} failing, "
        f"{skipped} multi-document fence(s) skipped"
    )
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

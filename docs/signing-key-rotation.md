# Rotating the org's helper signing key

The privileged `veld-helper` runs as root and will only relaunch onto — or
install — a binary carrying a valid org ed25519 signature. That key's private
half lives in the org vault and in the `SIGNING_PRIVATE_KEY` GitHub secret. This
document is what to do when it leaks, and it is written on the assumption that
you are reading it on a bad day.

The mechanism is issue #261 slice C. The design argument, including the
alternatives that were rejected and why, is in the PR that added it; this file is
the operating manual.

> **Do not improvise here.** Every step below is one release. The failure mode
> for getting it wrong is not "the rotation didn't work" — it is *every
> privileged install stops accepting updates forever*, and the only repair left
> is asking every user to run `sudo`. That is the outcome the whole #338 chain
> exists to prevent, and this is the mechanism most able to cause it.

## The whole rotation, in order

**Two releases. Never one.** This list is complete on its own — everything below it
is the reasoning, not more steps. Every step here was executed against this repo,
twice in a row, before it was written down.

> A note on numbering, because a comment in the source will send you looking. The
> `## Step 1/2/3` headings further down are the **three phases** — make the key,
> add it, retire the old one — and that is what "step 2" or "step 3" means wherever
> `crates/veld-core/src/signing.rs` and `.github/workflows/ci.yml` mention one. The
> numbers in the list below are just the order of doing things. They are not the
> same numbering, and nothing outside this file refers to them.
>
> Test-failure messages name **tests**, never step numbers, precisely so that this
> cannot go stale: grep the name the failure gives you.

Against the four-edits-plus-two-test-deletions procedure this replaces, what a
rotation costs now:

| | before | now |
|---|---|---|
| `crates/veld-core/src/signing.rs` | 3 edits (key constant, two lists, slot count) | **1 row appended** |
| `.github/workflows/release.yml` | 3 edits (`--key-env`, its `env:` entry, the expected-key hex) | **none** |
| tests to delete | 2, one per release | **none** |
| GitHub secrets to set | 1 | 1 |
| releases | 2 | 2 |

### Release 1 — teach every helper the new key

**1. Make the key, on a machine that is not the one that leaked.** macOS's
`/usr/bin/openssl` is LibreSSL and **cannot do this**; use an OpenSSL 3.

```sh
OPENSSL=/opt/homebrew/opt/openssl@3/bin/openssl   # Intel Mac: /usr/local/opt/openssl@3/bin/openssl
                                                  # Linux: openssl
"$OPENSSL" version                                # must say OpenSSL, not LibreSSL

"$OPENSSL" genpkey -algorithm ed25519 -out "$PWD/veld-signing-2.pem"
"$OPENSSL" pkey -in "$PWD/veld-signing-2.pem" -pubout -outform DER \
  | tail -c 32 | xxd -p -c 32
```

That hex is the **new public key**. You paste it once, into the key constant in
step 4. The same 32 bytes in the form `signing.rs` wants:

```sh
python3 -c "
import sys, textwrap
h = sys.argv[1]
print(chr(10).join(textwrap.wrap(', '.join(f'0x{h[i:i+2]}' for i in range(0, len(h), 2)), 96)))
" <the hex>
```

**2. Put the private half in the org vault, beside the original key.** Before the
GitHub secret, not after. A repository secret **cannot be read back**, so the vault
is the only copy anything can ever recover — and a retired key's slot keeps being
signed forever (step 9), so losing its private half is a flag day, not an
inconvenience. Delete the local `.pem` once the vault has it and the secret in
step 3 is set.

**3. Put the private half in a GitHub secret named `SIGNING_PRIVATE_KEY_2`.** This
is the only step that is not a code change, and it needs **repository admin**. It
is a **repository** secret — not an environment secret, not an org secret;
`release.yml`'s `build` job has no `environment:`, so an environment secret would
simply not be there.

```sh
# The whole PEM, including the BEGIN/END lines. `gh` reads the file, so nothing
# lands in your shell history.
gh secret set SIGNING_PRIVATE_KEY_2 < "$PWD/veld-signing-2.pem"

gh secret list          # you see the NAME and a timestamp; never the value
```

By hand instead: **Settings → Secrets and variables → Actions → New repository
secret**. Name `SIGNING_PRIVATE_KEY_2`, value = the entire file contents.

**The name is not yours to choose, and it is not descriptive — it is positional.**
`release.yml` carries a permanent roster of eight names,
`SIGNING_PRIVATE_KEY` through `SIGNING_PRIVATE_KEY_8`, written once and never
edited again. Take the next free number. Three things follow:

- **Never re-upload `SIGNING_PRIVATE_KEY`.** It holds the org's *original* key and
  it is slot 0, which every helper shipped up to v16.59.0 verifies bytes `0..64`
  against and nothing else. Overwriting it is the single most expensive mistake
  available in this procedure.
- **Do not invent a name like `SIGNING_PRIVATE_KEY_OLD`.** A name relative to *now*
  reads as the obvious scheme and is fatal on the **second** rotation: the second
  key would land in slot 0, where no already-shipped helper has ever trusted it,
  and the original key would have no slot at all. Positional names cannot do that.
- **Setting the secret early is safe.** Nothing reads it until step 4's row names
  it, so
  releases in between are unaffected.

**You cannot read a secret back**, so nothing can confirm the upload by inspection.
What confirms it is the expected-key check at the **release** — step 7, after the
merge, because that is the only moment anything can read the secret. If it holds a
different key the release fails rather than publishing an artifact no installed
helper would accept. **A green pull request says nothing about the secret**, which
is why the local probe below is worth the one command. Check the file itself first — this is the last moment it is
inspectable. **Run it from your veld checkout** (`-p veld-sign` resolves through
the workspace):

```sh
# An ABSOLUTE path to the key: step 1 may have made it on another machine, and this
# has to run from your veld checkout. The probe target needs a `.` in its name —
# veld-sign will not print a path segment that could be a chunk of encoded key
# material, so a bare /tmp/probe comes back as `<redacted: …>` in the SUCCESS line.
printf 'x' > /tmp/veld-probe.bin
cargo run -q -p veld-sign -- --key-file /absolute/path/to/veld-signing-2.pem \
  /tmp/veld-probe.bin
```

It prints the public key of each slot it signed. That hex must equal the one from
step 1. Any other outcome — wrong PEM label, encrypted key, OpenSSH format, a
byte-order mark, the `.pub` by mistake — is named exactly by the error.

**4. Append one row to `ORG_KEYS` in `crates/veld-core/src/signing.rs`.** This is
the entire code change, and it is the only edit a rotation makes:

```rust
pub const ORG_KEY_2: PubKey = [
    /* the 32 bytes from step 1 */
];

pub const ORG_KEYS: &[OrgKey] = &[
    OrgKey {
        key: ORG_KEY_1,
        secret: "SIGNING_PRIVATE_KEY",
        added_after: "0.0.0",
        status: KeyStatus::Accepted,
    },
    OrgKey {
        key: ORG_KEY_2,
        secret: "SIGNING_PRIVATE_KEY_2",
        // Cargo.toml's `version` right now — read it, do not copy this line.
        added_after: "<Cargo.toml's version>",
        status: KeyStatus::Accepted,
    },
];
```

**Append. Never reorder, never delete a row.** A row's position in this table *is*
its slot in `<binary>.sig`, and slot position is the format's only key identifier.

`added_after` is `Cargo.toml`'s `version` at the moment you write the row — the
last release before the one this change will become. You can always know it, and
that is what makes it useful: `Cargo.toml` still holds the *previous* release until
semantic-release bumps it after your merge, so it is the one thing about the future
release you can state truthfully. [The two-release rule, and its
guard](#the-two-release-rule-and-its-guard) is what it is for.

**5. `cargo fmt --all`.** A hand-written 32-byte array will not match rustfmt, and
CI's `fmt --check` is a hard failure.

**6. Open the pull request. Nothing else needs editing — check that nothing was.**

```sh
cargo test --workspace          # no test is deleted by a rotation any more
python3 tests/signing-slots.py  # what release.yml will pass to veld-sign
git status --short              # signing.rs, and nothing else
```

`tests/signing-slots.py` prints `<public key>=<secret name>` per slot, in slot
order. Read it: the first entry must still be the original key and
`SIGNING_PRIVATE_KEY`, and the new entry's hex must match step 1. That string is
literally what `release.yml` hands `veld-sign`, derived from the table you just
edited — which is why there is nothing to edit in the workflow, and why the
workflow and the source cannot disagree.

If something *is* wrong, a gate names the edit you missed. The ones you can hit:
a secret name the permanent roster does not carry; a table row this repo's parser
cannot read; `ORG_KEY_1` no longer first; two rows sharing a key or a secret; a
secret name without the `SIGNING_` prefix the leak gate matches on.

**7. Merge — and `feat:` or `fix:` in the squashed subject, or nothing ships.**
Merging **is** releasing here: semantic-release cuts the version from the commit
subject. A `chore:` or `docs:` subject publishes nothing, silently, with CI green,
and you will believe the key is deployed when it is not. Check the Releases page
before you go any further.

At the release, `veld-sign` prints one line per slot naming the public key it
signed with. That log is the record of which keys a published artifact is
verifiable under.

After this release:

- helpers still on the old release accept it via slot 0 (the original key), install
  it, and relaunch onto it — nothing is asked of any user;
- helpers on this release accept anything signed by either key;
- `veld doctor` on a machine that has taken it says which key verified — `signed by
  org key 2 of 2`, the row number in `ORG_KEYS`.

Nothing is retired yet, and nothing is safe yet. The leaked key still works. That
is the point of the step: it moves the *knowledge* of the new key onto machines,
using the only channel those machines trust.

### Release 2 — stop accepting the old key

**8. Wait.** Until you are willing to say the previous release has reached the
machines you care about. There is no telemetry and this is not a check you can
automate — see [Why retirement is a separate
release](#why-retirement-is-a-separate-release).

**9. Change the old key's `status`, in a second pull request.** One field:

```rust
    OrgKey {
        key: ORG_KEY_1,
        secret: "SIGNING_PRIVATE_KEY",
        added_after: "0.0.0",
        status: KeyStatus::Retired {
            // Cargo.toml's `version` right now — which, because release 1 has
            // shipped, is no longer what release 1's row says.
            retired_after: "<Cargo.toml's version>",
        },
    },
```

The row stays. Its slot keeps being produced — harmless, since anyone holding the
old key already holds it — and helpers on this release stop *accepting* it. Then
`cargo fmt --all`, and `cargo test --workspace`, which is green: **no test is
deleted here either.**

> **Never delete a retired key's GitHub secret.** A retired row still gets a slot,
> so `SIGNING_PRIVATE_KEY` is still read on every release *after* you retire it,
> forever. Deleting it — the instinctive tidy-up on the day a key leaked, which is
> the day you are reading this — makes every subsequent release die at the signing
> step, push-only, after merge. And **nothing can warn you**: secrets cannot be
> enumerated, which is the same limit the permanent roster exists to work around.
> The key is already leaked; leaving it in the secret store costs nothing, because
> what made it dangerous was helpers accepting it, and that is what you just
> stopped.

From here a leaked copy of the old key is no longer a way past the install gate or
the relaunch gate.

### The two-release rule, and its guard

A release that both adds a key and retires one costs something real, and it is worth
stating exactly, because the overstated version of this sentence is one a reader
dismisses. **Nothing is stranded and nobody needs a password.** Every helper still
*accepts* such a release: the retired key keeps its slot, because
`ORG_REQUIRED_SLOT_KEYS` is every row of the table, retired rows included.

What it does is inflict [the truncation
window](#the-truncation-window-and-why-it-is-fine) on the oldest machines in the
fleet, deliberately. A helper shipped up to v16.59.0 keeps only the first 64 bytes of
the `.sig` when it installs, so it writes the retired key's slot into its store and
comes up running a build whose keyring no longer holds that key: `restart` and
`shutdown` refused until the installer is re-run, updates still working, and healed
by the next release. Splitting the rotation removes the window completely, because
the adding release's own keyring still holds the old key — which is the whole content
of [Why retirement is a separate release](#why-retirement-is-a-separate-release).

The word "sudo" belongs to a different mistake: **deleting** a row rather than
retiring it. Then the slot stops being produced, a helper holding only that key has
nothing to verify, and there is no repair but a password. The guard below reports
those two cases separately.
`a_combined_release_is_a_truncation_window_not_a_stranding` in
`crates/veld-core/src/signing.rs` asserts all of it, so this paragraph cannot drift
back to the version that conflated them.

It is guarded twice, in two different places, because neither layer alone is
enough. Both are worth knowing, because they fail differently.

**Layer 1 — a plain unit test, no git, runs everywhere.**
`one_release_never_both_adds_a_key_and_retires_one` reads the two dates. Both halves
of the edit record `Cargo.toml`'s version at the time they were written, and
`Cargo.toml` holds the *previous* release until semantic-release bumps it after the
merge — so a combined edit written in one sitting puts the **same** version in both
fields, and the test fails. `added_after_increases_down_the_table` stops the obvious
way around it: a new row's date must be strictly later than the row above, which
refuses row 0's `0.0.0` and every earlier row's version, the two stale values
actually lying around to copy. This layer runs in `cargo test --workspace`, on your
machine, and on a direct push to `main`.

**Its hole, named rather than glossed:** if the branch's `Cargo.toml` moves between
the two edits — an unrelated release lands, you update the branch, then retire — the
two dates are both *honest* and *different*, neither equals the current version, and
layer 1 sees nothing. The tree cannot tell "added last release" from "added in this
pull request". Nothing written in the file can close that.

**Layer 2 — the pull request's own diff.** `ci.yml`'s *One release never both adds a
key and retires one* fails if this pull request's change to `signing.rs` both
introduces an `OrgKey` row and introduces a `KeyStatus::Retired`. That is exact, and
it is the layer that closes layer 1's hole. It reads the merge commit's own parents
(`HEAD^1` is the base GitHub merged against, `HEAD` the result), which the job's
`fetch-depth: 2` checkout already has — no fetch, and nothing from the event payload,
whose `base.sha` goes stale exactly when the base branch moves mid-review, which is
the situation this step is for. It does not run on a direct push to `main`, which is
precisely the case layer 1 does cover.

**What neither catches:** a value invented for `added_after` that is neither the
truth nor anything already written down — a version between the previous row's and
the current one. Nothing in the tree can prove a date honest, and a pull request that
deletes a row rather than retiring it is a [flag day](#the-flag-day-probably-never)
by another name. Closing those needs evidence only the release infrastructure can
mint; that is a follow-up, not something this file pretends to have.

### What is still not "just set a secret", and why

Two things, and neither is an oversight:

- **The `signing.rs` row.** A helper accepts a signature by a key **compiled into
  it**; the only way to teach it a new key is to ship a binary carrying that key.
  So the *signing* side can be entirely secret-driven and the *accepting* side is a
  source change by construction. `release.yml` could be made to derive the keyring
  from the secrets at build time — that is the tempting way to reach "that's it",
  and it moves the trust root out of git, where a reviewer can see which keys a
  release trusts, and into the CI secret store, where a compromised Actions run can
  mint a keyring. It was considered and rejected. The trust root is still in git.
- **The second release.** A machine that never installs release 1 lands in
  `SigTrust::RetiredOnly` — see [The truncation
  window](#the-truncation-window-and-why-it-is-fine). That is a property of
  software already in the field, not a policy choice.

## What you are working with

`<binary>.sig` is a list of **slots**, each a raw 64-byte ed25519 signature over
the whole binary:

```
byte 0                 64                 128                192
  | signature by key 1 | signature by key 2 | signature by key 3 | ...
```

A helper accepts a binary when **any** slot verifies under **any** key in
`ORG_SIGNING_KEYRING` — the list compiled into *that* helper. So one artifact can
satisfy several generations of helper at once, which is the only reason a
transition is possible at all.

Two facts about slot 0 that are not conventions but contracts:

- **Every helper shipped up to and including v16.59.0 reads exactly bytes `0..64`
  and verifies them against `ORG_KEY_1`.** Their code cannot be changed. Move
  that key off slot 0 and every one of those installs refuses the next release.
- **Those same helpers truncate.** Their install RPC held the signature in a
  `[u8; 64]`, so when one of them installs a multi-slot release it writes slot 0
  into the store and drops the rest. That is a fixed property of software already
  in the field; see [The truncation
  window](#the-truncation-window-and-why-it-is-fine).

**`ORG_KEYS` in `crates/veld-core/src/signing.rs` is the single table all of this
is read from.** One row per key, in slot order, each row carrying the public key,
the GitHub secret whose private half signs that slot, when the row was added, and
whether this build still accepts it. Everything else is a view of it:

- `ORG_SIGNING_KEYRING` — the rows this build accepts. The trust root, compiled in.
- `ORG_REQUIRED_SLOT_KEYS` — every row, in order. A release must carry a slot for
  each, accepted or not.
- `RELEASE_SIG_SLOTS` — `ORG_KEYS.len()`.
- what `release.yml` passes `veld-sign` — derived at build time by
  `tests/signing-slots.py`, which `ci.yml` runs on every pull request too.

It is a table of records rather than two lists of keys for a reason worth knowing:
two flat lists can only express *state*, so removing a key from a keyring destroyed
every trace that the key was ever there, and a release that both added and retired
one was undetectable in principle. A row that survives its own retirement is where
that evidence lives — see [The two-release rule, and its
guard](#the-two-release-rule-and-its-guard).

## Step 1 — generate the new key, off CI

Do this on a machine that is not the one that leaked, and do not put the private
half anywhere the compromised key was.

**On macOS, `/usr/bin/openssl` is LibreSSL and will refuse this** —
`Algorithm ed25519 not found`, measured against the LibreSSL 3.3.6 that Sequoia
ships. Install an OpenSSL 3 (`brew install openssl@3`) and call it by path, or do
this on Linux where `openssl` already is one. The commands below were run against
OpenSSL 3.6.3.

```sh
# Apple silicon: /opt/homebrew/opt/openssl@3/bin/openssl
# Intel Mac:     /usr/local/opt/openssl@3/bin/openssl   <- the LibreSSL trap bites
#                                                          hardest exactly here
# Linux:         openssl
OPENSSL=/opt/homebrew/opt/openssl@3/bin/openssl
"$OPENSSL" version                                # must say OpenSSL, not LibreSSL

"$OPENSSL" genpkey -algorithm ed25519 -out "$PWD/veld-signing-2.pem"

# The 32 raw public bytes as hex. This is what goes in the ORG_KEYS row, what
# veld-sign prints per slot, and what the derived --slots argument carries, so
# all of them must agree:
"$OPENSSL" pkey -in "$PWD/veld-signing-2.pem" -pubout -outform DER \
  | tail -c 32 | xxd -p -c 32

# `xxd` ships in vim-common and is missing on a minimal Debian. Same output, no
# dependency (verified to match byte for byte):
"$OPENSSL" pkey -in "$PWD/veld-signing-2.pem" -pubout -outform DER \
  | tail -c 32 | od -An -tx1 | tr -d ' \n'
```

The same 32 bytes in the form `signing.rs` wants, from that hex:

```sh
python3 -c "
import sys, textwrap
h = sys.argv[1]
print(chr(10).join(textwrap.wrap(', '.join(f'0x{h[i:i+2]}' for i in range(0, len(h), 2)), 96)))
" <the hex from above>
```

Check the key against the real tool before it goes anywhere near a secret store.
It costs one command, and it is the only way to know the PEM is a shape
`veld-sign` accepts and that the hex you are about to paste into `signing.rs`
matches the key you just made. **Run it from your veld checkout** — `-p veld-sign`
resolves through the workspace:

```sh
printf 'x' > /tmp/veld-probe.bin
# An ABSOLUTE path to the key: the step above may have generated it on another
# machine or in another directory, and this command has to be run from your veld
# checkout, which is not the same place.
cargo run -q -p veld-sign -- --key-file /absolute/path/to/veld-signing-2.pem \
  --expect-slot-pubkeys <the hex> /tmp/veld-probe.bin
```

(`--key-file` plus `--expect-slot-pubkeys` is the hand spelling. The release path
uses `--slots <hex>=<VAR>`, which is exactly one of each per entry — the
equivalence is pinned by `slots_is_the_same_signature_as_key_env_plus_expected_pubkeys`
in `crates/veld-sign/tests/smoke.rs`, so probing this way really does exercise the
path that ships.)

(The filename needs a `.` in it. `veld-sign` will not print a path segment that
could be a chunk of encoded key material, so a bare `/tmp/probe` comes back as
`<redacted: …>` in the success line — nothing is wrong, but it reads alarmingly at
2am, which is when you are reading this.)

It prints `slot 0: --key-file … -> <hex>`. That hex is authoritative: it is
derived from the private key by the same code that signs releases.

`veld-sign` accepts PKCS#8 PEM and nothing else — `-----BEGIN PRIVATE KEY-----`.
If you have an OpenSSH key, `ssh-keygen -p -m PKCS8 -f <key>` converts it; the
tool will tell you so if you get it wrong.

Add the private half as a **new** repository secret, taking the next free number
in the permanent roster: `SIGNING_PRIVATE_KEY_2`, then `_3`, up to `_8`. The
`SIGNING_` prefix is not decoration — `ci.yml`'s "no PR-startable job reaches a
signing secret" gate matches it, so every name in the roster is covered without an
edit, and a name without it is a secret nothing checks. `release.yml` already lists
all eight; you are filling one in, not adding one.

**Do not overwrite `SIGNING_PRIVATE_KEY`.** It holds the original key, which must
keep signing slot 0. Overwriting it is the single most expensive mistake available
here, and it is why the release checks each slot's expected public key: the release
fails rather than publishing an artifact no installed helper can accept.

## Step 2 — the release that adds the key

One PR, **one** edit, one release: append a row to `ORG_KEYS` in
`crates/veld-core/src/signing.rs`, then `cargo fmt --all`. The exact shape is in
[The whole rotation, in order](#the-whole-rotation-in-order) above; this section is
why it is only one edit and what each part of it is load-bearing for.

```rust
OrgKey {
    key: ORG_KEY_2,                        // the constant you added above the table
    secret: "SIGNING_PRIVATE_KEY_2",       // the secret from step 1
    added_after: "<Cargo.toml's version>", // read it; a pasted literal is refused
    status: KeyStatus::Accepted,
}
```

- **`key`'s position in the table is its slot.** Append; never reorder. Slot 0 is a
  contract with every already-shipped helper, and slot position is this format's
  only key identifier, so moving a row silently re-points a signature at a
  different key.
- **`secret` binds the key to the private half that signs its slot, in source.**
  This used to be implicit in the order of flags in `release.yml`, which is exactly
  the kind of positional coupling this format keeps producing bugs from. `ci.yml`
  checks every name here against the workflow's permanent `env:` roster on each
  pull request, so a name the roster does not carry fails a PR rather than the
  release.
- **`added_after` is `Cargo.toml`'s version right now** — see [The two-release
  rule, and its guard](#the-two-release-rule-and-its-guard).
- **`status`** is `Accepted` here. Retirement is release 2 and nothing else.

**Three things that used to be edits and are not any more.** `RELEASE_SIG_SLOTS`,
the `--key-env` flag and its `env:` entry, and the `--expect-slot-pubkeys` hex were
all transcriptions of what the table already said. `RELEASE_SIG_SLOTS` is
`ORG_KEYS.len()`; `release.yml` derives the whole slot layout with
`tests/signing-slots.py` and lists the eight secret names permanently. A
transcription in two files is a transcription that drifts, and the symptom of drift
here is a release no privileged install can accept.

**And no test is deleted.** There used to be one —
`no_rotation_has_happened_yet_so_the_artifact_is_unchanged` — that went red for a
*correct* rotation and whose own doc comment told you to delete it. Which meant
step 2 of a rotation was "delete a red test", rehearsed on the worst day available,
next to permanent invariants that look identical when the whole file is red. It has
been restated as `the_artifact_carries_exactly_one_slot_per_key`, which is true
before and after every rotation. **If a test in `signing.rs` is red, the answer is
never to delete it.**

`cargo test --workspace` should be green, and
`python3 tests/signing-slots.py` should print your new key as the last entry with
the original still first.

After this release:

- helpers still on the old release accept it via slot 0 (the old key), install it,
  and relaunch onto it — nothing is asked of any user;
- helpers on this release accept anything signed by either key.

Nothing is retired yet, and nothing is safe yet. The leaked key still works. That
is the point of the step: it moves the *knowledge* of the new key onto machines,
using the only channel those machines trust.

## Step 3 — the release that retires the old key

Wait until you are willing to say the previous release has reached the machines you
care about. Then, one field:

```rust
    status: KeyStatus::Retired {
        retired_after: "<Cargo.toml's version>", // read it; a pasted literal is refused
    },
```

That is the whole retirement. **The row stays** — releases keep carrying a slot 0
signed by the old key, which is harmless since anyone holding it already holds it —
and helpers on this release stop *accepting* it. `ORG_REQUIRED_SLOT_KEYS` is every
row of the table, so keeping the retired key's slot is not something you can forget;
it is unrepresentable to drop it without deleting the row, which is [the flag
day](#the-flag-day-probably-never) and not something a rotation does.

Then `cargo fmt --all` and `cargo test --workspace`, which is **green**. No test is
deleted here either. `nothing_is_retired_yet` used to fail at exactly this point,
with a comment telling you to delete it and nothing else in the file — a red test
whose remedy was deletion, on the day this document says you are reading it under
pressure. Its successor,
`one_release_never_both_adds_a_key_and_retires_one`, goes red when a retirement is
*unsafe* rather than whenever one happens, so it never needs deleting and it
actually catches something. See [The two-release rule, and its
guard](#the-two-release-rule-and-its-guard).

`the_keyring_is_never_empty` and `every_accepted_key_is_one_releases_are_signed_by`
sit beside it and must survive: the first is what catches a step 3 performed on a
tree where step 2 was skipped, reverted or mis-merged, which ships a helper that
trusts nothing — every candidate refused, every relaunch refused, `restart` and
`shutdown` refused, and `sudo` the only repair.

## Why retirement is a separate release

Because of the truncating installers. A single release that both introduced the
new key and retired the old one would be installed by an old helper — which keeps
only slot 0, the retired key's — and the machine would come up on a helper that
cannot verify its own store. Splitting it means the machine that crosses is
running a helper which accepts *both* keys, so its store is verifiable whichever
slot survived.

It also gives you a fleet you can observe between the two steps, which is the
only way to make the step-3 timing a decision rather than a guess.

## The truncation window, and why it is fine

A machine that skips step 2 entirely — it was off for a quarter, and updates
straight from a pre-rotation release to a post-retirement one — lands with the
right binary in its store beside a `.sig` holding only the retired slot. What
happens:

- **the helper refuses `restart` and `shutdown`** over its socket while in this
  state, because both gate on the relaunch guard against the on-disk binary — and
  that binary is the store's. Listed first because it is the one an operator
  actually hits; everything else below is something that keeps working.
- **updates keep working.** The incoming candidate's `.sig` comes from the
  tarball, complete, and is verified before anything is written.
- **`veld doctor` says so precisely**, naming the truncation rather than reporting
  tampering, and naming the repair below. No password.
- **the repair is re-running the installer**, not `veld update`:

  ```sh
  curl -fsSL https://veld.oss.life.li/get | bash
  ```

  **`veld update` does not work here, and that is not an oversight in this
  document — it was this document's first answer and it was wrong.** `veld update`
  resolves a target, finds nothing newer, and takes its "already on the latest
  version" branch without running the installer. A machine in this state is *by
  construction* on the latest release, because installing it is how it got there —
  so `veld update` is a no-op, and so is `--target-version` at the current
  version. Re-running the installer *does* re-drive the helper handoff at the
  current version, and the store's floor accepts an equal version, so the full
  slot list is written and the row goes green. No password either way.

  The command lives in the code once, as `signing::INSTALLER_COMMAND`, so the
  doctor row, the relaunch refusal and this document cannot drift apart.
- **nothing is accepted on the strength of a retired key.** `SigTrust::RetiredOnly`
  is a diagnosis, never a permission — and `is_org_binary`, the one place a retired
  key changes an outcome, additionally requires the file to sit under a root-owned
  chain, so the laxity is bounded by the property that makes it safe.

This is why the code distinguishes three outcomes rather than two: the honest
answer to "your root binary is signed with a key we no longer accept" is
different from "your root binary is not ours", and giving the second answer to
the first situation sends somebody hunting a compromise that did not happen.

## What rotation does not do

State these plainly rather than discovering them under pressure.

- **It does not make the leaked key useless — only insufficient.** Slot 0 keeps
  being produced, so a pre-rotation helper keeps trusting the old key. Nothing
  can change that: their code is shipped.
- **It does not rescue a machine the attacker already reached.** Anyone holding
  the leaked key plus local access on a machine still trusting it can install a
  binary of their choosing and is root there. No later release runs on that
  machine, because the code that would run is theirs.
- **It cannot be hurried past step 2.** The transition needs one release to land
  before the next is safe, and that is a property of software already in the
  field, not a policy choice.

What it *does* buy is the thing worth having: for every machine the attacker has
not reached, the barrier between "the logged-in user" and "root" — which is the
signature, and nothing else, because the socket's uid gate admits that user by
design — is restored.

## Three designs somebody will propose mid-incident

The full §0 argument lives in the PR that added this mechanism. These three are
here because they are the ones that get proposed *during* an incident, when the
PR is not what anybody is reading.

- **"Require *every* keyring key to have a verifying slot"** (all-of-N instead of
  any-of-N). It sounds strictly stronger — one leaked key would stop being
  sufficient rather than merely retirable. It makes retiring a compromised key
  **inexpressible**: the retired key is exactly the one you can no longer demand a
  signature from, and demanding it is what a pre-rotation helper does. It also
  turns the loss of any single private half into a permanent `sudo`-only state for
  every generation that required it. Any-of-N is not a weaker choice; it is the
  only one that can express a retirement at all.
- **"Persist the keyring to disk so a helper can learn a key without a new
  binary."** This is what makes a successor pre-commitment necessary, because it
  creates the state where an attacker captures the trust root without running
  code. Trust riding the binary means adopting a key *is* executing the binary
  that carries it, and it means rotation inherits its replay protection from the
  version floor instead of needing its own.
- **"Publish `H(next_key)` in every release so the successor is pinned in
  advance."** Defends the state the previous bullet creates and this design does
  not have. Its cost is a second private key that must exist now, survive
  untouched for years, and never be lost — and if it is lost, rotation becomes
  impossible, which is the wedge the whole chain exists to prevent.

## The flag day (probably never)

Dropping a key from `ORG_REQUIRED_SLOT_KEYS` stops producing its slot. Any helper
generation whose keyring holds only that key then refuses every future release,
and its repair needs `sudo`. That breaks #338's rule 1, so it is not something a
rotation does. It is a separate, announced decision, taken when you can show the
population still on those releases is empty — and until you can, keep signing
slot 0.

## Things that will bite

- **`release.yml`'s signing step is push-only.** Nothing in it runs on a pull
  request. Everything that *can* be checked at PR time is: every secret `ORG_KEYS`
  names having an entry in that step's permanent `env:` roster; that roster
  covering all `MAX_SIG_SLOTS` names, so a rotation never has to touch the workflow;
  that the step still *derives* its slot layout and still passes `--slots`, rather
  than hard-coding something; `ORG_KEY_1` still first; no two rows sharing a key or
  a secret; every secret name carrying the `SIGNING_` prefix the leak gate matches
  on; `veld-sign`'s slot ceiling against `veld-core`'s; and the existence of each
  rotation test in `crates/veld-sign/tests/smoke.rs` and of each rotation invariant
  in `crates/veld-core/src/signing.rs`. If you add a release-time guard, pin it
  there too.
- **The two-release rule now has a guard — two layers of one — and it is worth
  knowing which layer covers what.** It used to be ungated entirely, and after the
  first retirement nothing at all would have caught it. Layer 1 is
  `one_release_never_both_adds_a_key_and_retires_one`, a plain test over the two
  date fields: no git, runs locally and on a direct push to `main`, catches the
  combined edit written in one sitting. Layer 2 is `ci.yml`'s *One release never
  both adds a key and retires one*, which reads the pull request's own diff (the
  merge commit against its first parent) and catches the case layer 1 structurally
  cannot — a branch whose `Cargo.toml` moved
  between the two edits, where both dates are honest. Neither catches an *invented*
  `added_after`. Full account: [The two-release rule, and its
  guard](#the-two-release-rule-and-its-guard).
- **`veld doctor` judges by the CLI's key list, not the helper's, and after a
  rotation those can disagree.** They ship together so they normally match, but an
  update whose helper half was refused — or an `install.sh` run with `VELD_VERSION`
  pinned older, which reinstalls the CLI unconditionally while the store's version
  floor keeps the newer helper — leaves them skewed. Two directions matter, and
  **it is not safe in all of them**, which an earlier draft of this file claimed:
  - **CLI newer than the helper**: the row reports the truncation state
    correctly and its remedy works, but the *running* (older) helper is not
    actually refusing anything, because it still trusts the retired key. Right
    remedy, overstated symptom. The row now says "THIS veld (version)" and makes
    the helper's behaviour conditional rather than asserted.
  - **CLI older than the helper**: a pre-rotation CLI still trusts the retired key,
    so in the truncation state it prints a **green** "Helper binary signature OK"
    while the running post-retirement helper genuinely is refusing `restart` and
    `shutdown`. A false green, with no flag day involved. If you are diagnosing a
    rotation, check `veld version` against the helper's version in `veld doctor`'s
    Installation block before trusting this row.
- **`veld-sign` writes nothing unless every key parses.** A half-written `.sig`
  would verify for one generation and no other, which no reader can detect, so it
  fails before the first byte lands. The same is true of a secret that is simply
  not set: a key `ORG_KEYS` names and CI cannot read fails the release, naming the
  variable. That is the loud half of the design — the source decides which slots
  exist, and the secrets must be there to fill them.
- **A secret set under a name `ORG_KEYS` does not name is simply unused**, so
  uploading the new key before the source change merges cannot break an unrelated
  release in between. Nothing warns about it either, so a typo in the secret's name
  surfaces as "the release could not read `SIGNING_PRIVATE_KEY_2`", not as "you set
  `SIGNING_PRIVATE_KEY_5`".
- **The converse is the dangerous one: a retired key's secret is still read.** Its
  row still gets a slot. Deleting the secret after retiring the key breaks every
  release from then on, at the push-only signing step, and nothing can catch it at
  pull-request time because secrets cannot be enumerated.
- **The same key in two slots is refused.** It would be a release claiming to
  cover two generations while covering one.
- **No error from `veld-sign` may contain key material, and a second key doubles
  that surface.** Read the module doc in `crates/veld-sign/src/main.rs` before
  touching any message there; it enumerates every way this broke and names the
  guard for each. (Deliberately not repeated as a number here — a count in two
  places is a count that drifts, which is exactly how this line came to
  disagree with that doc during review.) The `--expect-slot-pubkeys` value comes from `argv` and is
  never echoed, because a 64-character hex argument cannot be told apart from a
  hex-encoded private seed.

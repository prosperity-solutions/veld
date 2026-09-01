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

## The 60-second version

```sh
# 1. Generate the new key, air-gapped, and add its private half as a NEW secret.
#    macOS's /usr/bin/openssl is LibreSSL and CANNOT do this — see step 1.
openssl genpkey -algorithm ed25519 -out veld-signing-2.pem
openssl pkey -in veld-signing-2.pem -pubout -outform DER | tail -c 32 | xxd -p -c 32

# 2. ONE release that ADDS it — FOUR edits:
#    - crates/veld-core/src/signing.rs: add ORG_KEY_2, put it in BOTH
#      ORG_SIGNING_KEYRING and ORG_REQUIRED_SLOT_KEYS, bump RELEASE_SIG_SLOTS to 2
#    - .github/workflows/release.yml: a second --key-env line BELOW the first
#    - .github/workflows/release.yml: the matching entry in that step's env:
#      block. Easy to miss; veld-sign reads a key by variable NAME, so a flag
#      with no env: entry looks exactly like an unset secret.
#    - .github/workflows/release.yml: APPEND the new public key hex to
#      --expect-slot-pubkeys, comma-separated, in slot order. Not optional:
#      it is what stops a secret holding the wrong key shipping invisibly.
#    Then: cargo fmt --all, and delete ONE test —
#    no_rotation_has_happened_yet_so_the_artifact_is_unchanged — and nothing
#    else. Step 2 below says why.
#
# 3. Wait for that release to reach the fleet. Then a SECOND release that RETIRES
#    the old key: remove ORG_KEY_1 from ORG_SIGNING_KEYRING only. It stays in
#    ORG_REQUIRED_SLOT_KEYS and keeps its slot.
```

Two releases. Never one. The reason is in [Why retirement is a separate
release](#why-retirement-is-a-separate-release).

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

`RELEASE_SIG_SLOTS` in `crates/veld-core/src/signing.rs` is the number of slots a
release must carry. `ci.yml`'s `schema` job fails a **pull request** if it stops
matching the number of key flags in `release.yml`, so the two cannot drift.

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

# The 32 raw public bytes as hex. This is the value --expect-slot-pubkeys takes
# and the value veld-sign prints per slot, so all three must agree:
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

(The filename needs a `.` in it. `veld-sign` will not print a path segment that
could be a chunk of encoded key material, so a bare `/tmp/probe` comes back as
`<redacted: …>` in the success line — nothing is wrong, but it reads alarmingly at
2am, which is when you are reading this.)

It prints `slot 0: --key-file … -> <hex>`. That hex is authoritative: it is
derived from the private key by the same code that signs releases.

`veld-sign` accepts PKCS#8 PEM and nothing else — `-----BEGIN PRIVATE KEY-----`.
If you have an OpenSSH key, `ssh-keygen -p -m PKCS8 -f <key>` converts it; the
tool will tell you so if you get it wrong.

Add the private half as a **new** repository secret. Name it with the
`SIGNING_` prefix — `SIGNING_PRIVATE_KEY_2` — because `ci.yml`'s
"no PR-startable job reaches a signing secret" gate matches that prefix and will
therefore cover the new secret automatically. A name without it is a secret
nothing checks.

**Do not overwrite `SIGNING_PRIVATE_KEY`.** The old key must keep signing slot 0.
Overwriting it is the single most expensive mistake available here, and it is why
`release.yml` passes `--expect-slot-pubkeys`: the release fails rather than
publishing an artifact no installed helper can accept.

## Step 2 — the release that adds the key

One PR, **four** edits, one release:

1. `crates/veld-core/src/signing.rs`
   - add `pub const ORG_KEY_2: PubKey = [ ... ];` with the hex from step 1
   - add it to `ORG_SIGNING_KEYRING` **and** to `ORG_REQUIRED_SLOT_KEYS`
   - bump `RELEASE_SIG_SLOTS` to `2`
2. `.github/workflows/release.yml`, in `Package client binaries`: add
   `--key-env SIGNING_PRIVATE_KEY_2 \` **below** the existing `--key-env` line.
   Order is slot order. Below, never above.
3. `.github/workflows/release.yml`, in the **same step's `env:` block**: add
   `SIGNING_PRIVATE_KEY_2: ${{ secrets.SIGNING_PRIVATE_KEY_2 }}`.

   **This is the edit that gets forgotten**, and the symptom is the release dying
   at the signing step *after* the PR has merged. GitHub does not expose
   repository secrets as environment variables on their own, and `veld-sign` reads
   a key by variable **name** — so a `--key-env` with no matching `env:` entry is
   indistinguishable from a secret that never arrived. `ci.yml`'s `schema` job
   fails a pull request for exactly this, and for a secret whose name does not
   carry the `SIGNING_` prefix the leak gate matches on.

4. `.github/workflows/release.yml`, on the `--expect-slot-pubkeys` line:
   **append the new key's hex**, comma-separated, in slot order —
   `--expect-slot-pubkeys <ORG_KEY_1 hex>,<ORG_KEY_2 hex>`.

   This is not optional and it is not cosmetic. That flag is the only thing that
   catches a signing secret holding a valid key which is *not* the one you pasted
   into the keyring — and for a **later** slot, that mistake ships invisibly,
   because every already-shipped helper still verifies the release through slot 0.
   It would then wedge every privileged install one release later, when that key
   becomes the only one a helper accepts: fleet-wide, sudo-only. `ci.yml` compares
   this list element-wise against `ORG_REQUIRED_SLOT_KEYS`, so leaving it at one
   entry fails the pull request with `1 declared vs 2 required`.

   Order matters: slot position is this format's only key identifier, and the gate
   checks the order too.

Then two housekeeping steps, both of which a simulated run of this procedure
found the hard way:

- **`cargo fmt --all`.** A hand-written 32-byte array will not match rustfmt, and
  CI's `fmt --check` is a hard failure.
- **Delete `no_rotation_has_happened_yet_so_the_artifact_is_unchanged`** from
  `crates/veld-core/src/signing.rs`, and nothing else. It asserts that releases are
  still byte-identical to the pre-rotation shape, which is exactly what you have
  just stopped being true; it goes red for a correct change. It sits alone in its
  own function for that reason. `a_one_key_signature_is_exactly_64_bytes` beside it
  is a permanent invariant about the format and must survive.

`cargo test -p veld-core -p veld-sign` should then be green, and `ci.yml`'s
slot-layout gate should report `signature slots: 2`.

After this release:

- helpers still on the old release accept it via slot 0 (old key), install it,
  and relaunch onto it — nothing is asked of any user;
- helpers on this release accept anything signed by either key.

Nothing is retired yet, and nothing is safe yet. The leaked key still works. That
is the point of the step: it moves the *knowledge* of the new key onto machines,
using the only channel those machines trust.

## Step 3 — the release that retires the old key

Wait until you are willing to say the previous release has reached the machines
you care about. Then, one edit:

- `crates/veld-core/src/signing.rs`: remove `ORG_KEY_1` from
  `ORG_SIGNING_KEYRING`. **Leave it in `ORG_REQUIRED_SLOT_KEYS`, and leave
  `RELEASE_SIG_SLOTS` at 2.**

That is the whole retirement. Releases keep carrying a slot 0 signed by the old
key — which is harmless, since anyone holding it already holds it — and helpers
on this release stop *accepting* it. From here a leaked copy of the old key is no
longer a way past the install gate or the relaunch gate.

Then `cargo fmt --all` again — removing an element can reflow the list — and
`cargo test -p veld-core -p veld-sign`, which should be green apart from the one
test named below.

`nothing_is_retired_yet` in `signing.rs` fails as soon as you do this, with a
message pointing back here. That is deliberate. **Delete that one test function
and nothing else in the file.**

It is alone in its own function precisely so that this instruction is safe.
`the_keyring_is_never_empty` and `every_accepted_key_is_one_releases_are_signed_by`
sit beside it and must survive: the first is what catches a step 3 performed on a
tree where step 2 was skipped, reverted or mis-merged, which ships a helper that
trusts nothing — every candidate refused, every relaunch refused, `restart` and
`shutdown` refused, and `sudo` the only repair. All three used to be one test, and
deleting a red test is the natural reaction on the day this document says you are
reading it under pressure.

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
  request. Everything that *can* be checked at PR time is:
  `ci.yml`'s `schema` job pins the slot count against `RELEASE_SIG_SLOTS`; that
  every `--key-env NAME` is written with a space and has a matching entry in that
  step's `env:` block; that every such NAME carries the `SIGNING_` prefix the leak
  gate matches on; **every** slot's expected key, element-wise and in order,
  against `ORG_REQUIRED_SLOT_KEYS`, plus that `ORG_KEY_1` is still first;
  `veld-sign`'s slot ceiling against `veld-core`'s; and the existence of each
  rotation test in `crates/veld-sign/tests/smoke.rs` and of each rotation invariant
  in `crates/veld-core/src/signing.rs`. If you add a release-time guard, pin it
  there too.
- **One thing is *not* gated, deliberately, and it is the rule this document
  repeats most.** Nothing fails a pull request that both adds a key to
  `ORG_SIGNING_KEYRING` and removes one in the same release — that is a property of
  the *diff*, not of the tree, so checking it needs the merge base in a job that is
  otherwise static checkout-only validation. Until the first retirement,
  `nothing_is_retired_yet` catches it. **After that, review is the only thing
  standing between you and it**, so if you are the reviewer of a rotation PR, this
  is the line to check.
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
  fails before the first byte lands.
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

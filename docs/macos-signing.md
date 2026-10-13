# macOS signing: Developer ID and notarization

Every macOS part of a veld release is signed with the team's **Developer ID
Application** certificate in release CI. The desktop app and its disk image are
also notarized and stapled. This document is the operating manual: first-time
setup, renewal, what to do when a release fails on signing, and why it is built
the way it is.

## Why: a stable code identity

The Gatekeeper prompt is the visible part, but the real reason is identity. macOS
remembers a program by its **designated requirement** (DR). For a Developer ID
signature the DR is:

```
identifier "dev.veld.daemon" and anchor apple generic and certificate leaf[subject.OU] = "<TEAM ID>" and …
```

TCC grants (Full Disk Access, Accessibility, Automation), Keychain ACLs, Login
Items and Background Items, and Squirrel.Mac all match a new version against the
old one's DR. An ad-hoc signature's DR is its own hash, so before this, macOS
treated every update as a new program.

The DR pins the **identifier and the team**, not the certificate. So renewing the
certificate every five years, or replacing a leaked one, keeps every install's
identity. `scripts/release/macos-codesign.sh` refuses a signature whose DR pins a
hash instead.

| Part | Identifier | Hardened runtime | Notarized |
|---|---|---|---|
| Veld.app | `dev.veld.desktop` | yes (entitlements in `desktop/build/`) | yes, stapled |
| `veld-desktop-*.dmg` | `dev.veld.desktop.dmg` | n/a | yes, stapled |
| `veld` | `dev.veld.cli` | **no**, see below | no |
| `veld-daemon` | `dev.veld.daemon` | **no**, see below | no |
| `veld-helper` | `dev.veld.helper` | yes | no |
| `caddy` | `dev.veld.caddy` | yes | no |
| `veld-gateway` | `dev.veld.gateway` | yes | no |

**Never change an identifier.** Changing one costs every user every grant made to
that program.

**Why `veld` and `veld-daemon` run without the hardened runtime.** With the
hardened runtime, macOS strips every `DYLD_*` variable from the environment of a
signed process's children. These two start the user's own dev servers, and a
toolchain that depends on `DYLD_LIBRARY_PATH` (Oracle Instant Client, some Python
and Ruby builds) would break with no error message. The PR smoke job
(`tests/macos-signing-smoke.sh`) asserts on every PR that `DYLD_*` survives a
`--no-runtime` parent. GitHub's runners have System Integrity Protection off, and
dyld strips only while it is on, so there the job checks the hardened half by
its signature flag; with SIP on it checks the stripping itself.

**Why the CLI binaries are not notarized.** Gatekeeper checks notarization only
for files that carry `com.apple.quarantine`. The installer fetches them with curl,
which never sets that flag. Notarization also does not affect identity. They can
be notarized later without changing anything above. The one browser path is a
`veld-gateway` tarball an operator downloads from the release page: Gatekeeper
blocks that until `xattr -d com.apple.quarantine veld-gateway`, and the README
says so.

## First-time setup (maintainer, ~15 minutes)

You need:

- a Mac with the Xcode command line tools (`xcode-select --install`);
- `gh` logged in as an **admin** of this repository;
- the Apple Developer **Account Holder** login (only that role can create
  Developer ID certificates) and **Admin** in App Store Connect (to create the
  API key) — or someone who has them, see below.

Run this from the veld checkout:

```sh
scripts/release/apple-signing-setup.sh
```

The script opens each web page, tells you exactly what to click and waits for
the download. You do two things by hand:

1. **developer.apple.com → Certificates → +** → *Developer ID Application* →
   Profile type *G2 Sub-CA* → upload the `devid.csr` the script made →
   *Download*.
2. **App Store Connect → Users and Access → Integrations → Team Keys → ⊕ next
   to "Active (N)"** →
   name *veld notarization*, access *Developer* → *Generate* → *Download* → copy
   the *Issuer ID*.

The script does the rest:

- checks the certificate against its private key and Apple's G2 intermediate;
- reads the Team ID from the certificate, so nobody types it;
- signs and **notarizes a test binary on your Mac** with the same scripts release
  CI runs;
- creates the GitHub environment `release`, restricted to deploy from `main`
  only;
- stores the secrets and the variable in that environment.

**Not the Account Holder?** Answer *no* when the script asks. It writes
`~/Desktop/veld-signing-request/` with plain `INSTRUCTIONS.txt` (and `devid.csr`)
and a message to paste. Send that folder to the Account Holder. The certificate
request and the certificate they send back are public (email or Slack is fine).
The notary key is a secret and should come back through the password manager.
The private key never leaves your Mac. Save what comes back to `~/Downloads` and
re-run the script.

If it stops at any point, re-run it. Everything it made is kept in
`~/.veld-apple-signing`, so it resumes where it stopped.

When it finishes, **back up `~/.veld-apple-signing`** to the team password
manager. With that backup, the next maintainer can re-upload instead of redoing
the Apple steps.

Check the state at any time without changing anything:

```sh
scripts/release/apple-signing-setup.sh --check
```

### What lives where

Everything is in GitHub environment `release`, which only `main` can deploy to.
Pull-request jobs cannot reach it, and CI's workflow gate fails any job that
names it without a push guard.

| Name | Kind | What |
|---|---|---|
| `APPLE_DEVID_P12` | secret | base64 `.p12`: certificate, private key and Apple's Developer ID G2 intermediate |
| `APPLE_DEVID_P12_PASSWORD` | secret | its random password |
| `APPLE_NOTARY_KEY_P8` | secret | App Store Connect team API key |
| `APPLE_NOTARY_KEY_ID` | secret | its key id |
| `APPLE_NOTARY_ISSUER_ID` | secret | the team's issuer id |
| `APPLE_TEAM_ID` | variable | 10-character team id, read from the certificate |

## The first signed release: what users see

The first signed release follows releases that were ad-hoc signed, so it changes
every install's identity once. Expect, once per machine:

- **Login Items / Background Items:** macOS posts a "Background item added"
  notice naming the team. The old ad-hoc entry may linger, disabled, in
  **System Settings → General → Login Items**.
- **Privacy & Security** grants made to veld (Full Disk Access, Accessibility,
  Automation) may need to be re-enabled, or toggled off and on.

From the next release on, nothing changes. Put a line about this in the release
notes for the first signed release.

Developer builds (`just dev-install`, `just desktop-package`) stay ad-hoc, which is a
different identity from a release. A machine that runs both gets two permission
entries. That is expected.

## Renewing the certificate

Developer ID certificates last five years. Release CI starts **failing 30 days
before expiry** (`macos-keychain.sh` checks), and its error message names the
command to run:

```sh
scripts/release/apple-signing-setup.sh --renew
```

This archives the old certificate files, walks you through step 1 above again,
proves the new certificate locally and uploads it. The notary key is kept. The
identity of every install is unchanged.

**Do not revoke the old certificate unless it leaked.** Revoking does not break
software that was already signed and timestamped with it. There is simply no
reason to revoke it.

### Never run `fastlane match nuke developer_id`

The team's iOS app (the Capacitor shell) manages its certificates with fastlane
match, and its README says to run `fastlane match nuke …` once a year. `nuke`
revokes **every** certificate of that type on the Apple account, not only the
ones match stored. For `development` and `appstore` that is harmless to veld.
For `developer_id` it would revoke veld's certificate, and Gatekeeper would then
reject new downloads of every release signed with it. veld's certificate is not
stored in match, so nothing in that workflow needs `developer_id`.

## If the certificate or notary key leaks

1. Run `--renew` (for the certificate). For the key, delete
   `~/.veld-apple-signing/AuthKey.p8`, `notary_key_id` and `notary_issuer_id`,
   then run the script again.
2. Revoke the leaked certificate at developer.apple.com, or the leaked key in App
   Store Connect. Revoking a Developer ID certificate makes Gatekeeper reject
   *new downloads* of software signed after the revocation date Apple records.
   Ask Apple to set that date to the leak date, not earlier.
3. Cut a release.

## When a release fails on signing

Release CI **fails closed**. If any of the material is missing or wrong, no macOS
artifact is produced, and nothing falls back to ad-hoc. A single ad-hoc release
between two signed ones would change every install's identity twice.

| Error says | Do |
|---|---|
| `APPLE_DEVID_P12 is not set` | Setup never ran or the environment is wrong: `apple-signing-setup.sh --check` |
| `expected exactly one valid 'Developer ID Application: … (TEAM)' identity, found 0` | Wrong team in `APPLE_TEAM_ID`, or the `.p12` is broken. Re-run the setup script; it re-uploads |
| `expires … less than 30 days left` | `apple-signing-setup.sh --renew` |
| `codesign failed 3 times` | Usually Apple's timestamp server. Re-run the job |
| desktop job: electron-builder signing error mentioning `timestamp` | Same cause; electron-builder does not retry. Re-run failed jobs |
| desktop job cancelled at its 90-minute timeout | Apple can hold the first notarization of a new app for over an hour. Re-run failed jobs; later submissions are fast |
| notarization `Invalid` | The job prints `notarytool log`. Usually a binary inside the app missing the hardened runtime or a timestamp |
| notarization `401`/`403` | Notary key revoked or the issuer id is wrong. Delete the key files in `~/.veld-apple-signing` and re-run the setup script |

## How it is wired

- `scripts/release/macos-keychain.sh` creates a throwaway keychain, imports the
  `.p12` with a non-extractable private key, picks the identity **by team id**
  and prints its SHA-1. It also fails 30 days before expiry.
- `scripts/release/macos-codesign.sh` signs one file with a pinned identifier,
  then asserts the identifier, team, timestamp, runtime flag and DR.
- `release.yml` → `build` (darwin legs) checks the certificate before compiling,
  then deletes the keychain again. After the last build it imports it once
  more, signs `veld`, `veld-daemon`, `veld-helper`, `caddy` and `veld-gateway`
  in one step, and deletes it in that same step. So no `npm ci`, build script or
  proc-macro can call codesign as the team while it runs. This narrows
  exposure; it is not isolation. Code that ran earlier in the job can still
  tamper with the checkout or the environment the signing step uses, which
  has the `.p12` in it (the org signing key in the same job already has that
  exposure). Signing in a separate job on a fresh runner is the follow-up that
  closes it. This happens before
  `veld-sign` makes the org `.sig`, so the `.sig` covers the signed bytes.
- `release.yml` → `desktop` (macos) passes `-c.mac.identity=$APPLE_TEAM_ID` to
  electron-builder. electron-builder signs and notarizes the app; then the job
  signs, notarizes and staples each dmg itself, because electron-builder's dmg
  signing adds no timestamp. Before uploading, it asserts every zip and dmg:
  identifier, team, timestamp, runtime, stapled ticket and `spctl`.
- `desktop/electron-builder.yml` keeps `identity: null`, so local and PR builds
  stay ad-hoc and never depend on the building machine's keychain.
- `ci.yml` → `apple-signing-smoke` runs both scripts on every PR with a
  throwaway self-signed certificate. The workflow gate checks that release CI
  never sets the smoke-only `VELD_CODESIGN_SELFTEST`.
- `install.sh` never re-signs a binary whose signature verifies. If a Developer
  ID binary fails verification and has to be re-signed ad-hoc, it warns: that
  permissions will reset, or, for a binary that carries an org `.sig` (the
  helper), that the helper will not restart onto it.

## Not done yet

- **Squirrel.Mac self-install** (`MACOS_SIGNED` in `desktop/src/updatePolicy.js`)
  stays off. Installs running an ad-hoc build cannot accept a signed update
  through Squirrel. Flip it once signed releases are what people run.
- **`disable-library-validation`** is in the app's entitlements only because it
  is electron-builder's default and removing it can be validated only by
  launching a Developer ID build (`desktop/build/entitlements.mac.plist`). Try
  dropping it once a signed release exists to test against.

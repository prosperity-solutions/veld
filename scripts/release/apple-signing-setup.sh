#!/usr/bin/env bash
#
# One-time (and once-every-five-years) setup for Developer ID signing of veld's
# macOS release artifacts. Walks a maintainer through the two things only a human
# with the Apple account can click, does everything else itself, proves the result
# works on this Mac, and only then uploads it to GitHub.
#
# Usage:
#   scripts/release/apple-signing-setup.sh            # set up; safe to re-run, resumes
#   scripts/release/apple-signing-setup.sh --check    # report status, change nothing
#   scripts/release/apple-signing-setup.sh --renew    # new certificate (expiry, leak),
#                                                     # same notary key, same identity
#   scripts/release/apple-signing-setup.sh --p12 FILE # use a Developer ID Application
#                                                     # certificate the team already has,
#                                                     # exported from Keychain Access
#
# A new certificate and an existing one give every install the same identity —
# macOS matches the team, not the certificate — so --p12 only saves the Apple
# click. It needs the *private key*, which exists only on the Mac that created
# the certificate: developer.apple.com lets you re-download a certificate, never
# its key.
#
# What it produces, in GitHub environment `release` (deploys from `main` only):
#   secrets   APPLE_DEVID_P12           base64 .p12: certificate + key + Apple's G2 intermediate
#             APPLE_DEVID_P12_PASSWORD  its random password
#             APPLE_NOTARY_KEY_P8       App Store Connect team API key (.p8 text)
#             APPLE_NOTARY_KEY_ID       its key id
#             APPLE_NOTARY_ISSUER_ID    the team's issuer id
#   variable  APPLE_TEAM_ID             read out of the certificate, never typed
#
# Everything it makes is kept in ~/.veld-apple-signing (mode 700) so a re-run
# resumes where the last one stopped. Back that folder up to the team password
# manager when it finishes; the script says so.
#
# Requires: macOS, Xcode command line tools (`xcode-select --install`), `gh`
# logged in as a repo admin, and the Apple Developer **Account Holder** login —
# only that role may create Developer ID certificates.
#
# Design notes for whoever edits this next:
#   * The local proof uses the SAME scripts release CI runs
#     (macos-keychain.sh, macos-codesign.sh), against the real certificate, and
#     then a real notarization. So by the time anything is uploaded, the exact
#     import and signing CI will do has already succeeded once.
#   * /usr/bin/openssl (LibreSSL) on purpose, never one from PATH: Homebrew's
#     OpenSSL 3 writes AES-encrypted .p12 files that `security import` on older
#     macOS rejects, which would fail in CI only.
#   * Secrets reach `gh` on stdin, and openssl reads passwords from the
#     environment, never from a command line (visible in `ps`). The one
#     exception is `security import -P` inside macos-keychain.sh, which has no
#     other way to take the .p12 password.
set -euo pipefail

STATE="${VELD_APPLE_SIGNING_DIR:-$HOME/.veld-apple-signing}"
DOWNLOADS="$HOME/Downloads"
OPENSSL=/usr/bin/openssl
G2_URL="https://www.apple.com/certificateauthority/DeveloperIDG2CA.cer"
CERT_PAGE="https://developer.apple.com/account/resources/certificates/add"
KEYS_PAGE="https://appstoreconnect.apple.com/access/integrations/api"
ENV_NAME=release
here=$(cd "$(dirname "$0")" && pwd)
# From anywhere: `gh` works out the repository from the checkout it runs in.
cd "$(git -C "$here" rev-parse --show-toplevel)"

# ── output ─────────────────────────────────────────────────────────────────
if [ -t 1 ]; then
  B=$'\033[1m' G=$'\033[32m' Y=$'\033[33m' R=$'\033[31m' N=$'\033[0m'
else
  B='' G='' Y='' R='' N=''
fi
step() { printf '\n%s━━ %s%s\n' "$B" "$*" "$N"; }
ok() { printf '  %s✓%s %s\n' "$G" "$N" "$*"; }
todo() { printf '  %s→%s %s\n' "$Y" "$N" "$*"; }
die() { printf '\n%s✗ %s%s\n' "$R" "$*" "$N" >&2; exit 1; }
press_enter() { printf '\n  %s[press Enter when done]%s ' "$B" "$N"; read -r _; }
# A file's contents with the trailing newline gone. Secrets go to `gh` through
# this: `gh secret set` stores stdin byte for byte, and a password or key id
# with a stray "\n" on the end is a different value in CI than it was here.
chomp() { printf '%s' "$(cat "$1")"; }
yes_no() {
  local answer
  printf '  %s [y/N] ' "$1"
  read -r answer
  case "$answer" in y|Y|yes|YES) return 0 ;; *) return 1 ;; esac
}

# Wait for a file matching a glob in ~/Downloads that is newer than $2, or for
# the user to paste a path. Prints the path.
wait_for_download() {
  local glob=$1 newer_than=$2 what=$3 line f
  printf '  waiting for %s in ~/Downloads … (or drag the file here and press Enter)\n' "$what" >&2
  local best
  while :; do
    # The newest match: a rejected earlier download may still be lying there.
    best=
    for f in "$DOWNLOADS"/$glob; do
      if [ -f "$f" ] && [ "$f" -nt "$newer_than" ] && { [ -z "$best" ] || [ "$f" -nt "$best" ]; }; then
        best=$f
      fi
    done
    if [ -n "$best" ]; then
      printf '%s\n' "$best"
      return
    fi
    if read -r -t 2 line; then
      line=${line%"${line##*[![:space:]]}"}
      line=${line#\'}; line=${line%\'}; line=${line//\\ / }
      if [ -f "$line" ]; then
        printf '%s\n' "$line"
        return
      fi
      [ -z "$line" ] || printf '  not a file: %s — try again\n' "$line" >&2
    fi
  done
}

# ── the facts each stage leaves behind ──────────────────────────────────────
key_file="$STATE/devid.key"
csr_file="$STATE/devid.csr"
cer_file="$STATE/devid.cer"
leaf_pem="$STATE/devid.pem"
g2_pem="$STATE/DeveloperIDG2CA.pem"
p12_file="$STATE/devid.p12"
p12_pass_file="$STATE/devid.p12.password"
team_file="$STATE/team_id"
p8_file="$STATE/AuthKey.p8"
key_id_file="$STATE/notary_key_id"
issuer_file="$STATE/notary_issuer_id"
verified_file="$STATE/verified"
who_file="$STATE/who"             # cert=self|other key=self|other
notary_marker="$STATE/.notary-started"
HANDOFF="${VELD_APPLE_SIGNING_HANDOFF:-$HOME/Desktop/veld-signing-request}"

team_id() { cat "$team_file"; }
# RFC 2253 form without the "subject= " prefix. Values may contain escaped
# commas ("O=Foo\, Inc."), so fields are picked with patterns, not by splitting.
dn() { "$OPENSSL" x509 -in "$2" -noout "-$1" -nameopt RFC2253 2>/dev/null | sed 's/^[a-z]*= *//'; }
cert_subject() { dn subject "$leaf_pem"; }
cert_cn() { cert_subject | sed -n 's/.*CN=\(Developer ID Application: .*([A-Z0-9]\{10\})\).*/\1/p' | sed 's/\\,/,/g'; }
# The team id in a certificate's subject (OU), empty if there is none.
team_of() { dn subject "$1" | grep -Eo '(^|,)OU=[A-Z0-9]{10}(,|$)' | head -1 | sed -E 's/^,?OU=//; s/,$//' || true; }
# Fails (reason on stderr) unless $1, a PEM leaf, is a Developer ID Application
# certificate of the team veld releases are already signed by. Prints the team.
#
# The team is half of every install's code identity. A certificate from another
# team (an Account Holder signed in to the wrong one, a --p12 from elsewhere)
# would sign, notarize and pass every CI check — and silently cost every user
# their permissions. So a renewal must keep the team.
vet_leaf() {
  local subject team was gh_team out
  subject=$(dn subject "$1")
  case "$subject" in
    *"CN=Developer ID Application: "*) ;;
    *) echo "  that is not a Developer ID Application certificate ($subject). Pick Developer ID Application" >&2; return 1 ;;
  esac
  team=$(team_of "$1")
  printf '%s' "$team" | grep -Eqx '[A-Z0-9]{10}' \
    || { echo "  could not read a team id out of the certificate ($subject)" >&2; return 1; }
  # Only "no such variable or environment" (404) counts as unset: a gh that
  # cannot answer must not quietly switch the check off.
  if out=$(gh api "repos/$REPO/environments/$ENV_NAME/variables/APPLE_TEAM_ID" --jq .value 2>&1); then
    gh_team=$out
  else
    case "$out" in
      *"HTTP 404"*) gh_team= ;;
      *) echo "  could not read APPLE_TEAM_ID from GitHub environment $ENV_NAME to compare teams: $out" >&2; return 1 ;;
    esac
  fi
  for was in "$(cat "$team_file" 2>/dev/null || true)" "$gh_team"; do
    if [ -n "$was" ] && [ "$was" != "$team" ] && [ "${VELD_APPLE_SIGNING_CHANGE_TEAM:-}" != 1 ]; then
      echo "  that certificate is for team $team, but veld releases are signed by team $was. Changing teams resets every user's permissions. Get a certificate from team $was (on developer.apple.com, pick the team top right), or, if you really mean to switch, re-run with VELD_APPLE_SIGNING_CHANGE_TEAM=1" >&2
      return 1
    fi
  done
  printf '%s\n' "$team"
}
cert_expiry() { "$OPENSSL" x509 -in "$leaf_pem" -noout -enddate | sed 's/^notAfter=//'; }

repo_slug() {
  gh repo view --json nameWithOwner --jq .nameWithOwner 2>/dev/null \
    || die "could not work out the GitHub repository — run this from inside the veld checkout"
}

# ── stages ─────────────────────────────────────────────────────────────────
preflight() {
  step "Checking this Mac"
  [ "$(uname -s)" = Darwin ] || die "this has to run on a Mac"
  command -v gh >/dev/null || die "GitHub CLI missing: brew install gh"
  gh auth status >/dev/null 2>&1 || die "GitHub CLI not logged in: gh auth login"
  xcrun --find notarytool >/dev/null 2>&1 || die "Xcode command line tools missing: xcode-select --install"
  command -v cc >/dev/null || die "no C compiler: xcode-select --install"
  [ -x "$OPENSSL" ] || die "$OPENSSL is missing (it ships with macOS)"
  ok "macOS, gh, notarytool, cc"
  REPO=$(repo_slug)
  local admin
  admin=$(gh api "repos/$REPO" --jq .permissions.admin 2>/dev/null || echo false)
  [ "$admin" = true ] || die "your GitHub account is not an admin of $REPO — an admin has to run this (environments and their secrets need admin)"
  ok "GitHub: admin on $REPO"
  mkdir -p "$STATE"
  chmod 700 "$STATE"
  ok "working folder: $STATE"
}

make_csr() {
  step "1/6  Private key and certificate request"
  if [ -s "$cer_file" ]; then
    ok "not needed — already have a certificate"
    return
  fi
  if [ -s "$key_file" ] && [ -s "$csr_file" ]; then
    ok "already made ($csr_file)"
    return
  fi
  (umask 077 && "$OPENSSL" genrsa -out "$key_file" 2048 2>/dev/null)
  "$OPENSSL" req -new -key "$key_file" -out "$csr_file" -subj "/CN=veld Developer ID/O=veld release signing"
  ok "made $csr_file (the private key stays in $STATE)"
}

# Who does the two Apple steps. Creating a Developer ID certificate takes the
# Account Holder; creating an API key takes Admin. Whoever runs this script is
# often neither — so the Apple half can go to somebody else as a folder of plain
# instructions, while the private key and everything else stays on this Mac.
plan_handoff() {
  local need_cert=0 need_key=0 cert=self key=self
  [ -s "$cer_file" ] || need_cert=1
  [ -s "$p8_file" ] || need_key=1
  [ "$need_cert$need_key" != 00 ] || return 0
  [ ! -s "$who_file" ] || return 0
  step "Who does the Apple part?"
  if [ "$need_cert" = 1 ] && ! yes_no "Can YOU log in to developer.apple.com as the team's ${B}Account Holder${N}?"; then
    cert=other
  fi
  if [ "$need_key" = 1 ] && ! yes_no "Do YOU have ${B}Admin${N} (or Account Holder) access in App Store Connect?"; then
    key=other
  fi
  printf 'cert=%s key=%s\n' "$cert" "$key" > "$who_file"
  [ ! -e "$notary_marker" ] || rm -f "$notary_marker"
  touch "$notary_marker"
  if [ "$cert$key" != selfself ]; then
    write_handoff "$cert" "$key"
  fi
}

write_handoff() {
  local cert=$1 key=$2 doc
  mkdir -p "$HANDOFF"
  doc="$HANDOFF/INSTRUCTIONS.txt"
  [ "$cert" = other ] && cp "$csr_file" "$HANDOFF/devid.csr"
  {
    cat <<'EOF'
veld macOS release signing: a 5-minute task on the Apple Developer account
===========================================================================

Hi! veld (the dev tool) is going to sign its macOS releases with the
company's Apple Developer account, so macOS recognises each update as the same
app instead of a stranger. Creating the signing credentials needs a login with
more rights than the person who sent you this has. Below is everything to
click. Nothing here changes or affects the iOS app or any existing
certificate.
EOF
    if [ "$cert" = other ]; then
      cat <<'EOF'


PART 1 - Certificate  (needs the Account Holder login)
------------------------------------------------------
You need the file devid.csr that came with these instructions.

 1. Open  https://developer.apple.com/account/resources/certificates/add
    and log in as the Account Holder. If your Apple ID is in several teams,
    pick the right one in the top right corner first: it must be the team
    that signed earlier veld releases.
 2. Select "Developer ID Application" (under "Software"). Click Continue.
 3. Profile Type: "G2 Sub-CA (Xcode 11.4.1 or later)". 
 4. Click "Choose File", pick devid.csr. Click Continue.
 5. Click "Download". You get a file named developerID_application.cer.
 6. Send that .cer file back to whoever sent you this.
    It is NOT secret - email or Slack is fine.

 - Use the devid.csr you were sent. Do not make a new request in Keychain
   Access or Xcode: the certificate only works together with the private key
   that was created alongside devid.csr, and that key never leaves the
   sender's Mac.
 - "Developer ID Application" greyed out? Either you are not logged in as the
   Account Holder, or the team already has the maximum number of Developer ID
   certificates - then revoke an unused one first, or reply and ask.
EOF
    fi
    if [ "$key" = other ]; then
      cat <<'EOF'


PART 2 - Notarization key  (needs Admin or Account Holder in App Store Connect)
------------------------------------------------------------------------------
 1. Open  https://appstoreconnect.apple.com/access/integrations/api
 2. Click the "Team Keys" tab. (If you see "Request Access" instead: click it,
    accept the terms, reload the page.)
 3. Click the blue (+) next to "Active (N)" (if there are no keys yet:
    "Generate API Key").
    Name: veld notarization      Access: Developer
    Click Generate.
 4. On the new row, click "Download". You get AuthKey_XXXXXXXXXX.p8.
    Apple allows this download ONCE. Do not rename the file.
 5. Above the table it says "Issuer ID" with a long code like
    69a6de7f-1234-...; copy it.
 6. Send BOTH the .p8 file and the Issuer ID back
    -> through the password manager (a shared item / secure share),
       NOT email or Slack: this file is a secret.
 7. Then delete the .p8 from your Downloads folder.

 - This key can only submit software to Apple's malware scan (notarization).
   With "Developer" access it cannot change apps, users or payments, and it can
   be revoked any time on the same page.
EOF
    fi
    cat <<'EOF'


Questions? Reply to whoever sent you this. Thank you!
EOF
  } > "$doc"
  open "$HANDOFF"
  cat <<EOF

  ${B}Send the folder that just opened on your Desktop to the person who has the access:${N}
    $HANDOFF
      INSTRUCTIONS.txt$([ "$cert" = other ] && printf '\n      devid.csr   (public: email or Slack is fine)')

  A message you can paste with it:
  ┌──────────────────────────────────────────────────────────────────────────
  │ Hi! Could you do a ~5 minute task on our Apple Developer account? veld's
  │ macOS releases are getting signed, and creating the credentials needs your
  │ login. Everything to click is in the attached INSTRUCTIONS.txt.
  └──────────────────────────────────────────────────────────────────────────

  When you get the files back, save them to ~/Downloads (or drag them into this
  window when asked). You can close this now and re-run the script later — it
  picks up where it stopped.
EOF
}

who() { sed -n "s/.*$1=\([a-z]*\).*/\1/p" "$who_file" 2>/dev/null; }

get_certificate() {
  step "2/6  Developer ID Application certificate"
  if [ -s "$cer_file" ]; then
    ok "already have it"
    return
  fi
  local existing
  existing=$(security find-identity -v -p codesigning 2>/dev/null | grep -F '"Developer ID Application: ' || true)
  if [ -n "$existing" ]; then
    printf '%s\n' "$existing" | sed 's/^/    /'
    cat <<EOF
  This Mac already has the Developer ID certificate above. To use it instead of
  making a new one: Keychain Access → My Certificates → right-click it →
  Export… → .p12, then re-run with:  $0 --p12 <that file>
  (A new one is just as good — same team, same identity. Continue to make one.)
EOF
  fi
  if [ "$(who cert)" = other ]; then
    printf '\n  Waiting for the Account Holder to send back developerID_application.cer.\n'
  else
  printf '%s' "$csr_file" | pbcopy
  open -R "$csr_file"
  open "$CERT_PAGE"
  cat <<EOF

  A browser tab is open on Apple's "Create a New Certificate" page (log in as the
  ${B}Account Holder${N} if asked), and Finder shows the file you will upload.

    1. Pick ${B}Developer ID Application${N}  →  Continue
    2. Profile Type: ${B}G2 Sub-CA (Xcode 11.4.1 or later)${N}
    3. Choose File → pick ${B}devid.csr${N}
         (in the file dialog: ⌘⇧G, ⌘V, Enter — the path is on your clipboard)
       →  Continue
    4. ${B}Download${N}

  "Developer ID Application" greyed out? You are not the Account Holder, or the
  team already has the maximum of five — revoke an unused one first.
  Not the Account Holder after all? Delete $who_file and re-run: the script
  then writes instructions to send to them.
EOF
  fi
  local got newer=$csr_file key_mod cert_mod
  key_mod=$("$OPENSSL" rsa -in "$key_file" -noout -modulus 2>/dev/null)
  while :; do
    got=$(wait_for_download 'developerID_application*.cer' "$newer" "developerID_application.cer")
    cert_mod=$( { "$OPENSSL" x509 -inform DER -in "$got" -noout -modulus \
      || "$OPENSSL" x509 -in "$got" -noout -modulus; } 2>/dev/null || true)
    [ "$cert_mod" != "$key_mod" ] || break
    printf '  %s is not from this devid.csr (an older download?) — ignoring it, still waiting\n' "$(basename "$got")" >&2
    newer=$got
  done
  cp "$got" "$cer_file"
  ok "got $(basename "$got")"
}

check_certificate() {
  step "3/6  Checking the certificate"
  "$OPENSSL" x509 -inform DER -in "$cer_file" -out "$leaf_pem" 2>/dev/null \
    || "$OPENSSL" x509 -in "$cer_file" -out "$leaf_pem" 2>/dev/null \
    || die "$cer_file is not a certificate — delete it and re-run"
  local cert_mod key_mod team
  cert_mod=$("$OPENSSL" x509 -in "$leaf_pem" -noout -modulus)
  key_mod=$("$OPENSSL" rsa -in "$key_file" -noout -modulus 2>/dev/null)
  if [ "$cert_mod" != "$key_mod" ]; then
    rm -f "$cer_file" "$leaf_pem"
    die "that certificate does not belong to the private key it came with. Re-run: upload devid.csr from $STATE, or export the certificate *with its key* for --p12"
  fi
  ok "matches the private key"
  if ! team=$(vet_leaf "$leaf_pem"); then
    rm -f "$cer_file" "$leaf_pem"
    die "not using that certificate (reason above). Delete the developerID_application*.cer you downloaded from ~/Downloads, then re-run"
  fi
  ok "type: Developer ID Application"
  printf '%s\n' "$team" > "$team_file"
  ok "team id: $team"
  ok "expires: $(cert_expiry)"

  if [ ! -s "$g2_pem" ]; then
    curl -fsSL "$G2_URL" | "$OPENSSL" x509 -inform DER -out "$g2_pem" \
      || die "could not download Apple's Developer ID G2 intermediate from $G2_URL"
  fi
  local issuer g2_subject
  issuer=$(dn issuer "$leaf_pem")
  g2_subject=$(dn subject "$g2_pem")
  [ "$issuer" = "$g2_subject" ] \
    || die "the certificate was not issued by Apple's G2 intermediate (issuer: $issuer). Re-run and pick Profile Type G2 Sub-CA"
  ok "issued by Apple's Developer ID G2 CA"

  if [ ! -s "$p12_file" ] || [ ! -s "$p12_pass_file" ]; then
    (umask 077 && "$OPENSSL" rand -hex 24 > "$p12_pass_file")
    P12_PASS=$(cat "$p12_pass_file") "$OPENSSL" pkcs12 -export \
      -inkey "$key_file" -in "$leaf_pem" -certfile "$g2_pem" \
      -name "veld Developer ID" -out "$p12_file" -passout env:P12_PASS
    chmod 600 "$p12_file"
    rm -f "$verified_file"
  fi
  ok "bundled into devid.p12"
}

get_notary_key() {
  step "4/6  Notarization API key"
  if [ -s "$p8_file" ] && [ -s "$key_id_file" ] && [ -s "$issuer_file" ]; then
    ok "already have key $(cat "$key_id_file")"
    return
  fi
  if [ -s "$p8_file" ] && [ -s "$key_id_file" ]; then
    # Only the Issuer ID is missing (or was deleted as wrong): the key itself
    # can be downloaded once only, so never ask for it again.
    ok "already have key $(cat "$key_id_file") — only the Issuer ID is missing"
  else
  [ -e "$notary_marker" ] || touch "$notary_marker"
  if [ "$(who key)" = other ]; then
    printf '\n  Waiting for the notarization key (AuthKey_….p8) from whoever has App Store Connect access.\n'
    printf '  It is a secret: it should arrive through the password manager. Save it to ~/Downloads.\n'
  else
  open "$KEYS_PAGE"
  cat <<EOF

  A browser tab is open on App Store Connect → Users and Access → Integrations.

    1. Tab ${B}Team Keys${N}  (first time ever: click "Request Access", accept, reload)
    2. Click the blue ${B}⊕${N} next to "Active (N)"  (no keys yet: "Generate API Key")
    3. Name: ${B}veld notarization${N}    Access: ${B}Developer${N}   →  Generate
    4. On the new row: ${B}Download${N}  (Apple lets you download it exactly once)
EOF
  fi
  local got name
  got=$(wait_for_download 'AuthKey_*.p8' "$notary_marker" "AuthKey_XXXXXXXXXX.p8")
  grep -q 'BEGIN PRIVATE KEY' "$got" || die "$got is not an API key"
  name=$(basename "$got" .p8)
  name=${name#AuthKey_}
  # A password manager may have renamed it; the id is also in the Keys table.
  while ! printf '%s' "$name" | grep -Eqx '[A-Z0-9]{8,12}'; do
    printf '  the file name does not carry the key id. Type the Key ID (the KEY ID column, e.g. 2X9R4HXF34): '
    read -r name
    name=$(printf '%s' "$name" | tr -d '[:space:]')
  done
  (umask 077 && cp "$got" "$p8_file")
  printf '%s\n' "$name" > "$key_id_file"
  ok "key id: $name"
  fi

  cat <<EOF

  ${B}Issuer ID${N}: shown above the Team Keys table (or sent along with the key).
  Copy it and press Enter, or paste it and press Enter.
EOF
  local issuer typed
  while :; do
    printf '\n  %sIssuer ID:%s ' "$B" "$N"
    read -r typed
    issuer=$(printf '%s' "${typed:-$(pbpaste)}" | tr -d '[:space:]')
    if printf '%s' "$issuer" | grep -Eqix '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'; then
      break
    fi
    printf '  the clipboard does not hold an Issuer ID (it looks like 69a6de7f-…-…). Copy it and press Enter again.\n'
  done
  printf '%s\n' "$issuer" > "$issuer_file"
  rm -f "$notary_marker"
  ok "issuer id: $issuer"
}

verify_locally() {
  step "5/6  Proving it works: sign and notarize a test binary on this Mac"
  if [ -f "$verified_file" ] && [ "$verified_file" -nt "$p12_file" ] && [ "$verified_file" -nt "$p8_file" ]; then
    ok "already proven ($(cat "$verified_file"))"
    return
  fi
  local work kc identity submission status id
  work=$(mktemp -d)
  kc="$work/proof.keychain-db"
  # shellcheck disable=SC2064  # expand now: $work and $kc are local
  trap "'$here/macos-keychain.sh' delete '$kc' >/dev/null 2>&1 || true; rm -rf '$work'" EXIT

  export APPLE_TEAM_ID APPLE_DEVID_P12 APPLE_DEVID_P12_PASSWORD
  APPLE_TEAM_ID=$(team_id)
  APPLE_DEVID_P12=$(base64 < "$p12_file")
  APPLE_DEVID_P12_PASSWORD=$(cat "$p12_pass_file")
  identity=$("$here/macos-keychain.sh" import "$kc")
  unset APPLE_DEVID_P12 APPLE_DEVID_P12_PASSWORD
  ok "imported the way release CI will"

  printf 'int main(void){return 0;}\n' > "$work/hello.c"
  cc -o "$work/veld-signing-proof" "$work/hello.c"
  KEYCHAIN="$kc" "$here/macos-codesign.sh" "$identity" dev.veld.signing-proof "$work/veld-signing-proof"
  ok "signed: Developer ID, team $APPLE_TEAM_ID, timestamped, hardened"

  ditto -c -k --keepParent "$work/veld-signing-proof" "$work/proof.zip"
  printf '  sending it to Apple for notarization (usually under 5 minutes) …\n'
  submission=$(xcrun notarytool submit "$work/proof.zip" \
    --key "$p8_file" --key-id "$(cat "$key_id_file")" --issuer "$(cat "$issuer_file")" \
    --wait --timeout 30m --output-format json) || true
  status=$(printf '%s' "$submission" | sed -n 's/.*"status" *: *"\([^"]*\)".*/\1/p' | head -1)
  id=$(printf '%s' "$submission" | sed -n 's/.*"id" *: *"\([^"]*\)".*/\1/p' | head -1)
  if [ "$status" != Accepted ]; then
    printf '%s\n' "$submission" >&2
    if [ -n "$id" ]; then
      xcrun notarytool log "$id" --key "$p8_file" --key-id "$(cat "$key_id_file")" --issuer "$(cat "$issuer_file")" >&2 || true
    fi
    die "notarization came back '${status:-no answer}'. A wrong Issuer ID: delete $issuer_file and re-run (the key is kept). A revoked or deleted key (401/403): delete $p8_file, $key_id_file and $issuer_file, make a new key (Access: Developer) and re-run."
  fi
  ok "Apple notarized it (submission $id)"
  date '+%Y-%m-%d %H:%M' > "$verified_file"
  "$here/macos-keychain.sh" delete "$kc" >/dev/null
  rm -rf "$work"
  trap - EXIT
}

upload() {
  step "6/6  Uploading to GitHub ($REPO, environment '$ENV_NAME')"
  [ -f "$verified_file" ] || die "refusing to upload something that has not been proven to work"
  cat <<EOF
  This will:
    • create/update environment '$ENV_NAME' so it deploys from ${B}main only${N}
    • set 5 secrets and 1 variable in it (existing values are replaced)
EOF
  yes_no "Upload to $REPO now?" || die "stopped before uploading — re-run when ready, nothing is lost"

  local custom
  custom=$(gh api "repos/$REPO/environments/$ENV_NAME" \
    --jq '.deployment_branch_policy.custom_branch_policies // false' 2>/dev/null || echo false)
  if [ "$custom" != true ]; then
    gh api -X PUT "repos/$REPO/environments/$ENV_NAME" --input - >/dev/null <<'JSON'
{"deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
JSON
  fi
  if ! gh api "repos/$REPO/environments/$ENV_NAME/deployment-branch-policies" \
       --jq '.branch_policies[] | select(.type != "tag") | .name' | grep -qx main; then
    gh api -X POST "repos/$REPO/environments/$ENV_NAME/deployment-branch-policies" \
      -f name=main -f type=branch >/dev/null
  fi
  local others
  others=$(gh api "repos/$REPO/environments/$ENV_NAME/deployment-branch-policies" \
    --jq '[.branch_policies[].name] | map(select(. != "main")) | join(", ")')
  # Any other branch or tag pattern would let whoever can push a matching ref
  # run their own workflow with these secrets, so it stops the upload.
  [ -z "$others" ] || die "environment '$ENV_NAME' also deploys from: $others. Remove those in https://github.com/$REPO/settings/environments, then re-run — nothing is lost"
  ok "environment '$ENV_NAME' deploys from main"

  base64 < "$p12_file" | tr -d '\n' | gh secret set APPLE_DEVID_P12 --env "$ENV_NAME" --repo "$REPO"
  chomp "$p12_pass_file" | gh secret set APPLE_DEVID_P12_PASSWORD --env "$ENV_NAME" --repo "$REPO"
  gh secret set APPLE_NOTARY_KEY_P8 --env "$ENV_NAME" --repo "$REPO" < "$p8_file"
  chomp "$key_id_file" | gh secret set APPLE_NOTARY_KEY_ID --env "$ENV_NAME" --repo "$REPO"
  chomp "$issuer_file" | gh secret set APPLE_NOTARY_ISSUER_ID --env "$ENV_NAME" --repo "$REPO"
  gh variable set APPLE_TEAM_ID --env "$ENV_NAME" --repo "$REPO" --body "$(team_id)"
  date '+%Y-%m-%d %H:%M' > "$STATE/uploaded"
  report_github
}

report_github() {
  local names missing=0 n
  names=$(gh secret list --env "$ENV_NAME" --repo "$REPO" --json name --jq '.[].name' 2>/dev/null || true)
  for n in APPLE_DEVID_P12 APPLE_DEVID_P12_PASSWORD APPLE_NOTARY_KEY_P8 APPLE_NOTARY_KEY_ID APPLE_NOTARY_ISSUER_ID; do
    if printf '%s\n' "$names" | grep -qx "$n"; then ok "secret $n"; else todo "secret $n missing"; missing=1; fi
  done
  if gh variable list --env "$ENV_NAME" --repo "$REPO" --json name --jq '.[].name' 2>/dev/null | grep -qx APPLE_TEAM_ID; then
    ok "variable APPLE_TEAM_ID"
  else
    todo "variable APPLE_TEAM_ID missing"; missing=1
  fi
  return "$missing"
}

check() {
  preflight
  step "Local state ($STATE)"
  if [ -s "$csr_file" ]; then ok "certificate request"; else todo "certificate request not made"; fi
  if [ -s "$leaf_pem" ]; then
    ok "certificate: $(cert_cn)"
    if "$OPENSSL" x509 -in "$leaf_pem" -noout -checkend $((30 * 24 * 3600)) >/dev/null; then
      ok "expires $(cert_expiry)"
    else
      todo "expires $(cert_expiry) — under 30 days: run with --renew (releases fail from 30 days out)"
    fi
  else
    todo "no certificate yet"
  fi
  if [ -s "$p8_file" ]; then ok "notary key $(cat "$key_id_file" 2>/dev/null)"; else todo "no notary key yet"; fi
  if [ -f "$verified_file" ]; then ok "proven locally $(cat "$verified_file")"; else todo "not proven locally yet"; fi
  step "GitHub ($REPO, environment '$ENV_NAME')"
  report_github || true
}

archive_certificate() {
  local archive f
  archive="$STATE/old-$(date +%Y%m%d-%H%M%S)"
  rm -f "$who_file"
  for f in "$key_file" "$csr_file" "$cer_file" "$leaf_pem" "$p12_file" "$p12_pass_file" "$verified_file"; do
    [ -e "$f" ] || continue
    mkdir -p "$archive"
    mv "$f" "$archive/"
  done
  [ ! -d "$archive" ] || ok "previous certificate files moved to $archive"
}

# Takes the key and the leaf out of someone's exported .p12; check_certificate
# then vets them exactly like a fresh download, and a new .p12 is built with
# Apple's intermediate and a fresh password.
use_existing() {
  local in=$1 pass
  [ -f "$in" ] || die "no such file: $in"
  preflight
  step "1-2/6  Using an existing Developer ID certificate ($(basename "$in"))"
  # The password file next to it, when this is our own devid.p12 from a backup.
  if [ "$(basename "$in")" = devid.p12 ] && [ -s "$(dirname "$in")/devid.p12.password" ]; then
    pass=$(cat "$(dirname "$in")/devid.p12.password")
  else
    printf '  password you set when exporting it (input hidden): '
    read -rs pass
    printf '\n'
  fi
  # Read into a scratch folder first: $in may well be $STATE/devid.p12 itself,
  # which archive_certificate is about to move away.
  local tmp
  tmp=$(mktemp -d)
  (umask 077 && P12_IN_PASS=$pass "$OPENSSL" pkcs12 -in "$in" -passin env:P12_IN_PASS -nocerts -nodes 2>/dev/null \
    | "$OPENSSL" rsa -out "$tmp/key" 2>/dev/null) \
    || { rm -rf "$tmp"; die "could not read a private key from $in — wrong password, or no key in it (export from Keychain Access → My Certificates, so the key comes along)"; }
  P12_IN_PASS=$pass "$OPENSSL" pkcs12 -in "$in" -passin env:P12_IN_PASS -clcerts -nokeys 2>/dev/null \
    | "$OPENSSL" x509 -outform DER -out "$tmp/cer" 2>/dev/null \
    || { rm -rf "$tmp"; die "could not read a certificate from $in"; }
  # Vetted before archive_certificate moves a working setup out of the way.
  if ! { "$OPENSSL" x509 -inform DER -in "$tmp/cer" -out "$tmp/pem" 2>/dev/null && vet_leaf "$tmp/pem" >/dev/null; }; then
    rm -rf "$tmp"
    die "not using $in (reason above); nothing was changed"
  fi
  archive_certificate
  mv "$tmp/key" "$key_file"
  mv "$tmp/cer" "$cer_file"
  rm -rf "$tmp"
  ok "read the key and certificate (checked next)"
  run_all
}

renew() {
  preflight
  archive_certificate
  cat <<EOF
  A new certificate keeps every install's code identity: macOS matches the
  identifier and the ${B}team${N}, not the certificate. Revoke the old one at
  developer.apple.com only if it leaked — revoking does not break software
  already signed and timestamped with it, but there is no reason to rush it.
EOF
  run_all
}

run_all() {
  [ -t 0 ] || die "run this in a terminal — it asks you to do things and waits"
  [ -n "${REPO:-}" ] || preflight
  make_csr
  plan_handoff
  get_certificate
  check_certificate
  get_notary_key
  verify_locally
  upload
  step "Done"
  cat <<EOF
  Release CI can now sign. Two things left for you:

  1. ${B}Back up${N} the whole folder that just opened (${STATE}) into the team
     password manager. Restored to the same place on another Mac, the script
     resumes from it: the next maintainer re-runs it and re-uploads instead of
     redoing the Apple steps. Without it, they redo them; nothing is lost but time.
  2. Certificate expires $(cert_expiry). Releases start failing 30 days before
     that. Then: scripts/release/apple-signing-setup.sh --renew
EOF
  open "$STATE"
}

case "${1:-}" in
  "") run_all ;;
  --check) check ;;
  --renew) renew ;;
  --p12) [ -n "${2:-}" ] || die "usage: $0 --p12 <exported .p12>"; use_existing "$2" ;;
  -h|--help) sed -n '2,/^set -euo/p' "$0" | sed -e '$d' -e 's/^# \{0,1\}//' ;;
  *) die "unknown option $1 (try --help)" ;;
esac

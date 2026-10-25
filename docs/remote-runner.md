# Paired Veld runners (trial)

A Veld Desktop window can control a headless Veld instance on another machine.
Both machines use Tailscale. Discovery finds candidate hosts; it does not grant
access. The host owner issues a short-lived, single-use invitation. Desktop
verifies the host's signed identity before redeeming that invitation.

Pairing grants control as the runner's OS user, including terminals and project
commands. Use a host account whose files and commands the paired client may use.
Provider credentials remain on the host. Desktop keeps its separate pairing
credential in OS-encrypted storage, outside the renderer and URLs.

## Start the Linux host

Build `veld` and `veld-daemon` from this branch and keep the binaries together:

```sh
cargo build -p veld -p veld-daemon
target/debug/veld runner --name remote-trial install
target/debug/veld runner --name remote-trial status
```

Installation requires Tailscale login and user lingering. It creates the named
user service `veld-runner-remote-trial.service`, a separate database and identity,
and an authenticated loopback listener. It does not replace `veld-daemon.service`.
The default API port is 19895; the remote ingress port is 19896. Installation
checks the service, database, listener and signed identity over HTTPS before
reporting success. Conflicting service definitions or Tailscale mappings fail.
After rebuilding a binary used by the trial service, restart that named service
before testing. A running Linux process can otherwise reference the deleted old
executable when it starts a holder.

If Tailscale requires administrator access, the host administrator runs:

```sh
sudo tailscale serve --bg --https=19896 http://127.0.0.1:19896
```

Then repeat installation. Do not use Funnel: this endpoint belongs on the tailnet.
The endpoint uses the host's full Tailscale DNS name and port 19896.

For foreground operation, use `veld runner --name remote-trial serve --endpoint
https://HOST.TAILNET.ts.net:19896`. Configure HTTPS publication separately.
Service installation currently supports Linux. The foreground runner supports
Unix hosts; Windows hosting is outside this trial.

## Pair a Desktop client

1. Start Veld Desktop built from this branch on the Mac.
2. On the host, run `veld runner --name remote-trial pair` using the branch binary.
3. Open **File → Remote Connections…** in Desktop. Paste the invitation and name
   the connection. Confirm the displayed host and runner identity.
4. Open the saved connection. Import a repository using its path on the host.
5. Open a terminal, or choose **Remote agent** and explicitly select the host
   runtime, account, model and options.

Invitations expire after five minutes. Issuing another invitation invalidates the
previous one. Discovery is bounded and optional; a manual invitation works even
when the host does not appear in discovery. SSH is useful for installation and
testing, but pairing and ordinary operation do not use SSH.

Remote windows have no native preload. Their persistent browser storage belongs
to the verified runner and paired client. They cannot reuse local Desktop browser
profiles or native filesystem authority. Application previews use a scoped local
proxy and locally generated certificates; OpenSSL must be available on the client.
The proxy forwards HTTP and WebSocket messages to an approved host application,
not an unrestricted TCP tunnel. Remote windows deny arbitrary popups, downloads
and native permissions. Use the preview's separate-window control when an
application needs first-party session cookies. Inline previews still obey
browser cookie and iframe restrictions; Veld does not weaken cookie attributes.

## Accounts, reconnect and retries

The concrete adapter currently supports the **inherited, verified Codex ChatGPT
account**. The host shell selects its existing account context. Discovery reads
the runtime's account and model protocol without submitting an inference turn.
Veld validates the same account, model, options and catalogue revision immediately
before execution, after shell startup. Unsupported account selectors and provider
types remain unavailable. See [the adapter contract](remote-runtime-adapter.md)
and [the Codex integration](remote-codex-runtime.md).

Closing a client or losing the network leaves host jobs running. Reconnection
attaches to the same holder. It never starts replacement execution silently.
Daemon restart adopts retained holders. A daemon outage has a separate 24-hour
orphan policy; ordinary client disconnection has no detach timeout for these
remote terminals. A terminated holder requires an explicit new terminal.
Native provider-session resume is unavailable in this trial.

Start, stop, restart and action submissions carry durable operation keys. An
uncertain response retains the same public request and key across reconnect and
Desktop closure. Reusing a key with changed input fails. Finished receipts follow
the existing seven-day pruning policy; other mutation routes do not share this
deduplication contract.

## Revoke and remove

```sh
veld runner --name remote-trial clients
veld runner --name remote-trial revoke CLIENT_ID
veld runner --name remote-trial uninstall
```

Use the branch binary for these commands. Revocation rejects new requests, closes
streams and waits for retained holders to reject queued client input. Bytes
already delivered to the application cannot be recalled. Host jobs continue.
**Forget** in Desktop only removes the local registration; revoke on the host to
remove access. Uninstall removes only the matching named service and Tailscale
mapping. Private state remains available for deliberate reinstallation.

## Verification

```sh
uv run tests/remote/runner_trial.py --restart-service
python3 tests/remote/listener_ownership.py
node tests/remote/preview_trial.cjs
node --test desktop/src/remoteConnections.test.js desktop/src/remoteWindows.test.js desktop/src/remotePreview.test.js
cargo test -p veld-daemon revoke_
```

The live trial requires an explicitly installed runner whose name ends in
`trial`. It creates temporary repositories, pairs test clients, checks operation
deduplication, reconnects a terminal, restarts the named service and checks
revocation while host work continues. It removes its fixtures and test clients.
The listener test uses private temporary state and an occupied API port.
The preview trial starts its own temporary application and tests the actual
CONNECT, TLS, HTTP and WebSocket path through the published runner. Only the
Electron session interface is replaced for this command-line check.

These Linux checks do not establish macOS Electron, keychain or preview behavior.
Record a real Mac-to-host trial before treating this branch as release-ready.

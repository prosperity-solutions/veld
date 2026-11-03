# Inherited Codex runtime

`crates/veld-daemon/src/agent_runtime.rs` implements the inherited-account Codex
adapter. The provider-neutral contract remains in `veld_core::agent_runtime`.
The adapter starts a short-lived `codex app-server --listen stdio://`, initializes
the protocol with `experimentalApi: true`, then uses `account/read`, `config/read`
and paginated `model/list`. It never creates a thread or submits a model turn.

Protocol references: [Codex app-server](https://learn.chatgpt.com/docs/app-server)
and the installed binary's `codex app-server generate-json-schema --experimental`.
`model/list` supplies a capability catalogue; it does not prove that a future
inference request will succeed or that the account has quota.

## Integration API

Export the new database module with `pub mod agent_runtime;` from
`crates/veld-core/src/db/mod.rs`. Add `mod agent_runtime;` to the daemon. No schema
migration or additional dependency is required.

The public daemon helpers are asynchronous:

```rust
describe_worktree(worktree_id: i64) -> Result<RuntimeCatalogue>
stage_launch(
    worktree_id: i64,
    session_id: &str,
    selection: &LaunchSelection,
    client_id: &str,
) -> Result<SessionSelection>
session_selection(
    worktree_id: i64,
    session_id: &str,
    client_id: &str,
) -> Result<SessionSelection>
launch(session_id: &str) -> Result<()>
```

The internal `describe(directory: &Path)` primitive resolves the configured host
shell through `Db::terminal_shell()`. Its path parameter must never become remote
request authority. Remote catalogue handlers accept a registered worktree ID and
call `describe_worktree` after authentication. Shell startup runs inside that
checkout with `-l -i`; a command supplied on stdin allows prompt and directory
hooks to run before discovery. `TERM` matches the holder's `xterm-256color`.
Supported shells are bash, zsh, sh and dash.
An unsupported shell fails explicitly.

For a new terminal:

1. Authenticate the remote principal. Derive `client_id` from that principal.
2. Call `stage_launch` before reserving a terminal receipt or minting a PTY ticket.
   Only runtime/account/connection/model/options/revision come from the request.
   The helper resolves defaults and persists the full tuple before returning.
3. Build the PTY command from the trusted daemon binary and session ID:
   `veld-daemon --remote-agent <session-id>`. Execute that command **after** the
   host's selected login/interactive shell starts in the registered checkout.
   Preserve the runner's instance environment, especially `VELD_DB_PATH`.
4. Dispatch `--remote-agent` before normal daemon startup. Return
   `agent_runtime::launch(&session_id).await`. Report an error on stderr and exit
   nonzero. Do not start a shell, default agent, or fallback conversation on error.

For an existing holder, call `session_selection` and the existing terminal-owner
and holder-liveness checks. Return the saved metadata unchanged. Do not call
`launch` again. If the holder is dead, report native resume unavailable and require
a new session ID with an explicit selection. A missing selection record never
grants permission to turn an old terminal into an agent session.

The parent owns HTTP authentication, request-size limits, catalogue error/status
mapping, terminal tickets, holder creation/attachment, and response metadata.
The adapter returns discovery failures as errors, rather than publishing a
launchable placeholder account. The parent must display those failures as
unavailable runtime state.

## Identity and selection

The only connection is `codex-inherited-chatgpt`, under runtime `codex`. It means
the account inherited from the host shell in the chosen directory. It does not
mean an alternate home directory, a copied login, or a configurable account slot.
Nothing assigns `CODEX_HOME` or `CLAUDE_CONFIG_DIR`, reads provider credential
files, invokes login/logout, or changes provider configuration files.

Discovery requires all of these runtime assertions:

- `account.type == "chatgpt"`, `requiresOpenaiAuth == true`, and a nonempty email.
- Experimental `workspaceRouting.chatgptAccountId` is present and nonempty.
- `workspaceRouting.backendOrigin` is `https://chatgpt.com`.
- The configured provider is the built-in OpenAI provider, including its implicit
  default. Custom OpenAI provider definitions and endpoint overrides are rejected.
  The built-in `https://chatgpt.com/backend-api` ChatGPT endpoint is accepted,
  including its trailing-slash form returned by the installed runtime.
- Repeated account/config reads agree across model discovery.

The account ID is a domain-separated SHA-256 hash of the runtime-reported user,
workspace, backend and routing identity. The catalogue contains generic display
text and the hash; it contains no email, workspace ID, provider home path or raw
configuration. Account identity is distinct from plan metadata: a plan change
changes the revision rather than reassigning the account ID.

API-key auth, absent email, absent experimental workspace identity, and other
providers are unavailable. The adapter does not guess identity from a directory
or read a credential file to fill a missing runtime field.

Models use `model/list`'s executable `model` value, not its picker `id` alias.
Reasoning values and defaults come from `supportedReasoningEfforts` and
`defaultReasoningEffort`. Service tiers come from `serviceTiers`, with Codex's
explicit `default` sentinel for standard routing. This sentinel is defined by
[the Codex protocol](https://github.com/openai/codex/blob/main/codex-rs/protocol/src/config_types.rs)
and is not included in its tier catalogue. An inherited
configured tier takes precedence over `defaultServiceTier`. If the runtime gives
no service-tier default, selection must explicitly choose standard routing or an advertised tier.
The adapter does not substitute an absent default or translate a new runtime value to
an older value. Other options are not exposed without an implemented TUI mapping.

The revision hashes the sorted effective catalogue, account response, effective
configuration and initialization metadata. Model and option changes, account or
workspace changes, configuration changes, and provider-version/home changes
invalidate an earlier revision. Defaults become explicit in the persisted
`SessionSelection`; launch never rebases a stale selection.

## Persistence and final launch

`veld_core::db::agent_runtime` stores immutable receipts in existing `kv` rows:
`remote.agent-launch:<session-id>` and a separate
`remote.agent-claim:<session-id>` one-shot claim. Transactions prevent concurrent
requests from changing an existing receipt or claiming a launch twice. A legacy
terminal receipt without an agent receipt cannot acquire agent launch authority.

Each receipt contains the authenticated client ID, registered worktree ID,
canonical directory, and complete `SessionSelection`. The checkout ID hashes the
registry ID, registered path, repository root, registration timestamp, canonical
path, device and inode. A branch rename preserves identity; directory replacement,
registry replacement, path retargeting or trash state invalidates the binding.

The runner ID comes from the live `RunnerStore` descriptor through the runner's
owner-only admin socket. The helper also checks that the saved client remains in
the live client list. It does not open or create runner credential files. The
runner admin server must be running during staging, attachment checks and launch.

`launch` reads the authoritative receipt, checks runner/client/checkout/current
directory, and starts a fresh metadata probe **without starting another shell**.
The probe inherits the actual final shell environment. The helper validates the
saved revision and all resolved options, rechecks the host binding, and consumes
the launch claim before Unix `exec`.

The exact argument mapping is:

```text
codex --no-daemon --model <model/list.model>
  --config model_provider="openai"
  --config forced_login_method="chatgpt"
  --config forced_chatgpt_workspace_id="<verified workspace>"
  --config model_reasoning_effort="<resolved reasoningEffort>"
  --config service_tier="<resolved serviceTier>"  # only when advertised
```

Arguments pass directly to `exec`, with TOML string encoding; no selection value
is interpolated into shell code. The workspace ID is nonsecret account metadata,
not an authentication token. The helper uses `--no-daemon` so the TUI cannot
connect to another shared server with a different account context. It preserves
host approval and sandbox settings. There is no client policy payload and no
repository policy resolver in this initial adapter; the trusted host capability
is limited to the verified inherited Codex connection.

Discovery has a 25-second overall deadline, four concurrent probe slots, a 1 MiB
frame limit, a 4 MiB total output limit, and bounded model pagination. Shutdown
kills the owned probe process group; normal cleanup reaps the child. Provider
stderr and raw protocol errors are not forwarded, because they can contain
private configuration or account data. Only fixed errors identify failed methods.

This does not freeze the provider's login files against a simultaneous user
logout/account switch. Verification occurs immediately before launch, with native
ChatGPT/workspace constraints applied to the TUI. The adapter does not claim a
provider session ID from terminal output. `provider_session_id` remains `None`;
native resume is unsupported. A failed exec or crash after claim consumption
requires a new session ID rather than silently starting another conversation.
Discovery uses pipes, so startup hooks that require a real TTY may differ. Final
verification rejects any resulting account or catalogue mismatch.

## Verification boundary

Focused tests cover verified identity, workspace/user changes, alternate-provider
rejection, runtime-driven argument mapping, stale selections, capability revision
changes, checkout replacement, bounded protocol frames, error redaction, durable
receipts, legacy terminals and one-shot claims. The implementation sub-agent does
not run builds or tests; the parent owns those commands and final PTY verification.
Read-only inspection of the installed runtime confirms ChatGPT workspace metadata
and dynamic model/option fields. No paid model request is part of that inspection.

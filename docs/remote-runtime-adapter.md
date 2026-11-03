# Remote runtime adapter contract

`veld_core::agent_runtime` defines provider-neutral metadata and validation. It
does not discover credentials, select an account in a provider runtime, or launch
a process. This contract alone does **not** establish multi-account support.
Provider-specific discovery and execution belong in an execution-host adapter.
Existing pane commands remain commands; do not derive a model catalogue from
them, hardcode provider models, or scrape authentication files.

## Installed adapter and transport

The host administrator installs and trusts an executable outside the checkout.
Resolve its absolute path through host configuration. A repository, request, or
catalogue must never supply its path, shell script, environment overrides, or
arguments. Invoke the executable with one fixed argument, `describe` or `launch`.
Send one JSON object on stdin, never credentials or selection data in argv.

- `describe` receives `DescribeRequest { directory }` and returns
  `RuntimeCatalogue` on its control stdout. Call `validate_describe_request`
  before host-side directory authorization and `validate_catalogue` on the reply.
- `launch` receives `LaunchRequest { directory, session, policy }`. Its control
  channel returns one `LaunchResponse { effective_session }` acknowledgement.
  Keep control metadata separate from the runtime's PTY output. Do not parse
  provider terminal output as an acknowledgement.
- Use nonzero exit status for failure. Do not return a successful acknowledgement
  for an unavailable operation. Diagnostics must not contain credentials or raw
  authentication output. Apply a host timeout and a 1 MiB frame limit before
  buffering; `decode_payload` also checks that limit and returns static errors.
  Deserialize directly from bytes, without an intermediate JSON map that could
  discard duplicate keys. Then run the semantic validators.

The directory must be an absolute, control-free path of at most 4096 bytes.
The host must authorize and resolve that directory to its checkout identity;
core validation does not perform filesystem access or authorize paths.

## Catalogue and explicit selection

`RuntimeCatalogue` contains `revision` and `runtimes`. Each runtime has accounts;
each account has a provider connection, authentication status, billing kind, and
models. Each model declares string/boolean option values and optional defaults.
`identity_summary` is optional, safe display text, never a token or credential.
Do not infer `authenticated` from a directory's existence. Unknown authentication
and unauthenticated accounts are visible metadata but cannot launch.

The host must change `revision` whenever account identity, availability, model
capabilities, defaults, or launch-relevant configuration changes. Keep account
and connection IDs stable and never reassign an ID to another identity.
`validate_catalogue` checks all entries, including unavailable accounts.

IDs are 1–128 ASCII bytes, start with an alphanumeric character, and contain only
alphanumeric characters or `._:/@+-`. Display text is 1–256 UTF-8 bytes, has no
surrounding whitespace or control characters. Option strings are at most 256
bytes without control characters; empty option strings are allowed. Limits are
16 runtimes, 32 accounts per runtime, 128 models per account, 16 options per
model, and 32 values per option. The entire catalogue has at most 4096 entries,
counting runtimes, accounts, models, options, and values. IDs are unique within
their parent; supported values are unique within their descriptor. Shared
connection IDs are allowed.

`validate_selection(selection, catalogue, policy)` rejects stale revisions,
unknown or mismatched tuple members, unavailable authentication, unknown options,
unsupported values, and policy violations. It returns a resolved
`LaunchSelection`: option defaults become explicit `BTreeMap` values. A descriptor
without a default requires an explicit value. There is no default model or
account. Persist this result, not the unresolved input. Unknown launch fields
and duplicate option keys are rejected during deserialization.

`RuntimePolicy` is supplied separately by the host's trusted repository-policy
resolver. `None` means unrestricted; an empty `permitted` list denies everything.
Each `PermittedTuple` requires an exact runtime and provider connection; optional
account/model fields restrict those dimensions when present. Policy has at most
128 entries. Resolve policy again on resume and immediately before execution.
Never treat a client's policy payload as authority. `validate_launch_request`
requires the payload policy to equal the independently resolved host policy.

## Launch verification and persistence

1. Describe on the execution host in the intended directory and shell context.
   Let the user choose runtime, connection, account, model, and options.
2. Validate, resolve defaults, and persist `SessionSelection` with host-issued
   `instance_id` and immutable `checkout_id`. Persist the full selection,
   including revision and options. A branch name is not a checkout identity.
3. Use `build_launch_request` with the resolved session, catalogue, and policy.
   The helper returns the typed stdin payload and refuses unresolved defaults.
4. In the adapter, **after the host's login/interactive shell startup**, verify
   the actual effective account and provider connection using runtime-supported
   identity facilities. Refresh the catalogue and policy and call
   `validate_launch_request` immediately before exec. Execute the exact selected
   model and all resolved options. If identity or capabilities cannot be verified,
   fail. Do not silently launch the inherited/default account or model.
5. Return verified effective metadata through the separate control channel.
   Call `validate_launch_response` against the expected request and current
   catalogue/policy. It checks the complete tuple, revision, options, instance,
   checkout, and existing provider session ID. First launch may add a provider
   session ID. Persist the accepted result atomically before reporting success.
   On mismatch, terminate the attempted launch; do not adopt its session.

Never rewrite `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, user configuration, or global
logins. Selecting another account requires a provider-supported mechanism that
preserves these constraints. The first trial may expose only the inherited
account, **verified by that runtime after shell startup**. If selecting other
accounts is not safely supported, report explicit unavailability. Do not list
an account as launchable merely because another login or directory exists.

## Reconnect and resume

Load the authoritative `SessionSelection` from persistent host storage. Supply
host-derived current identities in the attempted session, rather than trusting
client-supplied instance/checkout keys. Call `validate_resume(recorded, attempted,
catalogue, policy)`. Both sessions must match exactly, including options,
revision, and optional provider session ID. Resume rechecks availability,
authentication, and current policy. It never fills new defaults or adopts a
different account or connection. A changed catalogue revision fails closed and
requires a fresh explicit selection; never silently rewrite the saved revision.
Recorded option values remain explicit even if a default changes.

Reconnect to an existing holder must retain its recorded selection. A dead
provider process can resume only through a runtime-supported session ID and the
same verified tuple. If `provider_session_id` is absent or native resume is
unsupported, report resume unavailability; do not start a fresh conversation.

## Integration boundary

The parent change must export `agent_runtime` from `veld-core`, wire remote
routes and pane spawn, install/configure the trusted adapter, enforce transport
limits/timeouts and host path authorization, persist selections, and manage the
separate metadata channel and launch cleanup. Provider adapters must prove their
identity and option handling against real runtimes. Core metadata equality is
not authentication or proof that a runtime honored its settings.

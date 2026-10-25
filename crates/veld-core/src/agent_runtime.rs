//! Provider-neutral selection and host-adapter wire contract.
//!
//! This module does not discover accounts, inspect credentials, or execute commands.
//! Validate at ingress and again in the execution host after shell startup. Persist
//! the resolved selection, including defaults, before starting the provider session.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;

pub const MAX_JSON_BYTES: usize = 1024 * 1024;
pub const MAX_ID_BYTES: usize = 128;
pub const MAX_LABEL_BYTES: usize = 256;
pub const MAX_DIRECTORY_BYTES: usize = 4096;
pub const MAX_RUNTIMES: usize = 16;
pub const MAX_ACCOUNTS: usize = 32;
pub const MAX_MODELS: usize = 128;
pub const MAX_OPTIONS: usize = 16;
pub const MAX_OPTION_VALUES: usize = 32;
pub const MAX_CATALOGUE_ITEMS: usize = 4096;
pub const MAX_POLICY_ENTRIES: usize = 128;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCatalogue {
    pub revision: String,
    pub runtimes: Vec<RuntimeInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeInfo {
    pub id: String,
    pub label: String,
    pub accounts: Vec<AccountInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountInfo {
    pub id: String,
    pub label: String,
    pub provider_connection_id: String,
    pub auth_status: AuthStatus,
    pub identity_summary: Option<String>,
    pub billing_kind: BillingKind,
    pub models: Vec<ModelInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStatus {
    Authenticated,
    Unauthenticated,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingKind {
    Subscription,
    Api,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelInfo {
    pub id: String,
    pub label: String,
    pub options: Vec<OptionDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionDescriptor {
    pub id: String,
    pub values: Vec<Value>,
    pub default: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchSelection {
    pub runtime_id: String,
    pub account_id: String,
    pub provider_connection_id: String,
    pub model_id: String,
    #[serde(deserialize_with = "deserialize_options")]
    pub options: BTreeMap<String, Value>,
    pub catalogue_revision: String,
}

/// An allow-list supplied by the host's trusted repository-policy resolver.
/// `None` at the call site means unrestricted; an empty allow-list denies all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimePolicy {
    pub permitted: Vec<PermittedTuple>,
}

/// Runtime and connection always match exactly. Absent account/model permits
/// any account/model within that runtime and connection, subject to validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermittedTuple {
    pub runtime_id: String,
    pub provider_connection_id: String,
    pub account_id: Option<String>,
    pub model_id: Option<String>,
}

/// Persist atomically with the remote session. Identity keys come from the host,
/// never from a reconnecting client's claim or a mutable branch name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSelection {
    pub selection: LaunchSelection,
    pub provider_session_id: Option<String>,
    pub instance_id: String,
    pub checkout_id: String,
}

/// Stdin payload for the installed adapter's fixed `describe` operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DescribeRequest {
    pub directory: String,
}

/// Stdin payload for the installed adapter's fixed `launch` operation.
/// Policy is separate from the selection and must be resolved by the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchRequest {
    pub directory: String,
    pub session: SessionSelection,
    pub policy: Option<RuntimePolicy>,
}

/// Control-channel acknowledgement of the runtime's verified effective tuple.
/// This is metadata, not proof of identity: the trusted adapter must verify it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchResponse {
    pub effective_session: SessionSelection,
}

/// Messages contain no supplied identifiers, provider output, or credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeError {
    MalformedJson,
    LimitExceeded,
    InvalidId,
    InvalidLabel,
    InvalidDirectory,
    DuplicateId,
    DuplicateOptionValue,
    InvalidOptionValue,
    InvalidDefault,
    StaleCatalogue,
    UnknownRuntime,
    UnknownAccount,
    ProviderConnectionMismatch,
    AuthenticationUnavailable,
    UnknownModel,
    UnknownOption,
    UnsupportedOptionValue,
    MissingOption,
    UnauthorizedSelection,
    UnresolvedSelection,
    SelectionMismatch,
    InstanceMismatch,
    CheckoutMismatch,
    ProviderSessionMismatch,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MalformedJson => "invalid runtime JSON payload",
            Self::LimitExceeded => "runtime contract limit exceeded",
            Self::InvalidId => "invalid runtime contract identifier",
            Self::InvalidLabel => "invalid runtime display text",
            Self::InvalidDirectory => "invalid adapter working directory",
            Self::DuplicateId => "duplicate runtime contract identifier",
            Self::DuplicateOptionValue => "duplicate supported option value",
            Self::InvalidOptionValue => "option value must be a bounded string or boolean",
            Self::InvalidDefault => "option default is not a supported value",
            Self::StaleCatalogue => "runtime catalogue revision changed; selection required",
            Self::UnknownRuntime => "selected runtime is unavailable",
            Self::UnknownAccount => "selected account is unavailable",
            Self::ProviderConnectionMismatch => {
                "selected provider connection does not match account"
            }
            Self::AuthenticationUnavailable => "selected account authentication is unavailable",
            Self::UnknownModel => "selected model is unavailable for account",
            Self::UnknownOption => "selected model option is unknown",
            Self::UnsupportedOptionValue => "selected model option value is unsupported",
            Self::MissingOption => "model option requires an explicit value",
            Self::UnauthorizedSelection => "repository policy does not permit selected tuple",
            Self::UnresolvedSelection => "session selection has unresolved option defaults",
            Self::SelectionMismatch => {
                "effective runtime selection differs from recorded selection"
            }
            Self::InstanceMismatch => "session belongs to a different execution instance",
            Self::CheckoutMismatch => "session belongs to a different checkout",
            Self::ProviderSessionMismatch => "provider session differs from recorded session",
        };
        f.write_str(message)
    }
}

impl std::error::Error for RuntimeError {}

/// Decode one size-limited JSON frame without exposing serde's input excerpts.
/// Semantic validation is still required; cap the transport before buffering.
pub fn decode_payload<T: de::DeserializeOwned>(bytes: &[u8]) -> Result<T, RuntimeError> {
    if bytes.len() > MAX_JSON_BYTES {
        return Err(RuntimeError::LimitExceeded);
    }
    serde_json::from_slice(bytes).map_err(|_| RuntimeError::MalformedJson)
}

/// Check a decoded describe payload before host-side directory authorization.
pub fn validate_describe_request(request: &DescribeRequest) -> Result<(), RuntimeError> {
    validate_directory(&request.directory)
}

/// Validate all catalogue entries, including unavailable accounts. IDs are
/// unique within their parent; connection IDs may be shared by accounts.
pub fn validate_catalogue(catalogue: &RuntimeCatalogue) -> Result<(), RuntimeError> {
    validate_id(&catalogue.revision)?;
    let mut remaining = MAX_CATALOGUE_ITEMS;
    validate_items(&catalogue.runtimes, MAX_RUNTIMES, &mut remaining)?;
    for runtime in &catalogue.runtimes {
        validate_label(&runtime.label)?;
        validate_items(&runtime.accounts, MAX_ACCOUNTS, &mut remaining)?;
        for account in &runtime.accounts {
            validate_account(account, &mut remaining)?;
        }
    }
    Ok(())
}

fn validate_account(account: &AccountInfo, remaining: &mut usize) -> Result<(), RuntimeError> {
    validate_label(&account.label)?;
    validate_id(&account.provider_connection_id)?;
    if let Some(summary) = &account.identity_summary {
        validate_label(summary)?;
    }
    validate_items(&account.models, MAX_MODELS, remaining)?;
    for model in &account.models {
        validate_label(&model.label)?;
        validate_items(&model.options, MAX_OPTIONS, remaining)?;
        for option in &model.options {
            validate_option(option, remaining)?;
        }
    }
    Ok(())
}

fn validate_option(option: &OptionDescriptor, remaining: &mut usize) -> Result<(), RuntimeError> {
    consume_items(option.values.len(), MAX_OPTION_VALUES, remaining)?;
    if option.values.is_empty() {
        return Err(RuntimeError::InvalidOptionValue);
    }
    for (index, value) in option.values.iter().enumerate() {
        validate_option_value(value)?;
        if option.values[..index].contains(value) {
            return Err(RuntimeError::DuplicateOptionValue);
        }
    }
    if let Some(default) = &option.default {
        validate_option_value(default)?;
        if !option.values.contains(default) {
            return Err(RuntimeError::InvalidDefault);
        }
    }
    Ok(())
}

/// Resolve only option defaults. Runtime, account, connection and model must
/// already be explicit. A descriptor without a default requires a value.
pub fn validate_selection(
    selection: &LaunchSelection,
    catalogue: &RuntimeCatalogue,
    policy: Option<&RuntimePolicy>,
) -> Result<LaunchSelection, RuntimeError> {
    validate_catalogue(catalogue)?;
    validate_selection_shape(selection)?;
    if selection.catalogue_revision != catalogue.revision {
        return Err(RuntimeError::StaleCatalogue);
    }
    let runtime = catalogue
        .runtimes
        .iter()
        .find(|r| r.id == selection.runtime_id)
        .ok_or(RuntimeError::UnknownRuntime)?;
    let account = runtime
        .accounts
        .iter()
        .find(|a| a.id == selection.account_id)
        .ok_or(RuntimeError::UnknownAccount)?;
    if account.provider_connection_id != selection.provider_connection_id {
        return Err(RuntimeError::ProviderConnectionMismatch);
    }
    if account.auth_status != AuthStatus::Authenticated {
        return Err(RuntimeError::AuthenticationUnavailable);
    }
    let model = account
        .models
        .iter()
        .find(|m| m.id == selection.model_id)
        .ok_or(RuntimeError::UnknownModel)?;
    validate_policy(selection, policy)?;
    resolve_options(selection, model)
}

fn resolve_options(
    selection: &LaunchSelection,
    model: &ModelInfo,
) -> Result<LaunchSelection, RuntimeError> {
    for (id, value) in &selection.options {
        let descriptor = model
            .options
            .iter()
            .find(|option| option.id == *id)
            .ok_or(RuntimeError::UnknownOption)?;
        if !descriptor.values.contains(value) {
            return Err(RuntimeError::UnsupportedOptionValue);
        }
    }
    let mut resolved = selection.clone();
    for descriptor in &model.options {
        if !resolved.options.contains_key(&descriptor.id) {
            let default = descriptor
                .default
                .clone()
                .ok_or(RuntimeError::MissingOption)?;
            resolved.options.insert(descriptor.id.clone(), default);
        }
    }
    Ok(resolved)
}

/// Revalidate the persisted selection against current availability and policy.
/// Compare the attempted resume with the authoritative record, including both
/// host identities and the provider session ID. Never rebase a stale revision.
pub fn validate_resume(
    recorded: &SessionSelection,
    attempted: &SessionSelection,
    catalogue: &RuntimeCatalogue,
    policy: Option<&RuntimePolicy>,
) -> Result<(), RuntimeError> {
    validate_session_shape(recorded)?;
    validate_session_shape(attempted)?;
    compare_sessions(recorded, attempted, false)?;
    validate_resolved_session(recorded, catalogue, policy)
}

/// Build the adapter stdin payload from a resolved, persistable session. This
/// helper never chooses identities or re-resolves defaults for an existing run.
pub fn build_launch_request(
    directory: &str,
    session: &SessionSelection,
    catalogue: &RuntimeCatalogue,
    policy: Option<&RuntimePolicy>,
) -> Result<LaunchRequest, RuntimeError> {
    validate_directory(directory)?;
    validate_resolved_session(session, catalogue, policy)?;
    Ok(LaunchRequest {
        directory: directory.to_owned(),
        session: session.clone(),
        policy: policy.cloned(),
    })
}

/// Recheck a decoded request against the host's current catalogue and policy.
/// The host must independently resolve policy; the payload is not authority.
pub fn validate_launch_request(
    request: &LaunchRequest,
    catalogue: &RuntimeCatalogue,
    host_policy: Option<&RuntimePolicy>,
) -> Result<(), RuntimeError> {
    validate_directory(&request.directory)?;
    if request.policy.as_ref() != host_policy {
        return Err(RuntimeError::UnauthorizedSelection);
    }
    validate_resolved_session(&request.session, catalogue, host_policy)
}

/// Accept only the requested effective tuple and host bindings. A first launch
/// may add a provider session ID; a resume must retain the recorded ID exactly.
/// Persist the returned session before acknowledging launch to the client.
pub fn validate_launch_response(
    request: &LaunchRequest,
    response: &LaunchResponse,
    catalogue: &RuntimeCatalogue,
    host_policy: Option<&RuntimePolicy>,
) -> Result<SessionSelection, RuntimeError> {
    validate_launch_request(request, catalogue, host_policy)?;
    validate_session_shape(&response.effective_session)?;
    compare_sessions(&request.session, &response.effective_session, true)?;
    Ok(response.effective_session.clone())
}

fn validate_resolved_session(
    session: &SessionSelection,
    catalogue: &RuntimeCatalogue,
    policy: Option<&RuntimePolicy>,
) -> Result<(), RuntimeError> {
    validate_session_shape(session)?;
    let resolved = validate_selection(&session.selection, catalogue, policy)?;
    if resolved != session.selection {
        return Err(RuntimeError::UnresolvedSelection);
    }
    Ok(())
}

fn compare_sessions(
    expected: &SessionSelection,
    effective: &SessionSelection,
    allow_new_provider_session: bool,
) -> Result<(), RuntimeError> {
    if expected.instance_id != effective.instance_id {
        return Err(RuntimeError::InstanceMismatch);
    }
    if expected.checkout_id != effective.checkout_id {
        return Err(RuntimeError::CheckoutMismatch);
    }
    if expected.selection != effective.selection {
        return Err(RuntimeError::SelectionMismatch);
    }
    if expected.provider_session_id != effective.provider_session_id
        && !(allow_new_provider_session && expected.provider_session_id.is_none())
    {
        return Err(RuntimeError::ProviderSessionMismatch);
    }
    Ok(())
}

fn validate_policy(
    selection: &LaunchSelection,
    policy: Option<&RuntimePolicy>,
) -> Result<(), RuntimeError> {
    let Some(policy) = policy else {
        return Ok(());
    };
    if policy.permitted.len() > MAX_POLICY_ENTRIES {
        return Err(RuntimeError::LimitExceeded);
    }
    for entry in &policy.permitted {
        validate_id(&entry.runtime_id)?;
        validate_id(&entry.provider_connection_id)?;
        if let Some(id) = &entry.account_id {
            validate_id(id)?;
        }
        if let Some(id) = &entry.model_id {
            validate_id(id)?;
        }
    }
    let permitted = policy.permitted.iter().any(|entry| {
        entry.runtime_id == selection.runtime_id
            && entry.provider_connection_id == selection.provider_connection_id
            && entry
                .account_id
                .as_ref()
                .is_none_or(|id| id == &selection.account_id)
            && entry
                .model_id
                .as_ref()
                .is_none_or(|id| id == &selection.model_id)
    });
    if !permitted {
        return Err(RuntimeError::UnauthorizedSelection);
    }
    Ok(())
}

fn validate_selection_shape(selection: &LaunchSelection) -> Result<(), RuntimeError> {
    for id in [
        &selection.runtime_id,
        &selection.account_id,
        &selection.provider_connection_id,
        &selection.model_id,
        &selection.catalogue_revision,
    ] {
        validate_id(id)?;
    }
    if selection.options.len() > MAX_OPTIONS {
        return Err(RuntimeError::LimitExceeded);
    }
    for (id, value) in &selection.options {
        validate_id(id)?;
        validate_option_value(value)?;
    }
    Ok(())
}

fn validate_session_shape(session: &SessionSelection) -> Result<(), RuntimeError> {
    validate_id(&session.instance_id)?;
    validate_id(&session.checkout_id)?;
    if let Some(id) = &session.provider_session_id {
        validate_id(id)?;
    }
    validate_selection_shape(&session.selection)
}

fn validate_id(id: &str) -> Result<(), RuntimeError> {
    if id.is_empty()
        || id.len() > MAX_ID_BYTES
        || !id.as_bytes()[0].is_ascii_alphanumeric()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:/@+-".contains(&b))
    {
        return Err(RuntimeError::InvalidId);
    }
    Ok(())
}

fn validate_label(label: &str) -> Result<(), RuntimeError> {
    if label.is_empty()
        || label.len() > MAX_LABEL_BYTES
        || label.trim() != label
        || label.chars().any(char::is_control)
    {
        return Err(RuntimeError::InvalidLabel);
    }
    Ok(())
}

fn validate_directory(directory: &str) -> Result<(), RuntimeError> {
    if directory.len() > MAX_DIRECTORY_BYTES
        || !Path::new(directory).is_absolute()
        || directory.chars().any(char::is_control)
    {
        return Err(RuntimeError::InvalidDirectory);
    }
    Ok(())
}

fn validate_option_value(value: &Value) -> Result<(), RuntimeError> {
    match value {
        Value::Bool(_) => Ok(()),
        Value::String(text)
            if text.len() <= MAX_LABEL_BYTES && !text.chars().any(char::is_control) =>
        {
            Ok(())
        }
        _ => Err(RuntimeError::InvalidOptionValue),
    }
}

trait Identified {
    fn id(&self) -> &str;
}
impl Identified for RuntimeInfo {
    fn id(&self) -> &str {
        &self.id
    }
}
impl Identified for AccountInfo {
    fn id(&self) -> &str {
        &self.id
    }
}
impl Identified for ModelInfo {
    fn id(&self) -> &str {
        &self.id
    }
}
impl Identified for OptionDescriptor {
    fn id(&self) -> &str {
        &self.id
    }
}

fn validate_items<T: Identified>(
    items: &[T],
    maximum: usize,
    remaining: &mut usize,
) -> Result<(), RuntimeError> {
    consume_items(items.len(), maximum, remaining)?;
    let mut ids = BTreeSet::new();
    for item in items {
        validate_id(item.id())?;
        if !ids.insert(item.id()) {
            return Err(RuntimeError::DuplicateId);
        }
    }
    Ok(())
}

fn consume_items(count: usize, maximum: usize, remaining: &mut usize) -> Result<(), RuntimeError> {
    if count > maximum || count > *remaining {
        return Err(RuntimeError::LimitExceeded);
    }
    *remaining -= count;
    Ok(())
}

// serde's BTreeMap implementation overwrites duplicate keys. A launch must
// reject ambiguity rather than silently retaining the last selected value.
fn deserialize_options<'de, D>(deserializer: D) -> Result<BTreeMap<String, Value>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptionsVisitor;
    impl<'de> de::Visitor<'de> for OptionsVisitor {
        type Value = BTreeMap<String, Value>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a bounded map of unique option identifiers")
        }

        fn visit_map<M: de::MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut options = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, Value>()? {
                if options.len() >= MAX_OPTIONS || options.contains_key(&key) {
                    return Err(de::Error::custom("duplicate or excessive model options"));
                }
                validate_id(&key).map_err(de::Error::custom)?;
                validate_option_value(&value).map_err(de::Error::custom)?;
                options.insert(key, value);
            }
            Ok(options)
        }
    }
    deserializer.deserialize_map(OptionsVisitor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalogue() -> RuntimeCatalogue {
        RuntimeCatalogue {
            revision: "revision-1".into(),
            runtimes: vec![RuntimeInfo {
                id: "runtime-a".into(),
                label: "Runtime A".into(),
                accounts: vec![AccountInfo {
                    id: "account-a".into(),
                    label: "Work".into(),
                    provider_connection_id: "connection-a".into(),
                    auth_status: AuthStatus::Authenticated,
                    identity_summary: Some("Verified work account".into()),
                    billing_kind: BillingKind::Subscription,
                    models: vec![ModelInfo {
                        id: "provider/model-a".into(),
                        label: "Model A".into(),
                        options: vec![
                            OptionDescriptor {
                                id: "effort".into(),
                                values: vec![json!("low"), json!("high")],
                                default: Some(json!("low")),
                            },
                            OptionDescriptor {
                                id: "fast".into(),
                                values: vec![json!(false), json!(true)],
                                default: Some(json!(false)),
                            },
                        ],
                    }],
                }],
            }],
        }
    }

    fn selection() -> LaunchSelection {
        LaunchSelection {
            runtime_id: "runtime-a".into(),
            account_id: "account-a".into(),
            provider_connection_id: "connection-a".into(),
            model_id: "provider/model-a".into(),
            options: BTreeMap::new(),
            catalogue_revision: "revision-1".into(),
        }
    }

    fn session() -> SessionSelection {
        SessionSelection {
            selection: validate_selection(&selection(), &catalogue(), None).unwrap(),
            provider_session_id: Some("provider-session-a".into()),
            instance_id: "instance-a".into(),
            checkout_id: "checkout-a".into(),
        }
    }

    fn policy(account_id: Option<&str>) -> RuntimePolicy {
        RuntimePolicy {
            permitted: vec![PermittedTuple {
                runtime_id: "runtime-a".into(),
                provider_connection_id: "connection-a".into(),
                account_id: account_id.map(str::to_owned),
                model_id: Some("provider/model-a".into()),
            }],
        }
    }

    #[test]
    fn defaults_are_explicit_deterministic_and_round_trip() {
        let resolved = validate_selection(&selection(), &catalogue(), None).unwrap();
        assert_eq!(
            resolved.options,
            BTreeMap::from([
                ("effort".into(), json!("low")),
                ("fast".into(), json!(false)),
            ])
        );
        let encoded = serde_json::to_vec(&resolved).unwrap();
        let decoded: LaunchSelection = decode_payload(&encoded).unwrap();
        assert_eq!(decoded, resolved);
        assert_eq!(
            validate_selection(&decoded, &catalogue(), None).unwrap(),
            resolved
        );
    }

    #[test]
    fn no_runtime_account_connection_or_model_fallback() {
        type Case = (fn(&mut LaunchSelection), RuntimeError);
        let cases: [Case; 5] = [
            (
                |s| s.runtime_id = "absent".into(),
                RuntimeError::UnknownRuntime,
            ),
            (
                |s| s.account_id = "absent".into(),
                RuntimeError::UnknownAccount,
            ),
            (
                |s| s.provider_connection_id = "absent".into(),
                RuntimeError::ProviderConnectionMismatch,
            ),
            (|s| s.model_id = "absent".into(), RuntimeError::UnknownModel),
            (
                |s| s.catalogue_revision = "stale".into(),
                RuntimeError::StaleCatalogue,
            ),
        ];
        for (change, expected) in cases {
            let mut selected = selection();
            change(&mut selected);
            assert_eq!(
                validate_selection(&selected, &catalogue(), None),
                Err(expected)
            );
        }
    }

    #[test]
    fn unknown_and_unauthenticated_accounts_cannot_launch() {
        for status in [AuthStatus::Unknown, AuthStatus::Unauthenticated] {
            let mut current = catalogue();
            current.runtimes[0].accounts[0].auth_status = status;
            assert_eq!(
                validate_selection(&selection(), &current, None),
                Err(RuntimeError::AuthenticationUnavailable)
            );
        }
    }

    #[test]
    fn catalogue_rejects_duplicate_ids_at_every_scope() {
        for scope in 0..4 {
            let mut current = catalogue();
            match scope {
                0 => current.runtimes.push(current.runtimes[0].clone()),
                1 => {
                    let runtime = &mut current.runtimes[0];
                    runtime.accounts.push(runtime.accounts[0].clone());
                }
                2 => {
                    let account = &mut current.runtimes[0].accounts[0];
                    account.models.push(account.models[0].clone());
                }
                _ => {
                    let model = &mut current.runtimes[0].accounts[0].models[0];
                    model.options.push(model.options[0].clone());
                }
            }
            assert_eq!(validate_catalogue(&current), Err(RuntimeError::DuplicateId));
        }
    }

    #[test]
    fn catalogue_rejects_ambiguous_values_and_invalid_defaults() {
        let cases = [
            (
                vec![json!(true), json!(true)],
                Some(json!(true)),
                RuntimeError::DuplicateOptionValue,
            ),
            (
                vec![json!(true)],
                Some(json!(false)),
                RuntimeError::InvalidDefault,
            ),
            (vec![], None, RuntimeError::InvalidOptionValue),
            (
                vec![json!({"nested": true})],
                None,
                RuntimeError::InvalidOptionValue,
            ),
            (vec![json!(1)], None, RuntimeError::InvalidOptionValue),
        ];
        for (values, default, error) in cases {
            let mut current = catalogue();
            let option = &mut current.runtimes[0].accounts[0].models[0].options[0];
            option.values = values;
            option.default = default;
            assert_eq!(validate_catalogue(&current), Err(error));
        }
    }

    #[test]
    fn options_are_model_scoped_and_values_are_typed() {
        let mut current = catalogue();
        current.runtimes[0].accounts[0].models.push(ModelInfo {
            id: "model-b".into(),
            label: "Model B".into(),
            options: vec![],
        });
        let mut selected = selection();
        selected.options.insert("fast".into(), json!(true));
        let resolved = validate_selection(&selected, &current, None).unwrap();
        assert_eq!(resolved.options["fast"], json!(true));
        selected.options.insert("fast".into(), json!("true"));
        assert_eq!(
            validate_selection(&selected, &current, None),
            Err(RuntimeError::UnsupportedOptionValue)
        );
        selected.options.insert("fast".into(), json!(1));
        assert_eq!(
            validate_selection(&selected, &current, None),
            Err(RuntimeError::InvalidOptionValue)
        );
        selected.options.insert("fast".into(), json!(true));
        selected.model_id = "model-b".into();
        assert_eq!(
            validate_selection(&selected, &current, None),
            Err(RuntimeError::UnknownOption)
        );
    }

    #[test]
    fn no_default_requires_an_explicit_choice() {
        let mut current = catalogue();
        current.runtimes[0].accounts[0].models[0].options[0].default = None;
        assert_eq!(
            validate_selection(&selection(), &current, None),
            Err(RuntimeError::MissingOption)
        );
    }

    #[test]
    fn repository_policy_controls_account_connection_and_model() {
        assert!(
            validate_selection(&selection(), &catalogue(), Some(&policy(Some("account-a"))))
                .is_ok()
        );
        assert!(validate_selection(&selection(), &catalogue(), Some(&policy(None))).is_ok());
        let mut denied = policy(Some("account-b"));
        assert_eq!(
            validate_selection(&selection(), &catalogue(), Some(&denied)),
            Err(RuntimeError::UnauthorizedSelection)
        );
        denied.permitted[0].account_id = None;
        denied.permitted[0].provider_connection_id = "connection-b".into();
        assert_eq!(
            validate_selection(&selection(), &catalogue(), Some(&denied)),
            Err(RuntimeError::UnauthorizedSelection)
        );
        denied.permitted[0].provider_connection_id = "connection-a".into();
        denied.permitted[0].model_id = Some("model-b".into());
        assert_eq!(
            validate_selection(&selection(), &catalogue(), Some(&denied)),
            Err(RuntimeError::UnauthorizedSelection)
        );
        denied.permitted.clear();
        assert_eq!(
            validate_selection(&selection(), &catalogue(), Some(&denied)),
            Err(RuntimeError::UnauthorizedSelection)
        );
    }

    #[test]
    fn changed_defaults_never_replace_a_persisted_value() {
        let recorded = session();
        let mut current = catalogue();
        current.runtimes[0].accounts[0].models[0].options[0].default = Some(json!("high"));
        assert_eq!(
            validate_selection(&recorded.selection, &current, None).unwrap(),
            recorded.selection
        );
        assert!(validate_resume(&recorded, &recorded, &current, None).is_ok());
        current.revision = "revision-2".into();
        assert_eq!(
            validate_resume(&recorded, &recorded, &current, None),
            Err(RuntimeError::StaleCatalogue)
        );
        let mut missing = recorded.clone();
        missing.selection.options.clear();
        assert_eq!(
            validate_resume(&missing, &missing, &catalogue(), None),
            Err(RuntimeError::UnresolvedSelection)
        );
    }

    #[test]
    fn resume_refuses_adoption_even_when_the_other_account_is_available() {
        let recorded = session();
        let mut current = catalogue();
        let mut other = current.runtimes[0].accounts[0].clone();
        other.id = "account-b".into();
        current.runtimes[0].accounts.push(other);
        let mut attempted = recorded.clone();
        attempted.selection.account_id = "account-b".into();
        assert!(validate_selection(&attempted.selection, &current, None).is_ok());
        assert_eq!(
            validate_resume(&recorded, &attempted, &current, None),
            Err(RuntimeError::SelectionMismatch)
        );
    }

    #[test]
    fn resume_preserves_provider_instance_checkout_options_and_session_id() {
        let recorded = session();
        type Case = (fn(&mut SessionSelection), RuntimeError);
        let cases: [Case; 7] = [
            (
                |s| s.selection.provider_connection_id = "connection-b".into(),
                RuntimeError::SelectionMismatch,
            ),
            (
                |s| s.selection.runtime_id = "runtime-b".into(),
                RuntimeError::SelectionMismatch,
            ),
            (
                |s| s.selection.model_id = "model-b".into(),
                RuntimeError::SelectionMismatch,
            ),
            (
                |s| {
                    s.selection.options.insert("fast".into(), json!(true));
                },
                RuntimeError::SelectionMismatch,
            ),
            (
                |s| s.instance_id = "instance-b".into(),
                RuntimeError::InstanceMismatch,
            ),
            (
                |s| s.checkout_id = "checkout-b".into(),
                RuntimeError::CheckoutMismatch,
            ),
            (
                |s| s.provider_session_id = None,
                RuntimeError::ProviderSessionMismatch,
            ),
        ];
        for (change, expected) in cases {
            let mut attempted = recorded.clone();
            change(&mut attempted);
            assert_eq!(
                validate_resume(&recorded, &attempted, &catalogue(), None),
                Err(expected)
            );
        }
        assert_eq!(
            validate_resume(
                &recorded,
                &recorded,
                &catalogue(),
                Some(&policy(Some("account-b")))
            ),
            Err(RuntimeError::UnauthorizedSelection)
        );
    }

    #[test]
    fn launch_payload_and_effective_metadata_round_trip_without_adoption() {
        let current = catalogue();
        let host_policy = policy(Some("account-a"));
        let request = build_launch_request(
            "/srv/checkouts/example",
            &session(),
            &current,
            Some(&host_policy),
        )
        .unwrap();
        let encoded = serde_json::to_vec(&request).unwrap();
        let decoded: LaunchRequest = decode_payload(&encoded).unwrap();
        assert_eq!(decoded, request);
        let mut response = LaunchResponse {
            effective_session: session(),
        };
        assert_eq!(
            validate_launch_response(&request, &response, &current, Some(&host_policy)).unwrap(),
            session()
        );
        response.effective_session.selection.account_id = "account-b".into();
        assert_eq!(
            validate_launch_response(&request, &response, &current, Some(&host_policy)),
            Err(RuntimeError::SelectionMismatch)
        );
        response.effective_session = session();
        response.effective_session.provider_session_id = Some("different".into());
        assert_eq!(
            validate_launch_response(&request, &response, &current, Some(&host_policy)),
            Err(RuntimeError::ProviderSessionMismatch)
        );
        assert_eq!(
            validate_launch_request(&request, &current, None),
            Err(RuntimeError::UnauthorizedSelection)
        );
        let mut stale = current;
        stale.revision = "revision-2".into();
        assert_eq!(
            validate_launch_request(&request, &stale, Some(&host_policy)),
            Err(RuntimeError::StaleCatalogue)
        );
    }

    #[test]
    fn first_launch_can_record_a_new_provider_session_only() {
        let mut first = session();
        first.provider_session_id = None;
        let request = build_launch_request("/srv/example", &first, &catalogue(), None).unwrap();
        let mut response = LaunchResponse {
            effective_session: session(),
        };
        assert!(validate_launch_response(&request, &response, &catalogue(), None).is_ok());
        response.effective_session.instance_id = "instance-b".into();
        assert_eq!(
            validate_launch_response(&request, &response, &catalogue(), None),
            Err(RuntimeError::InstanceMismatch)
        );
        assert_eq!(
            build_launch_request("relative/path", &first, &catalogue(), None),
            Err(RuntimeError::InvalidDirectory)
        );
        first.selection.options.clear();
        assert_eq!(
            build_launch_request("/srv/example", &first, &catalogue(), None),
            Err(RuntimeError::UnresolvedSelection)
        );
    }

    #[test]
    fn launch_json_rejects_unknown_fields_and_duplicate_options() {
        let encoded = serde_json::to_vec(&selection()).unwrap();
        let valid = String::from_utf8(encoded).unwrap();
        let duplicate = valid.replace(
            "\"options\":{}",
            "\"options\":{\"fast\":true,\"fast\":false}",
        );
        assert_eq!(
            decode_payload::<LaunchSelection>(duplicate.as_bytes()),
            Err(RuntimeError::MalformedJson)
        );
        let mut unknown = serde_json::to_value(selection()).unwrap();
        unknown["command"] = json!("untrusted executable");
        assert_eq!(
            decode_payload::<LaunchSelection>(&serde_json::to_vec(&unknown).unwrap()),
            Err(RuntimeError::MalformedJson)
        );
        let mut request = serde_json::to_value(
            build_launch_request("/srv/example", &session(), &catalogue(), None).unwrap(),
        )
        .unwrap();
        request["environment"] = json!({"ACCOUNT": "other"});
        assert_eq!(
            decode_payload::<LaunchRequest>(&serde_json::to_vec(&request).unwrap()),
            Err(RuntimeError::MalformedJson)
        );
    }

    #[test]
    fn limits_apply_to_frames_text_each_collection_and_total_items() {
        assert_eq!(
            decode_payload::<RuntimeCatalogue>(&vec![b' '; MAX_JSON_BYTES + 1]),
            Err(RuntimeError::LimitExceeded)
        );
        assert_eq!(
            validate_id(&"a".repeat(MAX_ID_BYTES + 1)),
            Err(RuntimeError::InvalidId)
        );
        assert_eq!(validate_id("bad\nidentifier"), Err(RuntimeError::InvalidId));
        assert_eq!(
            validate_label(&"a".repeat(MAX_LABEL_BYTES + 1)),
            Err(RuntimeError::InvalidLabel)
        );
        assert_eq!(
            validate_label("terminal\u{1b}escape"),
            Err(RuntimeError::InvalidLabel)
        );
        assert_eq!(
            validate_option_value(&json!([true])),
            Err(RuntimeError::InvalidOptionValue)
        );
        let mut current = catalogue();
        current.runtimes = vec![current.runtimes[0].clone(); MAX_RUNTIMES + 1];
        assert_eq!(
            validate_catalogue(&current),
            Err(RuntimeError::LimitExceeded)
        );
        let mut current = catalogue();
        let runtime = &mut current.runtimes[0];
        runtime.accounts = vec![runtime.accounts[0].clone(); MAX_ACCOUNTS + 1];
        assert_eq!(
            validate_catalogue(&current),
            Err(RuntimeError::LimitExceeded)
        );
        let mut current = catalogue();
        let account = &mut current.runtimes[0].accounts[0];
        account.models = vec![account.models[0].clone(); MAX_MODELS + 1];
        assert_eq!(
            validate_catalogue(&current),
            Err(RuntimeError::LimitExceeded)
        );
        let mut current = catalogue();
        let model = &mut current.runtimes[0].accounts[0].models[0];
        model.options = vec![model.options[0].clone(); MAX_OPTIONS + 1];
        assert_eq!(
            validate_catalogue(&current),
            Err(RuntimeError::LimitExceeded)
        );
        let mut current = catalogue();
        current.runtimes[0].accounts[0].models[0].options[0].values =
            vec![json!(true); MAX_OPTION_VALUES + 1];
        assert_eq!(
            validate_catalogue(&current),
            Err(RuntimeError::LimitExceeded)
        );
        let mut remaining = 1;
        assert_eq!(
            consume_items(2, MAX_MODELS, &mut remaining),
            Err(RuntimeError::LimitExceeded)
        );
        let mut current = catalogue();
        let template = current.runtimes[0].accounts[0].models[0].clone();
        let account = &mut current.runtimes[0].accounts[0];
        account.models = (0..MAX_MODELS)
            .map(|index| {
                let mut model = template.clone();
                model.id = format!("model-{index}");
                model.options = (0..MAX_OPTIONS)
                    .map(|index| OptionDescriptor {
                        id: format!("option-{index}"),
                        values: vec![json!(true), json!(false)],
                        default: None,
                    })
                    .collect();
                model
            })
            .collect();
        assert_eq!(
            validate_catalogue(&current),
            Err(RuntimeError::LimitExceeded)
        );
    }
}

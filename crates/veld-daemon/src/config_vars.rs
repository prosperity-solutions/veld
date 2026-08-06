//! Machine-overridable vars over HTTP, for the management UI and Desktop.
//!
//! The CLI can ask a human at a terminal; a run started from the UI cannot —
//! there is no TTY, and the safe branch of `veld start` refuses rather than
//! guessing. That refusal is correct and it is also a dead end in a GUI, which
//! is what these endpoints exist to fix: the human is right there, so the UI
//! asks them and writes the answer through here.
//!
//! Three rules, all of them load-bearing:
//!
//! - **A value declared `secret` never travels to a client.** Reads return a
//!   description (`<secret, from environment variable PGPASS>`), exactly as
//!   `veld config vars` prints it. There is no "reveal" endpoint; the value's
//!   only egress remains a child process's environment.
//! - **Writes are CSRF-gated and refuse an undeclared var.** Storing an answer
//!   for a var no config declares would put a row in the database that every run
//!   silently ignores.
//! - **The project is identified the same way the CLI identifies it**
//!   ([`veld_core::project_id`]), so an answer given in the UI is the same answer
//!   `veld start` reads in a terminal, in any worktree.

use axum::{
    Json, Router,
    extract::Query,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::Deserialize;
use tracing::warn;
use veld_core::config::{self, ConfigValue, MachineVar, SecretSource};
use veld_core::db::OverrideScope;
use veld_core::project_id::{ProjectId, project_id_for};

use super::management::{check_csrf, open_db};

pub fn routes() -> Router {
    Router::new()
        .route(
            "/api/config/vars",
            get(get_vars).put(put_var).delete(delete_var),
        )
        .route("/api/config/vars/preflight", axum::routing::post(preflight))
}

/// Which project's vars. A path to any directory inside the checkout — the same
/// thing every other project-scoped endpoint takes.
#[derive(Deserialize)]
struct ProjectQuery {
    project: String,
}

/// The config plus the two identities a var override is keyed by.
struct Resolved {
    config: config::VeldConfig,
    project_root: std::path::PathBuf,
    project_id: ProjectId,
}

/// Find and parse the config for a client-supplied directory.
///
/// `discover_config` walks upward, so the UI may pass a worktree root or any
/// subdirectory of it and get the same answer the CLI would from that cwd.
fn resolve_project(project: &str) -> Result<Resolved, StatusCode> {
    let dir = std::path::PathBuf::from(project);
    if !dir.is_absolute() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let config_path = config::discover_config(&dir).map_err(|_| StatusCode::NOT_FOUND)?;
    let config = config::parse_config(&config_path).map_err(|e| {
        warn!(
            "config vars: {} failed to parse: {e}",
            config_path.display()
        );
        StatusCode::UNPROCESSABLE_ENTITY
    })?;
    let project_root = config::project_root(&config_path);
    let project_id = project_id_for(&project_root);
    Ok(Resolved {
        config,
        project_root,
        project_id,
    })
}

/// How a value is shown to a client. Mirrors the CLI's `describe` — a secret is
/// described, never printed.
fn describe(value: &ConfigValue) -> String {
    if value.secret {
        return format!("<secret, from {}>", value.source_label());
    }
    match value.as_literal() {
        Some(literal) => literal.to_owned(),
        None => format!("<from {}>", value.source_label()),
    }
}

/// Look up a var and insist the config declared it machine-overridable.
fn machine_var<'a>(
    config: &'a config::VeldConfig,
    name: &str,
) -> Result<&'a MachineVar, StatusCode> {
    config
        .vars
        .as_ref()
        .and_then(|v| v.get(name))
        .and_then(|d| d.machine())
        .ok_or(StatusCode::NOT_FOUND)
}

/// Every machine-overridable var, its effective value, and which scope it came
/// from.
async fn get_vars(Query(q): Query<ProjectQuery>) -> Result<Json<serde_json::Value>, StatusCode> {
    let r = resolve_project(&q.project)?;
    let db = open_db()?;
    let stored = db
        .effective_var_overrides(&r.project_id, &r.project_root)
        .map_err(|e| {
            warn!("failed to read var overrides: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut vars: Vec<serde_json::Value> = r
        .config
        .vars
        .iter()
        .flatten()
        .filter_map(|(name, decl)| decl.machine().map(|m| (name, m)))
        .map(|(name, m)| {
            let over = stored.get(name.as_str());
            let effective = over.map(|o| &o.value).or(m.default.as_ref());
            serde_json::json!({
                "name": name,
                "from": match over {
                    Some(o) => o.scope.as_str(),
                    None if m.default.is_some() => "default",
                    None => "unset",
                },
                "value": effective.map(describe),
                // Whether the stored answer is a pointer rather than a literal,
                // so the UI can show the right editor without ever holding the
                // value it points at.
                "isPointer": over.is_some_and(|o| o.value.as_literal().is_none()),
                "secret": m.secret,
                "default": m.default.as_ref().map(describe),
                "choices": m.choices,
                "description": m.description,
                "prompt": m.prompt,
            })
        })
        .collect();
    // `config.vars` is a HashMap, so sort or the UI list reorders on every poll.
    vars.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));

    Ok(Json(serde_json::json!({
        "projectId": r.project_id.as_str(),
        "worktree": r.project_root,
        "vars": vars,
    })))
}

/// Store one answer.
#[derive(Deserialize)]
struct PutVarBody {
    project: String,
    name: String,
    /// A literal value. Mutually exclusive with the pointer fields.
    #[serde(default)]
    value: Option<String>,
    /// Read from this environment variable at run start.
    #[serde(default)]
    env: Option<String>,
    /// Read from this file at run start.
    #[serde(default)]
    file: Option<String>,
    /// Run this and take its stdout at run start.
    #[serde(default)]
    shell: Option<String>,
    /// `true` to scope the answer to this checkout instead of the project.
    #[serde(default)]
    worktree: bool,
}

async fn put_var(
    headers: HeaderMap,
    Json(body): Json<PutVarBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    check_csrf(&headers)?;
    let r = resolve_project(&body.project)?;
    let machine = machine_var(&r.config, &body.name)?;

    // Sensitivity comes from the declaration, never from the client: a UI that
    // forgot to send the flag must not be able to downgrade a secret var into a
    // value this API will happily echo back.
    let secret = machine.secret;
    let source = match (
        body.value.as_deref(),
        body.env.as_deref(),
        body.file.as_deref(),
        body.shell.as_deref(),
    ) {
        (Some(v), None, None, None) => SecretSource::Literal(v.to_owned()),
        (None, Some(v), None, None) => SecretSource::Env(v.to_owned()),
        (None, None, Some(v), None) => SecretSource::File(v.to_owned()),
        (None, None, None, Some(v)) => SecretSource::Shell(v.to_owned()),
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    let value = ConfigValue { source, secret };

    // A literal is checkable now; a pointer's value is not known until run start
    // and is checked there.
    if let Some(choices) = machine.choices.as_ref().filter(|c| !c.is_empty())
        && let Some(literal) = value.as_literal()
        && !choices.iter().any(|c| c == literal)
    {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }

    let scope = if body.worktree {
        OverrideScope::Worktree
    } else {
        OverrideScope::Project
    };
    let db = open_db()?;
    db.set_var_override(&r.project_id, scope, &r.project_root, &body.name, &value)
        .map_err(|e| {
            warn!("failed to store var override: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(serde_json::json!({
        "name": body.name,
        "scope": scope.as_str(),
        "value": describe(&value),
    })))
}

/// What a start would need, so the UI can ask before it fires one.
#[derive(Deserialize)]
struct PreflightBody {
    project: String,
    /// A preset name, as `veld start --preset` takes.
    #[serde(default)]
    preset: Option<String>,
    /// Explicit `node:variant` selections. Ignored when `preset` is set.
    #[serde(default)]
    selections: Vec<String>,
}

/// Which machine-overridable vars *this* start would need and this machine has
/// not answered.
///
/// Scoped to the plan, not to the whole config, and expanded through
/// `build_execution_plan` — the same call `veld start` makes. Two consequences,
/// both required: a var only a transitive dependency uses is reported (the case
/// that would otherwise fail after three nodes are already up), and a var no
/// selected node touches is *not* (so starting the docs site does not demand the
/// database password).
///
/// A read, but a POST: the selection list is unbounded and belongs in a body.
async fn preflight(Json(body): Json<PreflightBody>) -> Result<Json<serde_json::Value>, StatusCode> {
    let r = resolve_project(&body.project)?;

    let selections: Vec<veld_core::graph::NodeSelection> = if let Some(preset) = &body.preset {
        let Some(chosen) = veld_core::presets::find_by_name_then_key(&r.config, preset) else {
            return Err(StatusCode::NOT_FOUND);
        };
        veld_core::graph::expand_preset(&chosen.name, &r.config)
            .map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?
    } else {
        let parsed: Result<Vec<_>, _> = body
            .selections
            .iter()
            .map(|s| veld_core::graph::parse_selection(s))
            .collect();
        parsed.map_err(|_| StatusCode::BAD_REQUEST)?
    };

    let resolved = veld_core::graph::resolve_selections(&selections, &r.config)
        .map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
    let plan = veld_core::graph::build_execution_plan(&resolved, &r.config)
        .map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
    let planned: Vec<veld_core::graph::NodeSelection> = plan.into_iter().flatten().collect();

    let db = open_db()?;
    let stored = db
        .effective_var_overrides(&r.project_id, &r.project_root)
        .map_err(|e| {
            warn!("failed to read var overrides: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let overrides: veld_core::values::VarOverrides =
        stored.into_iter().map(|(k, v)| (k, v.value)).collect();

    let missing = veld_core::values::unanswered_machine_vars(&r.config, &overrides, &planned);
    Ok(Json(serde_json::json!({
        "needed": missing.iter().map(|v| serde_json::json!({
            "name": v.name,
            "question": v.question(),
            "choices": v.choices,
            "secret": v.secret,
            "stale": v.stale.is_some(),
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct DeleteVarQuery {
    project: String,
    name: String,
    #[serde(default)]
    worktree: bool,
}

async fn delete_var(
    headers: HeaderMap,
    Query(q): Query<DeleteVarQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    check_csrf(&headers)?;
    let r = resolve_project(&q.project)?;
    let scope = if q.worktree {
        OverrideScope::Worktree
    } else {
        OverrideScope::Project
    };
    let db = open_db()?;
    // Deliberately not gated on the var still being declared: a var that lost
    // its `machine` block leaves a row behind, and refusing to delete it would
    // strand the row with no way to clear it from the UI.
    let removed = db
        .unset_var_override(&r.project_id, scope, &r.project_root, &q.name)
        .map_err(|e| {
            warn!("failed to clear var override: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(serde_json::json!({
        "name": q.name,
        "scope": scope.as_str(),
        "removed": removed,
    })))
}

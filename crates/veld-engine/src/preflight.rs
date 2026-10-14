//! Everything the engine decides about a request **before** anything spawns:
//! which nodes, which run, which answers are missing, which env names the face
//! must supply.
//!
//! This used to be the first half of `veld start`, `veld restart` and `veld stop`
//! in the CLI. It moved here so every face asks the same questions and gets the
//! same refusals — the IDE's start button no longer execs a CLI to find out.
//! What stayed in the face is only what needs a human: the picker and the
//! prompts, both fed by what this returns.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use veld_core::config::{self, SecretSource, StepType, VeldConfig};
use veld_core::db::Db;
use veld_core::graph::{self, NodeSelection};
use veld_core::state::ProjectState;

use crate::api::{
    ActionKind, AnswerProvenance, Choice, Preflight, PreflightPlan, PreflightRequest,
    PreflightResponse, VarAnswer,
};
use crate::orchestrator::{Orchestrator, VarProvenance};

fn refused(message: impl Into<String>) -> PreflightResponse {
    PreflightResponse::Refused {
        message: message.into(),
        payload: None,
    }
}

/// Load `veld.json` from a project root. Structural parsing only — stop and
/// status must keep working against a config that has since been broken.
pub fn load_config(project_root: &Path) -> Result<(PathBuf, VeldConfig), String> {
    let path = config::root_config_in(project_root)
        .ok_or_else(|| format!("No veld.json in {}", project_root.display()))?;
    let cfg = config::parse_config(&path).map_err(|e| format!("Failed to load config: {e}"))?;
    Ok((path, cfg))
}

/// The run a name-less `stop`/`restart`/`status` means, or why there is none.
///
/// Two-tiered: if exactly one environment has a *live* run, that one; only when
/// none is live, a sole environment (stopped ones persist as history, so `veld
/// restart` finds last night's crashed run). The tiers are deliberately not
/// collapsed — a crashed `dev` next to a running `staging` must not turn a bare
/// `veld stop` into an ambiguity error.
///
/// `Ok((name, note))`: `note` is the "Using environment …" line to show when the
/// name was inferred.
pub fn resolve_run_name(
    name: Option<String>,
    state: &ProjectState,
    include_stopped: bool,
) -> Result<(String, Option<String>), String> {
    if let Some(n) = name {
        return Ok((n, None));
    }
    let mut live: Vec<&String> = state
        .runs
        .iter()
        .filter(|(_, r)| r.is_live())
        .map(|(name, _)| name)
        .collect();
    match live.len() {
        1 => {
            let resolved = live[0].clone();
            let note = format!("Using environment '{resolved}' (only live environment).");
            return Ok((resolved, Some(note)));
        }
        n if n > 1 => {
            live.sort();
            let names: Vec<&str> = live.iter().map(|s| s.as_str()).collect();
            return Err(format!(
                "Multiple live environments found. Specify one with --name: {}",
                names.join(", ")
            ));
        }
        _ => {}
    }
    if include_stopped && state.runs.len() == 1 {
        let resolved = state.runs.keys().next().expect("len is 1").clone();
        let note = format!("Using environment '{resolved}' (only environment).");
        return Ok((resolved, Some(note)));
    }
    if include_stopped && state.runs.len() > 1 {
        let mut names: Vec<&str> = state.runs.keys().map(|s| s.as_str()).collect();
        names.sort();
        return Err(format!(
            "Multiple environments found. Specify one with --name: {}",
            names.join(", ")
        ));
    }
    Err("No environments found. Start one with `veld start`.".to_owned())
}

/// Every env name `{ "env": … }` reads, in the config or in this machine's
/// stored var sources (`veld config set X --env NAME`).
///
/// A superset of what any one plan reaches, on purpose: working out reachability
/// is graph logic, and sending a value the plan does not read is harmless — the
/// face only ever sends names on this list, so nothing undeclared crosses.
pub fn env_names(config: &VeldConfig, db: &Db, project_root: &Path) -> Vec<String> {
    let mut names = BTreeSet::new();
    if let Ok(json) = serde_json::to_value(config) {
        collect_env_sources(&json, &mut names);
    }
    let project_id = veld_core::project_id::project_id_for(project_root);
    if let Ok(stored) = db.effective_var_overrides(&project_id, project_root) {
        for o in stored.values() {
            if let SecretSource::Env(name) = &o.value.source {
                names.insert(name.clone());
            }
        }
    }
    names.into_iter().collect()
}

/// A value source serializes as `{ "env": "NAME", … }`; a node's `env` *map*
/// serializes as an object, so only a string under `env` is a source.
fn collect_env_sources(v: &serde_json::Value, out: &mut BTreeSet<String>) {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(name)) = map.get("env") {
                out.insert(name.clone());
            }
            for child in map.values() {
                collect_env_sources(child, out);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_env_sources(child, out);
            }
        }
        _ => {}
    }
}

/// Answer a preflight.
pub fn preflight(req: &PreflightRequest) -> PreflightResponse {
    let (config_path, cfg) = match load_config(&req.project_root) {
        Ok(pair) => pair,
        Err(e) => return refused(e),
    };
    let project_root = config::project_root(&config_path);
    match &req.plan {
        PreflightPlan::Start {
            selections,
            preset,
            name,
            oneshot,
            all_logs,
        } => preflight_start(
            &config_path,
            &project_root,
            cfg,
            StartAsk {
                selections,
                preset: preset.as_deref(),
                name: name.as_deref(),
                oneshot: *oneshot,
                all_logs: *all_logs,
                var_flags: &req.var_flags,
                interactive: req.interactive,
            },
        ),
        PreflightPlan::Restart { name } => preflight_restart(
            &config_path,
            &project_root,
            cfg,
            name.clone(),
            &req.var_flags,
        ),
        PreflightPlan::Stop { name, all } => preflight_stop(&project_root, name.clone(), *all),
    }
}

struct StartAsk<'a> {
    selections: &'a [String],
    preset: Option<&'a str>,
    name: Option<&'a str>,
    oneshot: bool,
    all_logs: bool,
    var_flags: &'a [String],
    interactive: bool,
}

fn preflight_start(
    config_path: &Path,
    project_root: &Path,
    cfg: VeldConfig,
    ask: StartAsk<'_>,
) -> PreflightResponse {
    let mut notices = Vec::new();

    let (selections, origin_preset) = if let Some(token) = ask.preset {
        let Some(chosen) = veld_core::presets::find_by_name_then_key(&cfg, token) else {
            return refused(unknown_preset_message(&cfg, token));
        };
        match expand_and_resolve(&chosen.name, &cfg) {
            Ok(sels) => (sels, Some(chosen.name.clone())),
            Err(e) => return refused(e),
        }
    } else if ask.selections.is_empty() {
        let presets = veld_core::presets::resolve(&cfg);
        let default = veld_core::presets::default_preset(&cfg);
        match (default, ask.interactive) {
            // A declared default is the answer to "just start it" — but a human at
            // a picker gets to choose, with the default marked.
            (Some(default), false) => {
                let label = default
                    .label
                    .as_deref()
                    .map(|l| format!(" ({})", one_line(l)))
                    .unwrap_or_default();
                notices.push(format!(
                    "Starting default preset `{}`{label}.",
                    one_line(&default.name)
                ));
                match expand_and_resolve(&default.name, &cfg) {
                    Ok(sels) => (sels, Some(default.name.clone())),
                    Err(e) => return refused(e),
                }
            }
            (None, false) => {
                let node_names: Vec<String> = cfg.nodes.keys().cloned().collect();
                return PreflightResponse::Refused {
                    message: "No selections provided".to_owned(),
                    payload: Some(serde_json::json!({
                        "error": "No selections provided",
                        "nodes": node_names,
                        "presets": presets,
                        "hint": "Pass `--preset <name-or-key>`, explicit `node:variant` \
                                 selections, or set `default_preset` in veld.json so a bare \
                                 `veld start` has an answer.",
                    })),
                };
            }
            (_, true) => {
                if cfg.nodes.is_empty() {
                    return refused("No nodes defined in config.");
                }
                let mut nodes: Vec<(String, Vec<String>)> = cfg
                    .nodes
                    .iter()
                    .map(|(name, n)| {
                        let mut variants: Vec<String> = n.variants.keys().cloned().collect();
                        variants.sort();
                        (name.clone(), variants)
                    })
                    .collect();
                nodes.sort();
                return PreflightResponse::Choose(Box::new(Choice {
                    show_groups: veld_core::presets::has_groups(&cfg),
                    presets,
                    nodes,
                }));
            }
        }
    } else {
        let parsed: Result<Vec<NodeSelection>, _> = ask
            .selections
            .iter()
            .map(|s| graph::parse_selection(s))
            .collect();
        match parsed.and_then(|p| graph::resolve_selections(&p, &cfg)) {
            Ok(resolved) => (resolved, None),
            Err(e) => return refused(with_preset_hint(&format!("{e}"), ask.selections, &cfg)),
        }
    };

    // Non-localhost URL templates need privileged mode.
    let offenders = find_non_localhost_domains(&selections, &cfg);
    if !offenders.is_empty() {
        let mode = veld_core::setup::read_setup_mode().unwrap_or_else(|| "auto".to_owned());
        if mode != "privileged" {
            let mut detail =
                String::from("Custom apex domains are only supported in privileged mode.\n");
            detail.push_str("\n  Affected nodes:\n");
            for (label, hostname) in &offenders {
                detail.push_str(&format!("    - {label} => {hostname}\n"));
            }
            detail.push_str(
                "\n  In unprivileged/auto mode, veld cannot write to /etc/hosts or manage\n  \
                 system DNS, so only .localhost domains work (RFC 6761).\n\
                 \n  To fix this, either:\n  \
                 - Change your url_template to use .localhost (e.g. {service}.{run}.{project}.localhost)\n  \
                 - Run `veld setup privileged` (one-time sudo) to enable custom domains",
            );
            return refused(detail);
        }
    }

    let oneshot = if ask.oneshot {
        if selections.len() != 1 {
            return refused(
                "--oneshot requires exactly one command-type selection (the terminal node whose \
                 exit ends the run). Its dependencies are started automatically.",
            );
        }
        let sel = selections[0].clone();
        let is_command = cfg
            .resolved(&sel.node, &sel.variant)
            .map(|r| r.step_type == StepType::Command)
            .unwrap_or(false);
        if !is_command {
            return refused(format!(
                "--oneshot requires a command-type node; '{}:{}' is long_running (it never \
                 exits, so it cannot be the terminal node).",
                sel.node, sel.variant
            ));
        }
        Some(sel.to_string())
    } else {
        if ask.all_logs {
            return refused("--all-logs only applies with --oneshot.");
        }
        None
    };

    let run = match ask.name {
        Some(n) => n.to_owned(),
        None => veld_core::url::generate_run_name(project_root),
    };

    let flag_answers = match parse_var_answers(ask.var_flags, &cfg) {
        Ok(a) => a,
        Err(e) => return refused(e),
    };

    let db = match Db::open() {
        Ok(db) => db,
        Err(e) => return refused(format!("Failed to open veld's database: {e}")),
    };
    let env_names = env_names(&cfg, &db, project_root);

    let mut orchestrator = match Orchestrator::new(config_path.to_path_buf(), cfg) {
        Ok(o) => o,
        Err(e) => return refused(format!("Failed to initialize: {e}")),
    };
    apply_answers(&mut orchestrator, &flag_answers);
    let unanswered = match orchestrator.unanswered_vars(&selections) {
        Ok(m) => m.into_iter().map(Into::into).collect(),
        Err(e) => return refused(format!("{e}")),
    };
    let stranded_at_teardown = orchestrator.flag_answers_needed_at_teardown(&selections);

    PreflightResponse::Ready(Box::new(Preflight {
        action: ActionKind::Start {
            run,
            selections: selections.iter().map(ToString::to_string).collect(),
            origin_preset,
            oneshot,
            all_logs: ask.all_logs,
        },
        unanswered,
        env_names,
        stranded_at_teardown,
        notices,
        flag_answers,
    }))
}

fn preflight_restart(
    config_path: &Path,
    project_root: &Path,
    cfg: VeldConfig,
    name: Option<String>,
    var_flags: &[String],
) -> PreflightResponse {
    // `veld restart` has no `--var`; a face passes here only the prompt answers
    // a human kept for this run, so the stranded-teardown warning can name them.
    let flag_answers = match parse_var_answers(var_flags, &cfg) {
        Ok(a) => a,
        Err(e) => return refused(e),
    };
    let db = match Db::open() {
        Ok(db) => db,
        Err(e) => return refused(format!("Failed to open veld's database: {e}")),
    };
    let state = match db.load_project_state(project_root) {
        Ok(s) => s,
        Err(e) => return refused(format!("Failed to load state: {e}")),
    };
    let (run, note) = match resolve_run_name(name, &state, true) {
        Ok(pair) => pair,
        Err(e) => return refused(e),
    };
    let Some(run_state) = state.get_run(&run) else {
        return refused(format!("Run '{run}' not found."));
    };
    let selections = run_selections(run_state);
    let env_names = env_names(&cfg, &db, project_root);
    let mut orchestrator = match Orchestrator::new(config_path.to_path_buf(), cfg) {
        Ok(o) => o,
        Err(e) => return refused(format!("Failed to initialize: {e}")),
    };
    apply_answers(&mut orchestrator, &flag_answers);
    let unanswered = match orchestrator.unanswered_vars(&selections) {
        Ok(m) => m.into_iter().map(Into::into).collect(),
        Err(e) => return refused(format!("{e}")),
    };
    let stranded_at_teardown = orchestrator.flag_answers_needed_at_teardown(&selections);
    PreflightResponse::Ready(Box::new(Preflight {
        action: ActionKind::Restart { run },
        unanswered,
        env_names,
        stranded_at_teardown,
        notices: note.into_iter().collect(),
        flag_answers,
    }))
}

fn preflight_stop(project_root: &Path, name: Option<String>, all: bool) -> PreflightResponse {
    let db = match Db::open() {
        Ok(db) => db,
        Err(e) => return refused(format!("Failed to open veld's database: {e}")),
    };
    let state = match db.load_project_state(project_root) {
        Ok(s) => s,
        Err(e) => return refused(format!("Failed to load state: {e}")),
    };
    let env_names = load_config(project_root)
        .map(|(_, cfg)| env_names(&cfg, &db, project_root))
        .unwrap_or_default();
    if all {
        if state.runs.is_empty() {
            return refused("No runs to stop.");
        }
        return PreflightResponse::Ready(Box::new(Preflight {
            action: ActionKind::StopAll,
            unanswered: Vec::new(),
            env_names,
            stranded_at_teardown: Vec::new(),
            notices: Vec::new(),
            flag_answers: Vec::new(),
        }));
    }
    let (run, note) = match resolve_run_name(name, &state, true) {
        Ok(pair) => pair,
        Err(e) => return refused(e),
    };
    PreflightResponse::Ready(Box::new(Preflight {
        action: ActionKind::Stop { run },
        unanswered: Vec::new(),
        env_names,
        stranded_at_teardown: Vec::new(),
        notices: note.into_iter().collect(),
        flag_answers: Vec::new(),
    }))
}

/// The selections a run was started with, from its node rows.
pub fn run_selections(run: &veld_core::state::RunState) -> Vec<NodeSelection> {
    run.nodes
        .values()
        .map(|ns| NodeSelection {
            node: ns.node_name.clone(),
            variant: ns.variant.clone(),
        })
        .collect()
}

/// Apply a face's answers with the provenance each must carry.
///
/// **By provenance, never all as one.** A saved answer labelled per-run makes the
/// teardown warning fire for a var the user just set and records the wrong
/// origin in the run snapshot — three review rounds found that bug in this seam.
pub fn apply_answers(orchestrator: &mut Orchestrator, answers: &[VarAnswer]) {
    let mut saved = veld_core::values::VarOverrides::new();
    let mut per_run = veld_core::values::VarOverrides::new();
    for a in answers {
        let v = config::ConfigValue::literal(&a.value);
        match a.provenance {
            AnswerProvenance::Saved => saved.insert(a.name.clone(), v),
            AnswerProvenance::PerRun => per_run.insert(a.name.clone(), v),
        };
    }
    if !saved.is_empty() {
        orchestrator.set_var_answers(saved, VarProvenance::Project);
    }
    if !per_run.is_empty() {
        orchestrator.set_var_answers(per_run, VarProvenance::Flag);
    }
}

/// Save answers the human chose to keep for this machine.
pub fn save_var_answers(
    req: &crate::api::SaveVarAnswers,
) -> Result<crate::api::SavedVarAnswers, String> {
    let db = Db::open().map_err(|e| format!("veld's database could not be opened: {e}"))?;
    let project_id = veld_core::project_id::project_id_for(&req.project_root);
    let mut saved = Vec::new();
    let mut failed = Vec::new();
    for (name, value) in &req.answers {
        let cv = config::ConfigValue {
            source: SecretSource::Literal(value.clone()),
            // Sensitivity is applied from the declaration at resolution.
            secret: false,
        };
        match db.set_var_override(
            &project_id,
            veld_core::db::OverrideScope::Project,
            &req.project_root,
            name,
            &cv,
        ) {
            Ok(()) => saved.push(name.clone()),
            Err(e) => failed.push((name.clone(), e.to_string())),
        }
    }
    Ok(crate::api::SavedVarAnswers {
        saved,
        failed,
        project_id: project_id.to_string(),
    })
}

/// Config text in a terminal line: control characters and bidi overrides become
/// spaces, so a preset label cannot move the cursor or reorder the line.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

fn expand_and_resolve(name: &str, cfg: &VeldConfig) -> Result<Vec<NodeSelection>, String> {
    graph::expand_preset(name, cfg)
        .and_then(|sels| graph::resolve_selections(&sels, cfg))
        .map_err(|e| format!("Invalid preset `{name}`: {e}"))
}

fn unknown_preset_message(cfg: &VeldConfig, token: &str) -> String {
    let available = veld_core::presets::resolve(cfg);
    if available.is_empty() {
        return format!("Unknown preset `{token}` — this project defines no presets.");
    }
    let list = available
        .iter()
        .map(|p| format!("{} ({})", one_line(&p.name), p.key))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Unknown preset `{}`. Available (name and key): {list}",
        one_line(token)
    )
}

/// Append the `--preset` correction when a rejected selection token is in fact a
/// preset of this project.
///
/// The single most common thing a coding agent gets wrong about veld: `docker
/// compose up <service>`, `npm run <script>`, `just <recipe>` all take the thing
/// you want as a positional. The error message reaches the agent exactly when the
/// belief is wrong. Matches by **name only** — a key is a small integer and `veld
/// start 2` is not a plausible thing to have meant. A token that names a real
/// node is skipped even if a preset shares the name, so the scan does not accuse
/// the innocent token. Not accepted as a preset silently: node and preset are
/// separate namespaces, and the run records what it was started *from*.
fn with_preset_hint(message: &str, selections: &[String], cfg: &VeldConfig) -> String {
    let presets = veld_core::presets::resolve(cfg);
    let hit = selections.iter().find_map(|token| {
        let node = token.split(':').next().unwrap_or(token);
        if cfg.nodes.contains_key(node) {
            return None;
        }
        presets.iter().find(|p| p.name == node)
    });
    match hit {
        Some(p) => format!(
            "{message}\n\n  `{}` is a preset, not a node. Presets go behind the flag:\n    \
             veld start --preset {}",
            one_line(&p.name),
            one_line(&p.name),
        ),
        None => message.to_owned(),
    }
}

/// Parse `--var NAME=VALUE` pairs, refusing a name the config does not declare
/// machine-overridable — silence would drop `--var typo=x` without a word.
fn parse_var_answers(pairs: &[String], cfg: &VeldConfig) -> Result<Vec<VarAnswer>, String> {
    let mut declared: Vec<&str> = cfg
        .vars
        .iter()
        .flatten()
        .filter(|(_, d)| d.machine().is_some())
        .map(|(n, _)| n.as_str())
        .collect();
    declared.sort_unstable();
    let mut out = Vec::new();
    for pair in pairs {
        let Some((name, value)) = pair.split_once('=') else {
            return Err(format!(
                "--var expects NAME=VALUE, got `{pair}`. An empty value is written `NAME=`"
            ));
        };
        if name.is_empty() {
            return Err(format!("--var `{pair}` has no name before the `=`"));
        }
        if !declared.contains(&name) {
            return Err(format!(
                "--var `{name}` is not declared machine-overridable in this project, so it \
                 would be ignored. Machine-overridable vars: {}",
                if declared.is_empty() {
                    "(none)".to_owned()
                } else {
                    declared.join(", ")
                }
            ));
        }
        out.retain(|a: &VarAnswer| a.name != name);
        out.push(VarAnswer {
            name: name.to_owned(),
            value: value.to_owned(),
            provenance: AnswerProvenance::PerRun,
        });
    }
    Ok(out)
}

fn find_non_localhost_domains(
    selections: &[NodeSelection],
    cfg: &VeldConfig,
) -> Vec<(String, String)> {
    use veld_core::url;
    let dummy =
        url::build_url_template_values("svc", "var", "run", "proj", "branch", "wt", "user", "host");
    let mut offenders = Vec::new();
    for sel in selections {
        let Some(node_cfg) = cfg.nodes.get(&sel.node) else {
            continue;
        };
        let Some(variant_cfg) = node_cfg.variants.get(&sel.variant) else {
            continue;
        };
        let Some(resolved) = cfg.resolved(&sel.node, &sel.variant) else {
            continue;
        };
        if resolved.step_type != StepType::LongRunning {
            continue;
        }
        let template = url::resolve_url_template(
            &cfg.url_template,
            node_cfg.url_template.as_deref(),
            variant_cfg.url_template.as_deref(),
        );
        if let Ok(hostname) = url::evaluate_url_template(template, &dummy) {
            if !url::is_localhost_domain(&hostname) {
                offenders.push((sel.to_string(), hostname));
            }
        }
    }
    offenders
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn env_sources_are_found_and_env_maps_are_not() {
        let json = serde_json::json!({
            "nodes": { "api": { "variants": { "local": {
                "env": { "TOKEN": { "env": "GITHUB_TOKEN" }, "PLAIN": "x" }
            }}}},
            "vars": { "region": { "default": { "env": "AWS_REGION" } } }
        });
        let mut out = BTreeSet::new();
        collect_env_sources(&json, &mut out);
        assert_eq!(
            out.into_iter().collect::<Vec<_>>(),
            vec!["AWS_REGION".to_owned(), "GITHUB_TOKEN".to_owned()]
        );
    }

    #[test]
    fn a_flag_for_an_undeclared_var_is_refused() {
        let cfg: VeldConfig = serde_json::from_value(serde_json::json!({
            "schemaVersion": "3", "name": "p",
            "nodes": { "api": { "variants": { "local": { "command": "true" } } } }
        }))
        .unwrap();
        let err = parse_var_answers(&["nope=1".to_owned()], &cfg).unwrap_err();
        assert!(err.contains("not declared machine-overridable"), "{err}");
    }

    // --- the `--preset` correction (moved with `with_preset_hint`) ---

    fn config_with_a_preset() -> veld_core::config::VeldConfig {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("veld.json");
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(
            br#"{
              "schemaVersion": "3",
              "name": "hinted",
              "nodes": {
                "api": {
                  "default_variant": "local",
                  "variants": { "local": { "type": "command", "shell": "true" } }
                }
              },
              "presets": { "dev-headless": { "selections": ["api:local"] } }
            }"#,
        )
        .expect("write");
        // The parse has to happen before `dir` goes out of scope — a `TempDir`
        // deletes its tree on drop, and the failure that produces is a
        // file-not-found from a path the test can see right there in the source.
        let config = veld_core::config::parse_config(&path).expect("parse");
        drop(dir);
        config
    }

    /// The correction is the whole point of the function — an agent that reached
    /// here believes presets are positional, and only this message is in front of
    /// it at the moment the belief is wrong.
    #[test]
    fn a_preset_used_as_a_positional_is_named_as_one() {
        let config = config_with_a_preset();
        let out = with_preset_hint(
            "unknown node \"dev-headless\"",
            &["dev-headless".to_owned()],
            &config,
        );
        assert!(out.contains("is a preset, not a node"), "{out}");
        assert!(out.contains("veld start --preset dev-headless"), "{out}");
        // The original diagnostic survives: the hint adds, it does not replace.
        assert!(out.contains("unknown node"), "{out}");
    }

    /// `dev-headless:local` is the same misunderstanding one step further on, and
    /// the node half is what has to be matched.
    #[test]
    fn a_preset_with_a_variant_appended_is_caught_too() {
        let config = config_with_a_preset();
        let out = with_preset_hint("boom", &["dev-headless:local".to_owned()], &config);
        assert!(out.contains("is a preset, not a node"), "{out}");
    }

    /// A node and a preset may legally share a name, and then the node wins: the
    /// token was used correctly and the failure is about some other selection.
    #[test]
    fn a_real_node_is_never_accused_of_being_a_preset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("veld.json");
        std::fs::write(
            &path,
            br#"{
              "schemaVersion": "3",
              "name": "collide",
              "nodes": {
                "api": {
                  "default_variant": "local",
                  "variants": { "local": { "type": "command", "shell": "true" } }
                },
                "web": {
                  "default_variant": "local",
                  "variants": { "local": { "type": "command", "shell": "true" } }
                }
              },
              "presets": { "api": { "selections": ["web:local"] } }
            }"#,
        )
        .expect("write");
        let config = veld_core::config::parse_config(&path).expect("parse");
        drop(dir);

        // `web:nosuchvariant` is what failed; `api` is a node used correctly and
        // must not be rewritten into `veld start --preset api`, which starts
        // something else.
        let out = with_preset_hint(
            "unknown variant \"nosuchvariant\"",
            &["api".to_owned(), "web:nosuchvariant".to_owned()],
            &config,
        );
        assert_eq!(out, "unknown variant \"nosuchvariant\"", "{out}");
    }

    /// A genuinely unknown node must not be told it is a preset, and a numeric
    /// token must not match a preset's printed key — `veld start 2` is not a
    /// plausible thing to have meant, and a node called `1` would be slandered.
    #[test]
    fn an_unrelated_token_is_left_alone() {
        let config = config_with_a_preset();
        for token in ["typo", "1", "api"] {
            let out = with_preset_hint("boom", &[token.to_owned()], &config);
            assert_eq!(out, "boom", "token `{token}` should not produce a hint");
        }
    }
}

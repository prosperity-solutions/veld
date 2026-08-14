pub mod action;
pub mod agent;
pub mod config;
pub mod desktop;
pub mod doctor;
pub mod feedback;
pub mod gc;
pub mod graph;
pub mod init;
pub mod lint;
pub mod list;
pub mod logs;
pub mod nodes;
pub mod open_url;
pub mod presets;
pub mod restart;
pub mod runs;
pub mod settings;
pub mod setup;
pub mod share;
pub mod start;
pub mod stats;
pub mod status;
pub mod stop;
pub mod ui;
pub mod uninstall;
pub mod update;
pub mod urls;
pub mod version;

use crate::output;

/// Record CPU/memory for the `command` steps an orchestrator is about to run.
///
/// **Every command that executes a run's graph must call this**, not just
/// `veld start` — `veld restart` re-runs the same builds and installs, into the
/// same run name, so leaving it out puts a hole in the middle of a node's curve
/// that reads as a sampling bug rather than as an unsupported path.
///
/// The daemon samples what has a persisted PID, which is only the `start_server`
/// nodes. A `command` step's process is spawned, awaited and reaped inside *this*
/// process, so if this process does not measure it, nothing can.
///
/// **The orchestrator owns the recorder, so sampling ends when it does** — which
/// is the end of the command, and is why nothing is returned here. An earlier
/// version handed back a guard whose docs promised that dropping it stopped
/// sampling; it did not, because `with_step_observer` keeps an `Arc` of its own
/// and clones it into every node execution context. A handle that has to be held
/// but cannot enforce it is a footgun with a `#[must_use]` on it — and
/// `let _ = …` silences that anyway.
pub fn observe_command_stats(
    orchestrator: &mut veld_core::orchestrator::Orchestrator,
    run_name: &str,
) {
    orchestrator.with_step_observer(std::sync::Arc::new(
        veld_stats::CommandStatsRecorder::start(
            orchestrator.db.clone(),
            orchestrator.project_root.clone(),
            run_name.to_owned(),
        ),
    ));
}

/// Clean up the Spoon left behind by the Hammerspoon menu bar integration veld
/// used to ship, and report what it did.
///
/// Shared by `veld update` and `veld uninstall`. It lives in the CLI rather than
/// in `veld_core::setup::uninstall()` because the useful half is the *report*: a
/// `hs.loadSpoon("Veld")` line left in the user's `init.lua` errors on every
/// Hammerspoon reload, and veld never edits a user's config, so saying so is the
/// only remedy available. `uninstall()` returns `Result<()>`, so a call from
/// there could not surface it — and uninstall is the last moment veld will ever
/// be able to.
///
/// One-shot with an expiry, but **do not delete it after one release**. See
/// `veld_core::setup::remove_legacy_hammerspoon`.
pub async fn remove_legacy_hammerspoon() {
    let result = veld_core::setup::remove_legacy_hammerspoon().await;
    if !result.removed {
        return;
    }

    output::print_info(
        "Removed the retired Hammerspoon widget (~/.hammerspoon/Spoons/Veld.spoon).",
    );
    if result.needs_hammerspoon_reload {
        // The files are gone but the Spoon is still loaded, so the icon is still
        // in the menu bar. Saying "removed" and leaving it there reads as a bug.
        output::print_info("Reload Hammerspoon to drop its menu bar icon.");
    }
    if let Some(init_lua) = result.stale_init_lua {
        output::print_info(&format!(
            "Remove the `hs.loadSpoon(\"Veld\")` line from {} — it now points at nothing.",
            init_lua.display()
        ));
    }
}

/// Open the central veld database. On failure prints an error and returns
/// `None`.
pub fn open_db(json: bool) -> Option<veld_core::db::Db> {
    match veld_core::db::Db::open() {
        Ok(db) => Some(db),
        Err(e) => {
            output::print_error(&format!("Failed to open veld database: {e}"), json);
            None
        }
    }
}

/// Read the setup mode from `~/.veld/setup.json`.
/// Delegates to the shared implementation in `veld-core` so the two never drift.
pub fn read_setup_mode() -> Option<String> {
    veld_core::setup::read_setup_mode()
}

/// One line describing what a run was started from, checked against the config
/// as it reads *now*.
///
/// The check is the reason this exists rather than printing the stored name: a
/// preset can be renamed, deleted or re-pointed at different nodes while the run
/// is live, so a bare `preset web-only` would state something that stopped being
/// true. Comparing the recorded expansion with a fresh one turns that into a
/// visible qualifier instead of a silent lie.
///
/// `None` when the run predates the record — callers omit the line entirely
/// rather than printing "unknown", which would read as a property of the run.
pub fn start_origin_label(
    origin: Option<&veld_core::state::StartOrigin>,
    config: &veld_core::config::VeldConfig,
) -> Option<String> {
    let origin = origin?;
    let selections = origin.selections.join(", ");
    let Some(preset) = origin.preset.as_deref() else {
        // An explicit-token start. Naming the tokens is the whole answer, and
        // saying "no preset" would imply a preset was expected.
        return Some(if selections.is_empty() {
            "selections (none recorded)".to_owned()
        } else {
            format!("selections {selections}")
        });
    };
    let current = veld_core::graph::expand_preset(preset, config)
        .and_then(|sels| veld_core::graph::resolve_selections(&sels, config))
        .map(|sels| veld_core::state::StartOrigin::new(None, &sels).selections);
    let qualifier = match current {
        Ok(now) if now == origin.selections => "",
        // Expanded, but to something else: the name still resolves, so the run
        // is not "preset web-only" any more in any useful sense.
        Ok(_) => " (redefined since start)",
        // Only *this* name being absent means the preset is gone. Every other
        // failure — a dangling `@ref`, a cycle, a since-removed node, a tree over
        // the expansion budget — is a preset that still exists and cannot be
        // expanded, and reporting those as "no longer defined" asserts something
        // false about the config the reader is looking at. It also contradicted the
        // UI, which calls the same state "redefined".
        Err(veld_core::graph::GraphError::UnknownPreset(ref missing)) if missing == preset => {
            " (no longer defined)"
        }
        Err(_) => " (cannot be expanded — see `veld lint`)",
    };
    Some(format!("preset `{preset}`{qualifier} — {selections}"))
}

/// Resolve the environment name to use. If `name` is given, use it directly.
/// Otherwise look at the project state, two-tiered: if exactly one environment
/// has a *live* run (starting/running/stopping), use that; only when zero are
/// live, fall back to a sole environment (stopped ones persist as history now,
/// so `veld restart`/`veld stop` can find last night's crashed environment).
/// The tiers are deliberately not collapsed — a crashed `dev` next to a
/// running `staging` must not turn a bare `veld stop` into an ambiguity error.
pub fn resolve_run_name(
    name: Option<String>,
    project_state: &veld_core::state::ProjectState,
    include_stopped: bool,
    json: bool,
) -> Option<String> {
    if let Some(n) = name {
        return Some(n);
    }

    // Tier 1: environments with a live run.
    let live: Vec<&String> = project_state
        .runs
        .iter()
        .filter(|(_, r)| r.is_live())
        .map(|(name, _)| name)
        .collect();

    match live.len() {
        1 => {
            let resolved = live[0].clone();
            if !json {
                output::print_info(&format!(
                    "Using environment '{resolved}' (only live environment)."
                ));
            }
            return Some(resolved);
        }
        n if n > 1 => {
            let mut names: Vec<&str> = live.iter().map(|s| s.as_str()).collect();
            names.sort();
            output::print_error(
                &format!(
                    "Multiple live environments found. Specify one with --name: {}",
                    names.join(", ")
                ),
                json,
            );
            return None;
        }
        _ => {}
    }

    // Tier 2: nothing live — fall back to a sole environment (its latest run
    // is history at this point).
    if include_stopped && project_state.runs.len() == 1 {
        let resolved = project_state.runs.keys().next().unwrap().clone();
        if !json {
            output::print_info(&format!(
                "Using environment '{resolved}' (only environment)."
            ));
        }
        return Some(resolved);
    }

    if include_stopped && project_state.runs.len() > 1 {
        let mut names: Vec<&str> = project_state.runs.keys().map(|s| s.as_str()).collect();
        names.sort();
        output::print_error(
            &format!(
                "Multiple environments found. Specify one with --name: {}",
                names.join(", ")
            ),
            json,
        );
        return None;
    }

    output::print_error("No environments found. Start one with `veld start`.", json);
    None
}

/// Parse the project configuration from the current working directory.
/// On failure prints an error and returns `None`.
///
/// Structural parsing only — see [`veld_core::config::parse_config`]. Never add
/// a semantic check to this path: it runs on `stop`, `status`, and `logs`, which
/// must keep working against a config that has since been broken.
pub fn parse_config(json: bool) -> Option<(std::path::PathBuf, veld_core::config::VeldConfig)> {
    match veld_core::config::parse_config_from_cwd() {
        Ok(pair) => Some(pair),
        Err(e) => {
            output::print_error(&format!("Failed to load config: {e}"), json);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use veld_core::state::StartOrigin;

    /// A config with one preset naming `api:local`, plus a second node so a
    /// "redefined" case has somewhere to move to.
    fn config(preset_selections: &[&str]) -> veld_core::config::VeldConfig {
        let sels = preset_selections
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let json = format!(
            r#"{{
              "schemaVersion": "3",
              "name": "proj",
              "nodes": {{
                "api": {{ "variants": {{ "local": {{ "command": "true" }} }} }},
                "web": {{ "variants": {{ "local": {{ "command": "true" }} }} }}
              }},
              "presets": {{ "stack": [{sels}] }}
            }}"#
        );
        serde_json::from_str(&json).expect("test config parses")
    }

    fn origin(preset: Option<&str>, selections: &[&str]) -> StartOrigin {
        StartOrigin {
            preset: preset.map(str::to_owned),
            selections: selections.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn a_matching_preset_is_named_without_a_qualifier() {
        let cfg = config(&["api:local"]);
        let label = super::start_origin_label(Some(&origin(Some("stack"), &["api:local"])), &cfg);
        assert_eq!(label.as_deref(), Some("preset `stack` — api:local"));
    }

    #[test]
    fn an_edited_preset_is_reported_as_redefined() {
        // The whole reason the expansion is recorded next to the name: the name
        // still resolves, so printing it bare would state something that stopped
        // being true while the run was live.
        let cfg = config(&["api:local", "web:local"]);
        let label = super::start_origin_label(Some(&origin(Some("stack"), &["api:local"])), &cfg);
        assert_eq!(
            label.as_deref(),
            Some("preset `stack` (redefined since start) — api:local")
        );
    }

    #[test]
    fn a_deleted_preset_says_so_rather_than_naming_it_plainly() {
        let cfg = config(&["api:local"]);
        let label = super::start_origin_label(Some(&origin(Some("gone"), &["api:local"])), &cfg);
        assert_eq!(
            label.as_deref(),
            Some("preset `gone` (no longer defined) — api:local")
        );
    }

    #[test]
    fn a_preset_that_exists_but_cannot_expand_is_not_called_undefined() {
        // A dangling `@ref` leaves `stack` defined and unexpandable. Saying "no
        // longer defined" about it is a false claim about the config on disk — and
        // the UI calls this same state "redefined", so the two surfaces would
        // contradict each other over one config.
        let cfg = config(&["@missing"]);
        let label = super::start_origin_label(Some(&origin(Some("stack"), &["api:local"])), &cfg);
        assert_eq!(
            label.as_deref(),
            Some("preset `stack` (cannot be expanded — see `veld lint`) — api:local")
        );
    }

    #[test]
    fn an_explicit_start_names_its_tokens_and_no_preset() {
        let cfg = config(&["api:local"]);
        let label =
            super::start_origin_label(Some(&origin(None, &["api:local", "web:local"])), &cfg);
        assert_eq!(label.as_deref(), Some("selections api:local, web:local"));
    }

    #[test]
    fn a_run_with_no_record_produces_no_line() {
        // Not "unknown": that reads as a property of the run rather than of the
        // recording, so every caller omits the line entirely.
        let cfg = config(&["api:local"]);
        assert!(super::start_origin_label(None, &cfg).is_none());
    }
}

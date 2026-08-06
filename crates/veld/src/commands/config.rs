use veld_core::config::{self, ConfigValue, VeldConfig};
use veld_core::db::OverrideScope;
use veld_core::project_id::{ProjectId, project_id_for};

use crate::output;

/// What the machine-var subcommands need: the config, where it lives, and which
/// project it belongs to across worktrees.
struct VarContext {
    config: VeldConfig,
    /// The directory holding the config — also the `worktree` scope's key.
    project_root: std::path::PathBuf,
    project_id: ProjectId,
}

/// Load the config and work out which project this checkout belongs to.
fn var_context(json: bool) -> Option<VarContext> {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            output::print_error(&format!("Failed to get current directory: {e}"), json);
            return None;
        }
    };
    let config_path = match config::discover_config(&cwd) {
        Ok(p) => p,
        Err(e) => {
            output::print_error(&format!("{e}"), json);
            return None;
        }
    };
    let config = match config::parse_config(&config_path) {
        Ok(c) => c,
        Err(e) => {
            output::print_error(&format!("{e}"), json);
            return None;
        }
    };
    let project_root = config::project_root(&config_path);
    let project_id = project_id_for(&project_root);
    Some(VarContext {
        config,
        project_root,
        project_id,
    })
}

/// Look up a var and insist it is machine-overridable.
///
/// Refusing to store an answer for an ordinary var is the point: the value would
/// sit in the database being silently ignored by every run, which is a worse
/// outcome than the error.
fn machine_var<'a>(
    config: &'a VeldConfig,
    name: &str,
    json: bool,
) -> Option<&'a veld_core::config::MachineVar> {
    let declared: Vec<&str> = {
        let mut v: Vec<&str> = config
            .vars
            .iter()
            .flatten()
            .filter(|(_, d)| d.machine().is_some())
            .map(|(n, _)| n.as_str())
            .collect();
        v.sort_unstable();
        v
    };
    match config.vars.as_ref().and_then(|v| v.get(name)) {
        Some(decl) => match decl.machine() {
            Some(m) => Some(m),
            None => {
                output::print_error(
                    &format!(
                        "`{name}` is declared in vars, but not as machine-overridable, so an \
                         answer here would never be read. Add a `machine` block to it in \
                         veld.json, or pick one of: {}",
                        if declared.is_empty() {
                            "(none declared)".to_owned()
                        } else {
                            declared.join(", ")
                        }
                    ),
                    json,
                );
                None
            }
        },
        None => {
            output::print_error(
                &format!(
                    "no var named \"{name}\" is declared. Machine-overridable vars: {}",
                    if declared.is_empty() {
                        "(none)".to_owned()
                    } else {
                        declared.join(", ")
                    }
                ),
                json,
            );
            None
        }
    }
}

/// `veld config vars [--json]`
///
/// Every machine-overridable var, its effective value, and **which scope that
/// value came from**. The scope column is not decoration: a value arriving
/// silently from a scope the reader forgot about is worse than no feature.
pub async fn list_vars(json: bool) -> i32 {
    let Some(ctx) = var_context(json) else {
        return 1;
    };
    let Some(db) = crate::commands::open_db(json) else {
        return 1;
    };
    let stored = match db.effective_var_overrides(&ctx.project_id, &ctx.project_root) {
        Ok(m) => m,
        Err(e) => {
            output::print_error(&format!("Failed to read machine overrides: {e}"), json);
            return 1;
        }
    };

    let mut rows: Vec<(
        String,
        &veld_core::config::MachineVar,
        Option<&veld_core::db::VarOverride>,
    )> = ctx
        .config
        .vars
        .iter()
        .flatten()
        .filter_map(|(name, decl)| {
            decl.machine()
                .map(|m| (name.clone(), m, stored.get(name.as_str())))
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));

    if json {
        let items: Vec<serde_json::Value> = rows
            .iter()
            .map(|(name, m, over)| {
                let effective = over.map(|o| &o.value).or(m.default.as_ref());
                serde_json::json!({
                    "name": name,
                    "from": match over {
                        Some(o) => o.scope.as_str(),
                        None if m.default.is_some() => "default",
                        None => "unset",
                    },
                    "value": effective.map(|v| m.describe(v)),
                    "secret": m.secret,
                    "default": m.default.as_ref().map(|d| m.describe(d)),
                    "choices": m.choices,
                    "description": m.description,
                    "prompt": m.prompt,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "project_id": ctx.project_id.as_str(),
                "worktree": ctx.project_root,
                "vars": items,
            }))
            .expect("serializable")
        );
        return 0;
    }

    if rows.is_empty() {
        output::print_info(
            "No machine-overridable vars are declared. Add a `machine` block to a var in \
             veld.json to let each machine answer it.",
        );
        return 0;
    }

    let table: Vec<Vec<String>> = rows
        .iter()
        .map(|(name, m, over)| {
            let (from, value) = match over {
                Some(o) => (o.scope.as_str().to_owned(), m.describe(&o.value)),
                None => match &m.default {
                    Some(d) => ("default".to_owned(), m.describe(d)),
                    None => ("unset".to_owned(), "-".to_owned()),
                },
            };
            vec![
                name.clone(),
                value,
                from,
                m.choices
                    .as_ref()
                    .map(|c| c.join(", "))
                    .unwrap_or_else(|| "-".to_owned()),
                m.description.clone().unwrap_or_else(|| "-".to_owned()),
            ]
        })
        .collect();
    output::print_table(&["VAR", "VALUE", "FROM", "CHOICES", "DESCRIPTION"], &table);
    eprintln!();
    eprintln!("project: {}", ctx.project_id);
    eprintln!("this checkout: {}", ctx.project_root.display());
    if rows
        .iter()
        .any(|(_, m, o)| o.is_none() && m.default.is_none())
    {
        eprintln!("`unset` vars have no default — `veld start` will ask, or refuse if it cannot.");
    }
    0
}

/// `veld config set <name> <value|--env|--file|--shell> [--worktree]`
pub async fn set_var(
    name: &str,
    value: Option<&str>,
    env: Option<&str>,
    file: Option<&str>,
    shell: Option<&str>,
    worktree: bool,
    json: bool,
) -> i32 {
    let Some(ctx) = var_context(json) else {
        return 1;
    };
    let Some(machine) = machine_var(&ctx.config, name, json) else {
        return 1;
    };

    // The var's declared sensitivity travels with the stored answer, so a
    // `secret` var's override is redacted everywhere the declaration would be —
    // including by a *different* veld that reads this row later.
    let source = match (value, env, file, shell) {
        (Some(v), None, None, None) => ConfigValue {
            source: veld_core::config::SecretSource::Literal(v.to_owned()),
            secret: machine.secret,
        },
        (None, Some(v), None, None) => ConfigValue {
            source: veld_core::config::SecretSource::Env(v.to_owned()),
            secret: machine.secret,
        },
        (None, None, Some(v), None) => ConfigValue {
            source: veld_core::config::SecretSource::File(v.to_owned()),
            secret: machine.secret,
        },
        (None, None, None, Some(v)) => ConfigValue {
            source: veld_core::config::SecretSource::Shell(v.to_owned()),
            secret: machine.secret,
        },
        (None, None, None, None) => {
            output::print_error(
                &format!(
                    "no value given. Pass one literally (`veld config set {name} <value>`) or \
                     as a pointer (`--env NAME`, `--file PATH`, `--shell 'command'`)"
                ),
                json,
            );
            return 1;
        }
        _ => {
            output::print_error(
                "give either a literal value or one source flag, not both",
                json,
            );
            return 1;
        }
    };

    // A literal is checkable now; a pointer's value is not known until run
    // start, so it is checked there instead (`MachineVarNotAChoice`).
    if let Some(choices) = machine.choices.as_ref().filter(|c| !c.is_empty())
        && let Some(literal) = source.as_literal()
        && !choices.iter().any(|c| c == literal)
    {
        output::print_error(
            &format!(
                "\"{literal}\" is not one of the choices declared for `{name}`: {}",
                choices.join(", ")
            ),
            json,
        );
        return 1;
    }

    // Storing a secret's *value* is the one path where veld takes custody of one
    // rather than carrying a pointer to it. It is allowed — the alternative
    // leaves a developer with no offline answer at all — but never silently.
    if machine.secret && source.as_literal().is_some() {
        // Deliberately not `print_error`: this is a note on a command that is
        // about to succeed, and the ✗ that helper prints reads as a failure.
        // stderr, so it stays out of a `--json` consumer's stdout.
        eprintln!(
            "Note: `{name}` is declared secret and this stores the value itself in veld's \
             database (owner-readable, not encrypted). To keep veld holding only a pointer, \
             use `--env NAME`, `--file PATH`, or `--shell 'op read …'`."
        );
    }

    let scope = if worktree {
        OverrideScope::Worktree
    } else {
        OverrideScope::Project
    };
    let Some(db) = crate::commands::open_db(json) else {
        return 1;
    };
    if let Err(e) = db.set_var_override(&ctx.project_id, scope, &ctx.project_root, name, &source) {
        output::print_error(&format!("Failed to store the override: {e}"), json);
        return 1;
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "name": name,
                "scope": scope.as_str(),
                "value": machine.describe(&source),
                "project_id": ctx.project_id.as_str(),
            }))
            .expect("serializable")
        );
    } else {
        output::print_success(&format!(
            "{name} = {} ({scope} scope)",
            machine.describe(&source)
        ));
        match scope {
            OverrideScope::Project => {
                output::print_info(&format!("Applies to every worktree of {}.", ctx.project_id))
            }
            OverrideScope::Worktree => {
                output::print_info(&format!("Applies to {} only.", ctx.project_root.display()))
            }
        }
    }
    0
}

/// `veld config unset <name> [--worktree]`
pub async fn unset_var(name: &str, worktree: bool, json: bool) -> i32 {
    let Some(ctx) = var_context(json) else {
        return 1;
    };
    // Deliberately not gated on the var still being machine-overridable: a var
    // that lost its `machine` block leaves a row behind, and refusing to remove
    // it would strand the row with no way to clear it.
    let scope = if worktree {
        OverrideScope::Worktree
    } else {
        OverrideScope::Project
    };
    let Some(db) = crate::commands::open_db(json) else {
        return 1;
    };
    let removed = match db.unset_var_override(&ctx.project_id, scope, &ctx.project_root, name) {
        Ok(r) => r,
        Err(e) => {
            output::print_error(&format!("Failed to clear the override: {e}"), json);
            return 1;
        }
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "name": name,
                "scope": scope.as_str(),
                "removed": removed,
            }))
            .expect("serializable")
        );
        return 0;
    }
    if removed {
        output::print_success(&format!("cleared {name} ({scope} scope)"));
        output::print_info("Run `veld config vars` to see what it falls back to.");
    } else {
        output::print_info(&format!(
            "{name} had no {scope}-scoped answer on this machine; nothing to clear."
        ));
    }
    0
}

/// `veld config [--path] [--why <pointer>] [--json]`
///
/// Print the resolved veld.json contents. With `--path`, print only the file
/// path. With `--why`, print one effective value and where it came from.
pub async fn run(path_only: bool, files: bool, why: Option<String>, json: bool) -> i32 {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            output::print_error(&format!("Failed to get current directory: {e}"), json);
            return 1;
        }
    };

    let config_path = match config::discover_config(&cwd) {
        Ok(p) => p,
        Err(e) => {
            output::print_error(&format!("{e}"), json);
            return 1;
        }
    };

    if path_only {
        println!("{}", config_path.display());
        return 0;
    }

    if files {
        return match config::parse_config_with_files(&config_path) {
            Ok(loaded) => list_files(&loaded, json),
            Err(e) => {
                output::print_error(&format!("{e}"), json);
                1
            }
        };
    }

    if let Some(pointer) = why {
        return match config::parse_config(&config_path) {
            Ok(cfg) => explain(&cfg, &pointer, json),
            Err(e) => {
                output::print_error(&format!("{e}"), json);
                1
            }
        };
    }

    match std::fs::read_to_string(&config_path) {
        Ok(contents) => {
            print!("{contents}");
            0
        }
        Err(e) => {
            output::print_error(
                &format!("Failed to read {}: {e}", config_path.display()),
                json,
            );
            1
        }
    }
}

/// `veld config --why nodes.api.variants.dev.env.DATABASE_URL`
///
/// Prints the effective value and its single definition site. With one node per
/// file plus the node→variant cascade there are at most three layers, so this
/// prints them rather than building a provenance engine — it is a small command,
/// and the whole point of "deduplicate values, never structure" is that there is
/// exactly one place to look.
///
/// A secret is **never** resolved here: `veld config` prints a redacted
/// placeholder and there is deliberately no flag that prints a resolved secret.
fn explain(config: &VeldConfig, pointer: &str, json: bool) -> i32 {
    let parts: Vec<&str> = pointer.split('.').collect();

    // Only `env` lookups are supported so far; the shape is
    // `nodes.<node>.variants.<variant>.env.<KEY>`.
    let (node, variant, key) = match parts.as_slice() {
        ["nodes", node, "variants", variant, "env", key] => (*node, *variant, *key),
        _ => {
            output::print_error(
                &format!(
                    "Cannot explain '{pointer}'. Supported form: \
                     nodes.<node>.variants.<variant>.env.<KEY>"
                ),
                json,
            );
            return 2;
        }
    };

    let Some(node_cfg) = config.nodes.get(node) else {
        output::print_error(&format!("No node '{node}'"), json);
        return 1;
    };
    let Some(variant_cfg) = node_cfg.variants.get(variant) else {
        output::print_error(&format!("Node '{node}' has no variant '{variant}'"), json);
        return 1;
    };

    // Walk the layers most-specific first: the first one that mentions the key is
    // its definition site, and a `null` there means a more specific layer erased
    // an inherited value.
    let layers: [(String, Option<&config::NullableMap<config::ConfigValue>>); 3] = [
        (
            format!("nodes.{node}.variants.{variant}.env.{key}"),
            variant_cfg.env.as_ref(),
        ),
        (format!("nodes.{node}.env.{key}"), node_cfg.env.as_ref()),
        (format!("env.{key}"), config.env.as_ref()),
    ];

    let mut definition: Option<(String, Option<&config::ConfigValue>)> = None;
    let mut shadowed: Vec<String> = Vec::new();
    for (loc, map) in layers {
        let Some(entry) = map.and_then(|m| m.get(key)) else {
            continue;
        };
        if definition.is_none() {
            definition = Some((loc, entry.as_ref()));
        } else {
            shadowed.push(loc);
        }
    }

    let Some((where_defined, value)) = definition else {
        output::print_error(
            &format!("'{key}' is not set for {node}:{variant} at any level"),
            json,
        );
        return 1;
    };

    // A `secret` value — or any non-inline source — is described, never resolved.
    let (effective, redacted) = match value {
        None => ("(erased with null)".to_owned(), false),
        Some(v) if v.secret => (format!("(secret, from {})", v.source_label()), true),
        Some(v) => match v.as_literal() {
            Some(literal) => (literal.to_owned(), false),
            None => (format!("(from {})", v.source_label()), false),
        },
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "pointer": pointer,
                "effective": effective,
                "redacted": redacted,
                "defined_at": where_defined,
                "shadows": shadowed,
            }))
            .unwrap()
        );
    } else {
        println!("{} {}", output::bold("value:"), effective);
        println!("{} {}", output::bold("set at:"), where_defined);
        if shadowed.is_empty() {
            println!(
                "{}",
                output::dim("(nothing else sets it — one definition point)")
            );
        } else {
            println!("{} {}", output::dim("overrides:"), shadowed.join(", "));
        }
        if redacted {
            println!(
                "{}",
                output::dim(
                    "This value is declared secret, so veld will not print it. There is no \
                     flag that will."
                )
            );
        }
    }
    0
}

/// `veld config --files`
///
/// Lists each include glob, the files it matched, and the nodes each defines.
///
/// This is the answer to the four different causes of "unknown node" once a config
/// is split: never defined, defined but not matched by a glob, file renamed out of
/// a glob, or file present but unparseable. Seeing the glob→file→node chain tells
/// them apart immediately; a node name on its own never can.
fn list_files(loaded: &veld_core::include::LoadedConfig, json: bool) -> i32 {
    if json {
        let globs: Vec<serde_json::Value> = loaded
            .globs
            .iter()
            .map(|glob| {
                let matched: Vec<serde_json::Value> = loaded
                    .files
                    .iter()
                    .filter(|f| f.matched_by.as_deref() == Some(glob.as_str()))
                    .map(|f| {
                        serde_json::json!({
                            "file": f.relative,
                            "nodes": f.nodes.keys().collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                serde_json::json!({ "glob": glob, "matched": matched })
            })
            .collect();
        let root = loaded.files.first();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "root": root.map(|f| &f.relative),
                "root_nodes": root.map(|f| f.nodes.keys().collect::<Vec<_>>()),
                "config_hash": loaded.config_hash,
                "include": globs,
            }))
            .unwrap()
        );
        return 0;
    }

    let describe = |f: &veld_core::include::LoadedFile| {
        if f.nodes.is_empty() {
            output::dim("(no nodes)")
        } else {
            f.nodes
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        }
    };

    if let Some(root) = loaded.files.first() {
        println!("{} {}", output::bold("root:"), root.relative.display());
        println!("      {}", describe(root));
    }

    if loaded.globs.is_empty() {
        println!();
        println!(
            "{}",
            output::dim("No `include` globs — this project is a single config file.")
        );
        return 0;
    }

    for glob in &loaded.globs {
        println!();
        println!("{} {}", output::bold("include:"), glob);
        let matched: Vec<&veld_core::include::LoadedFile> = loaded
            .files
            .iter()
            .filter(|f| f.matched_by.as_deref() == Some(glob.as_str()))
            .collect();
        if matched.is_empty() {
            // The likeliest cause of a "missing" node: the glob matches nothing,
            // usually because a directory was renamed.
            println!("  {}", output::yellow("matched no files"));
            continue;
        }
        for f in matched {
            println!("  {}", f.relative.display());
            println!("      {}", describe(f));
        }
    }
    println!();
    println!(
        "{} {}",
        output::dim("config hash:"),
        output::dim(&loaded.config_hash[..12])
    );
    0
}

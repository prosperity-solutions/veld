use veld_core::config::{self, VeldConfig};

use crate::output;

/// `veld config [--path] [--why <pointer>] [--json]`
///
/// Print the resolved veld.json contents. With `--path`, print only the file
/// path. With `--why`, print one effective value and where it came from.
pub async fn run(
    path_only: bool,
    files: bool,
    migrate: bool,
    write: bool,
    why: Option<String>,
    json: bool,
) -> i32 {
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

    if migrate {
        return run_migrate(&config_path, write, json);
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

/// `veld config --migrate [--write]`
///
/// Converts a v1/v2 config to `schemaVersion: "3"`. **Dry-run by default**: it
/// prints a diff and what it could not do automatically, and only writes with
/// `--write`.
///
/// That default is not politeness. Turning a shell string into an argv is a
/// heuristic — `sh -c "a | b"` and `["a", "|", "b"]` are different programs — so
/// anything with shell syntax in it is deliberately left as `shell` and listed for
/// a human. Leaving a command alone is always a correct answer, which is exactly
/// why `shell` stays first-class.
fn run_migrate(config_path: &std::path::Path, write: bool, json: bool) -> i32 {
    let plan = match veld_core::migrate::plan(config_path) {
        Ok(p) => p,
        Err(e) => {
            output::print_error(&format!("{e}"), json);
            return 1;
        }
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "config": config_path.display().to_string(),
                "changed": !plan.is_noop(),
                "written": write && !plan.is_noop(),
                "changes": plan.changes,
                "needs_review": plan.manual,
            }))
            .unwrap()
        );
    }

    if plan.is_noop() {
        if !json {
            output::print_success(&format!(
                "{} is already schemaVersion 3 — nothing to migrate",
                config_path.display()
            ));
        }
        return 0;
    }

    let original = std::fs::read_to_string(config_path).unwrap_or_default();

    if !json {
        println!(
            "{}",
            veld_core::migrate::unified_diff(&original, &plan.migrated)
        );
        println!("{}", output::bold("Changes:"));
        for change in &plan.changes {
            println!("  {change}");
        }
        if !plan.manual.is_empty() {
            println!();
            println!(
                "{}",
                output::yellow("Left as `shell` — review these yourself:")
            );
            for item in &plan.manual {
                println!("  {item}");
            }
            println!(
                "{}",
                output::dim(
                    "  `shell` is permanently supported, so leaving them is fine. Convert one \
                     to `argv` only if you want the no-word-splitting guarantee."
                )
            );
        }
    }

    if !write {
        if !json {
            println!();
            println!(
                "{}",
                output::dim("Dry run — nothing was written. Re-run with --write to apply.")
            );
        }
        return 0;
    }

    if let Err(e) = std::fs::write(config_path, &plan.migrated) {
        output::print_error(
            &format!("Failed to write {}: {e}", config_path.display()),
            json,
        );
        return 1;
    }
    if !json {
        println!();
        output::print_success(&format!("Wrote {}", config_path.display()));
        println!(
            "{}",
            output::dim("Run `veld lint` to check the result before starting anything.")
        );
    }
    0
}

use veld_core::config::{self, VeldConfig};

use crate::output;

/// `veld config [--path] [--why <pointer>] [--json]`
///
/// Print the resolved veld.json contents. With `--path`, print only the file
/// path. With `--why`, print one effective value and where it came from.
pub async fn run(path_only: bool, why: Option<String>, json: bool) -> i32 {
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

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use veld_core::config;
use veld_core::state::{NodeState, ProjectState, RunState};

use crate::output;

/// Output keys a node must expose to be considered a database connection.
const REQUIRED_KEYS: [&str; 3] = ["DB_HOST", "DB_PORT", "DB_NAME"];

/// Percent-encoding set for the userinfo (user/password) portion of the URL:
/// everything except the RFC 3986 unreserved characters. This keeps a password
/// containing `@`, `:`, `/`, `?`, etc. from corrupting the connection URL.
const USERINFO: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// `veld postico [--name <run>] [--node <node>] [--print] [--json]`
///
/// Open a running environment's database in Postico (macOS) with the connection
/// pre-filled. Reads the connection details from the node's live `outputs`
/// (DB_HOST/DB_PORT/DB_USER/DB_PASS/DB_NAME), so the rotating clone port and
/// password never have to be copied by hand.
pub async fn run(name: Option<String>, node: Option<String>, print: bool, json: bool) -> i32 {
    let Some((config_path, _cfg)) = super::load_config(json) else {
        return 1;
    };
    let project_root = config::project_root(&config_path);

    let project_state = match ProjectState::load(&project_root) {
        Ok(s) => s,
        Err(e) => {
            output::print_error(&format!("Failed to load state: {e}"), json);
            return 1;
        }
    };

    // `--print` and `--json` are machine modes: keep stdout limited to the URL
    // / JSON payload by suppressing the "Using run …" notice (and printing any
    // resolution error in the same quiet form).
    let machine = print || json;
    // A stopped run's database is no longer reachable, so don't offer one.
    let run_name = match super::resolve_run_name(name, &project_state, false, machine) {
        Some(n) => n,
        None => return 1,
    };

    let run_state = match project_state.get_run(&run_name) {
        Some(r) => r,
        None => {
            output::print_error(&format!("Run '{run_name}' not found."), json);
            return 1;
        }
    };

    let db_node = match find_db_node(run_state, node.as_deref()) {
        Ok(ns) => ns,
        Err(DbNodeError::NotFound) => {
            let detail = match &node {
                Some(n) => format!("Node '{n}' in run '{run_name}' exposes no database outputs."),
                None => format!(
                    "No node in run '{run_name}' exposes database outputs ({}).",
                    REQUIRED_KEYS.join(", ")
                ),
            };
            output::print_error(&detail, json);
            return 1;
        }
        Err(DbNodeError::Ambiguous(names)) => {
            output::print_error(
                &format!(
                    "Multiple database nodes in run '{run_name}'. Pick one with --node: {}",
                    names.join(", ")
                ),
                json,
            );
            return 1;
        }
    };

    let url = match build_connection_url(db_node) {
        Some(u) => u,
        None => {
            // find_db_node guarantees the required keys, so this is unreachable
            // in practice; surface it rather than connecting to a bad URL.
            output::print_error(
                &format!(
                    "Database node '{}' is missing connection details.",
                    db_node.node_name
                ),
                json,
            );
            return 1;
        }
    };

    let host = db_node
        .outputs
        .get("DB_HOST")
        .map(String::as_str)
        .unwrap_or("");
    let port = db_node
        .outputs
        .get("DB_PORT")
        .map(String::as_str)
        .unwrap_or("");
    let database = db_node
        .outputs
        .get("DB_NAME")
        .map(String::as_str)
        .unwrap_or("");
    let user = db_node
        .outputs
        .get("DB_USER")
        .map(String::as_str)
        .unwrap_or("");
    let node_key = format!("{}:{}", db_node.node_name, db_node.variant);

    if json {
        // Machine-readable: the `url` field includes the password by design
        // (the point is a ready-to-use connection string for scripting).
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "run": run_name,
                "node": node_key,
                "host": host,
                "port": port,
                "user": user,
                "database": database,
                "url": url,
            }))
            .unwrap()
        );
        return 0;
    }

    if print {
        println!("{url}");
        return 0;
    }

    // Human path: open Postico. The info line omits the password.
    let target = if user.is_empty() {
        format!("{host}:{port}/{database}")
    } else {
        format!("{user}@{host}:{port}/{database}")
    };

    if !cfg!(target_os = "macos") {
        output::print_info(&format!(
            "Auto-open is macOS-only. Connect your client to: {url}"
        ));
        return 0;
    }

    output::print_info(&format!(
        "Opening Postico {} ({})",
        output::cyan(&target),
        output::dim(&format!("run: {run_name}, node: {node_key}")),
    ));

    // Route the URL to Postico by name; fall back to the default postgresql://
    // handler if Postico isn't installed under that name.
    let opened = std::process::Command::new("open")
        .arg("-a")
        .arg("Postico")
        .arg(&url)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if opened {
        return 0;
    }

    match std::process::Command::new("open").arg(&url).status() {
        Ok(status) if status.success() => 0,
        _ => {
            output::print_error(
                "Could not open Postico. Install it from https://eggerapps.at/postico2/ \
                 or connect your client manually (see --print).",
                json,
            );
            1
        }
    }
}

/// Reasons [`find_db_node`] cannot resolve a single database node.
enum DbNodeError {
    /// No node (matching the optional filter) exposes the required DB outputs.
    NotFound,
    /// More than one node qualifies; the caller must disambiguate with `--node`.
    Ambiguous(Vec<String>),
}

/// True if the node exposes every output key needed to build a connection URL.
fn is_db_node(ns: &NodeState) -> bool {
    REQUIRED_KEYS.iter().all(|k| ns.outputs.contains_key(*k))
}

/// Find the single database node in `run_state`. When `node_filter` is given,
/// only nodes whose `node_name` matches are considered.
fn find_db_node<'a>(
    run_state: &'a RunState,
    node_filter: Option<&str>,
) -> Result<&'a NodeState, DbNodeError> {
    let mut candidates: Vec<&NodeState> = run_state
        .nodes
        .values()
        .filter(|ns| node_filter.is_none_or(|f| ns.node_name == f))
        .filter(|ns| is_db_node(ns))
        .collect();
    candidates.sort_by(|a, b| {
        (a.node_name.as_str(), a.variant.as_str()).cmp(&(b.node_name.as_str(), b.variant.as_str()))
    });

    match candidates.as_slice() {
        [] => Err(DbNodeError::NotFound),
        [only] => Ok(only),
        many => Err(DbNodeError::Ambiguous(
            many.iter()
                .map(|ns| format!("{}:{}", ns.node_name, ns.variant))
                .collect(),
        )),
    }
}

/// Build a `postgresql://` URL from a node's outputs, percent-encoding the
/// user and password. Returns `None` if a required key is absent.
fn build_connection_url(ns: &NodeState) -> Option<String> {
    let host = ns.outputs.get("DB_HOST")?;
    let port = ns.outputs.get("DB_PORT")?;
    let database = ns.outputs.get("DB_NAME")?;

    let userinfo = match (ns.outputs.get("DB_USER"), ns.outputs.get("DB_PASS")) {
        (Some(user), Some(pass)) => format!("{}:{}@", encode(user), encode(pass)),
        (Some(user), None) => format!("{}@", encode(user)),
        (None, _) => String::new(),
    };

    Some(format!("postgresql://{userinfo}{host}:{port}/{database}"))
}

fn encode(s: &str) -> String {
    utf8_percent_encode(s, USERINFO).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use veld_core::state::NodeState;

    fn node_with(name: &str, variant: &str, outputs: &[(&str, &str)]) -> NodeState {
        let mut ns = NodeState::new(name, variant);
        ns.outputs = outputs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        ns
    }

    fn run_with(nodes: Vec<NodeState>) -> RunState {
        let mut run = RunState::new("test-run", "test");
        for ns in nodes {
            run.nodes
                .insert(format!("{}:{}", ns.node_name, ns.variant), ns);
        }
        run
    }

    #[test]
    fn builds_url_with_user_and_password() {
        let ns = node_with(
            "database",
            "dblab",
            &[
                ("DB_HOST", "localhost"),
                ("DB_PORT", "5430"),
                ("DB_NAME", "prosperity"),
                ("DB_USER", "veld-feat"),
                ("DB_PASS", "dblab-Prosp3rity"),
            ],
        );
        assert_eq!(
            build_connection_url(&ns).unwrap(),
            "postgresql://veld-feat:dblab-Prosp3rity@localhost:5430/prosperity"
        );
    }

    #[test]
    fn percent_encodes_special_characters_in_credentials() {
        let ns = node_with(
            "database",
            "dblab",
            &[
                ("DB_HOST", "localhost"),
                ("DB_PORT", "5432"),
                ("DB_NAME", "app"),
                ("DB_USER", "user@corp"),
                ("DB_PASS", "p@ss:w/rd?#"),
            ],
        );
        assert_eq!(
            build_connection_url(&ns).unwrap(),
            "postgresql://user%40corp:p%40ss%3Aw%2Frd%3F%23@localhost:5432/app"
        );
    }

    #[test]
    fn builds_url_with_user_but_no_password() {
        let ns = node_with(
            "database",
            "dblab",
            &[
                ("DB_HOST", "localhost"),
                ("DB_PORT", "5432"),
                ("DB_NAME", "app"),
                ("DB_USER", "reader"),
            ],
        );
        assert_eq!(
            build_connection_url(&ns).unwrap(),
            "postgresql://reader@localhost:5432/app"
        );
    }

    #[test]
    fn builds_url_without_credentials() {
        let ns = node_with(
            "database",
            "docker",
            &[
                ("DB_HOST", "127.0.0.1"),
                ("DB_PORT", "5432"),
                ("DB_NAME", "app"),
            ],
        );
        assert_eq!(
            build_connection_url(&ns).unwrap(),
            "postgresql://127.0.0.1:5432/app"
        );
    }

    #[test]
    fn missing_required_key_yields_no_url() {
        let ns = node_with("database", "dblab", &[("DB_HOST", "localhost")]);
        assert!(build_connection_url(&ns).is_none());
    }

    #[test]
    fn auto_detects_the_only_db_node() {
        let run = run_with(vec![
            node_with("api", "local", &[("PORT", "3000")]),
            node_with(
                "database",
                "dblab",
                &[
                    ("DB_HOST", "localhost"),
                    ("DB_PORT", "5430"),
                    ("DB_NAME", "prosperity"),
                ],
            ),
        ]);
        let found = find_db_node(&run, None).ok().unwrap();
        assert_eq!(found.node_name, "database");
    }

    #[test]
    fn errors_when_no_db_node_present() {
        let run = run_with(vec![node_with("api", "local", &[("PORT", "3000")])]);
        assert!(matches!(
            find_db_node(&run, None),
            Err(DbNodeError::NotFound)
        ));
    }

    #[test]
    fn errors_when_multiple_db_nodes_without_filter() {
        let db = |name: &str| {
            node_with(
                name,
                "dblab",
                &[
                    ("DB_HOST", "localhost"),
                    ("DB_PORT", "5430"),
                    ("DB_NAME", "prosperity"),
                ],
            )
        };
        let run = run_with(vec![db("primary"), db("replica")]);
        match find_db_node(&run, None) {
            Err(DbNodeError::Ambiguous(names)) => {
                assert_eq!(names, vec!["primary:dblab", "replica:dblab"]);
            }
            _ => panic!("expected Ambiguous"),
        }
    }

    #[test]
    fn node_filter_disambiguates() {
        let db = |name: &str| {
            node_with(
                name,
                "dblab",
                &[
                    ("DB_HOST", "localhost"),
                    ("DB_PORT", "5430"),
                    ("DB_NAME", "prosperity"),
                ],
            )
        };
        let run = run_with(vec![db("primary"), db("replica")]);
        let found = find_db_node(&run, Some("replica")).ok().unwrap();
        assert_eq!(found.node_name, "replica");
    }

    #[test]
    fn node_filter_without_db_outputs_is_not_found() {
        let run = run_with(vec![
            node_with("api", "local", &[("PORT", "3000")]),
            node_with(
                "database",
                "dblab",
                &[
                    ("DB_HOST", "localhost"),
                    ("DB_PORT", "5430"),
                    ("DB_NAME", "prosperity"),
                ],
            ),
        ]);
        assert!(matches!(
            find_db_node(&run, Some("api")),
            Err(DbNodeError::NotFound)
        ));
    }
}

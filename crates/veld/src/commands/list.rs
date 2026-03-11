use veld_core::state::{GlobalRegistry, RunStatus};

use crate::output;

/// `veld list [--urls] [--json]`
pub async fn run(urls: bool, json: bool) -> i32 {
    let registry = match GlobalRegistry::load() {
        Ok(r) => r,
        Err(e) => {
            output::print_error(&format!("Failed to load registry: {e}"), json);
            return 1;
        }
    };

    if json {
        match serde_json::to_string_pretty(&registry) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                output::print_error(&format!("JSON serialization failed: {e}"), true);
                return 1;
            }
        }
    } else if registry.projects.is_empty() {
        output::print_info("No Veld projects found on this machine.");
    } else {
        let mut names: Vec<&String> = registry.projects.keys().collect();
        names.sort();

        for name in names {
            let entry = &registry.projects[name];
            let active_runs = entry
                .runs
                .values()
                .filter(|r| r.status == RunStatus::Running)
                .count();

            let status_label = if active_runs > 0 {
                output::green(&format!("{active_runs} running"))
            } else {
                output::dim("stopped")
            };

            println!(
                "  {} {} {}",
                output::bold(&entry.project_name),
                status_label,
                output::dim(&entry.project_root.display().to_string()),
            );

            // Show individual runs with their names and status.
            let mut run_names: Vec<&String> = entry.runs.keys().collect();
            run_names.sort();

            for run_name in run_names {
                let run_info = &entry.runs[run_name];
                let status_str = match run_info.status {
                    RunStatus::Running => output::green("running"),
                    RunStatus::Stopped => output::dim("stopped"),
                    _ => output::yellow(&format!("{:?}", run_info.status).to_lowercase()),
                };
                println!(
                    "    {} {}",
                    output::bold(run_name),
                    status_str,
                );

                if urls && run_info.status == RunStatus::Running {
                    let mut url_keys: Vec<&String> = run_info.urls.keys().collect();
                    url_keys.sort();
                    for node_key in url_keys {
                        println!(
                            "      {} {}",
                            output::cyan(node_key),
                            run_info.urls[node_key],
                        );
                    }
                }
            }
        }
    }

    0
}

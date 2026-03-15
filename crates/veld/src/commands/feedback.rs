use veld_core::feedback::FeedbackStore;
use veld_core::state::ProjectState;

use crate::output;

/// `veld feedback` — read, wait for, or list feedback batches.
pub async fn run(name: Option<String>, wait: bool, history: bool, json: bool) -> i32 {
    let (config_path, _config) = match super::load_config(json) {
        Some(pair) => pair,
        None => return 1,
    };
    let project_root = veld_core::config::project_root(&config_path);

    let project_state = match ProjectState::load(&project_root) {
        Ok(ps) => ps,
        Err(e) => {
            output::print_error(&format!("Failed to load project state: {e}"), json);
            return 1;
        }
    };

    // If --name was given explicitly, use it directly (feedback data may
    // outlive the run). Otherwise resolve from active runs.
    let run_name = match name {
        Some(n) => n,
        None => match super::resolve_run_name(None, &project_state, true, json) {
            Some(n) => n,
            None => return 1,
        },
    };

    let store = FeedbackStore::new(&project_root, &run_name);

    // Validate that feedback data exists — but skip for --wait mode since
    // we're waiting for feedback that doesn't exist yet.
    if !wait && !store.has_data() {
        // Check if the run is active (waiting for feedback) or truly unknown.
        if project_state.runs.contains_key(&run_name) {
            output::print_info("No feedback submitted yet for this run.");
        } else {
            output::print_error(
                &format!(
                    "No feedback data for run '{}'. Use `veld runs` to see available runs.",
                    run_name
                ),
                json,
            );
            return 1;
        }
        return 0;
    }

    if wait {
        return run_wait(&store, &run_name, json).await;
    }

    if history {
        return run_history(&store, json);
    }

    // Default: show latest batch.
    run_latest(&store, json)
}

fn run_latest(store: &FeedbackStore, json: bool) -> i32 {
    match store.get_latest_batch() {
        Ok(Some(batch)) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&batch).unwrap());
            } else {
                print_batch(&batch);
            }
            0
        }
        Ok(None) => {
            if json {
                println!("null");
            } else {
                output::print_info("No feedback batches submitted yet.");
            }
            0
        }
        Err(e) => {
            output::print_error(&format!("Failed to read feedback: {e}"), json);
            1
        }
    }
}

fn run_history(store: &FeedbackStore, json: bool) -> i32 {
    match store.get_batches() {
        Ok(batches) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&batches).unwrap());
            } else if batches.is_empty() {
                output::print_info("No feedback batches submitted yet.");
            } else {
                for (i, batch) in batches.iter().enumerate() {
                    if i > 0 {
                        println!();
                    }
                    print_batch(batch);
                }
            }
            0
        }
        Err(e) => {
            output::print_error(&format!("Failed to read feedback: {e}"), json);
            1
        }
    }
}

async fn run_wait(store: &FeedbackStore, run_name: &str, json: bool) -> i32 {
    if !json {
        output::print_info(&format!(
            "Waiting for feedback on run '{run_name}'... (Ctrl+C to cancel)"
        ));
    }

    let existing_count = store.get_batches().map(|b| b.len()).unwrap_or(0);

    // Poll every 2 seconds for new batches.
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        match store.get_batches() {
            Ok(batches) => {
                if batches.len() > existing_count {
                    // New batch(es) arrived.
                    let new_batches = &batches[existing_count..];
                    if json {
                        println!("{}", serde_json::to_string_pretty(&new_batches).unwrap());
                    } else {
                        for batch in new_batches {
                            print_batch(batch);
                        }
                    }
                    return 0;
                }
            }
            Err(e) => {
                output::print_error(&format!("Failed to read feedback: {e}"), json);
                return 1;
            }
        }
    }
}

fn print_batch(batch: &veld_core::feedback::FeedbackBatch) {
    println!(
        "{} Batch {} ({})",
        output::bold("Feedback"),
        output::dim(&batch.id[..8]),
        batch.submitted_at.format("%Y-%m-%d %H:%M:%S UTC"),
    );
    println!(
        "  {} comment(s) in run '{}'",
        batch.comments.len(),
        batch.run_name,
    );
    println!();
    for (i, comment) in batch.comments.iter().enumerate() {
        println!("  {}. {}", i + 1, comment.comment);
        if let Some(ref sel) = comment.element_selector {
            println!("     Element: {}", output::dim(sel));
        }
        if let Some(ref text) = comment.selected_text {
            let preview = if text.len() > 80 {
                format!("{}...", &text[..80])
            } else {
                text.clone()
            };
            println!("     Selected: \"{}\"", output::dim(&preview));
        }
        println!("     Page: {}", output::dim(&comment.page_url));
    }
}

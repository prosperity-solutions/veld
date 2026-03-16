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
            if json {
                println!("null");
            } else {
                output::print_info("No feedback submitted yet for this run.");
            }
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
                print_batch(&batch, Some(store));
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
                    print_batch(batch, Some(store));
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

    // Signal to the browser overlay that we're actively waiting.
    if let Err(e) = store.set_waiting() {
        output::print_error(&format!("Failed to set waiting marker: {e}"), json);
        return 1;
    }

    let existing_count = store.get_batches().map(|b| b.len()).unwrap_or(0);

    // Poll every 2 seconds for new batches or cancellation.
    let exit_code = loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        // Check if reviewer cancelled.
        if store.is_cancelled() {
            store.clear_cancelled();
            if json {
                println!("{}", serde_json::json!({ "outcome": "cancelled" }));
            } else {
                output::print_info("Feedback cancelled by reviewer.");
            }
            break 0;
        }

        match store.get_batches() {
            Ok(batches) => {
                if batches.len() > existing_count {
                    // New batch(es) arrived.
                    let new_batches = &batches[existing_count..];
                    if json {
                        println!("{}", serde_json::to_string_pretty(&new_batches).unwrap());
                    } else {
                        let has_comments = new_batches.iter().any(|b| !b.comments.is_empty());
                        if has_comments {
                            for batch in new_batches {
                                print_batch(batch, Some(store));
                            }
                        } else {
                            output::print_info("Reviewer approved — all good, no feedback.");
                        }
                    }
                    break 0;
                }
            }
            Err(e) => {
                output::print_error(&format!("Failed to read feedback: {e}"), json);
                break 1;
            }
        }
    };

    // Always clean up the waiting marker.
    store.clear_waiting();
    exit_code
}

fn print_batch(batch: &veld_core::feedback::FeedbackBatch, store: Option<&FeedbackStore>) {
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
            if !text.is_empty() {
                let preview = if text.chars().count() > 80 {
                    let truncated: String = text.chars().take(80).collect();
                    format!("{truncated}...")
                } else {
                    text.clone()
                };
                println!("     Selected: \"{}\"", output::dim(&preview));
            }
        }
        if let Some(ref trace) = comment.component_trace {
            if !trace.is_empty() {
                println!("     Components: {}", output::dim(&trace.join(" > ")));
            }
        }
        if let Some(ref screenshot) = comment.screenshot {
            if !screenshot.is_empty() {
                // Resolve to full path if store is available, otherwise show the ID.
                let display = if let Some(s) = store {
                    let id = screenshot
                        .trim_end_matches(".png")
                        .rsplit('/')
                        .next()
                        .unwrap_or(screenshot);
                    s.screenshot_path(&format!("{id}.png"))
                        .display()
                        .to_string()
                } else {
                    screenshot.clone()
                };
                println!("     Screenshot: {}", output::dim(&display));
            }
        }
        if let (Some(w), Some(h)) = (comment.viewport_width, comment.viewport_height) {
            println!("     Viewport: {}", output::dim(&format!("{w}x{h}")));
        }
        println!("     Page: {}", output::dim(&comment.page_url));
    }
}

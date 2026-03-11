use veld_core::state::GlobalRegistry;

use crate::output;

/// `veld gc` -- garbage-collect stale state, logs and orphaned processes.
pub async fn run() -> i32 {
    output::print_info("Garbage collecting stale state...");

    let mut registry = match GlobalRegistry::load() {
        Ok(r) => r,
        Err(e) => {
            output::print_error(&format!("Failed to load registry: {e}"), false);
            return 1;
        }
    };

    let before = registry.projects.len();

    // Remove entries whose project root no longer exists.
    registry
        .projects
        .retain(|_, entry| entry.project_root.exists());

    let removed = before - registry.projects.len();

    if let Err(e) = registry.save() {
        output::print_error(&format!("Failed to save registry: {e}"), false);
        return 1;
    }

    if removed > 0 {
        output::print_success(&format!(
            "Removed {removed} stale project(s) from registry."
        ));
    } else {
        output::print_success("Nothing to clean up.");
    }

    0
}

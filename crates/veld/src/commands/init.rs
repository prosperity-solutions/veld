use std::path::Path;

use crate::output;

const INIT_TEMPLATE: &str = r#"{
  "$schema": "https://veld.dev/schema/v1.json",
  "schemaVersion": "1",
  "name": "my-project",
  "url_template": "{service}.{run}.{project}.localhost",
  "presets": {
    "default": []
  },
  "nodes": {}
}
"#;

/// `veld init` -- create a starter veld.json in the current directory.
pub async fn run() -> i32 {
    let target = Path::new("veld.json");

    if target.exists() {
        output::print_error("veld.json already exists in this directory.", false);
        return 1;
    }

    match std::fs::write(target, INIT_TEMPLATE) {
        Ok(()) => {
            output::print_success(&format!("Created {}", target.display()));
            output::print_info("Edit the file to define your nodes, variants and presets.");
            0
        }
        Err(e) => {
            output::print_error(&format!("Failed to write veld.json: {e}"), false);
            1
        }
    }
}

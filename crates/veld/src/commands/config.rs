use crate::output;

/// `veld config [--path]`
///
/// Print the resolved veld.json contents. With `--path`, print only the file path.
pub async fn run(path_only: bool, json: bool) -> i32 {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            output::print_error(&format!("Failed to get current directory: {e}"), json);
            return 1;
        }
    };

    let config_path = match veld_core::config::discover_config(&cwd) {
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

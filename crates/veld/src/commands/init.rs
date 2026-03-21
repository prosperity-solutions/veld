use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use crate::output;

// ---------------------------------------------------------------------------
// Template fallback (non-TTY or empty detection)
// ---------------------------------------------------------------------------

const INIT_TEMPLATE: &str = r#"{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "my-project",
  "url_template": "{service}.{run}.{project}.localhost",
  "presets": {
    "default": []
  },
  "nodes": {}
}
"#;

// ---------------------------------------------------------------------------
// Detected service
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct DetectedService {
    name: String,
    path: String,
    dev_command: Option<String>,
    kind: ServiceKind,
}

#[derive(Debug, Clone)]
enum ServiceKind {
    Node,
    Cargo,
}

// ---------------------------------------------------------------------------
// Database detection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct DetectedDatabase {
    name: String,
    tool: String,
    script_hint: String,
}

// ---------------------------------------------------------------------------
// Project detection
// ---------------------------------------------------------------------------

fn detect_pnpm_workspaces(root: &Path) -> Vec<DetectedService> {
    let yaml_path = root.join("pnpm-workspace.yaml");
    let content = match fs::read_to_string(&yaml_path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    // Basic YAML parsing: look for lines like "  - packages/*" or "  - apps/*"
    // under a "packages:" key.
    let mut patterns: Vec<String> = Vec::new();
    let mut in_packages = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "packages:" {
            in_packages = true;
            continue;
        }
        if in_packages {
            if trimmed.starts_with("- ") {
                let pattern = trimmed
                    .strip_prefix("- ")
                    .unwrap()
                    .trim()
                    .trim_matches('\'')
                    .trim_matches('"');
                patterns.push(pattern.to_string());
            } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
                in_packages = false;
            }
        }
    }

    let mut services = Vec::new();
    for pattern in &patterns {
        // Expand simple globs: "packages/*" -> list dirs under packages/
        if let Some(base) = pattern.strip_suffix("/*") {
            let dir = root.join(base);
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        let pkg_json = entry.path().join("package.json");
                        if pkg_json.exists() {
                            let name = entry.file_name().to_string_lossy().to_string();
                            let dev_cmd = detect_dev_script(&pkg_json, &name, "pnpm");
                            services.push(DetectedService {
                                name: name.clone(),
                                path: format!("{}/{}", base, name),
                                dev_command: dev_cmd,
                                kind: ServiceKind::Node,
                            });
                        }
                    }
                }
            }
        } else if !pattern.contains('*') {
            // Exact path
            let dir = root.join(pattern);
            if dir.is_dir() {
                let pkg_json = dir.join("package.json");
                let name = dir
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let dev_cmd = if pkg_json.exists() {
                    detect_dev_script(&pkg_json, &name, "pnpm")
                } else {
                    None
                };
                services.push(DetectedService {
                    name,
                    path: pattern.clone(),
                    dev_command: dev_cmd,
                    kind: ServiceKind::Node,
                });
            }
        }
    }
    services
}

fn detect_npm_yarn_workspaces(root: &Path) -> Vec<DetectedService> {
    let pkg_path = root.join("package.json");
    let content = match fs::read_to_string(&pkg_path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    // Basic JSON parsing: look for "workspaces" array
    // Find "workspaces" key and extract string array entries
    let patterns = extract_json_string_array(&content, "workspaces");
    if patterns.is_empty() {
        return vec![];
    }

    // Determine package manager
    let pm = if root.join("yarn.lock").exists() {
        "yarn"
    } else {
        "npm"
    };

    let mut services = Vec::new();
    for pattern in &patterns {
        if let Some(base) = pattern.strip_suffix("/*") {
            let dir = root.join(base);
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        let pkg_json = entry.path().join("package.json");
                        if pkg_json.exists() {
                            let name = entry.file_name().to_string_lossy().to_string();
                            let dev_cmd = detect_dev_script(&pkg_json, &name, pm);
                            services.push(DetectedService {
                                name: name.clone(),
                                path: format!("{}/{}", base, name),
                                dev_command: dev_cmd,
                                kind: ServiceKind::Node,
                            });
                        }
                    }
                }
            }
        } else if !pattern.contains('*') {
            let dir = root.join(pattern);
            if dir.is_dir() {
                let pkg_json = dir.join("package.json");
                let name = dir
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let dev_cmd = if pkg_json.exists() {
                    detect_dev_script(&pkg_json, &name, pm)
                } else {
                    None
                };
                services.push(DetectedService {
                    name,
                    path: pattern.clone(),
                    dev_command: dev_cmd,
                    kind: ServiceKind::Node,
                });
            }
        }
    }
    services
}

fn detect_cargo_workspace(root: &Path) -> Vec<DetectedService> {
    let cargo_path = root.join("Cargo.toml");
    let content = match fs::read_to_string(&cargo_path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    // Check for [workspace] section
    if !content.contains("[workspace]") {
        return vec![];
    }

    // Extract members from workspace.members array
    // Look for `members = [...]`
    let members = extract_toml_string_array(&content, "members");

    let mut services = Vec::new();
    for pattern in &members {
        if let Some(base) = pattern.strip_suffix("/*") {
            let dir = root.join(base);
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        let crate_toml = entry.path().join("Cargo.toml");
                        if crate_toml.exists() {
                            let name = entry.file_name().to_string_lossy().to_string();
                            let has_bin = detect_cargo_binary(&crate_toml);
                            let dev_cmd = if has_bin {
                                Some(format!("cargo run -p {}", name))
                            } else {
                                None
                            };
                            services.push(DetectedService {
                                name,
                                path: format!("{}/{}", base, entry.file_name().to_string_lossy()),
                                dev_command: dev_cmd,
                                kind: ServiceKind::Cargo,
                            });
                        }
                    }
                }
            }
        } else if !pattern.contains('*') {
            let dir = root.join(pattern);
            if dir.is_dir() {
                let crate_toml = dir.join("Cargo.toml");
                let name = dir
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let has_bin = if crate_toml.exists() {
                    detect_cargo_binary(&crate_toml)
                } else {
                    false
                };
                let dev_cmd = if has_bin {
                    Some(format!("cargo run -p {}", name))
                } else {
                    None
                };
                services.push(DetectedService {
                    name,
                    path: pattern.clone(),
                    dev_command: dev_cmd,
                    kind: ServiceKind::Cargo,
                });
            }
        }
    }
    services
}

/// Check if a package.json has a "dev" script; return a suggested command.
fn detect_dev_script(pkg_json: &Path, name: &str, pm: &str) -> Option<String> {
    let content = fs::read_to_string(pkg_json).ok()?;
    // Look for "dev" inside "scripts"
    if has_json_script(&content, "dev") {
        Some(format!("{} --filter {} dev", pm, name))
    } else if has_json_script(&content, "start") {
        Some(format!("{} --filter {} start", pm, name))
    } else {
        None
    }
}

/// Basic check: does the JSON content have "scripts": { ... "dev": ... }?
fn has_json_script(content: &str, script_name: &str) -> bool {
    // Find "scripts" section, then look for the script name
    if let Some(scripts_pos) = content.find("\"scripts\"") {
        let rest = &content[scripts_pos..];
        if let Some(brace) = rest.find('{') {
            let scripts_block = &rest[brace..];
            // Find the matching close brace (simple: first '}')
            if let Some(end) = scripts_block.find('}') {
                let block = &scripts_block[..end];
                return block.contains(&format!("\"{}\"", script_name));
            }
        }
    }
    false
}

/// Check if a Cargo.toml has [[bin]] or src/main.rs
fn detect_cargo_binary(cargo_toml: &Path) -> bool {
    if let Ok(content) = fs::read_to_string(cargo_toml) {
        if content.contains("[[bin]]") {
            return true;
        }
    }
    // Also check for src/main.rs
    if let Some(dir) = cargo_toml.parent() {
        if dir.join("src/main.rs").exists() {
            return true;
        }
    }
    false
}

/// Extract a JSON string array value for a given key.
/// Very basic: finds `"key": [` and reads strings until `]`.
fn extract_json_string_array(content: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{}\"", key);
    let pos = match content.find(&needle) {
        Some(p) => p,
        None => return vec![],
    };
    let rest = &content[pos + needle.len()..];
    // Skip whitespace and colon
    let rest = rest.trim_start();
    let rest = rest.strip_prefix(':').unwrap_or(rest).trim_start();
    if !rest.starts_with('[') {
        return vec![];
    }
    let rest = &rest[1..];
    let end = match rest.find(']') {
        Some(e) => e,
        None => return vec![],
    };
    let block = &rest[..end];
    let mut result = Vec::new();
    for segment in block.split(',') {
        let s = segment.trim().trim_matches('"').trim_matches('\'');
        if !s.is_empty() {
            result.push(s.to_string());
        }
    }
    result
}

/// Extract a TOML string array: `key = ["a", "b"]`
fn extract_toml_string_array(content: &str, key: &str) -> Vec<String> {
    let needle = format!("{} = [", key);
    // Also try without spaces around =
    let pos = content
        .find(&needle)
        .or_else(|| content.find(&format!("{}= [", key)))
        .or_else(|| content.find(&format!("{} =[", key)))
        .or_else(|| content.find(&format!("{}=[", key)));
    let pos = match pos {
        Some(p) => p,
        None => return vec![],
    };
    let rest = &content[pos..];
    let bracket = match rest.find('[') {
        Some(b) => b,
        None => return vec![],
    };
    let rest = &rest[bracket + 1..];
    let end = match rest.find(']') {
        Some(e) => e,
        None => return vec![],
    };
    let block = &rest[..end];
    let mut result = Vec::new();
    for segment in block.split(',') {
        let s = segment.trim().trim_matches('"').trim_matches('\'').trim();
        if !s.is_empty() {
            result.push(s.to_string());
        }
    }
    result
}

/// Detect database tools (Prisma, Drizzle).
fn detect_databases(root: &Path) -> Vec<DetectedDatabase> {
    let mut dbs = Vec::new();

    // Prisma: look for prisma/schema.prisma anywhere in the tree (one level)
    if root.join("prisma/schema.prisma").exists() {
        dbs.push(DetectedDatabase {
            name: "database".to_string(),
            tool: "Prisma".to_string(),
            script_hint: "npx prisma db push".to_string(),
        });
    } else {
        // Check inside workspace packages
        for subdir in &["packages", "apps", "services"] {
            let dir = root.join(subdir);
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    if entry.path().join("prisma/schema.prisma").exists() {
                        dbs.push(DetectedDatabase {
                            name: "database".to_string(),
                            tool: "Prisma".to_string(),
                            script_hint: format!(
                                "cd {} && npx prisma db push",
                                entry
                                    .path()
                                    .strip_prefix(root)
                                    .unwrap_or(&entry.path())
                                    .display()
                            ),
                        });
                        break;
                    }
                }
            }
            if !dbs.is_empty() {
                break;
            }
        }
    }

    // Drizzle: look for drizzle.config.*
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("drizzle.config.") {
                dbs.push(DetectedDatabase {
                    name: "database".to_string(),
                    tool: "Drizzle".to_string(),
                    script_hint: "npx drizzle-kit push".to_string(),
                });
                break;
            }
        }
    }

    dbs
}

// ---------------------------------------------------------------------------
// Interactive helpers
// ---------------------------------------------------------------------------

fn prompt(msg: &str, default: &str) -> String {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    if default.is_empty() {
        print!("{}: ", msg);
    } else {
        print!("{} {}: ", msg, output::dim(&format!("[{}]", default)));
    }
    let _ = stdout.flush();

    let mut line = String::new();
    if stdin.lock().read_line(&mut line).is_ok() {
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            default.to_string()
        } else {
            trimmed
        }
    } else {
        default.to_string()
    }
}

fn prompt_yes_no(msg: &str, default_yes: bool) -> bool {
    let hint = if default_yes { "Y/n" } else { "y/N" };
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    print!("{} {}: ", msg, output::dim(&format!("[{}]", hint)));
    let _ = stdout.flush();

    let mut line = String::new();
    if stdin.lock().read_line(&mut line).is_ok() {
        let trimmed = line.trim().to_lowercase();
        if trimmed.is_empty() {
            default_yes
        } else {
            trimmed.starts_with('y')
        }
    } else {
        default_yes
    }
}

fn slugify(s: &str) -> String {
    veld_core::url::slugify(s)
}

// ---------------------------------------------------------------------------
// JSON generation
// ---------------------------------------------------------------------------

fn generate_veld_json(
    project_name: &str,
    url_template: &str,
    services: &[(DetectedService, String)], // (service, confirmed_command)
    db_steps: &[(String, String)],          // (name, script)
    deps: &[(String, Vec<String>)],         // (service_name, dep_names)
) -> String {
    let mut json = String::new();
    json.push_str("{\n");
    json.push_str("  \"$schema\": \"https://veld.oss.life.li/schema/v1/veld.schema.json\",\n");
    json.push_str("  \"schemaVersion\": \"1\",\n");
    json.push_str(&format!("  \"name\": \"{}\",\n", escape_json(project_name)));
    json.push_str(&format!(
        "  \"url_template\": \"{}\",\n",
        escape_json(url_template)
    ));
    json.push_str("  \"presets\": {\n");

    // Build default preset from all server nodes
    let preset_entries: Vec<String> = services
        .iter()
        .map(|(s, _)| format!("\"{}:local\"", escape_json(&s.name)))
        .collect();
    json.push_str(&format!(
        "    \"default\": [{}]\n",
        preset_entries.join(", ")
    ));
    json.push_str("  },\n");
    json.push_str("  \"nodes\": {\n");

    let mut node_entries: Vec<String> = Vec::new();

    // Database / command step nodes first
    for (name, script) in db_steps {
        let mut node = String::new();
        node.push_str(&format!("    \"{}\": {{\n", escape_json(name)));
        node.push_str("      \"default_variant\": \"local\",\n");
        node.push_str("      \"variants\": {\n");
        node.push_str("        \"local\": {\n");
        node.push_str("          \"type\": \"command\",\n");
        node.push_str(&format!(
            "          \"script\": \"{}\"\n",
            escape_json(script)
        ));
        node.push_str("        }\n");
        node.push_str("      }\n");
        node.push_str("    }");
        node_entries.push(node);
    }

    // Service nodes
    for (service, command) in services {
        let mut node = String::new();
        node.push_str(&format!("    \"{}\": {{\n", escape_json(&service.name)));
        node.push_str("      \"default_variant\": \"local\",\n");
        node.push_str("      \"variants\": {\n");
        node.push_str("        \"local\": {\n");
        node.push_str("          \"type\": \"start_server\",\n");
        node.push_str(&format!(
            "          \"command\": \"{}\",\n",
            escape_json(command)
        ));
        node.push_str("          \"health_check\": { \"type\": \"port\" }");

        // Add depends_on if any
        let service_deps: Vec<&String> = deps
            .iter()
            .filter(|(n, _)| n == &service.name)
            .flat_map(|(_, d)| d.iter())
            .collect();
        if !service_deps.is_empty() {
            node.push_str(",\n          \"depends_on\": {");
            let dep_strs: Vec<String> = service_deps
                .iter()
                .map(|d| format!(" \"{}\": \"local\"", escape_json(d)))
                .collect();
            node.push_str(&dep_strs.join(","));
            node.push_str(" }");
        }

        node.push('\n');
        node.push_str("        }\n");
        node.push_str("      }\n");
        node.push_str("    }");
        node_entries.push(node);
    }

    json.push_str(&node_entries.join(",\n"));
    json.push('\n');
    json.push_str("  }\n");
    json.push_str("}\n");
    json
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

// ---------------------------------------------------------------------------
// .gitignore management
// ---------------------------------------------------------------------------

fn add_veld_to_gitignore(root: &Path) {
    let gitignore = root.join(".gitignore");
    if gitignore.exists() {
        if let Ok(content) = fs::read_to_string(&gitignore) {
            // Check if .veld/ is already ignored
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed == ".veld/"
                    || trimmed == ".veld"
                    || trimmed == "/.veld/"
                    || trimmed == "/.veld"
                {
                    return; // Already present
                }
            }
            // Append
            let mut new_content = content.clone();
            if !new_content.ends_with('\n') {
                new_content.push('\n');
            }
            new_content.push_str("\n# Veld local state\n.veld/\n");
            let _ = fs::write(&gitignore, new_content);
        }
    } else {
        // Create .gitignore with .veld/
        let _ = fs::write(&gitignore, "# Veld local state\n.veld/\n");
    }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// `veld init` -- create a starter veld.json in the current directory.
pub async fn run() -> i32 {
    let target = Path::new("veld.json");

    if target.exists() {
        output::print_error("veld.json already exists in this directory.", false);
        return 1;
    }

    // Non-TTY fallback: write template directly
    if !output::is_tty() {
        return write_template(target);
    }

    // Interactive mode
    let root = match std::env::current_dir() {
        Ok(d) => d,
        Err(_) => PathBuf::from("."),
    };

    println!();
    println!("{}", output::bold("  Veld Init"));
    println!();

    // --- Detect project structure ---
    let mut services: Vec<DetectedService> = Vec::new();
    let mut workspace_type = String::new();

    let pnpm = detect_pnpm_workspaces(&root);
    if !pnpm.is_empty() {
        workspace_type = "pnpm workspace".to_string();
        services = pnpm;
    }

    if services.is_empty() {
        let npm_yarn = detect_npm_yarn_workspaces(&root);
        if !npm_yarn.is_empty() {
            workspace_type = if root.join("yarn.lock").exists() {
                "yarn workspace".to_string()
            } else {
                "npm workspace".to_string()
            };
            services = npm_yarn;
        }
    }

    if services.is_empty() {
        let cargo = detect_cargo_workspace(&root);
        if !cargo.is_empty() {
            workspace_type = "Cargo workspace".to_string();
            services = cargo;
        }
    }

    let databases = detect_databases(&root);

    // --- Print detection results ---
    if !workspace_type.is_empty() {
        println!(
            "  {} Detected {}",
            output::checkmark(),
            output::bold(&workspace_type)
        );
    }

    if services.is_empty() && databases.is_empty() {
        println!(
            "  {} No workspace or services detected. Starting from scratch.",
            output::dim("i")
        );
    } else {
        if !services.is_empty() {
            println!(
                "  {} Found {} service{}:",
                output::dim("i"),
                services.len(),
                if services.len() == 1 { "" } else { "s" }
            );
            for (i, s) in services.iter().enumerate() {
                let cmd_hint = s
                    .dev_command
                    .as_deref()
                    .map(|c| format!(" {}", output::dim(c)))
                    .unwrap_or_default();
                println!("      {}) {}{}", i + 1, output::bold(&s.name), cmd_hint,);
            }
        }
        if !databases.is_empty() {
            for db in &databases {
                println!(
                    "  {} Detected {} ({})",
                    output::checkmark(),
                    output::bold(&db.tool),
                    output::dim(&db.script_hint),
                );
            }
        }
    }

    println!();

    // --- Project name ---
    let dir_name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "my-project".to_string());
    let default_name = slugify(&dir_name);
    let project_name = prompt(
        &format!("  {}", output::bold("Project name")),
        &default_name,
    );

    // --- Select services ---
    let selected_services: Vec<DetectedService> = if services.is_empty() {
        vec![]
    } else {
        println!();
        let all_range = format!("1-{}", services.len());
        let selection = prompt(
            &format!(
                "  {} {}",
                output::bold("Include services"),
                output::dim("(comma-separated, e.g. 1,3 or 1-3)")
            ),
            &all_range,
        );
        parse_selection(&selection, &services)
    };

    // --- Confirm commands and ask about dependencies ---
    let mut confirmed: Vec<(DetectedService, String)> = Vec::new();
    let mut all_deps: Vec<(String, Vec<String>)> = Vec::new();

    if !selected_services.is_empty() {
        println!();
        println!("  {}", output::bold("Configure services:"));

        let service_names: Vec<String> = selected_services.iter().map(|s| s.name.clone()).collect();
        let db_names: Vec<String> = databases.iter().map(|d| d.name.clone()).collect();
        let all_dep_candidates: Vec<String> = service_names
            .iter()
            .chain(db_names.iter())
            .cloned()
            .collect();

        for service in &selected_services {
            println!();
            println!("    {}", output::bold(&service.name));

            let default_cmd = service.dev_command.clone().unwrap_or_default();
            let command = prompt(&format!("    {}", "Dev command"), &default_cmd);

            // Ask about dependencies (other selected services or databases)
            let other_deps: Vec<&String> = all_dep_candidates
                .iter()
                .filter(|n| *n != &service.name)
                .collect();

            let mut service_deps: Vec<String> = Vec::new();
            if !other_deps.is_empty() {
                let dep_list: Vec<String> = other_deps.iter().map(|d| d.to_string()).collect();
                let dep_hint = dep_list.join(", ");
                let dep_input = prompt(
                    &format!(
                        "    {} {}",
                        "Dependencies",
                        output::dim(&format!("(available: {})", dep_hint))
                    ),
                    "",
                );
                if !dep_input.is_empty() {
                    for dep in dep_input.split(',') {
                        let dep = dep.trim().to_string();
                        if all_dep_candidates.contains(&dep) {
                            service_deps.push(dep);
                        }
                    }
                }
            }

            if !service_deps.is_empty() {
                all_deps.push((service.name.clone(), service_deps));
            }

            confirmed.push((service.clone(), command));
        }
    }

    // --- Database steps ---
    let mut db_steps: Vec<(String, String)> = Vec::new();
    if !databases.is_empty() {
        println!();
        for db in &databases {
            if prompt_yes_no(
                &format!("  Add {} setup step?", output::bold(&db.tool),),
                true,
            ) {
                let script = prompt(&format!("    {}", "Setup script"), &db.script_hint);
                db_steps.push((db.name.clone(), script));
            }
        }
    }

    // --- URL template ---
    println!();
    let default_url = format!("{{service}}.{{run}}.{}.localhost", slugify(&project_name));
    let url_template = prompt(&format!("  {}", output::bold("URL template")), &default_url);

    // --- Generate and write ---
    let json = if confirmed.is_empty() && db_steps.is_empty() {
        // No services detected/selected: write basic template with project name
        format!(
            r#"{{
  "$schema": "https://veld.oss.life.li/schema/v1/veld.schema.json",
  "schemaVersion": "1",
  "name": "{}",
  "url_template": "{}",
  "presets": {{
    "default": []
  }},
  "nodes": {{}}
}}
"#,
            escape_json(&project_name),
            escape_json(&url_template),
        )
    } else {
        generate_veld_json(
            &project_name,
            &url_template,
            &confirmed,
            &db_steps,
            &all_deps,
        )
    };

    match fs::write(target, &json) {
        Ok(()) => {
            println!();
            output::print_success(&format!("Created {}", target.display()));

            // Add .veld/ to .gitignore
            add_veld_to_gitignore(&root);
            output::print_success("Added .veld/ to .gitignore");

            println!();
            output::print_info(&format!(
                "  Next: edit {} to fine-tune, then run {}",
                output::bold("veld.json"),
                output::bold("veld start"),
            ));
            println!();
            0
        }
        Err(e) => {
            output::print_error(&format!("Failed to write veld.json: {e}"), false);
            1
        }
    }
}

/// Non-TTY fallback: write the static template.
fn write_template(target: &Path) -> i32 {
    match fs::write(target, INIT_TEMPLATE) {
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

/// Parse a selection string like "1,3,5" or "1-3" or "1-3,5" into services.
fn parse_selection(input: &str, services: &[DetectedService]) -> Vec<DetectedService> {
    let mut indices: Vec<usize> = Vec::new();
    for part in input.split(',') {
        let part = part.trim();
        if let Some((start, end)) = part.split_once('-') {
            if let (Ok(s), Ok(e)) = (start.trim().parse::<usize>(), end.trim().parse::<usize>()) {
                for i in s..=e {
                    if i >= 1 && i <= services.len() {
                        indices.push(i - 1);
                    }
                }
            }
        } else if let Ok(n) = part.parse::<usize>() {
            if n >= 1 && n <= services.len() {
                indices.push(n - 1);
            }
        }
    }
    indices.sort();
    indices.dedup();
    indices.into_iter().map(|i| services[i].clone()).collect()
}

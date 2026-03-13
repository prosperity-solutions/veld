use std::collections::HashMap;

use crate::variables::{self, VariableError};

// ---------------------------------------------------------------------------
// Slugification
// ---------------------------------------------------------------------------

/// Slugify a string for use in URLs:
/// lowercase, non-alphanumeric -> `-`, collapse consecutive `-`,
/// strip leading/trailing `-`, max 48 characters.
pub fn slugify(input: &str) -> String {
    let mut slug = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else {
            slug.push('-');
        }
    }

    // Collapse consecutive dashes.
    let mut collapsed = String::with_capacity(slug.len());
    let mut prev_dash = false;
    for ch in slug.chars() {
        if ch == '-' {
            if !prev_dash {
                collapsed.push('-');
            }
            prev_dash = true;
        } else {
            collapsed.push(ch);
            prev_dash = false;
        }
    }

    // Strip leading/trailing dashes.
    let trimmed = collapsed.trim_matches('-');

    // Max 48 characters.
    if trimmed.len() > 48 {
        trimmed[..48].trim_end_matches('-').to_owned()
    } else {
        trimmed.to_owned()
    }
}

// ---------------------------------------------------------------------------
// Run name generation
// ---------------------------------------------------------------------------

/// Generate a random, human-friendly run name (e.g. "swift-falcon").
///
/// Uses two-word petnames (adjective-noun) with hyphen separators,
/// similar to Docker container names.
pub fn generate_run_name() -> String {
    petname::petname(2, "-").unwrap_or_else(|| "default".to_owned())
}

// ---------------------------------------------------------------------------
// URL template resolution (cascade: variant > node > project > built-in)
// ---------------------------------------------------------------------------

/// Resolve the effective URL template for a given node+variant, using the
/// most specific override: variant > node > project.
pub fn resolve_url_template<'a>(
    project_template: &'a str,
    node_template: Option<&'a str>,
    variant_template: Option<&'a str>,
) -> &'a str {
    if let Some(t) = variant_template {
        return t;
    }
    if let Some(t) = node_template {
        return t;
    }
    project_template
}

// ---------------------------------------------------------------------------
// URL template evaluation
// ---------------------------------------------------------------------------

/// Build the complete URL for a node given the URL template and context values.
///
/// Template syntax uses `{var}` (not `${var}`) and supports `{a ?? b}` fallback.
pub fn evaluate_url_template(
    template: &str,
    values: &HashMap<String, String>,
) -> Result<String, VariableError> {
    variables::interpolate_url_template(template, values)
}

/// Build the template variables map for a given node in a run.
#[allow(clippy::too_many_arguments)]
pub fn build_url_template_values(
    service: &str,
    variant: &str,
    run_name: &str,
    project: &str,
    branch: &str,
    worktree: &str,
    username: &str,
    hostname: &str,
) -> HashMap<String, String> {
    let mut values = HashMap::new();
    values.insert("service".to_owned(), slugify(service));
    values.insert("variant".to_owned(), slugify(variant));
    values.insert("run".to_owned(), slugify(run_name));
    values.insert("project".to_owned(), slugify(project));
    values.insert("branch".to_owned(), slugify(branch));
    values.insert("worktree".to_owned(), slugify(worktree));
    values.insert("username".to_owned(), slugify(username));
    values.insert("hostname".to_owned(), slugify(hostname));
    values
}

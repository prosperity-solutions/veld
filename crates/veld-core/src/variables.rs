use std::collections::HashMap;

use thiserror::Error;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum VariableError {
    #[error("unresolved variable reference: {0}")]
    Unresolved(String),

    #[error("unknown built-in variable: {0}")]
    UnknownBuiltin(String),
}

// ---------------------------------------------------------------------------
// Variable context — all values available during interpolation
// ---------------------------------------------------------------------------

/// Holds all resolvable values for a single node's interpolation context.
#[derive(Debug, Clone, Default)]
pub struct VariableContext {
    /// Built-in `veld.*` variables.
    pub builtins: HashMap<String, String>,

    /// Resolved outputs from upstream nodes.
    /// Key format: `"nodes.name.field"` or `"nodes.name:variant.field"`.
    pub node_outputs: HashMap<String, String>,

    /// The live outputs of the node an action runs against, referenced as
    /// `${output.KEY}`. Only populated in action contexts.
    pub outputs: HashMap<String, String>,

    /// Static action parameters, referenced as `${param.KEY}`. Only populated
    /// in action contexts.
    pub params: HashMap<String, String>,
}

impl VariableContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a built-in veld variable (e.g. "port", "run", "root").
    pub fn set_builtin(&mut self, key: &str, value: String) {
        self.builtins.insert(key.to_owned(), value);
    }

    /// Register an output from an upstream node.
    /// `key` should be like `"nodes.backend.url"` or `"nodes.backend:local.url"`.
    pub fn set_node_output(&mut self, key: &str, value: String) {
        self.node_outputs.insert(key.to_owned(), value);
    }

    /// Set one of the running node's own outputs, referenced as `${output.KEY}`.
    pub fn set_output(&mut self, key: &str, value: String) {
        self.outputs.insert(key.to_owned(), value);
    }

    /// Set an action parameter, referenced as `${param.KEY}`.
    pub fn set_param(&mut self, key: &str, value: String) {
        self.params.insert(key.to_owned(), value);
    }
}

// ---------------------------------------------------------------------------
// Interpolation
// ---------------------------------------------------------------------------

/// Interpolate all `${...}` references in a template string.
///
/// Supported forms:
/// - `${veld.port}`, `${veld.url}`, `${veld.run}`, etc.
/// - `${veld.url.hostname}`, `${veld.url.host}`, `${veld.url.origin}`, `${veld.url.scheme}`, `${veld.url.port}`
/// - `${nodes.name.field}`, `${nodes.name:variant.field}`
/// - `${nodes.name.url.hostname}`, `${nodes.name.url.host}`, etc.
pub fn interpolate(template: &str, ctx: &VariableContext) -> Result<String, VariableError> {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find("${") {
        result.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];

        let end = after_open.find('}').ok_or_else(|| {
            VariableError::Unresolved(format!("unclosed ${{ at position {start}"))
        })?;

        let ref_str = &after_open[..end];
        let value = resolve_reference(ref_str, ctx)?;
        result.push_str(&value);

        rest = &after_open[end + 1..];
    }

    result.push_str(rest);
    Ok(result)
}

/// Resolve a single reference (the part between `${` and `}`).
fn resolve_reference(reference: &str, ctx: &VariableContext) -> Result<String, VariableError> {
    if let Some(builtin_key) = reference.strip_prefix("veld.") {
        ctx.builtins
            .get(builtin_key)
            .cloned()
            .ok_or_else(|| VariableError::UnknownBuiltin(reference.to_owned()))
    } else if let Some(output_key) = reference.strip_prefix("output.") {
        ctx.outputs
            .get(output_key)
            .cloned()
            .ok_or_else(|| VariableError::Unresolved(format!("${{{reference}}}")))
    } else if let Some(param_key) = reference.strip_prefix("param.") {
        ctx.params
            .get(param_key)
            .cloned()
            .ok_or_else(|| VariableError::Unresolved(format!("${{{reference}}}")))
    } else if reference.starts_with("nodes.") {
        ctx.node_outputs
            .get(reference)
            .cloned()
            .ok_or_else(|| VariableError::Unresolved(format!("${{{reference}}}")))
    } else {
        Err(VariableError::Unresolved(format!("${{{reference}}}")))
    }
}

// ---------------------------------------------------------------------------
// URL template fallback operator
// ---------------------------------------------------------------------------

/// Evaluate the `??` fallback operator within a single template segment.
///
/// Given `"branch ?? run"`, returns the first non-empty value.
pub fn evaluate_fallback(expr: &str, values: &HashMap<String, String>) -> Option<String> {
    for part in expr.split("??") {
        let key = part.trim();
        if let Some(val) = values.get(key) {
            if !val.is_empty() {
                return Some(val.clone());
            }
        }
    }
    None
}

/// Interpolate a URL template that uses `{var}` syntax (not `${var}`).
///
/// Supports `{a ?? b}` fallback expressions.
pub fn interpolate_url_template(
    template: &str,
    values: &HashMap<String, String>,
) -> Result<String, VariableError> {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find('{') {
        result.push_str(&rest[..start]);
        let after_open = &rest[start + 1..];

        let end = after_open.find('}').ok_or_else(|| {
            VariableError::Unresolved(format!("unclosed {{ in URL template at position {start}"))
        })?;

        let expr = &after_open[..end];
        let value = if expr.contains("??") {
            evaluate_fallback(expr, values).ok_or_else(|| {
                VariableError::Unresolved(format!(
                    "no non-empty value for fallback expression \"{expr}\""
                ))
            })?
        } else {
            let key = expr.trim();
            values.get(key).cloned().ok_or_else(|| {
                VariableError::Unresolved(format!("unknown URL template variable \"{key}\""))
            })?
        };

        result.push_str(&value);
        rest = &after_open[end + 1..];
    }

    result.push_str(rest);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_output_and_param_namespaces() {
        let mut ctx = VariableContext::new();
        ctx.set_builtin("run", "dev".to_owned());
        ctx.set_output("DB_HOST", "localhost".to_owned());
        ctx.set_output("DB_PORT", "5432".to_owned());
        ctx.set_param("app", "Postico".to_owned());

        let out = interpolate(
            "open -a ${param.app} pg://${output.DB_HOST}:${output.DB_PORT}/${veld.run}",
            &ctx,
        )
        .unwrap();
        assert_eq!(out, "open -a Postico pg://localhost:5432/dev");
    }

    #[test]
    fn unknown_output_reference_errors() {
        let ctx = VariableContext::new();
        assert!(interpolate("${output.MISSING}", &ctx).is_err());
    }
}

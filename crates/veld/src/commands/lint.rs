use veld_core::config::{Finding, Severity};

use crate::output;

/// `veld lint [--json]`
///
/// Run every semantic config rule and report all of them at once. This is the
/// CI-facing half of the parse/validate split: `veld start` refuses on the same
/// `error`-severity findings, but only `lint` also surfaces warnings, and
/// neither set is reachable from the loader that `stop`/`status`/`logs` use.
///
/// Exit code is 1 when any finding is an error, 0 otherwise — warnings never
/// fail the command, so a repo can adopt them incrementally.
pub async fn run(json: bool) -> i32 {
    let Some((config_path, config)) = super::parse_config(json) else {
        return 1;
    };

    let findings = veld_core::config::validate(&config);
    let errors = findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .count();
    let warnings = findings
        .iter()
        .filter(|f| f.severity == Severity::Warning)
        .count();
    let notices = findings.len() - errors - warnings;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "config": config_path,
                "errors": errors,
                "warnings": warnings,
                "notices": notices,
                "findings": findings,
            }))
            .unwrap()
        );
    } else {
        print_human(&config_path, &findings, errors, warnings, notices);
    }

    if errors > 0 { 1 } else { 0 }
}

fn print_human(
    config_path: &std::path::Path,
    findings: &[Finding],
    errors: usize,
    warnings: usize,
    notices: usize,
) {
    if findings.is_empty() {
        output::print_success(&format!("{} is valid", config_path.display()));
        return;
    }
    // A config whose only findings are notices is valid; say so, then say what
    // there is to know.
    if errors == 0 && warnings == 0 {
        output::print_success(&format!("{} is valid", config_path.display()));
        println!();
    }

    for f in findings {
        let tag = match f.severity {
            Severity::Error => output::red("error"),
            Severity::Warning => output::yellow("warning"),
            // Nothing is wrong; this is information, so it is not coloured like a
            // problem.
            Severity::Notice => output::cyan("notice"),
        };
        println!(
            "{tag} {} {}",
            output::bold(&f.location),
            output::dim(&format!("[{}]", f.rule))
        );
        println!("      {}", f.message);
    }

    println!();
    let mut parts = Vec::new();
    if errors > 0 {
        parts.push(format!("{errors} error(s)"));
    }
    if warnings > 0 {
        parts.push(format!("{warnings} warning(s)"));
    }
    if notices > 0 {
        parts.push(format!("{notices} notice(s)"));
    }
    println!("{}", parts.join(", "));
}

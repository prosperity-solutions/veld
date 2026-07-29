use crate::output;
use veld_core::presets::{self, ResolvedPreset};

/// `veld presets [--json] [--pin]`
pub async fn run(json: bool, pin: bool) -> i32 {
    let Some((_config_path, config)) = super::parse_config(json) else {
        return 1;
    };

    let resolved = presets::resolve(&config);

    if json {
        // The whole record, `when_to_use` included: this is what a coding agent
        // reads to decide what to start.
        println!("{}", serde_json::to_string_pretty(&resolved).unwrap());
        return 0;
    }

    if resolved.is_empty() {
        output::print_info("No presets defined.");
        return 0;
    }

    if pin {
        print_pin_block(&resolved);
        return 0;
    }

    print_listing(&config, &resolved);
    0
}

/// The human listing.
///
/// Not a table, deliberately. `skills/veld/SKILL.md` injects this command's
/// output into every coding agent's context, so `when_to_use` has to appear
/// here — and prose does not fit in a column. The shape below stays scannable
/// for a person and unambiguous for an agent: one key, one label, one line of
/// intent, one line of what it actually starts.
fn print_listing(config: &veld_core::config::VeldConfig, resolved: &[ResolvedPreset]) {
    let show_groups = presets::has_groups(config);

    let mut current_group: Option<Option<String>> = None;
    for preset in resolved {
        if show_groups && current_group.as_ref() != Some(&preset.group) {
            println!();
            println!(
                "{}",
                output::bold(preset.group.as_deref().unwrap_or("Other")),
            );
            current_group = Some(preset.group.clone());
        }

        let key = output::cyan(&format!("[{}]", preset.key));
        // The config key is what `--preset` and scripts take, so it stays visible
        // even when a label is what the reader is scanning for. Parenthesised
        // rather than dimmed: this output is piped into coding agents' context
        // with colour stripped, where `Local dev dev-local` reads as one name.
        let name = if preset.label.is_some() {
            format!(" {}", output::dim(&format!("({})", preset.name)))
        } else {
            String::new()
        };
        let default_marker = if preset.is_default {
            format!(" {}", output::green("(default)"))
        } else {
            String::new()
        };
        println!(
            "  {key} {}{name}{default_marker}",
            output::bold(preset.display_label()),
        );
        if let Some(when) = &preset.when_to_use {
            println!("      {when}");
        }
        println!("      {}", output::dim(&preset.selections.join(", ")));
    }

    println!();

    // Which numbers are promises and which are conveniences. Without this the
    // distinction is invisible, and an auto key that silently moves is the exact
    // bug pinning exists to prevent.
    let auto: Vec<String> = resolved
        .iter()
        .filter(|p| !p.pinned)
        .map(|p| p.key.to_string())
        .collect();
    if !auto.is_empty() {
        output::print_info(&format!(
            "Auto-assigned keys: {}. These can change when presets are added — pin one \
             with \"key\": <n> (`veld presets --pin` prints the block to paste).",
            auto.join(", ")
        ));
    }
}

/// `--pin`: print the current numbering as a paste-ready `presets` block.
///
/// Prints rather than writes. veld does not rewrite a user's config — a serde
/// round-trip deletes every comment, and this file is JSONC precisely so those
/// comments can exist. So the author (or their agent) applies it, and `veld lint`
/// is the check afterwards.
fn print_pin_block(resolved: &[ResolvedPreset]) {
    println!("// Paste into veld.json to freeze the current preset numbering.");
    println!("// Pinned keys never move, whatever is added, removed, or regrouped around them.");
    println!("\"presets\": {{");
    for (i, preset) in resolved.iter().enumerate() {
        let mut fields = vec![format!("\"key\": {}", preset.key)];
        if let Some(label) = &preset.label {
            fields.push(format!("\"label\": {}", json_string(label)));
        }
        if let Some(when) = &preset.when_to_use {
            fields.push(format!("\"when_to_use\": {}", json_string(when)));
        }
        if let Some(group) = &preset.group {
            fields.push(format!("\"group\": {}", json_string(group)));
        }
        let selections: Vec<String> = preset.selections.iter().map(|s| json_string(s)).collect();
        fields.push(format!("\"selections\": [{}]", selections.join(", ")));
        let comma = if i + 1 == resolved.len() { "" } else { "," };
        println!(
            "  {}: {{ {} }}{comma}",
            json_string(&preset.name),
            fields.join(", ")
        );
    }
    println!("}}");
}

/// Quote a string as JSON, so a label containing `"` or `\` pastes back cleanly.
fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""))
}

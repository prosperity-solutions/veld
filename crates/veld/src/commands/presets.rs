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
                output::bold(&output::one_line(
                    preset.group.as_deref().unwrap_or("Other")
                )),
            );
            current_group = Some(preset.group.clone());
        }

        let key = output::cyan(&format!("[{}]", preset.key));
        // The config key is what `--preset` and scripts take, so it stays visible
        // even when a label is what the reader is scanning for. Parenthesised
        // rather than dimmed: this output is piped into coding agents' context
        // with colour stripped, where `Local dev dev-local` reads as one name.
        let name = if preset.label.is_some() {
            format!(
                " {}",
                output::dim(&format!("({})", output::one_line(&preset.name)))
            )
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
            output::bold(&output::one_line(preset.display_label())),
        );
        if let Some(when) = &preset.when_to_use {
            println!("      {}", output::one_line(when));
        }
        println!(
            "      {}",
            output::dim(&output::one_line(&preset.selections.join(", ")))
        );
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
            "Auto-assigned keys: {}. These move when a preset is added or removed ahead of \
             them — including from an include file that sorts earlier. Freeze them with \
             `veld presets --pin`, or pin one with \"key\": <n>.",
            auto.join(", ")
        ));
    }
}

/// `--pin`: print the current numbering as a paste-ready `presets` block.
///
/// The block itself is built by [`presets::pin_block`], which is where its
/// round-trip property is tested.
fn print_pin_block(resolved: &[ResolvedPreset]) {
    println!("// Paste into veld.json to freeze the current preset numbering.");
    println!("// Pinned keys never move, whatever is added, removed, or regrouped around them.");
    println!("{}", presets::pin_block(resolved));
}

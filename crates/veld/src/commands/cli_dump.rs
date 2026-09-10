//! `veld _cli-dump` — the clap command tree as JSON, for `tests/validate-doc-commands.py`.
//!
//! Hidden, and not meant to be typed. It exists because the first version of that
//! gate learned the CLI by parsing `--help` prose, and prose is not a data
//! structure. Three things went wrong, all measured on this tree:
//!
//! * **Descriptions leaked into the flag set.** clap wraps a long description
//!   onto its own indented line, so every hyphenated word in one became a flag:
//!   `veld logs` gained `--pin` (from the sentence describing `veld presets`),
//!   and `veld settings set` gained `-apple-system` out of a CSS font stack. A
//!   phantom flag does not fail the gate — it *widens* it, so a documented
//!   `veld logs --pin` would have passed.
//! * **A `hide = true` flag is real and absent from help**, which forced a
//!   hand-maintained allowlist for the ones the maintainer docs legitimately
//!   describe. Introspection reports them, so the allowlist is gone.
//! * **Help says nothing usable about *positionals*.** Without that, the gate
//!   could not tell `veld logs dev-daemon` (there is no positional; clap refuses
//!   it) from `veld start e2e` (there is one) — and `veld logs dev-daemon
//!   --follow` is a defect this repo actually shipped, in `CONTRIBUTING.md`.
//!
//! Everything above is a property clap already knows. Asking it is exact, is one
//! process instead of one per subcommand, and cannot drift with a help-layout
//! change.

use clap::CommandFactory;

/// Print the tree on stdout as JSON. Machine-readable output on stdout, per
/// AGENTS.md; there is no human form of this command.
pub fn run() -> i32 {
    let cmd = crate::Cli::command();
    let value = describe(&cmd, &[]);
    println!(
        "{}",
        serde_json::to_string_pretty(&value).unwrap_or_default()
    );
    0
}

/// `inherited` carries the `global = true` flags of every ancestor.
///
/// clap declares a global argument once, on the parent, and accepts it on every
/// descendant — `veld backup --dir D now` and `veld backup now --dir D` are the
/// same command, and `--debug` is legal everywhere. Reporting a global only
/// where it was written would make this dump describe a *narrower* CLI than the
/// real one, and a gate reading it would reject `veld backup now --dir D`, which
/// the README documents and clap accepts. Flattening it into every descendant is
/// what makes each node's `flags` the answer to "may this be written here".
fn describe(cmd: &clap::Command, inherited: &[String]) -> serde_json::Value {
    let mut flags: Vec<String> = inherited.to_vec();
    let mut globals: Vec<String> = inherited.to_vec();
    for arg in cmd.get_arguments() {
        let mut spellings = Vec::new();
        if let Some(long) = arg.get_long() {
            spellings.push(format!("--{long}"));
        }
        for long in arg.get_all_aliases().into_iter().flatten() {
            spellings.push(format!("--{long}"));
        }
        if let Some(short) = arg.get_short() {
            spellings.push(format!("-{short}"));
        }
        for short in arg.get_all_short_aliases().into_iter().flatten() {
            spellings.push(format!("-{short}"));
        }
        if arg.is_global_set() {
            globals.extend(spellings.iter().cloned());
        }
        flags.extend(spellings);
    }
    flags.sort();
    flags.dedup();
    globals.sort();
    globals.dedup();

    // Every name a subcommand answers to, not only its canonical one. An alias
    // absent here reads to the gate as an unknown word: at a node with no
    // positional that is a false failure on a correct doc line, and at one with
    // a positional it silently swallows the rest of the line. No alias exists in
    // `main.rs` today — this is so that adding the first one needs no second
    // change here.
    let mut subcommands = serde_json::Map::new();
    for s in cmd.get_subcommands() {
        // `help` is clap's own generated subcommand and is not something a doc
        // line should be validated against.
        if s.get_name() == "help" {
            continue;
        }
        let described = describe(s, &globals);
        for name in std::iter::once(s.get_name()).chain(s.get_all_aliases()) {
            subcommands.insert(name.to_owned(), described.clone());
        }
    }

    serde_json::json!({
        "flags": flags,
        // Whether a bare word is legal here at all. Not the arity — a doc line
        // showing two values where one is accepted is a different (and much
        // rarer) defect than one showing a value where none is.
        "takes_positional": cmd.get_positionals().next().is_some(),
        "subcommand_required": cmd.is_subcommand_required_set(),
        "subcommands": subcommands,
    })
}

#[cfg(test)]
mod tests {
    use super::describe;
    use clap::CommandFactory;

    /// The three properties the gate reads. Pinned here because a clap upgrade
    /// or an `#[arg]` edit could change any of them silently, and the only
    /// symptom downstream is a documentation gate that quietly stops failing.
    #[test]
    fn the_dump_reports_what_the_gate_needs() {
        let v = describe(&crate::Cli::command(), &[]);
        let subs = &v["subcommands"];

        // A positional exists on `start` (node selections) and not on `logs`.
        assert_eq!(subs["start"]["takes_positional"], true);
        assert_eq!(subs["logs"]["takes_positional"], false);

        // `veld feedback` alone is refused by clap; the gate has to know that.
        assert_eq!(subs["feedback"]["subcommand_required"], true);
        assert_eq!(subs["start"]["subcommand_required"], false);

        // Flags are per-node, and a hidden one is reported — which is the whole
        // reason the gate no longer carries an allowlist of them.
        let start_flags = subs["start"]["flags"].as_array().expect("array");
        assert!(start_flags.iter().any(|f| f == "--preset"));
        assert!(start_flags.iter().any(|f| f == "-a"));
        assert!(
            !start_flags.iter().any(|f| f == "--pin"),
            "no leakage across nodes"
        );
        let update_flags = subs["desktop"]["subcommands"]["update"]["flags"]
            .as_array()
            .expect("array");
        assert!(
            update_flags.iter().any(|f| f == "--wait-pid"),
            "a hidden flag must still be reported: {update_flags:?}"
        );

        // clap's generated `help` subcommand is not part of the documented surface.
        assert!(subs.get("help").is_none());

        // Every declared name reaches the map. `start_server` is not a CLI alias,
        // so this asserts the mechanism on the tree as it is: no subcommand today
        // declares one, and the count must equal the non-help subcommand count.
        let declared = crate::Cli::command()
            .get_subcommands()
            .filter(|s| s.get_name() != "help")
            .map(|s| 1 + s.get_all_aliases().count())
            .sum::<usize>();
        assert_eq!(
            subs.as_object().expect("object").len(),
            declared,
            "every subcommand name and alias must be a key"
        );

        // A global declared on a parent must appear on every descendant, or a
        // reader concludes `veld backup now --dir D` is illegal when clap accepts
        // it (and the README documents it).
        let backup_now = subs["backup"]["subcommands"]["now"]["flags"]
            .as_array()
            .expect("array");
        assert!(
            backup_now.iter().any(|f| f == "--dir"),
            "an ancestor's global must be inherited: {backup_now:?}"
        );
        assert!(
            subs["start"]["flags"]
                .as_array()
                .expect("array")
                .iter()
                .any(|f| f == "--debug"),
            "the root's global --debug reaches every subcommand"
        );
    }
}

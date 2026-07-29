//! Presets: the named starting points a human picks from a list and a coding
//! agent picks from `veld presets --json`.
//!
//! The problem this module exists to solve is that the number people actually
//! type — `veld start`, then `3` — used to be a *position* in an alphabetically
//! sorted list. Adding a preset renumbered every entry after it, so the number
//! somebody memorised, wrote in a runbook, or pasted into Slack silently came to
//! mean something else. Positions cannot be memorised in a config that grows.
//!
//! So the number is an identity, not a position:
//!
//! * A preset may pin its own `key`. A pinned key never moves, no matter what is
//!   added, removed, renamed, or regrouped around it.
//! * Unpinned presets are assigned keys after the highest pinned one, in
//!   declaration order — which makes appending a preset (the normal workflow)
//!   leave every existing key alone. Inserting one in the middle of the file
//!   still shifts the unpinned tail; that is the whole reason `key` exists, and
//!   why `veld presets` marks which keys are promises and which are not.
//! * Display order is derived from keys and never feeds back into them. Grouping
//!   moves headers around on screen; it cannot move a number.

use crate::config::VeldConfig;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A `presets` entry, in either the bare-array form or the object form.
///
/// The array form is not legacy and is not deprecated — for a two-preset project
/// `"dev": ["web:dev", "api:local"]` says everything there is to say, and every
/// config in the wild is written that way. The object form exists for the
/// configs that outgrew it: dozens of presets, picked by people who did not write
/// them and by agents that cannot guess what `web-prod-stg` starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresetDef {
    /// `"dev": ["web:dev", "api:local"]`
    Selections(Vec<String>),
    /// `"dev": { "label": …, "selections": [...] }`
    Detailed(Box<PresetSpec>),
}

/// The object form of a preset.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetSpec {
    /// `node:variant` selections and `@other-preset` references.
    pub selections: Vec<String>,

    /// The number shown in the picker. Pinned here, it never changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<u32>,

    /// Human-readable name, shown instead of the preset's config key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// When someone — or something — should pick this preset. Read by coding
    /// agents deciding what to start from a plain-English request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,

    /// Optional heading to chunk the list under. Purely visual.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

impl PresetDef {
    /// The raw selection list, which may still contain `@preset` references.
    #[must_use]
    pub fn selections(&self) -> &[String] {
        match self {
            Self::Selections(s) => s,
            Self::Detailed(spec) => &spec.selections,
        }
    }

    /// The pinned key, if the author pinned one.
    #[must_use]
    pub fn key(&self) -> Option<u32> {
        match self {
            Self::Selections(_) => None,
            Self::Detailed(spec) => spec.key,
        }
    }

    #[must_use]
    pub fn label(&self) -> Option<&str> {
        match self {
            Self::Selections(_) => None,
            Self::Detailed(spec) => spec.label.as_deref(),
        }
    }

    #[must_use]
    pub fn when_to_use(&self) -> Option<&str> {
        match self {
            Self::Selections(_) => None,
            Self::Detailed(spec) => spec.when_to_use.as_deref(),
        }
    }

    #[must_use]
    pub fn group(&self) -> Option<&str> {
        match self {
            Self::Selections(_) => None,
            Self::Detailed(spec) => spec.group.as_deref(),
        }
    }
}

impl Serialize for PresetDef {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // Round-trips to whichever form was written: an array-form preset must
        // not grow into an object just by passing through veld.
        match self {
            Self::Selections(items) => items.serialize(s),
            Self::Detailed(spec) => spec.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for PresetDef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        preset_def_from_value(serde_json::Value::deserialize(d)?).map_err(D::Error::custom)
    }
}

/// Parse a [`PresetDef`] from a JSON value.
///
/// Hand-written rather than `#[serde(untagged)]` because untagged collapses every
/// mistake inside the object form into "data did not match any variant of untagged
/// enum" — which, for a field whose whole point is being hand-authored, is the
/// least useful thing an error could say. Here a misspelled key names itself.
fn preset_def_from_value(value: serde_json::Value) -> Result<PresetDef, String> {
    match value {
        serde_json::Value::Array(_) => serde_json::from_value(value)
            .map(PresetDef::Selections)
            .map_err(|e| format!("preset selections must be a list of strings: {e}")),
        serde_json::Value::Object(ref map) => {
            if !map.contains_key("selections") {
                return Err(
                    "a preset object must have \"selections\" (a list of `node:variant` \
                     entries and `@preset` references)"
                        .to_owned(),
                );
            }
            let known = ["selections", "key", "label", "when_to_use", "group"];
            let unknown: Vec<&str> = map
                .keys()
                .map(String::as_str)
                .filter(|k| !known.contains(k))
                .collect();
            if !unknown.is_empty() {
                return Err(format!(
                    "unknown key(s) in preset: {}; expected {}",
                    unknown.join(", "),
                    known.join(", ")
                ));
            }
            serde_json::from_value(value)
                .map(|spec| PresetDef::Detailed(Box::new(spec)))
                .map_err(|e| format!("invalid preset object: {e}"))
        }
        _ => Err(
            "a preset must be a list of selections, or an object with \"selections\" \
             and optional \"key\", \"label\", \"when_to_use\", \"group\""
                .to_owned(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Key assignment and display order
// ---------------------------------------------------------------------------

/// A preset with its key assigned and its metadata flattened — what every
/// surface (picker, `veld presets`, `--json`, the management UI) renders.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResolvedPreset {
    /// The preset's config key, e.g. `dev-staging`. Always usable with
    /// `veld start --preset`.
    pub name: String,
    /// The number to type at the picker.
    pub key: u32,
    /// Whether `key` was pinned in the config. An unpinned key is a convenience,
    /// not a promise — it can change when presets are added ahead of it.
    pub pinned: bool,
    pub label: Option<String>,
    pub when_to_use: Option<String>,
    pub group: Option<String>,
    /// Raw selections, `@refs` not expanded.
    pub selections: Vec<String>,
    /// Whether this is the project's `default_preset`.
    pub is_default: bool,
}

impl ResolvedPreset {
    /// What to show a human: the label if there is one, else the config key.
    #[must_use]
    pub fn display_label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }
}

/// Assign every preset a key and return them in display order.
///
/// Keys: a pinned `key` is taken as-is; unpinned presets are numbered from
/// `max(pinned) + 1` upward in declaration order. Declaration order survives
/// because `presets` is an `IndexMap` all the way through
/// [`crate::include::merge`], and include globs load in sorted order — so this is
/// deterministic across machines, which is the point of a number two people can
/// say to each other.
///
/// Display order: groups sorted by their lowest key, presets within a group
/// sorted by key. Deriving group order from keys rather than declaring it
/// separately means there is exactly one number in the config controlling
/// everything, and the rendered list still reads top-to-bottom in ascending key
/// order.
///
/// This function is total. A config with duplicate pinned keys is a `veld lint`
/// error (`preset-duplicate-key`) and `veld start` refuses to run on it, but this
/// still returns every preset — a diagnostic that cannot list the thing it is
/// complaining about is not much of a diagnostic.
#[must_use]
pub fn resolve(config: &VeldConfig) -> Vec<ResolvedPreset> {
    let Some(presets) = config.presets.as_ref() else {
        return Vec::new();
    };

    let highest_pinned = presets.values().filter_map(PresetDef::key).max();
    let mut next_auto = highest_pinned.map_or(1, |k| k.saturating_add(1));

    let resolved: Vec<ResolvedPreset> = presets
        .iter()
        .map(|(name, def)| {
            let (key, pinned) = match def.key() {
                Some(k) => (k, true),
                None => {
                    let k = next_auto;
                    next_auto = next_auto.saturating_add(1);
                    (k, false)
                }
            };
            ResolvedPreset {
                name: name.clone(),
                key,
                pinned,
                label: def.label().map(ToOwned::to_owned),
                when_to_use: def.when_to_use().map(ToOwned::to_owned),
                group: def.group().map(ToOwned::to_owned),
                selections: def.selections().to_vec(),
                is_default: config.default_preset.as_deref() == Some(name.as_str()),
            }
        })
        .collect();

    // Group order is the group's lowest key. `name` is the final tiebreak so
    // that duplicate pinned keys — an error, but one we still have to render —
    // do not order differently run to run.
    let group_rank = |group: Option<&str>| -> u32 {
        resolved
            .iter()
            .filter(|p| p.group.as_deref() == group)
            .map(|p| p.key)
            .min()
            .unwrap_or(u32::MAX)
    };
    let ranks: Vec<u32> = resolved
        .iter()
        .map(|p| group_rank(p.group.as_deref()))
        .collect();
    let mut with_rank: Vec<(u32, ResolvedPreset)> = ranks.into_iter().zip(resolved).collect();
    with_rank
        .sort_by(|(ra, a), (rb, b)| ra.cmp(rb).then(a.key.cmp(&b.key)).then(a.name.cmp(&b.name)));
    with_rank.into_iter().map(|(_, p)| p).collect()
}

/// Presets in display order, chunked into their groups.
///
/// An empty `Option<String>` group is the ungrouped bucket. Callers render a
/// heading per chunk — but only when [`has_groups`] says the author asked for
/// any, so a config that never mentions `group` still prints one flat list.
#[must_use]
pub fn grouped(config: &VeldConfig) -> Vec<(Option<String>, Vec<ResolvedPreset>)> {
    let mut out: Vec<(Option<String>, Vec<ResolvedPreset>)> = Vec::new();
    for preset in resolve(config) {
        match out.last_mut() {
            Some((group, bucket)) if *group == preset.group => bucket.push(preset),
            _ => out.push((preset.group.clone(), vec![preset])),
        }
    }
    out
}

/// Whether any preset declares a `group`.
#[must_use]
pub fn has_groups(config: &VeldConfig) -> bool {
    config
        .presets
        .iter()
        .flatten()
        .any(|(_, def)| def.group().is_some())
}

/// Resolve what someone typed at the picker, or passed to `--preset`: either a
/// key (`3`) or a preset name (`dev-staging`).
///
/// Numbers are tried first, so a preset perversely *named* `3` does not shadow
/// the key `3` that the list just told the user to type. It stays reachable as
/// `veld start --preset 3` only if no key `3` exists — a collision `veld lint`
/// reports rather than silently resolving one way.
#[must_use]
pub fn find(config: &VeldConfig, token: &str) -> Option<ResolvedPreset> {
    let all = resolve(config);
    if let Ok(key) = token.trim().parse::<u32>()
        && let Some(hit) = all.iter().find(|p| p.key == key)
    {
        return Some(hit.clone());
    }
    all.into_iter().find(|p| p.name == token.trim())
}

/// The project's `default_preset`, if it is set and names a real preset.
#[must_use]
pub fn default_preset(config: &VeldConfig) -> Option<ResolvedPreset> {
    let name = config.default_preset.as_deref()?;
    resolve(config).into_iter().find(|p| p.name == name)
}

/// What a line typed at the interactive preset prompt means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    /// A preset was identified — by key, by name, or by pressing enter on a
    /// declared default.
    Chosen(ResolvedPreset),
    /// Nothing was typed and there is no default to fall back on.
    Cancelled,
    /// Something was typed and it matched no key and no name.
    NotFound,
}

/// Interpret a line typed at the preset prompt.
///
/// Split out from the prompt's IO so the rule is testable without a pty: an
/// empty line takes the default when one is declared and cancels otherwise, and
/// anything else goes through [`find`], which accepts a key or a name.
#[must_use]
pub fn interpret_pick(typed: &str, config: &VeldConfig) -> Pick {
    let typed = typed.trim();
    if typed.is_empty() {
        return match default_preset(config) {
            Some(d) => Pick::Chosen(d),
            None => Pick::Cancelled,
        };
    }
    match find(config, typed) {
        Some(hit) => Pick::Chosen(hit),
        None => Pick::NotFound,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a whole `presets` block the way a config file writes it, so these
    /// tests exercise the real deserializer rather than hand-built structs.
    fn config_with(presets_json: &str, default_preset: Option<&str>) -> VeldConfig {
        let default = default_preset
            .map(|d| format!(", \"default_preset\": \"{d}\""))
            .unwrap_or_default();
        let json = format!(
            "{{ \"schemaVersion\": \"3\", \"name\": \"t\", \"nodes\": {{}}, \
             \"presets\": {presets_json}{default} }}"
        );
        serde_json::from_str(&json).expect("test config should parse")
    }

    fn keys(config: &VeldConfig) -> Vec<(String, u32)> {
        resolve(config)
            .into_iter()
            .map(|p| (p.name, p.key))
            .collect()
    }

    /// Both forms are first-class. The array form is what every config in the
    /// wild is written in and stays valid forever; the object form is for the
    /// configs that outgrew it.
    #[test]
    fn array_and_object_forms_both_parse() {
        let config = config_with(
            r#"{
                "short": ["web:dev"],
                "long": {
                    "key": 4,
                    "label": "Site preview",
                    "when_to_use": "Reviewing visuals against staging content.",
                    "group": "For non-developers",
                    "selections": ["web:prod", "api:staging"]
                }
            }"#,
            None,
        );
        let presets = config.presets.as_ref().unwrap();
        assert_eq!(presets["short"].selections(), ["web:dev"]);
        assert_eq!(presets["short"].key(), None);
        assert_eq!(presets["long"].key(), Some(4));
        assert_eq!(presets["long"].label(), Some("Site preview"));
        assert_eq!(presets["long"].group(), Some("For non-developers"));
        assert_eq!(
            presets["long"].when_to_use(),
            Some("Reviewing visuals against staging content.")
        );
    }

    /// A typo inside the object form must name itself. This is the whole reason
    /// the deserializer is hand-written instead of `#[serde(untagged)]`, which
    /// collapses every such mistake into "data did not match any variant".
    #[test]
    fn a_misspelled_preset_key_is_named_in_the_error() {
        let err = serde_json::from_str::<PresetDef>(
            r#"{ "selections": ["web:dev"], "when_to_us": "typo" }"#,
        )
        .expect_err("an unknown key must not be silently dropped");
        let msg = err.to_string();
        assert!(msg.contains("when_to_us"), "must name the bad key: {msg}");
        assert!(
            msg.contains("when_to_use"),
            "must name the intended key: {msg}"
        );

        let missing = serde_json::from_str::<PresetDef>(r#"{ "label": "no selections" }"#)
            .expect_err("selections is required in the object form");
        assert!(
            missing.to_string().contains("selections"),
            "must say what is missing: {missing}"
        );
    }

    /// The bug this module exists for: a preset added to the config must not
    /// change the number of any preset already there.
    #[test]
    fn adding_a_preset_does_not_renumber_the_others() {
        let before = config_with(r#"{ "alpha": ["a:x"], "zulu": ["z:x"] }"#, None);
        assert_eq!(
            keys(&before),
            [("alpha".to_owned(), 1), ("zulu".to_owned(), 2)]
        );

        // Appended last, as new presets are. Under the old alphabetical-position
        // numbering, inserting "docker" would have made `zulu` 3 and everything
        // in between shift; here it takes the next free number and nothing moves.
        let after = config_with(
            r#"{ "alpha": ["a:x"], "zulu": ["z:x"], "docker": ["d:x"] }"#,
            None,
        );
        assert_eq!(
            keys(&after),
            [
                ("alpha".to_owned(), 1),
                ("zulu".to_owned(), 2),
                ("docker".to_owned(), 3)
            ],
            "an appended preset takes the next key and leaves the rest alone"
        );
    }

    /// A pinned key is a promise: it survives insertion *anywhere*, including
    /// ahead of it in the file, which is the one case auto-assignment cannot
    /// protect against.
    #[test]
    fn a_pinned_key_survives_insertion_before_it() {
        let before = config_with(
            r#"{ "dev": { "key": 1, "selections": ["a:x"] },
                 "docker": { "key": 2, "selections": ["d:x"] } }"#,
            None,
        );
        assert_eq!(
            keys(&before),
            [("dev".to_owned(), 1), ("docker".to_owned(), 2)]
        );

        let after = config_with(
            r#"{ "brand-new": { "key": 9, "selections": ["n:x"] },
                 "dev": { "key": 1, "selections": ["a:x"] },
                 "docker": { "key": 2, "selections": ["d:x"] } }"#,
            None,
        );
        let after_keys = keys(&after);
        assert!(after_keys.contains(&("dev".to_owned(), 1)));
        assert!(after_keys.contains(&("docker".to_owned(), 2)));
        assert!(after_keys.contains(&("brand-new".to_owned(), 9)));
    }

    /// Unpinned keys start above every pinned one, so auto-assignment can never
    /// collide with a number someone deliberately reserved.
    #[test]
    fn auto_keys_start_above_the_highest_pinned_key() {
        let config = config_with(
            r#"{ "floater": ["f:x"],
                 "pinned-high": { "key": 20, "selections": ["h:x"] },
                 "other-floater": ["o:x"] }"#,
            None,
        );
        let by_name: std::collections::HashMap<String, u32> = keys(&config).into_iter().collect();
        assert_eq!(by_name["pinned-high"], 20);
        assert_eq!(by_name["floater"], 21);
        assert_eq!(by_name["other-floater"], 22);
    }

    /// Display order is derived from keys, so the rendered list reads in
    /// ascending key order with group headings interleaved — and grouping can
    /// move a heading on screen without moving a single number.
    #[test]
    fn groups_are_ordered_by_their_lowest_key() {
        let config = config_with(
            r#"{
                "docker":   { "key": 5, "group": "Docker",   "selections": ["d:x"] },
                "dev":      { "key": 1, "group": "Everyday", "selections": ["a:x"] },
                "e2e":      { "key": 7, "selections": ["e:x"] },
                "dev-stg":  { "key": 2, "group": "Everyday", "selections": ["b:x"] }
            }"#,
            None,
        );
        assert_eq!(
            keys(&config),
            [
                ("dev".to_owned(), 1),
                ("dev-stg".to_owned(), 2),
                ("docker".to_owned(), 5),
                ("e2e".to_owned(), 7),
            ],
            "Everyday sorts first because it holds key 1, and the list stays \
             ascending across group boundaries"
        );

        let grouped = grouped(&config);
        assert_eq!(grouped.len(), 3);
        assert_eq!(grouped[0].0.as_deref(), Some("Everyday"));
        assert_eq!(grouped[0].1.len(), 2);
        assert_eq!(grouped[1].0.as_deref(), Some("Docker"));
        assert_eq!(grouped[2].0, None, "ungrouped presets are their own bucket");
        assert!(has_groups(&config));
    }

    /// A config that never mentions `group` renders as one flat list — nothing
    /// about the object form forces a heading on anyone.
    #[test]
    fn a_config_without_groups_has_one_bucket() {
        let config = config_with(r#"{ "a": ["a:x"], "b": ["b:x"] }"#, None);
        assert!(!has_groups(&config));
        assert_eq!(grouped(&config).len(), 1);
        assert_eq!(grouped(&config)[0].0, None);
    }

    /// `find` is what the picker and `--preset` share, so both accept a key and
    /// a name and cannot drift apart.
    #[test]
    fn find_accepts_a_key_or_a_name() {
        let config = config_with(r#"{ "dev": { "key": 3, "selections": ["a:x"] } }"#, None);
        assert_eq!(find(&config, "3").unwrap().name, "dev");
        assert_eq!(find(&config, "dev").unwrap().name, "dev");
        assert_eq!(find(&config, " dev ").unwrap().name, "dev");
        assert!(find(&config, "nope").is_none());
        assert!(find(&config, "4").is_none());
    }

    /// A key wins over a preset *named* the same digits. The picker just told the
    /// user to type that number, so it has to mean what the list said.
    #[test]
    fn a_key_beats_a_preset_named_like_a_number() {
        let config = config_with(
            r#"{ "dev": { "key": 3, "selections": ["a:x"] },
                 "3": { "key": 8, "selections": ["b:x"] } }"#,
            None,
        );
        assert_eq!(find(&config, "3").unwrap().name, "dev");
        assert_eq!(find(&config, "8").unwrap().name, "3");
    }

    #[test]
    fn default_preset_resolves_and_is_marked() {
        let config = config_with(r#"{ "a": ["a:x"], "b": ["b:x"] }"#, Some("b"));
        assert_eq!(default_preset(&config).unwrap().name, "b");
        let resolved = resolve(&config);
        assert!(!resolved[0].is_default);
        assert!(resolved[1].is_default);

        // A `default_preset` naming nothing is a lint error, not a panic here.
        let broken = config_with(r#"{ "a": ["a:x"] }"#, Some("ghost"));
        assert!(default_preset(&broken).is_none());
    }

    /// `resolve` has to stay total on a config `veld lint` rejects, or the
    /// diagnostic cannot list the presets it is complaining about.
    #[test]
    fn duplicate_pinned_keys_still_resolve_deterministically() {
        let config = config_with(
            r#"{ "one": { "key": 2, "selections": ["a:x"] },
                 "two": { "key": 2, "selections": ["b:x"] } }"#,
            None,
        );
        assert_eq!(
            keys(&config),
            [("one".to_owned(), 2), ("two".to_owned(), 2)],
            "name is the final tiebreak, so the order does not vary run to run"
        );
    }

    /// The prompt's rules, tested without a pty: enter takes the default, an
    /// empty line without one cancels rather than starting something arbitrary,
    /// and a key and a name are equally acceptable.
    #[test]
    fn interpret_pick_covers_enter_key_name_and_typo() {
        let config = config_with(
            r#"{ "dev": { "key": 3, "selections": ["a:x"] }, "prod": ["b:x"] }"#,
            Some("prod"),
        );
        assert!(matches!(
            interpret_pick("", &config),
            Pick::Chosen(p) if p.name == "prod"
        ));
        assert!(matches!(
            interpret_pick("   ", &config),
            Pick::Chosen(p) if p.name == "prod"
        ));
        assert!(matches!(
            interpret_pick("3", &config),
            Pick::Chosen(p) if p.name == "dev"
        ));
        assert!(matches!(
            interpret_pick("dev", &config),
            Pick::Chosen(p) if p.name == "dev"
        ));
        assert_eq!(interpret_pick("nope", &config), Pick::NotFound);
        // A number nobody holds is a typo, not a silent fallback to the default.
        assert_eq!(interpret_pick("99", &config), Pick::NotFound);

        let no_default = config_with(r#"{ "dev": ["a:x"] }"#, None);
        assert_eq!(interpret_pick("", &no_default), Pick::Cancelled);
    }

    /// The array form must not silently grow into an object by passing through
    /// veld — a config that round-trips wider than it was written is a config
    /// whose diff nobody can read.
    #[test]
    fn each_form_round_trips_as_itself() {
        let array = serde_json::from_str::<PresetDef>(r#"["web:dev"]"#).unwrap();
        assert_eq!(serde_json::to_string(&array).unwrap(), r#"["web:dev"]"#);

        let object =
            serde_json::from_str::<PresetDef>(r#"{"selections":["web:dev"],"key":2}"#).unwrap();
        assert_eq!(
            serde_json::to_string(&object).unwrap(),
            r#"{"selections":["web:dev"],"key":2}"#
        );
    }
}

/**
 * What a setting *is*, as the daemon describes it.
 *
 * The mirror of `crates/veld-core/src/db/settings_catalog.rs` — read that module's
 * header first; it owns the design, and this file is its wire shape plus the three
 * questions a renderer has to ask of an entry (what is it worth right now, is its
 * gate open, and what does an unlisted preset look like).
 *
 * **The point of it is that adding a setting is a Rust-only edit.** The dialog
 * renders from this document rather than from forty hand-written rows, so a new
 * `SettingKey` with a `spec` arm appears in `/ide` with no TypeScript at all. The
 * only thing that still needs a bundle change is a genuinely new *kind of control*.
 *
 * {@link Choices} is a discriminated union so the picker's `never` assertion asks
 * for a renderer — but be precise about when: this union is **hand-written**, so a
 * variant added in Rust does not stop TypeScript compiling. Nothing ties the two
 * declarations together. The `never` fires on the *second* edit, once the variant
 * is added here; between the two, the client simply does not know the variant and
 * the picker's runtime fallback renders a visible "this version cannot show this
 * setting" row naming the key. Degraded, never invisible — that is the guarantee,
 * and it is a weaker and more useful one than "the compiler catches it".
 *
 * Field names are the wire's, not TypeScript's taste. The whole document is
 * camelCase — `Choices` carries both `rename_all` (which renames *variants*, giving
 * `textList`) and `rename_all_fields` (which renames the fields inside a struct
 * variant, giving `emptyMeans`). The second one is easy to forget and fails
 * silently: an `Option` field that arrives under the wrong name is `undefined`,
 * and a missing placeholder renders as no placeholder rather than as an error.
 */

import { api, type SettingsDoc } from "../api";
import { useEffect, useState } from "react";

/** The JSON type of a setting's value — `ValueShape` in settings_catalog.rs. */
export type ValueShape = "bool" | "int" | "text" | "textList";

/** Anything a settings document can hold. */
export type SettingValue = string | number | boolean | string[];

/** One option, with the words to offer it in. `value` is what gets stored. */
export interface Choice {
  value: string;
  label: string;
}

/** Where a runtime-offered list comes from. The bundle owns each of these controls. */
export type RuntimeSource = "shells" | "fonts" | "directory";

/**
 * What a surface should **offer** — never what the validator accepts. See *Offered
 * is not accepted* in settings_catalog.rs: `presets` and `runtime` exist to state
 * that asymmetry rather than hide it.
 */
export type Choices =
  | { kind: "free" }
  | { kind: "static"; options: Choice[] }
  | {
      kind: "range";
      min: number;
      max: number;
      /** The stepper's increment; `null` means one. */
      step: number | null;
      /** What the number counts, for a suffix. `"%"` is what makes it a slider. */
      unit: string | null;
      /** What an empty box means, where the floor is an off switch. */
      emptyMeans: string | null;
    }
  | { kind: "presets"; offered: Choice[]; min: number; max: number; unit: string | null }
  | { kind: "runtime"; source: RuntimeSource };

/**
 * Another setting whose value decides whether this one applies.
 *
 * `equals: null` means "that key must be boolean `true`".
 */
export interface Requires {
  key: string;
  equals: string | null;
}

/** Everything needed to present one setting. */
export interface CatalogEntry {
  key: string;
  title: string;
  help: string;
  group: string;
  groupLabel: string;
  /**
   * The heading this sits under inside its group, or absent for a group that is a
   * flat list. Entries arrive already ordered with sections contiguous (a Rust test
   * enforces it), so a renderer emits a heading when this changes between
   * consecutive entries — and must never sort or regroup.
   */
  section?: string;
  type: ValueShape;
  default: SettingValue;
  choices: Choices;
  requires?: Requires;
}

/** One group, in presentation order. */
export interface CatalogGroup {
  id: string;
  label: string;
}

/** `GET /api/settings/catalog`. */
export interface SettingsCatalog {
  groups: CatalogGroup[];
  settings: CatalogEntry[];
}

/**
 * What a key is worth right now.
 *
 * The daemon serves *effective* values, so the document is complete and this only
 * falls back while it is still loading — which is exactly when every control is
 * disabled anyway. The fallback is the catalog's own default rather than a literal
 * here, so the bundle still owns no copy of a default that could drift.
 */
export function settingValue(
  doc: SettingsDoc | null,
  entry: CatalogEntry,
): SettingValue {
  const stored = doc?.[entry.key];
  return stored === undefined ? entry.default : stored;
}

export function asBool(v: SettingValue): boolean {
  return v === true;
}

export function asNumber(v: SettingValue): number {
  return typeof v === "number" && Number.isFinite(v) ? v : 0;
}

export function asString(v: SettingValue): string {
  return typeof v === "string" ? v : "";
}

export function asStringList(v: SettingValue): string[] {
  return Array.isArray(v) ? v.filter((s) => typeof s === "string") : [];
}

/**
 * Is this entry's gate open?
 *
 * Answered against the same fallback rule as {@link settingValue}, so a dependency
 * that has not loaded yet reads as its default rather than as "off" — a master
 * switch that defaults on must not grey out its own rows for one frame.
 */
export function requirementMet(
  doc: SettingsDoc | null,
  requires: Requires | undefined,
  byKey: Map<string, CatalogEntry>,
): boolean {
  if (!requires) return true;
  const dep = byKey.get(requires.key);
  // A dependency this build has never heard of cannot be evaluated, and refusing
  // to open the gate would make the dependent setting unreachable for ever. An
  // older bundle against a newer daemon shows the row and lets the daemon judge.
  if (!dep) return true;
  const value = settingValue(doc, dep);
  return requires.equals === null ? value === true : value === requires.equals;
}

/**
 * The offered presets, plus whatever is actually stored if it is not one of them.
 *
 * `Choices::Presets` accepts a whole range and offers six values from it, so a
 * stored 45 is legal and this list would not contain it — and a `NativeSelect`
 * whose `value` matches no `<option>` renders blank and warns. Showing the real
 * value is also the honest thing: the control must not imply the machine is set to
 * something it is not.
 */
export function presetOptions(
  offered: Choice[],
  value: number,
  unit: string | null,
): Choice[] {
  const stored = String(value);
  if (offered.some((o) => o.value === stored)) return offered;
  // The offered labels read "15 minutes" / "1 hour", so a spliced row labelled
  // "45 min" sits in a different register from every neighbour. The unit the
  // catalog carries is the abbreviation a NumberInput suffix wants, not the word
  // a menu wants, so expand the one case that has a menu.
  const label = unit === "min" ? `${value} minutes` : unit ? `${value} ${unit}` : stored;
  return [...offered, { value: stored, label }].sort(
    (a, b) => Number(a.value) - Number(b.value),
  );
}

/**
 * The catalog, fetched once per page load.
 *
 * Cached in a module local rather than in `localStorage` like `useSettings`'s
 * mirror: this document describes *this build's* settings, so a stale copy would
 * outlive the daemon that produced it, and nothing here is first-paint critical —
 * the dialog is mounted only when it is opened.
 */
let cached: SettingsCatalog | null = null;

export interface CatalogState {
  /** `null` until the first read resolves, or while it failed. */
  catalog: SettingsCatalog | null;
  /** Why there is no catalog. The dialog can render nothing without one. */
  error: string | null;
}

export function useCatalog(): CatalogState {
  const [catalog, setCatalog] = useState<SettingsCatalog | null>(cached);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (cached) return;
    let live = true;
    api
      .catalog()
      .then((doc) => {
        cached = doc;
        if (live) setCatalog(doc);
      })
      .catch((e) => {
        if (live) setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      live = false;
    };
  }, []);

  return { catalog, error };
}

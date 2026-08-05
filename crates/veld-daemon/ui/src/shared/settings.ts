/**
 * Typed reads over the settings document.
 *
 * The document arrives from the daemon as a flat map of dotted keys to JSON
 * scalars, with unknown keys preserved (so a preference written by a newer build
 * survives a downgrade). Typing therefore happens here, at the point of use,
 * rather than in the wire type.
 *
 * **This file does not hold the defaults.** `GET /api/settings` returns *effective*
 * values — the daemon's defaults with stored rows merged over them — so the normal
 * path always has every key. `FALLBACK` below is only reachable when the daemon is
 * *older* than this client and has never heard of a key, which is a real case but
 * not the common one. Keeping a full copy of the defaults here is what would drift.
 *
 * Pure and dependency-free on purpose: `vitest` runs with `environment: "node"`, so
 * logic that lives in a function like these is testable and logic that lives in a
 * component is not.
 */

import type { SettingsDoc } from "../api";

export type CursorStyle = "block" | "underline" | "bar";
export type MarkerStyle = "color" | "emoji";

/**
 * Last-resort values for a daemon that predates a key.
 *
 * Not the defaults — see the file header. Each entry exists because rendering
 * cannot proceed without *something*, and the honest choice is the behaviour the
 * release before the setting had.
 *
 * **One stated exception: a setting that decides whether a *new control exists*
 * takes the shipped default, not the previous release's behaviour.** By the rule
 * above `quickSwitch*` would be `false`, since the release before them had no
 * switches — but hiding a button this build's user has been told about, because the
 * daemon has not heard of the key, is the wrong answer: it makes an old daemon look
 * like a broken new UI.
 *
 * Note the reachable path this exception is chosen *for*, because it is not only the
 * old-daemon one: both callers read `quickSwitchPrefs(settings ?? {})`, so the
 * fallback also decides the **first paint** on any client with no `localStorage`
 * mirror. That is deliberate and it is the cheaper of two reflows — the switches
 * default on, so substituting the default matches what most clients are about to
 * receive and the bar does not move at all, where deferring until `settings !== null`
 * would add two buttons on every fresh client instead. The residual cost is real and
 * accepted: someone who turned both switches *off* sees them painted for one frame.
 * `useSettings`'s "prefer non-null for sized content" advice points the other way, so
 * do not quietly reverse this without re-deciding which population eats the reflow.
 *
 * Note what nothing checks: that these values match `defaults()` in
 * `veld-core/src/db/settings.rs`. `every_known_key_round_trips_and_has_a_default`
 * pins that a key *has* a Rust default, never that this copy agrees with it — so
 * this is the one Rust↔TS pair that can still drift, and a deliberate divergence
 * like the exception above has to stay written down rather than merely true.
 */
const FALLBACK = {
  fontSize: 12,
  fontFamily:
    '"JetBrains Mono Variable", "JetBrains Mono", ui-monospace, monospace',
  cursorStyle: "block" as CursorStyle,
  cursorBlink: true,
  scrollback: 10000,
  shiftEnterNewline: true,
  markerStyle: "color" as MarkerStyle,
  detachGraceMinutes: 30,
  quickSwitchResponsive: true,
  quickSwitchColorScheme: true,
  // Keep until emptied. Matches the Rust default, and the direction to err in if it
  // ever drifts: the value that cannot delete anybody's checkout.
  trashRetentionDays: 0,
  // Three days, matching the Rust default rather than the previous release's
  // behaviour — this is the `quickSwitch*` exception above, for the same reason: the
  // History tab says how many runs the horizon hid, so a client talking to an older
  // daemon that shows the shipped default is coherent, where one showing everything
  // would silently disagree with every other window.
  runHistoryDays: 3,
  // The `quickSwitch*` exception above, for the same reason: a build whose UI
  // announces "terminal links open here" must not silently behave like the release
  // before it because the daemon has not heard of the key.
  terminalOpenUrlsInApp: true,
} as const;

function strings(doc: SettingsDoc, key: string): string[] {
  const v = doc[key];
  // A non-array, or an array with anything but strings in it, means a value this
  // client did not write — degrade to "no exemptions", which is the direction that
  // shows a URL in a pane rather than silently sending it somewhere else.
  if (!Array.isArray(v)) return [];
  return v.filter((entry): entry is string => typeof entry === "string");
}

function num(doc: SettingsDoc, key: string, fallback: number): number {
  const v = doc[key];
  // `typeof v === "number"` alone would accept NaN, which reaches xterm as a
  // font size and renders nothing at all.
  return typeof v === "number" && Number.isFinite(v) ? v : fallback;
}

function bool(doc: SettingsDoc, key: string, fallback: boolean): boolean {
  const v = doc[key];
  return typeof v === "boolean" ? v : fallback;
}

function oneOf<T extends string>(
  doc: SettingsDoc,
  key: string,
  allowed: readonly T[],
  fallback: T,
): T {
  const v = doc[key];
  return typeof v === "string" && (allowed as readonly string[]).includes(v)
    ? (v as T)
    : fallback;
}

function str(doc: SettingsDoc, key: string, fallback: string): string {
  const v = doc[key];
  // An empty family would render as the browser default and read as a bug; the
  // daemon rejects it on write, and this covers a value that got in another way.
  return typeof v === "string" && v.trim() !== "" ? v : fallback;
}

/** Every terminal option the settings store owns, resolved in one place. */
export interface TerminalPrefs {
  fontSize: number;
  fontFamily: string;
  cursorStyle: CursorStyle;
  cursorBlink: boolean;
  scrollback: number;
  shiftEnterNewline: boolean;
}

export function terminalPrefs(doc: SettingsDoc): TerminalPrefs {
  return {
    fontSize: num(doc, "terminal.fontSize", FALLBACK.fontSize),
    fontFamily: str(doc, "terminal.fontFamily", FALLBACK.fontFamily),
    cursorStyle: oneOf(
      doc,
      "terminal.cursorStyle",
      ["block", "underline", "bar"] as const,
      FALLBACK.cursorStyle,
    ),
    cursorBlink: bool(doc, "terminal.cursorBlink", FALLBACK.cursorBlink),
    scrollback: num(doc, "terminal.scrollback", FALLBACK.scrollback),
    shiftEnterNewline: bool(
      doc,
      "terminal.shiftEnterNewline",
      FALLBACK.shiftEnterNewline,
    ),
  };
}

/**
 * Which face of a worktree's marker to render.
 *
 * Only ever consulted by DOM renderers. The two OS-level text contexts — the
 * native tray menu label and the window title — always use the glyph, because a
 * colour has no textual form. That rule lives where it is applied, in
 * `desktop/src/main.js`'s tray label.
 */
export function markerStyle(doc: SettingsDoc): MarkerStyle {
  return oneOf(
    doc,
    "worktree.markerStyle",
    ["color", "emoji"] as const,
    FALLBACK.markerStyle,
  );
}

/**
 * Whether a stored marker colour is usable.
 *
 * `""` is the daemon's "not assigned yet" sentinel for a row that predates the
 * column, cleared on the next sync — so a renderer must fall back to the glyph
 * rather than emitting an empty colour.
 *
 * Shape-checked rather than trusted: the value goes into a CSS colour position, and
 * `#` plus six lowercase hex digits is the only form the daemon stores.
 */
export function hasMarkerColor(color: string): boolean {
  return /^#[0-9a-f]{6}$/.test(color);
}

/**
 * What to show as a worktree's marker in the DOM.
 *
 * Returns the glyph when the style says emoji, when no colour has been assigned yet,
 * or when the glyph is the only face that exists — so a renderer never has to
 * special-case the upgrade window.
 */
export function markerFace(
  doc: SettingsDoc,
  wt: { emoji: string; marker_color: string },
):
  | { kind: "color"; color: string }
  | { kind: "emoji"; emoji: string }
  | null {
  if (markerStyle(doc) === "color" && hasMarkerColor(wt.marker_color)) {
    return { kind: "color", color: wt.marker_color };
  }
  if (wt.emoji) return { kind: "emoji", emoji: wt.emoji };
  // A colour exists but the style asked for a glyph that was never assigned: show
  // the colour rather than nothing.
  if (hasMarkerColor(wt.marker_color)) {
    return { kind: "color", color: wt.marker_color };
  }
  return null;
}

/**
 * Which one-click toggles a browser pane puts in its chrome.
 *
 * A preference rather than a fixed pair because the chrome already carries most of a
 * browser's toolbar before these — so whether two more buttons belong there is the
 * user's call. Global and standing, **not** an answer to one narrow pane: see the
 * note beside the Rust defaults for why a measured bar width would be that, and why
 * this is not it. Both default on.
 */
export interface QuickSwitchPrefs {
  responsive: boolean;
  colorScheme: boolean;
}

export function quickSwitchPrefs(doc: SettingsDoc): QuickSwitchPrefs {
  return {
    responsive: bool(
      doc,
      "browser.quickSwitch.responsive",
      FALLBACK.quickSwitchResponsive,
    ),
    colorScheme: bool(
      doc,
      "browser.quickSwitch.colorScheme",
      FALLBACK.quickSwitchColorScheme,
    ),
  };
}

/**
 * How long a detached shell is kept, in minutes.
 *
 * Has a reader of its own rather than being read raw at the call site, so the
 * settings surface cannot hardcode a second copy of the default — which it did, and
 * which is exactly the drift this store exists to remove. The daemon is the
 * authority and clamps this on both write and read; the fallback here is only for a
 * daemon too old to know the key.
 */
export function detachGraceMinutes(doc: SettingsDoc): number {
  return num(doc, "terminal.detachGraceMinutes", FALLBACK.detachGraceMinutes);
}

/**
 * Days a worktree stays in the trash before it is deleted, or `0` for "keep until
 * I empty it" — which is the default.
 *
 * Zero is not "clamp to the minimum": it is the off switch, and the daemon treats it
 * that way, because clamping it up would arm automatic deletion for a user trying to
 * turn it off.
 */
export function trashRetentionDays(doc: SettingsDoc): number {
  return num(doc, "worktree.trashRetentionDays", FALLBACK.trashRetentionDays);
}

/**
 * Days of ended runs the history views show. **Defaults to 3**; `0` means "show all".
 *
 * A view filter and nothing else: no run is deleted by it, and hiding one never costs
 * anything but a scroll. That is why this one can default to a number that hides
 * something, where [`trashRetentionDays`] above must default to the value that cannot
 * delete anybody's checkout — the two docblocks look alike and their defaults are
 * opposite for that reason.
 */
export function runHistoryDays(doc: SettingsDoc): number {
  return num(doc, "runs.historyDays", FALLBACK.runHistoryDays);
}

/**
 * Whether a URL a terminal produces opens in a Veld browser pane. **Defaults on.**
 *
 * Read here for display only — the *decision* is the daemon's
 * (`veld_core::ide::route_url`), because the other half of it lives in the
 * project's `veld.json` and the renderer never sees that. A copy of the policy on
 * this side would be a second implementation that drifts.
 */
export function terminalOpenUrlsInApp(doc: SettingsDoc): boolean {
  return bool(doc, "terminal.openUrlsInApp", FALLBACK.terminalOpenUrlsInApp);
}

/**
 * Origins that open in the system browser instead of a pane — the global half of
 * the exempt list, unioned with the project's `ide.externalOrigins`.
 *
 * Origins, not URLs: `https://accounts.google.com`, `https://*.okta.com`,
 * `http://localhost:*`. The daemon validates each one with the same parser a
 * project config goes through and refuses the write if any entry fails, so this
 * list is only ever the accepted spelling.
 */
export function externalOrigins(doc: SettingsDoc): string[] {
  return strings(doc, "browser.externalOrigins");
}

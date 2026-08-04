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
 */
const FALLBACK = {
  fontSize: 12,
  fontFamily:
    '"JetBrains Mono Variable", "JetBrains Mono", ui-monospace, monospace',
  cursorStyle: "block" as CursorStyle,
  cursorBlink: true,
  scrollback: 5000,
  shiftEnterNewline: true,
  copyOnSelect: false,
  middleClickPaste: false,
  markerStyle: "color" as MarkerStyle,
  quickSwitchResponsive: true,
  quickSwitchColorScheme: true,
} as const;

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
  copyOnSelect: boolean;
  middleClickPaste: boolean;
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
    copyOnSelect: bool(doc, "terminal.copyOnSelect", FALLBACK.copyOnSelect),
    middleClickPaste: bool(
      doc,
      "terminal.middleClickPaste",
      FALLBACK.middleClickPaste,
    ),
  };
}

/**
 * Which face of a worktree's marker to render.
 *
 * Only ever consulted by DOM renderers. The two OS-level text contexts — the
 * native tray menu label and the window title — always use the glyph, because a
 * colour has no textual form; see `markerGlyph`.
 */
export function markerStyle(doc: SettingsDoc): MarkerStyle {
  return oneOf(
    doc,
    "worktree.markerStyle",
    ["color", "emoji"] as const,
    FALLBACK.markerStyle,
  );
}

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
 * Whether a stored hue index is usable. `-1` is the daemon's "not assigned yet"
 * sentinel for a row that predates the column, cleared on the next sync — so a
 * renderer must fall back to the glyph rather than reach for hue `-1`.
 */
export function hasMarkerHue(hue: number): boolean {
  return Number.isInteger(hue) && hue >= 0;
}

/**
 * The CSS custom property carrying a hue's ink.
 *
 * The index is stored, the colour is not: the property is defined once per theme
 * in `styles.css`, so a light-theme swatch is not the same ink as a dark-theme one
 * and neither the database nor this module has to know either value.
 */
export function markerHueVar(hue: number): string {
  return `var(--wt-hue-${hue})`;
}

/**
 * What to show as a worktree's marker in the DOM.
 *
 * Returns the glyph when the style says emoji, when no hue has been assigned yet,
 * or when the glyph is the only face that exists — so a renderer never has to
 * special-case the upgrade window.
 */
export function markerFace(
  doc: SettingsDoc,
  wt: { emoji: string; marker_hue: number },
): { kind: "color"; hue: number } | { kind: "emoji"; emoji: string } | null {
  if (markerStyle(doc) === "color" && hasMarkerHue(wt.marker_hue)) {
    return { kind: "color", hue: wt.marker_hue };
  }
  if (wt.emoji) return { kind: "emoji", emoji: wt.emoji };
  // A hue exists but the style asked for a glyph that was never assigned: show
  // the colour rather than nothing.
  if (hasMarkerHue(wt.marker_hue)) return { kind: "color", hue: wt.marker_hue };
  return null;
}

/**
 * The marker for a plain-string context — an OS window title, a native menu row.
 *
 * Always the glyph, never the colour, and independent of `worktree.markerStyle`:
 * those strings are handed to the OS, where a CSS custom property means nothing.
 * Empty when the worktree has no glyph, so callers can omit the separator.
 */
export function markerGlyph(wt: { emoji: string }): string {
  return wt.emoji ?? "";
}

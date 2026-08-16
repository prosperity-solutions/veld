/**
 * The keyboard shortcuts registry: the single place every shortcut's display
 * metadata lives — title, one-line explanation, and the keys to show — read
 * by the Shortcuts overview dialog (⋯ menu → "Shortcuts…").
 *
 * **Metadata only. Dispatch stays where it already lived**, in `App.tsx`'s one
 * central keydown effect. Every existing chord there carries hard-won,
 * platform-specific nuance — `e.code` vs `e.key` for AZERTY digits, an
 * `isEditableTarget` guard for ⌘B's emacs binding — and a generic matcher
 * driven off this file would either lose that nuance or have to reinvent it
 * per entry anyway, for no reader benefit: nobody reads a dispatch table to
 * learn a shortcut exists: an unfindable in-app entry is much of why an
 * overview dialog earns its keep in the first place, and that entry lives
 * here.
 *
 * **This is the single source of truth for the overview, not for behaviour.**
 * Add, change or remove a mod+shift (or Ctrl-literal) chord and there are
 * **three** places to touch, not two: this file's row, `App.tsx`'s keydown
 * effect, and — easy to miss, and the one an earlier round of review on this
 * very diff caught missing — `panes/terminalKeys.ts`'s `isAppShortcutChord`,
 * which is what lets the chord reach the window listener at all while a
 * terminal pane has focus (xterm otherwise consumes it first). Skip that
 * third one and the shortcut silently does nothing in the pane most users
 * spend the most time in. `registry.test.ts` only checks this file's own
 * shape (unique kebab-case ids, every combo non-empty), not that the other
 * two still agree with it, so keeping all three in sync is a habit the docs
 * ask for (see `AGENTS.md`'s documentation checklist and
 * `.claude/skills/ship/SKILL.md`), not a guarantee the type system gives you.
 *
 * **Not every row dispatches from `App.tsx`'s keydown effect.** A handful of
 * shortcuts are real, shipped, and worth listing here for discoverability even
 * though a different mechanism owns them: `find-in-page` and `new-window` are
 * Electron menu accelerators (`desktop/src/main.js`, `browserViews.js`), and
 * `insert-newline` is `terminalKeys.ts`'s Shift+Enter substitution. The
 * dialog's claim to be a complete list depends on those staying here too.
 */

export type ShortcutCategory = "navigation" | "layout" | "run" | "general";

/**
 * One chord, platform-neutral.
 *
 * `mod` is the cross-platform accelerator every chord in the app uses (⌘ on
 * macOS, Ctrl elsewhere — `e.metaKey || e.ctrlKey` in the keydown effect).
 * `ctrl` is different: the *literal* Ctrl key on every platform, for a chord
 * where macOS itself already claims Cmd for something else — no row uses it
 * today, but the field stays for the chord that eventually needs it.
 */
export interface KeyCombo {
  mod?: boolean;
  ctrl?: boolean;
  shift?: boolean;
  alt?: boolean;
  /** The key token(s) after the modifiers, rendered as their own `Kbd`s. */
  keys: string[];
}

export interface ShortcutDef {
  /** Kebab-case, stable — not persisted anywhere today, but named the same
   *  way as a promotion id on the chance it ever needs to be. */
  id: string;
  category: ShortcutCategory;
  title: string;
  /**
   * One sentence, and optional: only where the title alone does not cover a
   * real behavioural detail — that a chord wraps or has an alias, which run a
   * selector cycles to, a desktop-only scope. A row whose title already says
   * everything (`"Toggle project column"`, `"Open Settings"`) does not get
   * one just to fill the column; that reads as narration, not information.
   */
  description?: string;
  /** Usually one combo; two for a pair shown on the same row (next/previous,
   *  up/down) rather than as two separate rows. */
  combos: KeyCombo[];
  /**
   * Unreachable in a plain browser tab. Some chords are claimed outright by
   * every mainstream browser's own chrome — ⌘1…⌘9 is the clearest case (see
   * the comment at `App.tsx`'s digit-shortcut handler) — so the dialog says
   * so rather than listing a row that silently does nothing for someone using
   * Veld in a tab.
   */
  desktopOnly?: boolean;
}

const CATEGORY_LABELS: Record<ShortcutCategory, string> = {
  navigation: "Navigation",
  layout: "Layout",
  run: "Run",
  general: "General",
};

/** Display order for the overview dialog's grouping. */
export const CATEGORY_ORDER: ShortcutCategory[] = ["navigation", "layout", "run", "general"];

export function categoryLabel(category: ShortcutCategory): string {
  return CATEGORY_LABELS[category];
}

export const SHORTCUTS: ShortcutDef[] = [
  // ---- navigation ----------------------------------------------------------
  {
    id: "navigate-worktrees",
    category: "navigation",
    title: "Navigate worktrees",
    description:
      "Move the rail's selection to the worktree above or below, wrapping at either end. ←/→ are aliases for ↑/↓.",
    combos: [
      { mod: true, keys: ["↑"] },
      { mod: true, keys: ["↓"] },
      { mod: true, keys: ["←"] },
      { mod: true, keys: ["→"] },
    ],
  },
  {
    id: "switch-project",
    category: "navigation",
    title: "Switch to project 1–9",
    combos: [{ mod: true, keys: ["1…9"] }],
    // Chrome and Safari reserve ⌘1…⌘9 for their own tab strip — see the
    // matching comment in App.tsx's digit-shortcut handler.
    desktopOnly: true,
  },
  {
    id: "previous-project",
    category: "navigation",
    title: "Back to previous project",
    combos: [{ mod: true, keys: ["`"] }],
  },
  // ---- layout ---------------------------------------------------------------
  {
    id: "toggle-project-column",
    category: "layout",
    title: "Toggle project column",
    combos: [{ mod: true, keys: ["B"] }],
  },
  {
    id: "switch-view",
    category: "layout",
    title: "Switch IDE / Runs view",
    // Not ⌘⇧V — the veld feedback overlay claims that chord for its own
    // toolbar toggle.
    combos: [{ mod: true, shift: true, keys: ["X"] }],
  },
  // ---- run --------------------------------------------------------------
  {
    id: "start-stop",
    category: "run",
    title: "Start / stop the run",
    description: "Start the selected run, or stop it while it's live.",
    combos: [{ mod: true, shift: true, keys: ["Enter"] }],
  },
  {
    id: "restart-run",
    category: "run",
    title: "Restart run",
    combos: [{ mod: true, shift: true, keys: ["K"] }],
    // Firefox reserves Ctrl+Shift+K for its Web Console.
    desktopOnly: true,
  },
  {
    id: "select-preset",
    category: "run",
    title: "Cycle run",
    description: "Step the run selector to the next live run for this worktree.",
    combos: [{ mod: true, shift: true, keys: ["O"] }],
    // Reserved by at least one mainstream browser (Chrome's bookmark manager).
    desktopOnly: true,
  },
  {
    id: "update-main",
    category: "run",
    title: "Update main",
    // Not checked against every Linux input method (IBus's Unicode
    // code-point entry uses Ctrl+Shift+U on some distributions and would
    // claim this chord ahead of the browser or the app either).
    combos: [{ mod: true, shift: true, keys: ["U"] }],
  },
  // ---- general ------------------------------------------------------------
  {
    id: "command-palette",
    category: "general",
    title: "Command palette",
    description: "Search everything and jump anywhere.",
    // Used to also have a Ctrl/⌘+Shift+P second accelerator, for reaching it
    // from a focused terminal on Linux/Windows where Ctrl+K belongs to
    // readline — removed because it collided with the veld feedback
    // overlay's own mod+Shift+P binding, and ⌘K already covers every other
    // case (macOS terminals included, since xterm never claims meta combos).
    combos: [{ mod: true, keys: ["K"] }],
  },
  {
    id: "open-shortcuts",
    category: "general",
    title: "Open this overview",
    combos: [{ mod: true, keys: ["/"] }],
  },
  {
    id: "focus-mode",
    category: "general",
    title: "Toggle focus mode",
    description: "Turn focus mode's notification silencing on or off.",
    // Not ⌘⇧F — the veld feedback overlay claims that chord for its own
    // "select an element" mode.
    combos: [{ mod: true, shift: true, keys: ["L"] }],
  },
  {
    id: "settings",
    category: "general",
    title: "Open Settings",
    combos: [{ mod: true, keys: [","] }],
  },
  {
    id: "close-dialog",
    category: "general",
    title: "Close dialog",
    combos: [{ keys: ["Esc"] }],
  },
  {
    id: "find-in-page",
    category: "general",
    title: "Find in page",
    description: "Search text in the focused embedded browser pane.",
    combos: [{ mod: true, keys: ["F"] }],
    // The pane it searches only exists in the desktop app.
    desktopOnly: true,
  },
  {
    id: "new-window",
    category: "general",
    title: "New window",
    combos: [{ mod: true, keys: ["N"] }],
    // A browser tab has no equivalent — ⌘N there opens a new *browser*
    // window, unrelated to Veld.
    desktopOnly: true,
  },
  {
    id: "insert-newline",
    category: "general",
    title: "Insert a newline in a terminal",
    description: "Send a newline to the focused terminal without submitting the line.",
    combos: [{ shift: true, keys: ["Enter"] }],
  },
];

/**
 * Whether this looks like a Mac, for choosing ⌘/⌥/⇧ over Ctrl/Alt/Shift.
 *
 * `userAgentData.platform` first — Chromium's replacement for `navigator.platform`,
 * which is deprecated but still present everywhere this needs to run — then the
 * user agent string as the fallback every other browser needs.
 */
export function isMac(): boolean {
  if (typeof navigator === "undefined") return false;
  const uaData = (navigator as { userAgentData?: { platform?: string } }).userAgentData;
  const platform = uaData?.platform || navigator.platform || "";
  return /mac/i.test(platform) || /mac/i.test(navigator.userAgent);
}

/** A combo's tokens in display order, platform-aware. */
export function comboTokens(combo: KeyCombo, mac: boolean): string[] {
  const tokens: string[] = [];
  if (combo.mod) tokens.push(mac ? "⌘" : "Ctrl");
  if (combo.ctrl) tokens.push("Ctrl");
  if (combo.alt) tokens.push(mac ? "⌥" : "Alt");
  if (combo.shift) tokens.push(mac ? "⇧" : "Shift");
  tokens.push(...combo.keys);
  return tokens;
}

/** Everything wrong with a shortcut definition, as human-readable lines.
 *  Empty means valid — mirrors `sectionProblems` in `promotions/model.ts`. */
export function shortcutProblems(s: ShortcutDef): string[] {
  const problems: string[] = [];
  if (!/^[a-z0-9]+(-[a-z0-9]+)*$/.test(s.id)) problems.push(`${s.id || "(empty)"}: id must be kebab-case`);
  if (!s.title) problems.push(`${s.id}: title is required`);
  // Description is optional (see the field's own doc comment) — but an empty
  // string is a mistake, not a deliberate omission, so it is still flagged.
  if (s.description !== undefined && !s.description) {
    problems.push(`${s.id}: description must not be an empty string — omit it instead`);
  }
  if (s.combos.length === 0) problems.push(`${s.id}: needs at least one combo`);
  for (const combo of s.combos) {
    if (combo.keys.length === 0) problems.push(`${s.id}: a combo needs at least one key`);
  }
  return problems;
}

/**
 * The next index in a wrapped `0..length-1` cycle, `delta` steps from `current`.
 *
 * `current === -1` means "nothing focused yet" — the first press after a fresh
 * window, before any tab/worktree/run has been touched — and is handled
 * explicitly rather than folded into the modulo: `((current + delta) % length
 * + length) % length` puts a *backward* first step at `length - 2`, not
 * `length - 1`, because `-1 - 1 = -2` wraps to the second-to-last slot instead
 * of the last one. Forward from nothing is index `0` either way, which is why
 * the bug is one-sided and easy to miss.
 */
export function nextIndex(current: number, delta: number, length: number): number {
  if (length <= 0) return -1;
  if (current === -1) return delta > 0 ? 0 : length - 1;
  return (((current + delta) % length) + length) % length;
}

/** Duplicate ids across the registry — mirrors `duplicateIds` in `promotions/model.ts`. */
export function duplicateShortcutIds(shortcuts: readonly ShortcutDef[]): string[] {
  const seen = new Set<string>();
  const dupes = new Set<string>();
  for (const s of shortcuts) {
    if (seen.has(s.id)) dupes.add(s.id);
    seen.add(s.id);
  }
  return [...dupes];
}

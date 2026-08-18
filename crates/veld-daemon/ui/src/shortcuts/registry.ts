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
 * spend the most time in. **`registry.test.ts` now gates that third one** —
 * every page-dispatched mod+shift row here must be accepted by
 * `isAppShortcutChord`, and the test names the file to edit when it fails. It
 * still cannot see `App.tsx`'s dispatch, so a row with no handler at all
 * remains a habit the docs ask for (see `AGENTS.md`'s documentation checklist
 * and `.claude/skills/ship/SKILL.md`) rather than something the type system
 * gives you.
 *
 * **Not every row dispatches from `App.tsx`'s keydown effect.** A handful of
 * shortcuts are real, shipped, and worth listing here for discoverability even
 * though a different mechanism owns them: `find-in-page`, `new-window`,
 * `new-tab`, `close-tab` and `close-window` are Electron menu accelerators
 * (`desktop/src/main.js`, `browserViews.js`), and `insert-newline` is
 * `terminalKeys.ts`'s Shift+Enter substitution. The dialog's claim to be a
 * complete list depends on those staying here too — `close-window` is the
 * standing proof, having existed as the `close` role's default accelerator
 * through several releases with no row to be discovered in.
 *
 * **A menu accelerator cannot be made conditional, and that is what picks every
 * chord below.** It is resolved before any web contents sees the key, which
 * makes it the only mechanism that survives a focused browser pane *and* a
 * focused terminal — but it fires in text fields too, so it may only carry a
 * chord that means nothing in one.
 *
 * **The entire arrow space fails that test.** On macOS `⌘`+arrow is
 * caret-to-line-bounds in every Cocoa text field *and* the `^A`/`^E` a terminal
 * maps for Claude Code and Codex — veld claimed it for a release and that was a
 * reported bug, not a trade. `⌘⇧`+arrow extends a selection, `⌥`+arrow is word
 * motion in fields and shells alike, `⌃`+arrow is Mission Control, `⌘⌥`+arrow is
 * Rectangle's and Magnet's window tiling, `⌘⌃`+arrow risks macOS's own tiling.
 * There is no arrow chord left, which is why the search kept failing.
 *
 * So the **navigation family is Tab-shaped**: `⌃Tab`/`⌃⇧Tab` steps tabs, and
 * `⌥Tab`/`⌥⇧Tab` steps worktrees on macOS — the same shape one level up. `Tab`
 * is more
 * than the last survivor: `⇧` gives the reverse direction for free, so the pair
 * is *felt* rather than memorised, which is the one property no letter chord
 * has.
 *
 * **They are page-dispatched, not menu accelerators, and that was measured
 * rather than chosen.** A `Control+Tab` accelerator does not work: Chromium's
 * focus manager handles `Tab` before the menu layer, so the accelerator never
 * consumes the key and the page tabs through its focusable elements anyway. So
 * these take the ordinary three-place route — `App.tsx`'s keydown effect (with
 * a load-bearing `preventDefault` to stop that traversal), `isAppShortcutChord`
 * for terminals, and `browserViews.js` forwarding for a focused pane. They are
 * `desktopOnly` because every browser claims `⌃Tab` for its own tab strip.
 *
 * The forwarding is affordable here in a way `⌘⇧`+arrow's never was: `⌃Tab` is
 * browser-reserved, so a guest page never receives it anyway, and `⌥Tab` merely
 * duplicates plain `Tab`'s focus traversal, which the page keeps. A forwarded
 * chord has to leave the page something, and these do.
 *
 * **The worktree pair is the app's one per-platform binding.** `⌥Tab` is free on
 * macOS (the OS switcher there is `⌘Tab`) and *is* the window switcher
 * everywhere else — and every other Tab variant is claimed off macOS too
 * (`Ctrl+Alt+Tab` is the persistent task switcher, `Super+Tab` is GNOME's). So
 * Linux and Windows get `Ctrl+Shift+B`/`Ctrl+Shift+N`: adjacent keys, left is
 * previous, right is next. That is safe there because readline sees
 * `Ctrl`+letter and not `Ctrl+Shift`+letter, and the usual owner of
 * `Ctrl+Shift`+letter is the terminal emulator's own copy/paste — but veld is
 * its own emulator.
 *
 * Still **one action, one shortcut**: `combosFor` shows a reader only their own
 * platform's, and `registry.test.ts` enforces the cap per platform rather than
 * per row. The dispatch matches the non-mac pair on *literal* Ctrl rather than
 * on `mod`, so a Mac is never offered both — `⌘⇧B`/`⌘⇧N` do nothing there. A Mac
 * keyboard can still physically produce literal `⌃⇧B`, which works; that is an
 * incidental spelling nothing advertises, not a second binding.
 *
 * **The rejected candidates, because the search otherwise repeats.**
 * `⌘⇧`+arrow shipped first and was additionally forwarded out of browser panes;
 * that forwarding took select-to-line-start from every guest page
 * unconditionally — a `WebContentsView` has no `isEditableTarget` equivalent —
 * and was reverted. `⌘`+arrow reads beautifully (the arrow points at the thing)
 * and is exactly what the terminal bug was. A user-rebindable system was
 * designed and dropped: a partial one confuses, and a full one needs a
 * table-driven matcher replacing a hand-tuned dispatcher whose `e.code`-vs-
 * `e.key` policy is contradictory by design.
 *
 * `⌘F` remains the sole chord forwarded out of a pane (`browserViews.js`), and
 * it earns that by *substituting* the pane's own find bar; a chord with no
 * substitute does not qualify.
 */

export type ShortcutCategory = "navigation" | "layout" | "run" | "general";

/**
 * One chord, platform-neutral.
 *
 * `mod` is the cross-platform accelerator every chord in the app uses (⌘ on
 * macOS, Ctrl elsewhere — `e.metaKey || e.ctrlKey` in the keydown effect).
 * `ctrl` is the **literal** Ctrl key on every platform, not the cross-platform
 * accelerator. The navigation family uses it (`⌃Tab`) precisely because Ctrl
 * is the same physical key everywhere and means nothing in a text field; `alt`
 * is the same idea one level up (`⌥Tab`, macOS only — see the header).
 */
export interface KeyCombo {
  mod?: boolean;
  ctrl?: boolean;
  shift?: boolean;
  alt?: boolean;
  /** The key token(s) after the modifiers, rendered as their own `Kbd`s. */
  keys: string[];
  /**
   * Which platform this combo is bound on, when the two differ.
   *
   * Omit for the overwhelming majority: `mod` already expresses ⌘-or-Ctrl, so a
   * chord only needs this when the *chord itself* differs rather than the
   * modifier's name. Worktree navigation is the one such row — `⌥Tab` on macOS,
   * `Ctrl+Shift+B/N` elsewhere — because `⌥Tab` is free on macOS and is the
   * window switcher everywhere else, and every other Tab variant is claimed off
   * macOS too.
   *
   * **Still one action, one shortcut**: a reader only ever sees the combos for
   * the platform they are on (`combosFor`), so the overview never offers two
   * ways to do one thing. `registry.test.ts` enforces that per platform.
   */
  platform?: "mac" | "other";
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
    title: "Previous / next worktree",
    description:
      "Step the rail's selection, wrapping at either end. Same shape as the tab chord, one level up.",
    combos: [
      // macOS: rhymes with the ⌃Tab that steps tabs one level down, and ⌥Tab is
      // genuinely free there (the OS switcher is ⌘Tab).
      { alt: true, keys: ["Tab"], platform: "mac" },
      { alt: true, shift: true, keys: ["Tab"], platform: "mac" },
      // Elsewhere ⌥Tab *is* the window switcher, and every other Tab variant is
      // claimed too — so adjacent letters, left is previous and right is next.
      { ctrl: true, shift: true, keys: ["B"], platform: "other" },
      { ctrl: true, shift: true, keys: ["N"], platform: "other" },
    ],
    // Page-dispatched, and reaches a focused terminal and browser pane through
    // `isAppShortcutChord` and `browserViews.js` forwarding — see the header for
    // why a Tab chord cannot be a menu accelerator.
    //
    // `desktopOnly` is the conservative call rather than a precise one, because
    // the flag is per row and the two halves differ: `Ctrl+Shift+N` opens a
    // private window in a browser, so the non-mac chord genuinely is unreachable
    // there — while `⌥Tab` on macOS would in fact work in a tab, since the
    // handler `preventDefault`s the focus traversal. Under-promising beats a row
    // that silently does nothing for half its readers.
    desktopOnly: true,
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
    id: "cycle-tabs",
    category: "navigation",
    title: "Next / previous tab",
    description:
      "Step through this worktree's tabs — left dock then right, continuing into its detached windows and wrapping at both ends.",
    combos: [
      { ctrl: true, keys: ["Tab"] },
      { ctrl: true, shift: true, keys: ["Tab"] },
    ],
    // Every browser claims ⌃Tab for its own tab strip, so it never reaches a
    // page — the row would otherwise promise something that silently does
    // nothing for someone running Veld in a tab.
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
    id: "split-dock",
    category: "layout",
    title: "Open to the side",
    description:
      "Move the active tab to the other half of the dock — or, when it is the only tab there, open a new pane on that side instead.",
    // ⌘⇧D, not the ⌘D iTerm and Ghostty use: `mod` covers literal Ctrl, and
    // Ctrl+D is EOF in every shell. Shift steps around that on both platforms.
    combos: [{ mod: true, shift: true, keys: ["D"] }],
    // Chrome and Firefox bind ⌘⇧D to "bookmark all tabs", so it does not reach
    // a page either.
    desktopOnly: true,
  },
  {
    id: "new-tab",
    category: "layout",
    title: "New tab",
    description: "Open a new pane in the focused half of the dock.",
    combos: [{ mod: true, keys: ["T"] }],
    // An Electron menu accelerator (File → New Tab), so a focused browser pane
    // cannot swallow it. Every mainstream browser claims ⌘T for a tab of its
    // own, so there is nothing to list for someone using Veld in one.
    desktopOnly: true,
  },
  {
    id: "close-tab",
    category: "layout",
    title: "Close tab",
    description: "Close the focused half's active tab. A busy terminal asks first.",
    combos: [{ mod: true, keys: ["W"] }],
    // Menu accelerator, and claimed by every browser, exactly as ⌘T above.
    desktopOnly: true,
  },
  {
    id: "close-window",
    category: "layout",
    title: "Close window",
    // ⌘⇧W rather than ⌘W — the Chrome/Safari/VS Code arrangement, which is what
    // frees ⌘W for the tab. Listed for the first time here even though window
    // closing is not new: it was the Electron `close` role's default
    // accelerator, so it had no row of its own to be found in.
    combos: [{ mod: true, shift: true, keys: ["W"] }],
    desktopOnly: true,
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
/**
 * The combos of a shortcut that actually exist on this platform.
 *
 * Everything with no `platform` applies everywhere; a tagged combo shows only on
 * its own platform. Every reader of `SHORTCUTS` that renders keys must go
 * through this, or a macOS user is shown a Linux chord as if it were a second
 * way to do the same thing.
 */
export function combosFor(shortcut: ShortcutDef, mac: boolean): KeyCombo[] {
  return shortcut.combos.filter((c) => !c.platform || c.platform === (mac ? "mac" : "other"));
}

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

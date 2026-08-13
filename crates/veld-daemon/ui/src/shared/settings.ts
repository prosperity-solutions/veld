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
/** Where a new worktree's branch is cut from. Mirrors the Rust `one_of` for
 *  `git.createFrom`; a value the daemon acts on. */
export type GitCreateFrom = "origin" | "local";
/** Where a *new* worktree's checkout lands. Mirrors the Rust `one_of` for
 *  `worktree.storageMode`; a value the daemon acts on in `create_worktree`. */
export type WorktreeStorageMode = "sibling" | "custom";
/**
 * Which zone a log timestamp is *shown* in — never which zone it is stored in.
 *
 * Every line arrives from the daemon stamped in UTC (`db::ts_to_str` in
 * `veld-core`, because lexicographic order has to equal chronological order there),
 * so this is a rendering choice made at `fmtTs`/`fmtTsFull` and nowhere else. Mirrors
 * `LogTimeZone` in `veld-core/src/db/settings.rs`; `veld logs` reads the same key.
 */
export type LogTimeZone = "local" | "utc";

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
  bellVolume: 50,
  shiftEnterNewline: true,
  reconnectTries: 3,
  reconnectBackoffSeconds: 5,
  reconnectFirstDelaySeconds: 1,
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
  terminalInterceptSystemOpen: true,
  // Both take the shipped default rather than the previous release's behaviour, which
  // is the exception this file's rule above allows for: each one decides whether a
  // control *exists* (the rail's unread badge), and a build whose rail has the badge
  // must not behave as though the feature were off because the daemon predates the key.
  terminalShellIntegration: true,
  terminalAgentIntegration: true,
  extensionsAutoRefresh: true,
  // **Off**, matching the Rust default — and here the file's "shipped default" exception
  // and the "previous release's behaviour" rule agree, because the honest reason is the
  // signal's own unevenness: exact for a shell command, absent for a supported agent, and
  // meaningless for an unsupported one. See `defaults()` in veld-core's settings.rs.
  activityShowWorking: false,
  // The notification table. Only `commandFinished` is off — see the Rust `defaults()`
  // for why each one is where it is. `notifyWaiting` in particular defaults on because
  // an OSC 9 banner already fired before this table existed, and a `false` here would
  // silently switch off shipped behaviour.
  activityNotifyCommandFinished: false,
  activityNotifyCommandFailed: true,
  activityNotifyAgentWaiting: true,
  activityNotifyNoticed: true,
  activityNotifyAgentFinished: true,
  // The one place this file's "previous release's behaviour" rule and the Rust default
  // happen to *agree*: this view already rendered browser-local time before the key
  // existed (its `fmtTs` read `Date` getters), so `local` is both.
  logsTimeZone: "local" as LogTimeZone,
  // Shipped default for a new control. By the file's `quickSwitch*` exception, a new
  // key that decides whether controls appear takes the shipped default so an older
  // daemon (which cannot know the key) does not look like a broken new UI.
  hideDisabledActions: true,
  // Matches the Rust default rather than the previous release's behaviour, and
  // the two disagree here: before this key there were no project cards at all,
  // so the rule would say `false`. But an older daemon cannot know the key, and
  // defaulting to silence would mean a project's news reaching nobody with no
  // way for either side to tell — the exact failure the channel cannot survive.
  showProjectNews: true,
  // The shipped default for a new control, by the file's `quickSwitch*`
  // exception: the create dialog renders "based on the latest origin" unless an
  // older daemon says otherwise, which is the behaviour this setting ships with.
  gitCreateFrom: "origin" as GitCreateFrom,
  // Sibling of the repo, matching the Rust default: today's only behaviour, and
  // what a fresh install already does before anyone has chosen a folder.
  worktreeStorageMode: "sibling" as WorktreeStorageMode,
  // Empty, matching the Rust default — meaningless in `sibling` mode, and in
  // `custom` mode read as "no folder chosen yet" by `worktreeStorageDir` below.
  worktreeStorageDir: "",
  // Mirrors `DEFAULT_SEARCH_URL` in `veld-core/src/db/settings.rs`. The
  // `quickSwitch*` exception again, and here the previous-release rule would be
  // actively misleading: the address bar's own placeholder tells the user they can
  // search, so falling back to "search is off" would print a promise this client
  // then refuses to keep.
  searchUrl: "https://www.google.com/search?q=%s",
  // "Work it out from the login shell" — both the shipped default and the
  // previous release's behaviour, so the file's two rules agree here. Nothing on
  // this side interprets the value: it names a shell the *daemon* spawns, and the
  // picker is the only reader.
  terminalShell: "auto",
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
  bellVolume: number;
  shiftEnterNewline: boolean;
  /** How many times a dropped socket reconnects to the same shell by itself
   *  before waiting for a click. `0` disables auto-reconnect. */
  reconnectTries: number;
  /** Seconds between auto-reconnect attempts after the first. */
  reconnectBackoffSeconds: number;
  /** Seconds before the first auto-reconnect attempt — the "nearly immediate"
   *  one. */
  reconnectFirstDelaySeconds: number;
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
    bellVolume: num(doc, "terminal.bellVolume", FALLBACK.bellVolume),
    shiftEnterNewline: bool(
      doc,
      "terminal.shiftEnterNewline",
      FALLBACK.shiftEnterNewline,
    ),
    reconnectTries: num(doc, "terminal.reconnectTries", FALLBACK.reconnectTries),
    reconnectBackoffSeconds: num(
      doc,
      "terminal.reconnectBackoffSeconds",
      FALLBACK.reconnectBackoffSeconds,
    ),
    reconnectFirstDelaySeconds: num(
      doc,
      "terminal.reconnectFirstDelaySeconds",
      FALLBACK.reconnectFirstDelaySeconds,
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
 * Which shell a terminal opens: `"auto"` (the user's login shell) or an absolute
 * path.
 *
 * Deliberately **not** part of [`TerminalPrefs`]: that bundle is the set of xterm
 * options `applyTerminalPrefs` pushes into every live terminal, and this is not one
 * — it is chosen by the daemon at spawn time and cannot change a running shell.
 * Putting it there would invite a re-render that looked like it did something.
 *
 * Returned raw rather than validated against the discovered list. The list comes
 * from `GET /api/shells` and is what this machine *has*; a stored path that is not
 * on it is a shell that was uninstalled or lives somewhere unusual, and the picker
 * shows it as a custom path rather than silently resetting a choice the user made.
 */
export function terminalShell(doc: SettingsDoc): string {
  return str(doc, "terminal.shell", FALLBACK.terminalShell);
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
 * Whether a terminal session gets Veld's shim directory on its `PATH`, so a program
 * that calls `open`/`xdg-open` instead of reading `$BROWSER` is routed too.
 * **Defaults on.**
 *
 * Display only, like the switch above: the daemon builds the session environment and
 * is the only thing that acts on this.
 */
export function terminalInterceptSystemOpen(doc: SettingsDoc): boolean {
  return bool(
    doc,
    "terminal.interceptSystemOpen",
    FALLBACK.terminalInterceptSystemOpen,
  );
}

/**
 * Whether a terminal reports when a command started and how it ended (OSC 133 shell
 * integration), which is what fills the rail's unread badge for plain commands.
 * **Defaults on.**
 *
 * Display only. Independent of `terminal.interceptSystemOpen` even though both ride the
 * same shell-startup handoff — see `veld-daemon/src/pty/shims.rs`, where the first
 * version coupled them and made this feature disappear when the *other* switch was off.
 */
export function terminalShellIntegration(doc: SettingsDoc): boolean {
  return bool(
    doc,
    "terminal.shellIntegration",
    FALLBACK.terminalShellIntegration,
  );
}

/**
 * Whether Veld wraps a coding agent's CLI to install lifecycle hooks, so an agent
 * waiting on you reaches the rail's badge. **Defaults on.**
 *
 * Display only. Nothing of the user's is edited either way: for Claude the hooks
 * ride an ephemeral `--settings` file, merged rather than replacing
 * `~/.claude/settings.json`; for Codex it's a one-invocation `-c notify=[...]`
 * override, never a write to `~/.codex/config.toml`; for Pi it's an ephemeral
 * extension module loaded once with `-e`, never a write to
 * `~/.pi/agent/settings.json` or `~/.pi/agent/extensions/`.
 */
export function terminalAgentIntegration(doc: SettingsDoc): boolean {
  return bool(
    doc,
    "terminal.agentIntegration",
    FALLBACK.terminalAgentIntegration,
  );
}

/**
 * Whether the daemon may evaluate a project's `ide.extensions` status badges on
 * a timer.
 *
 * The one machine-global off switch for the only thing Veld runs from a repo's
 * config with no user action. Off leaves the buttons clickable — a click is the
 * user asking — and stops the unattended half only.
 */
export function extensionsAutoRefresh(doc: SettingsDoc): boolean {
  return bool(doc, "extensions.autoRefresh", FALLBACK.extensionsAutoRefresh);
}

/** What the rail shows about unseen activity, and what is allowed to interrupt you. */
export interface ActivityPrefs {
  /** Show a spinner on a worktree with something running in it. */
  showWorking: boolean;
  /**
   * Which events raise a system notification, keyed by the **setting name** rather than
   * by a shape of its own.
   *
   * A `Record` and not four named fields, because the consumer looks the key up: the
   * inbox's `notifyKey(unseen)` decides which row an event belongs to, and a mapping
   * from an event to a *field name* would be a second vocabulary to keep in step with
   * the first.
   */
  notify: Record<string, boolean>;
}

/**
 * The activity group's preferences. **Everything defaults on except
 * `notifyCommandFinished`.**
 *
 * Read here for display and for the notification decision, both of which are the
 * client's: the daemon has no idea which window is focused, and these five keys are the
 * only ones in the settings document it never reads.
 */
export function activityPrefs(doc: SettingsDoc): ActivityPrefs {
  return {
    showWorking: bool(doc, "activity.showWorking", FALLBACK.activityShowWorking),
    notify: {
      "activity.notifyCommandFinished": bool(
        doc,
        "activity.notifyCommandFinished",
        FALLBACK.activityNotifyCommandFinished,
      ),
      "activity.notifyCommandFailed": bool(
        doc,
        "activity.notifyCommandFailed",
        FALLBACK.activityNotifyCommandFailed,
      ),
      "activity.notifyAgentWaiting": bool(
        doc,
        "activity.notifyAgentWaiting",
        FALLBACK.activityNotifyAgentWaiting,
      ),
      "activity.notifyNoticed": bool(
        doc,
        "activity.notifyNoticed",
        FALLBACK.activityNotifyNoticed,
      ),
      "activity.notifyAgentFinished": bool(
        doc,
        "activity.notifyAgentFinished",
        FALLBACK.activityNotifyAgentFinished,
      ),
    },
  };
}

/**
 * Which zone the logs view renders a line's timestamp in. **Defaults to `local`.**
 *
 * The same key `veld logs` reads, so the terminal and the app agree about what
 * `09:12:33` means without either being told twice — which was the actual defect: the
 * CLI printed UTC, this view printed local, and the legacy dashboard printed UTC.
 */
export function logsTimeZone(doc: SettingsDoc): LogTimeZone {
  return oneOf(
    doc,
    "logs.timeZone",
    ["local", "utc"] as const,
    FALLBACK.logsTimeZone,
  );
}

/**
 * Whether a top-bar action that is currently inapplicable is hidden rather than
 * shown greyed out. **Defaults on.**
 *
 * "Hide the restart button when nothing is running" and its siblings (the
 * machine-vars button for a project that asks for nothing, the URLs button with
 * nothing to open) are all one decision: is the bar a stable set of controls,
 * or does it shrink to what currently applies? Turning this off keeps every
 * control and disables the ones that cannot fire. Purely a rendering choice —
 * nothing the daemon enforces reads it, and it has no server-side validator
 * beyond the boolean shape check.
 */
export function hideDisabledActions(doc: SettingsDoc): boolean {
  return bool(doc, "ui.hideDisabledActions", FALLBACK.hideDisabledActions);
}

/**
 * Whether a project's own `ide.news` cards are shown. **Defaults on.**
 *
 * Veld's own promotions are not affected and have no such switch, and that
 * asymmetry is the whole point of the setting: this is the one surface where
 * somebody other than Veld — a teammate with commit access to `veld.json` — can
 * put a modal in front of the reader. The caps in `veld_core::ide` bound how much
 * a project can say; this is the reader's own answer to being told anything at
 * all.
 *
 * A user-level switch rather than a per-project one, because per-project consent
 * would have to be given before the first card, i.e. before there is anything to
 * consent to. Off hides the cards and their share of the unread badge; it does
 * not mark them read, so turning it back on shows what was missed.
 */
export function showProjectNews(doc: SettingsDoc): boolean {
  return bool(doc, "ui.showProjectNews", FALLBACK.showProjectNews);
}

/**
 * Where a new worktree's branch is cut from: `origin` (fetch, then base on
 * `origin/<default>`) or `local` (the main checkout's current HEAD).
 *
 * `origin` is the default and the point of the setting — a worktree created
 * from a stale local `main` is born behind the remote. The daemon enforces this
 * in `create_worktree`; this read is what the create dialog and the settings
 * surface show.
 */
export function gitCreateFrom(doc: SettingsDoc): GitCreateFrom {
  const v = doc["git.createFrom"];
  return v === "local" ? "local" : "origin";
}

/**
 * Where a *new* worktree's checkout lands: `sibling` (the `_worktrees` folder
 * next to the repo — today's only behaviour) or `custom` (a configured
 * directory, see [`worktreeStorageDir`]).
 *
 * The daemon enforces this in `create_worktree`; existing checkouts already on
 * disk are never moved by changing it.
 */
export function worktreeStorageMode(doc: SettingsDoc): WorktreeStorageMode {
  return oneOf(
    doc,
    "worktree.storageMode",
    ["sibling", "custom"] as const,
    FALLBACK.worktreeStorageMode,
  );
}

/**
 * The configured base directory for new worktree checkouts, or `""` if none
 * has been chosen yet.
 *
 * Read with its own function rather than through `str()`, for the same reason
 * as [`searchUrl`]: **`""` is a value here**, not an absent one — `str()`
 * would substitute the fallback (also `""`) either way, but a caller should
 * not have to know that to get the right answer.
 */
export function worktreeStorageDir(doc: SettingsDoc): string {
  const v = doc["worktree.storageDir"];
  return typeof v === "string" ? v.trim() : FALLBACK.worktreeStorageDir;
}

/**
 * The hue for the "update main" staleness pill, green → orange → red.
 *
 * Two facts the daemon exposes are blended: how many commits the main checkout
 * is behind, and how old the newest missing commit is. `sensitivity` (the
 * project's `ide.stalenessSensitivity`, default 1) scales both thresholds, so a
 * project tunes how urgent its own drift looks without changing the shape.
 *
 * The baseline (`sensitivity = 1`) is deliberately hot: **a single commit a
 * week old, or fifty commits in a day, are both at the top of the scale (red)**.
 * That is what makes the default usable — the pill is a nag, and the nag should
 * come on early. The two facts combine as a union (`1 - (1−a)(1−b)`) rather
 * than an average, because either reaching the top alone must read red; small
 * contributions still combine smoothly.
 *
 * Pure so it is testable and so a future extension badge can reuse the curve.
 */
export function stalenessHue(
  behind: number,
  ageSeconds: number,
  sensitivity = 1,
): number {
  // `sensitivity` is floored by the daemon to `0.1`, but floor here too so a
  // caller that passes 0 (or an unreadable config reading as 0) never divides
  // by zero or inverts the curve.
  const s = Math.max(sensitivity, 0.1);
  const countFactor = Math.min(behind / (50 / s), 1);
  const ageFactor = Math.min(ageSeconds / ((7 * 86_400) / s), 1);
  const score = 1 - (1 - countFactor) * (1 - ageFactor);
  // 140 is green, 0 is red; the linear sweep passes through orange (~30).
  return Math.round(140 * (1 - score));
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

/**
 * The search template a browser pane sends non-addresses to, or `""` for none.
 *
 * Read with its own function rather than through `str()`, because **`""` is a value
 * here** — the off switch — and `str()` treats an empty string as absent and
 * substitutes the fallback. Passing this key through it would make "no search" the
 * one setting a user cannot save.
 *
 * The `%s` and http(s) rules are the daemon's (`parse_search_template`), enforced on
 * write; `searchTarget` in `panes/model.ts` re-checks them at use rather than
 * trusting them, since a row written by another build or by hand reaches the same
 * navigation.
 */
export function searchUrl(doc: SettingsDoc): string {
  const v = doc["browser.searchUrl"];
  return typeof v === "string" ? v.trim() : FALLBACK.searchUrl;
}

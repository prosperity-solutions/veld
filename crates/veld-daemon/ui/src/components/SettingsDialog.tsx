/**
 * The settings surface.
 *
 * A dialog rather than a route or a pane, deliberately: it reuses the existing
 * discriminated-union dialog state and the native-view suspension that comes with
 * it, so it adds no new surface machinery. (Turning `/ide` into a real router is
 * the honest long-term shape and a refactor of a 2500-line component — it would
 * have eaten this batch and shipped no settings.)
 *
 * **There is no Save button, and that is the design.** Every control writes its own
 * field the moment it changes. Two windows can have this open at once, and with a
 * whole-document save the second one to close would silently revert the first. With
 * per-field writes there is no save event to conflict: two windows editing
 * different settings both win, and the same setting resolves last-write-wins, which
 * is the right answer for a font size.
 *
 * Text and number inputs commit on blur or Enter rather than per keystroke, so a
 * three-digit font size is one request instead of three — and each request's
 * response carries the daemon's clamped value, which is what makes an out-of-range
 * entry visibly snap back instead of appearing to have been accepted.
 *
 * **One group is shown at a time**, picked from a sidebar. Every setting used to be
 * on one scroll, six headings deep, which meant the font size and the worktree trash
 * retention were the same distance away — and finding either was a scroll rather
 * than a choice. The groups are Mantine `Tabs` in vertical orientation, so the
 * sidebar is a real tablist: arrow keys move between groups and each panel is
 * labelled by its tab, which a hand-rolled column of buttons would not give.
 *
 * The narrow layout drops the sidebar for a select, and it is chosen in CSS
 * (`visibleFrom` / `hiddenFrom`) rather than by measuring the viewport. Both
 * controls drive the same `group` state, so there is no second source of truth —
 * and no first-paint flash, which a `matchMedia` read would have on a surface that
 * mounts already open. This is the UI's first breakpoint of any kind; it is
 * viewport-keyed rather than modal-width-keyed because the modal's width *is* the
 * viewport once it stops fitting.
 */

import { type ReactNode, useEffect, useMemo, useState } from "react";
import {
  Button,
  Checkbox,
  Group,
  NativeSelect,
  NumberInput,
  Slider,
  Stack,
  Tabs,
  Text,
  TextInput,
  Textarea,
} from "@mantine/core";
import {
  IconActivity,
  IconAdjustments,
  IconAppWindow,
  IconGitBranch,
  IconLink,
  IconTerminal2,
} from "@tabler/icons-react";

import {
  api,
  type SettingsDoc,
  type ShellIntercept,
  type ShellList,
} from "../api";
import { searchTarget } from "../panes/model";
import { Modal } from "./dialogs";
import {
  availableFonts,
  matchFont,
} from "../shared/terminalFonts";
import {
  detachGraceMinutes,
  externalOrigins,
  gitCreateFrom,
  hideDisabledActions,
  logsTimeZone,
  showProjectNews,
  searchUrl,
  activityPrefs,
  extensionsAutoRefresh,
  terminalAgentIntegration,
  terminalInterceptSystemOpen,
  terminalShellIntegration,
  runHistoryDays,
  terminalOpenUrlsInApp,
  terminalShell,
  trashRetentionDays,
  markerStyle,
  quickSwitchPrefs,
  terminalPrefs,
  worktreeStorageDir,
  worktreeStorageMode,
  type GitCreateFrom,
  type LogTimeZone,
  type MarkerStyle,
  type WorktreeStorageMode,
} from "../shared/settings";

/** A labelled row with its explanation under it, used for every control. */
function Row(props: {
  label: string;
  help?: string;
  children: React.ReactNode;
}) {
  return (
    <Stack gap={2}>
      <Group justify="space-between" wrap="nowrap" align="center">
        <Text size="sm">{props.label}</Text>
        {props.children}
      </Group>
      {props.help && (
        <Text size="xs" c="dimmed">
          {props.help}
        </Text>
      )}
    </Stack>
  );
}

/**
 * Sentinel for the shell picker's "Custom path…" option.
 *
 * A `\u0000` prefix like `CUSTOM_FONT` below, and for the same reason: every real
 * value in that select is something the daemon would accept, and the daemon refuses
 * a shell path containing a NUL — so this sentinel cannot collide with one.
 *
 * Written as an escape, never as a literal NUL byte: a NUL in a source file makes
 * ripgrep and grep classify it as binary and skip it silently (see the AGENTS.md
 * note about `App.tsx`).
 */
const CUSTOM_SHELL = "\u0000custom-shell";

/** Sentinel for the "Custom…" option; not a font stack. */
const CUSTOM_FONT = "\u0000custom";

/**
 * A heading *inside* a group, for the one group long enough to need dividing.
 * Groups themselves are named by their tab, so most panels have none of these —
 * a heading repeating the tab you just clicked is noise.
 */
function SectionTitle(props: { children: ReactNode }) {
  return (
    <Text size="xs" fw={600} c="dimmed" tt="uppercase">
      {props.children}
    </Text>
  );
}

/**
 * The notification table, in the order it reads.
 *
 * Data rather than four hand-written rows, because the *only* thing that differs between
 * them is a key and two strings — and because the keys have to match
 * `inbox.notifyKey(unseen)` exactly. One list is one place for that to be true.
 */
const NOTIFY_ROWS: { key: string; label: string; help: string }[] = [
  {
    key: "activity.notifyCommandFinished",
    label: "A command finished",
    help: "Off by default: a build that succeeded is news, and the rail already carries it. This is the row most likely to make someone turn notifications off wholesale.",
  },
  {
    key: "activity.notifyCommandFailed",
    label: "A command failed",
    help: "The one 'it ended' event that is actionable — and finding out twenty minutes later is the cost this exists to remove.",
  },
  {
    key: "activity.notifyAgentWaiting",
    label: "A coding agent is waiting for you",
    help: "It stopped at a permission prompt, a question, or a plan to approve — and it will sit there until you answer. The single most actionable thing Veld can tell you about a pane you are not looking at.",
  },
  {
    key: "activity.notifyAgentFinished",
    label: "A coding agent finished",
    help: "Note the frequency: an agent's end-of-turn signal fires after every response, not once per session — so this is a banner each time one hands control back while you are elsewhere. That is the point if you walked away, and the first row to turn off if it is not. Sub-agents do not count: an agent that farms work out announces each helper finishing, and none of those is yours to act on, so only the session's own turn is reported here.",
  },
  {
    key: "activity.notifyNoticed",
    label: "A program asked to be noticed",
    help: "Any program can ring the terminal's notification sequence (OSC 9) with a message — a test runner, a deploy script, a tool Veld knows nothing about. Its own row rather than the agent one above, because that label has to be able to say what it covers. Veld cannot yet tell that a plain program is merely *waiting* for input: that is not observable from the browser at all, so a program has to say so itself.",
  },
];

/** Mirrors `MAX_WORKTREE_STORAGE_DIR_LEN` in veld-core's settings.rs. */
const MAX_WORKTREE_STORAGE_DIR_LEN = 1024;

/**
 * Every rule the daemon's `WorktreeStorageDir` validator enforces, mirrored
 * so a value this box would reject never round-trips through a save attempt
 * first — the daemon's 400 has no body a user would ever see. `null` means
 * empty is a real value here too (the off switch, not an error).
 */
function worktreeStorageDirError(path: string): string | null {
  const v = path.trim();
  if (v === "") return null;
  // Bytes, not JS's UTF-16 code units: the Rust validator this mirrors
  // (`WorktreeStorageDir` in settings.rs) measures `s.len()`, which is a
  // byte count — a path with any multi-byte character would otherwise pass
  // this check under the limit and still get 400'd server-side.
  if (new TextEncoder().encode(v).length > MAX_WORKTREE_STORAGE_DIR_LEN) {
    return `Must be ${MAX_WORKTREE_STORAGE_DIR_LEN} bytes or fewer`;
  }
  // Also rejects the Unicode C1 controls (U+0080–U+009F) `char::is_control`
  // catches on the Rust side, not only the ASCII C0 range + DEL.
  // biome-ignore lint/suspicious/noControlCharactersInRegex: rejecting them is the point.
  if (/[\x00-\x1f\x7f-\x9f]/.test(v)) return "Must not contain control characters";
  if (!v.startsWith("/")) return "Must be an absolute path";
  if (v.split("/").includes("..")) return 'Must not contain ".."';
  return null;
}

type GroupId = "general" | "git" | "terminal" | "activity" | "links" | "browser";

/**
 * The groups, in sidebar order. `general` is first and is the one the dialog opens
 * on: it holds the settings that answer to no larger surface, which is exactly the
 * set that had a one-row section of its own before.
 *
 * `links` is a group rather than the tail of `terminal`, which is where its three
 * settings lived. They decide where a URL goes — clickable output, `$BROWSER`, the
 * `open`/`xdg-open` shims, the exempt list — and two of the three are gated on the
 * first, so they read as one decision with two refinements. Filed under Terminal
 * they were eight rows below the font size, with `browser.externalOrigins` sitting
 * under a Terminal heading.
 *
 * `activity` is the same story one step further on. Its seven settings — what Veld
 * notices, what it shows in the rail, and what is allowed to interrupt you — are not
 * *about* the terminal even though two of them reach it: Terminal already carries
 * appearance, behaviour, shell and auto-reconnect, and burying a four-row notification
 * table under it would put "does this send me a system banner" below the cursor style.
 *
 * **A group is not a key prefix**, here or anywhere in this dialog: `browser.externalOrigins`
 * lives under *Links*, and `worktree.*`, `runs.*` and `logs.*` all live under *General*.
 * So the two producer switches keep their `terminal.*` names — they configure what veld
 * puts in your **shell** — while the presentation and notification keys are `activity.*`.
 */
const GROUPS: { id: GroupId; label: string; icon: ReactNode }[] = [
  { id: "general", label: "General", icon: <IconAdjustments size={15} /> },
  { id: "git", label: "Git", icon: <IconGitBranch size={15} /> },
  { id: "terminal", label: "Terminal", icon: <IconTerminal2 size={15} /> },
  { id: "activity", label: "Activity", icon: <IconActivity size={15} /> },
  { id: "links", label: "Links", icon: <IconLink size={15} /> },
  { id: "browser", label: "Browser panes", icon: <IconAppWindow size={15} /> },
];

/**
 * The panel scrolls, the sidebar and the footnote do not — the same inner-scroll
 * shape `NewWorktreeDialog` uses, and for the same reason: with the modal body as
 * the scroller, choosing a group would scroll the sidebar out of reach of the next
 * choice. `minHeight` is a floor, not a fixed height — it stops the two-row Browser
 * panes group from collapsing to a modal barely taller than its own title bar. The
 * modal does still grow between that floor and the `maxHeight` cap, which is the
 * shared `min(58vh, …)` so a laptop sees the bottom of the tallest panel; a fixed
 * height would stop the growth at the cost of a lot of dead space under the short
 * groups.
 */
const PANEL_STYLE = {
  flex: 1,
  // The repo's flexbox idiom: without it, one long help line sets the panel's
  // width and the sidebar gets squeezed instead of the text wrapping.
  minWidth: 0,
  minHeight: 260,
  maxHeight: "min(58vh, 520px)",
  overflowY: "auto" as const,
  // Keeps a focused control's ring off the scrollbar, as in NewWorktreeDialog.
  paddingRight: 8,
  marginRight: -8,
};

export function SettingsDialog(props: {
  settings: SettingsDoc | null;
  saving: boolean;
  error: string | null;
  onSave: (patch: SettingsDoc) => Promise<void>;
  onClose: () => void;
}) {
  const { settings } = props;
  const locked = !settings || props.saving;
  const term = terminalPrefs(settings ?? {});
  const marker = markerStyle(settings ?? {});
  const quick = quickSwitchPrefs(settings ?? {});
  const openInApp = terminalOpenUrlsInApp(settings ?? {});
  const intercept = terminalInterceptSystemOpen(settings ?? {});
  const shellIntegration = terminalShellIntegration(settings ?? {});
  const agentIntegration = terminalAgentIntegration(settings ?? {});
  const autoRefresh = extensionsAutoRefresh(settings ?? {});
  const activity = activityPrefs(settings ?? {});
  const logsTz = logsTimeZone(settings ?? {});
  const hideDisabled = hideDisabledActions(settings ?? {});
  const projectNews = showProjectNews(settings ?? {});

  // Not persisted, and deliberately so: the dialog is remounted on every open, so
  // it always opens on General rather than wherever a previous visit ended up.
  const [group, setGroup] = useState<GroupId>("general");

  // Number inputs are held locally while being typed and committed on blur —
  // see the file header. Re-seeded whenever the daemon's value changes so a
  // clamp lands in the box rather than leaving the rejected number on screen.
  const [fontSize, setFontSize] = useState<number | string>(term.fontSize);
  const [scrollback, setScrollback] = useState<number | string>(term.scrollback);
  const [bellVolume, setBellVolume] = useState(term.bellVolume);
  const graceValue = detachGraceMinutes(settings ?? {});
  const [grace, setGrace] = useState<number | string>(graceValue);
  const reconnectTriesValue = term.reconnectTries;
  const [reconnectTries, setReconnectTries] = useState<number | string>(
    reconnectTriesValue,
  );
  const reconnectBackoffValue = term.reconnectBackoffSeconds;
  const [reconnectBackoff, setReconnectBackoff] = useState<number | string>(
    reconnectBackoffValue,
  );
  const reconnectFirstValue = term.reconnectFirstDelaySeconds;
  const [reconnectFirst, setReconnectFirst] = useState<number | string>(
    reconnectFirstValue,
  );
  const retentionValue = trashRetentionDays(settings ?? {});
  const [retention, setRetention] = useState<number | string>(retentionValue);
  const historyValue = runHistoryDays(settings ?? {});
  const [history, setHistory] = useState<number | string>(historyValue);
  const [fontFamily, setFontFamily] = useState(term.fontFamily);
  const shellValue = terminalShell(settings ?? {});
  const [shellPath, setShellPath] = useState(shellValue);
  // This dialog's own intent — "I clicked Custom path…" — and nothing else. It is
  // deliberately **not** reset from the settings document, which is the last piece
  // of machinery this row had and the source of its fourth consecutive review
  // finding: `save` updates the document optimistically, so resetting on every
  // document change closed the field the moment a custom path was saved, whenever
  // the shells list happened to be absent. Intent belongs to the window that
  // expressed it and dies with the dialog, which is remounted on every open.
  const [customShell, setCustomShell] = useState(false);
  // What this machine has. Fetched once per open rather than read from the
  // settings document, because it is not a setting — see `api.shells`. A failure
  // leaves it null, which still renders a working picker: the stored value is
  // always an option, so the only thing lost is the list of alternatives.
  const [shells, setShells] = useState<ShellList | null>(null);
  // **A positive fact, never a negation.** This asks only "do we *know* the stored
  // shell is not one of the listed ones", which is answerable exactly when a list
  // actually arrived. Every previous shape of this was some flavour of "not
  // listed", which had to decide what an absent list meant and got it wrong three
  // times running: sticky-only went stale when another window changed the shell;
  // treating a missing list as listed hid the field for ever when the fetch
  // failed; treating it as unlisted flashed the field open on every dialog open
  // and, on a failed fetch, opened it for ordinary listed shells. With the
  // question posed positively there is no third state to get wrong — no list means
  // no claim, and the user can still open the field from the select.
  const knownUnlisted =
    shells !== null &&
    shellValue !== "auto" &&
    !shells.shells.some((s) => s.path === shellValue);
  const showCustomShell = customShell || knownUnlisted;
  useEffect(() => {
    let live = true;
    api
      .shells()
      .then((list) => {
        if (live) setShells(list);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, []);
  // Does the `open` shim actually win in this shell? Re-asked whenever the stored
  // shell changes, because the answer is per shell — and the daemon does not cache
  // it, so pasting the suggested line and reopening this dialog shows the change.
  // Null while in flight: the row says nothing rather than flashing a warning it is
  // about to withdraw.
  const [shimStatus, setShimStatus] = useState<ShellIntercept | null>(null);
  useEffect(() => {
    let live = true;
    setShimStatus(null);
    api
      .shellIntercept()
      .then((r) => {
        if (live) setShimStatus(r);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
    // Not just the shell: the endpoint's answer is gated on both Links switches,
    // and both are editable in this same mounted dialog. Without them, turning
    // interception off leaves a warning on screen telling the user to edit
    // ~/.bashrc for a feature they just disabled.
  }, [shellValue, openInApp, intercept]);
  // One origin per line, which is what an exempt list reads as. Held locally and
  // committed on blur like every other text field here — the daemon refuses the
  // whole list if one entry is not an origin, and its error lands in `props.error`.
  const exemptValue = externalOrigins(settings ?? {});
  const [exempt, setExempt] = useState(exemptValue.join("\n"));
  // Committed on blur like the other text fields. Empty is a real value here — it
  // turns search off — so this one is never coerced back to the default on the way
  // out; that is why `searchUrl` reads the key itself rather than going through the
  // shared `str()` helper.
  const searchValue = searchUrl(settings ?? {});
  const [search, setSearch] = useState(searchValue);
  const storageMode = worktreeStorageMode(settings ?? {});
  // Committed on blur like the other text fields (`search`, `exempt` above).
  const storageDirValue = worktreeStorageDir(settings ?? {});
  const [storageDir, setStorageDir] = useState(storageDirValue);
  const [pickingStorageDir, setPickingStorageDir] = useState(false);
  const [storageDirPickError, setStorageDirPickError] = useState<string | null>(
    null,
  );
  const [openingStorageDir, setOpeningStorageDir] = useState(false);
  const [openStorageDirError, setOpenStorageDirError] = useState<
    string | null
  >(null);
  // Shown under the field as it is typed, and the same predicate the blur handler
  // refuses on — so the reason the value did not save is on screen rather than inferred
  // from nothing having happened. Empty is not broken; it is the off switch.
  const searchBroken =
    search.trim() !== "" && searchTarget(search.trim(), "veld") === null;
  // Mirrors the daemon's own validator (`worktree.storageDir` in
  // veld-core's settings.rs) — see the blur handler below for why a
  // client-side mirror exists at all. All three of the daemon's rules, not
  // only the absolute-path one: a mirror that only caught the common case
  // would still let a pasted over-long path or one carrying a tab pass here
  // and 400 with no explanation, which is the exact failure this exists to
  // prevent.
  const storageDirError = worktreeStorageDirError(storageDir);
  const storageDirBroken = storageDirError !== null;
  // Availability is probed against the DOM, so compute it once per open rather
  // than on every render — the list cannot change while the dialog is up.
  const fonts = useMemo(() => availableFonts(), []);
  const fontOption = matchFont(term.fontFamily, fonts);
  // Sticky, so choosing "Custom…" keeps the field open while it is still empty and
  // therefore still matches nothing.
  const [customFont, setCustomFont] = useState(false);

  // Re-seeded from `settings` identity, not from each value. A clamp that resolves
  // to the value already stored leaves `term.scrollback` unchanged, so a
  // value-keyed effect never fires and the rejected number stays in the box —
  // contradicting the header's promise that an out-of-range entry visibly snaps
  // back. The daemon returns a fresh document on every write, so its identity is
  // the signal that a write landed.
  useEffect(() => {
    setFontSize(term.fontSize);
    setScrollback(term.scrollback);
    setBellVolume(term.bellVolume);
    setFontFamily(term.fontFamily);
    setGrace(graceValue);
    setReconnectTries(reconnectTriesValue);
    setReconnectBackoff(reconnectBackoffValue);
    setReconnectFirst(reconnectFirstValue);
    setRetention(retentionValue);
    setHistory(historyValue);
    setShellPath(shellValue);
    setExempt(exemptValue.join("\n"));
    setSearch(searchValue);
    setStorageDir(storageDirValue);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settings]);

  // Both errors belong to controls that only render in `custom` mode (the
  // field's Browse… button, the Open Folder button below it); leaving them
  // set after switching back to `sibling` would strand red text with no
  // control in view that it explains.
  useEffect(() => {
    if (storageMode !== "custom") {
      setStorageDirPickError(null);
      setOpenStorageDirError(null);
    }
  }, [storageMode]);

  const set = (patch: SettingsDoc) => {
    // Fire and forget: the hook holds the error, and awaiting here would freeze
    // the control that was just clicked for a loopback round trip.
    void props.onSave(patch).catch(() => {});
  };

  const commitNumber = (
    key: string,
    value: number | string,
    fallback: number,
    // The setter for *this* box. Passed rather than closed over: the first version
    // reset `fontSize` for every key, so clearing the scrollback box reset the font
    // size and left the bad scrollback text on screen.
    reset: (v: number | string) => void,
  ) => {
    // Mantine's NumberInput emits raw strings while typing, so an empty or
    // half-typed box ("1e") must not be sent as NaN — the daemon would reject it
    // and the user would see an error for having selected the text.
    const n = typeof value === "number" ? value : Number(value);
    if (!Number.isFinite(n)) {
      reset(fallback);
      return;
    }
    if (n === fallback) return;
    set({ [key]: n });
  };

  return (
    <Modal title="Settings" size={820} onClose={props.onClose}>
      <Stack gap="md">
        {!settings && (
          <Text size="sm" c="dimmed">
            Loading settings from the daemon…
          </Text>
        )}
        {props.error && (
          <Text size="sm" c="var(--danger)">
            {props.error}
          </Text>
        )}

        {/* The narrow-layout group picker. Above the Tabs rather than inside them:
            a non-tab child of a vertical Tabs root is laid out as a third flex
            column beside the sidebar. */}
        <NativeSelect
          hiddenFrom="sm"
          size="sm"
          aria-label="Settings group"
          value={group}
          data={GROUPS.map((g) => ({ value: g.id, label: g.label }))}
          onChange={(e) => setGroup(e.currentTarget.value as GroupId)}
        />

        <Tabs
          orientation="vertical"
          value={group}
          /* `settings-tabs` scopes the selected-tab background to this sidebar so
             the pane-strip tabs (custom classes) and any other Mantine Tabs are
             untouched — see styles.css. */
          className="settings-tabs"
          /* Mantine's onChange is nullable (a tab can be deselected); this surface
             always has exactly one group showing, so a null falls back to General
             rather than rendering an empty panel. */
          onChange={(v) => setGroup((v as GroupId | null) ?? "general")}
          /* The exempt-origins Textarea is `autosize`, and autosize measures the
             element — inside a `display: none` panel it measures zero and comes back
             collapsed to one row. Unmounting the inactive panels avoids that
             entirely; every draft value lives in this component, not in a panel, so
             nothing is lost by switching groups mid-edit. */
          keepMounted={false}
          /* Mantine gives a tab label `flex: 1; text-align: center`, which is right
             for a horizontal tab bar and wrong for a sidebar: it centred each label
             in the space left over after its icon, so four labels of different
             lengths started at four different x positions. `flex: 1` stays, so the
             label still fills the row and the whole tab is the hit target. */
          styles={{ tabLabel: { textAlign: "left" } }}
        >
          <Tabs.List visibleFrom="sm" w={180} style={{ flex: "none" }}>
            {GROUPS.map((g) => (
              <Tabs.Tab key={g.id} value={g.id} leftSection={g.icon}>
                {g.label}
              </Tabs.Tab>
            ))}
          </Tabs.List>

          {/* `pl` only from `sm`: below it the sidebar is display:none, and a fixed
              left padding would indent the panel against nothing. */}
          <Tabs.Panel
            value="general"
            style={PANEL_STYLE}
            pl={{ base: 0, sm: "lg" }}
          >
            <Stack gap="md">
              {/* Labels here carry their own subject — "Empty the worktree trash
                  after" rather than "Empty the trash after" under a Worktrees
                  heading — because General is deliberately a flat list of the
                  settings that belong to no larger surface, so there is no heading
                  above a row to complete its sentence. */}
              <Row
                label="Let projects refresh their own status badges"
                help="A project's veld.json can declare status badges for the top bar — a pull request's state, a deploy tag — each backed by a command Veld runs in that worktree and re-runs on the interval the project asked for. This is the only thing Veld runs from a repo's configuration without you clicking something, so it has a switch. Turning it off leaves the project's buttons and menus working (a click is you asking) and stops only the unattended half; badges then render nothing. Veld bounds these commands either way: no terminal is attached, so a tool that would ask for credentials fails instead of waiting; there is a hard timeout and an output limit; a minimum refresh interval and a cap on how many a project may declare; and every command is written to the daemon log with its full arguments."
              >
                <Checkbox
                  size="xs"
                  checked={autoRefresh}
                  disabled={locked}
                  onChange={(e) =>
                    set({ "extensions.autoRefresh": e.currentTarget.checked })
                  }
                />
              </Row>
              <Row
                label="Worktree marker"
                help="Both a colour and a glyph are stored for every worktree, so switching here never loses the other one. Pick either from a worktree's context menu → Change marker…"
              >
                <NativeSelect
                  size="xs"
                  w={140}
                  value={marker}
                  disabled={locked}
                  data={[
                    { value: "color", label: "Colour" },
                    { value: "emoji", label: "Emoji" },
                  ]}
                  onChange={(e) =>
                    set({
                      "worktree.markerStyle": e.currentTarget.value as MarkerStyle,
                    })
                  }
                />
              </Row>
              <Row
                label="Empty the worktree trash after"
                /* Blunt on purpose. This is the only thing veld does that deletes a
                   checkout without asking again, so it cannot be described in the
                   register of an ordinary preference — and the last sentence is why it
                   is defensible: the deletion is still git's un-forced one, so
                   uncommitted work is refused rather than discarded. */
                help="0 keeps trashed worktrees until you empty the trash yourself — the default. Set a number of days and a worktree you moved to the trash is deleted for good after that long. Deleting still goes through git, so a checkout that picked up uncommitted changes comes back with the reason instead of being discarded."
              >
                <NumberInput
                  size="xs"
                  w={140}
                  min={0}
                  /* Mirrors MAX_TRASH_RETENTION_DAYS in veld-core's settings.rs, the
                     same way the scrollback and grace boxes mirror theirs — a control
                     offering a range the server refuses is a #204 review finding. */
                  max={365}
                  step={7}
                  value={retention}
                  disabled={locked}
                  /* Zero is "keep until I empty it", so it must be reachable — and it
                     is what the placeholder says an empty box means. */
                  placeholder="keep"
                  suffix={
                    typeof retention === "number" && retention > 0 ? " days" : ""
                  }
                  onChange={setRetention}
                  onBlur={() =>
                    commitNumber(
                      "worktree.trashRetentionDays",
                      retention,
                      retentionValue,
                      setRetention,
                    )
                  }
                  onKeyDown={(e) => {
                    if (e.key === "Enter") e.currentTarget.blur();
                  }}
                />
              </Row>
              <Row
                label="Show run history from the last"
                /* The GC sentence is not padding: without it, a 7-day maximum on a
                   setting about "old runs" reads as an arbitrary cap, and someone who
                   wants a month asks for a bigger number instead of a longer
                   retention. It also says plainly that this hides rather than
                   deletes — the opposite of the trash setting above. */
                help="Hides ended runs older than this from the History tab and the past-run pickers. Nothing is deleted — and nothing is kept longer either: veld's housekeeping already removes ended runs after 7 days, which is why that is the maximum. Leave it empty to show everything the daemon still has."
              >
                <NumberInput
                  size="xs"
                  w={140}
                  min={0}
                  /* Mirrors MAX_RUN_HISTORY_DAYS in veld-core's settings.rs, itself tied
                     to the GC's MAX_LOG_AGE_HOURS by a test. */
                  max={7}
                  value={history}
                  disabled={locked}
                  /* Zero means "show everything", so it has to be reachable, and an
                     empty box is how that reads. */
                  placeholder="all"
                  suffix={typeof history === "number" && history > 0 ? " days" : ""}
                  onChange={setHistory}
                  onBlur={() =>
                    commitNumber(
                      "runs.historyDays",
                      history,
                      historyValue,
                      setHistory,
                    )
                  }
                  onKeyDown={(e) => {
                    if (e.key === "Enter") e.currentTarget.blur();
                  }}
                />
              </Row>
              <Row
                label="Show log timestamps in"
                /* Says plainly that this is display only. Every line is stored in UTC
                   because veld sorts and merges lines by comparing those strings, so
                   "convert" here can only ever mean "spell differently on screen" — and a
                   reader who thinks a setting rewrites stored data will not trust either
                   value. The last sentence is the one people come here for. */
                /* No backticks: `help` renders through a plain Text, not markdown, so they
                   would appear on screen — every other help string in this file writes
                   flags and commands bare for that reason. */
                help="Display only — every line is stored in UTC, which is what veld sorts and interleaves by. Local is your browser's zone; hover a timestamp for the date, both zones and the exact stored value. veld logs and veld start --attach follow this setting too, and veld logs takes --utc / --local to override it for one command."
              >
                <NativeSelect
                  size="xs"
                  w={140}
                  value={logsTz}
                  disabled={locked}
                  data={[
                    { value: "local", label: "Local time" },
                    { value: "utc", label: "UTC" },
                  ]}
                  onChange={(e) =>
                    set({ "logs.timeZone": e.currentTarget.value as LogTimeZone })
                  }
                />
              </Row>
              <Row
                label="Hide top-bar actions that can't fire"
                help="On, the restart, machine-vars and URLs buttons disappear while they have nothing to act on — a run stopped, a project that asks for no values, no URLs to open. Off, every button stays and the ones that can't fire are greyed out, so the bar never changes shape. This is only about hiding versus disabling; it never removes a control that could fire."
              >
                <Checkbox
                  size="xs"
                  checked={hideDisabled}
                  disabled={locked}
                  onChange={(e) =>
                    set({ "ui.hideDisabledActions": e.currentTarget.checked })
                  }
                />
              </Row>
              <Row
                label="Show news from your projects"
                help="On, a project can tell its own team something changed — a card written into its veld.json, shown once, labelled with the project's name. Off, only Veld's own news appears. Turning it off does not mark anything read, so anything you missed is still in What's new when you turn it back on."
              >
                <Checkbox
                  size="xs"
                  checked={projectNews}
                  disabled={locked}
                  onChange={(e) => set({ "ui.showProjectNews": e.currentTarget.checked })}
                />
              </Row>
            </Stack>
          </Tabs.Panel>

          <Tabs.Panel
            value="git"
            style={PANEL_STYLE}
            pl={{ base: 0, sm: "lg" }}
          >
            <Stack gap="md">
              <Row
                label="Create worktrees from"
                help="Origin (recommended): fetching the remote and cutting the new branch from origin's default branch, so a worktree is never born behind the latest database migrations and open PRs. Local: the main checkout's current HEAD — handy when you are offline or deliberately basing on un-pushed local work."
              >
                <NativeSelect
                  size="xs"
                  w={140}
                  value={gitCreateFrom(settings ?? {})}
                  disabled={locked}
                  data={[
                    { value: "origin", label: "Latest origin" },
                    { value: "local", label: "Local main" },
                  ]}
                  onChange={(e) =>
                    set({
                      "git.createFrom": e.currentTarget.value as GitCreateFrom,
                    })
                  }
                />
              </Row>
              <Row
                label="Worktree storage location"
                help="Next to repository (default): a new checkout lands in a _worktrees folder beside its repo — today's behaviour. Custom location: every new checkout, for every repository, lands under one folder you choose. Either way each repo gets its own subfolder there, so two repos can never collide on the same checkout path. Only affects worktrees created from now on; nothing already on disk moves."
              >
                <NativeSelect
                  size="xs"
                  w={220}
                  value={storageMode}
                  disabled={locked}
                  data={[
                    { value: "sibling", label: "Next to repository (default)" },
                    { value: "custom", label: "Custom location" },
                  ]}
                  onChange={(e) =>
                    set({
                      "worktree.storageMode": e.currentTarget
                        .value as WorktreeStorageMode,
                    })
                  }
                />
              </Row>
              {storageMode === "custom" && (
                <Row label="Custom worktree folder">
                  <Stack gap={4} style={{ alignItems: "flex-end" }}>
                    <Group gap="xs" wrap="nowrap">
                      <TextInput
                        size="xs"
                        w={220}
                        value={storageDir}
                        disabled={locked}
                        placeholder="/Users/you/veld-worktrees"
                        styles={{
                          input: {
                            fontFamily: "var(--mantine-font-family-monospace)",
                          },
                        }}
                        error={storageDirError ?? undefined}
                        onChange={(e) => setStorageDir(e.currentTarget.value)}
                        onBlur={() => {
                          const v = storageDir.trim();
                          if (v === storageDirValue) return;
                          // Reuses `storageDirBroken` (computed above from this
                          // same state) rather than a second inline copy of the
                          // predicate — see the note on `searchUrl` above for
                          // why the check exists here at all: the daemon's 400
                          // has no body a user would ever see, so a rejected
                          // value would otherwise just snap back with no
                          // explanation.
                          if (storageDirBroken) return;
                          set({ "worktree.storageDir": v });
                        }}
                      />
                      <Button
                        size="xs"
                        variant="default"
                        loading={pickingStorageDir}
                        disabled={locked}
                        onClick={async () => {
                          setPickingStorageDir(true);
                          setStorageDirPickError(null);
                          try {
                            const picked =
                              await api.pickDirectory("worktree-storage");
                            if (picked) {
                              setStorageDir(picked);
                              set({ "worktree.storageDir": picked });
                            }
                          } catch (e) {
                            setStorageDirPickError(
                              e instanceof Error ? e.message : String(e),
                            );
                          } finally {
                            setPickingStorageDir(false);
                          }
                        }}
                      >
                        Browse…
                      </Button>
                    </Group>
                    {storageDirPickError && (
                      <Text size="xs" c="red">
                        {storageDirPickError}
                      </Text>
                    )}
                    {!storageDirValue && !storageDirBroken && (
                      <Text size="xs" c="dimmed">
                        No folder chosen yet — new checkouts still land next to
                        each repository until one is.
                      </Text>
                    )}
                  </Stack>
                </Row>
              )}
              <Row
                label="Open worktree storage folder"
                help={
                  storageMode !== "custom"
                    ? "Only available with Custom location: the default has no single folder — each repo's worktrees live beside it, so open one from its own context menu instead."
                    : storageDirValue
                      ? "Opens the folder above in Finder (or your file manager)."
                      : "Choose a folder above first."
                }
              >
                <Button
                  size="xs"
                  variant="default"
                  loading={openingStorageDir}
                  disabled={locked || storageMode !== "custom" || !storageDirValue}
                  onClick={async () => {
                    setOpeningStorageDir(true);
                    setOpenStorageDirError(null);
                    try {
                      await api.openWorktreeStorageDir();
                    } catch (e) {
                      setOpenStorageDirError(
                        e instanceof Error ? e.message : String(e),
                      );
                    } finally {
                      setOpeningStorageDir(false);
                    }
                  }}
                >
                  Open Folder
                </Button>
              </Row>
              {openStorageDirError && (
                <Text size="xs" c="red">
                  {openStorageDirError}
                </Text>
              )}
            </Stack>
          </Tabs.Panel>

          <Tabs.Panel
            value="terminal"
            style={PANEL_STYLE}
            pl={{ base: 0, sm: "lg" }}
          >
            <Stack gap="md">
              {/* The only group with internal headings: eleven rows split cleanly
                  into how a terminal looks and how it behaves, and without the
                  split the font controls and the detach grace are one undivided
                  list — the thing this restructure exists to remove. */}
              <SectionTitle>Appearance</SectionTitle>
              <Row label="Font size">
                <NumberInput
                  size="xs"
                  w={100}
                  min={6}
                  max={72}
                  value={fontSize}
                  disabled={locked}
                  onChange={setFontSize}
                  onBlur={() =>
                    commitNumber(
                      "terminal.fontSize",
                      fontSize,
                      term.fontSize,
                      setFontSize,
                    )
                  }
                />
              </Row>
              <Row
                label="Font"
                help={
                  fontOption
                    ? fontOption.bundled
                      ? "Bundled with Veld, so it renders the same everywhere."
                      : "Installed on this machine. Another machine may not have it."
                    : "A CSS font-family list of your own."
                }
              >
                <NativeSelect
                  size="xs"
                  w={200}
                  // The stored stack is the value, not the label: two stacks that
                  // differ only in their fallback are genuinely different settings.
                  value={fontOption?.stack ?? CUSTOM_FONT}
                  disabled={locked}
                  data={[
                    ...fonts.map((f) => ({
                      value: f.stack,
                      label: f.bundled ? f.label : `${f.label} (system)`,
                    })),
                    { value: CUSTOM_FONT, label: "Custom…" },
                  ]}
                  onChange={(e) => {
                    const v = e.currentTarget.value;
                    if (v === CUSTOM_FONT) {
                      // Reveal the field seeded with what is in effect, so "Custom"
                      // starts from the current font rather than from empty.
                      setCustomFont(true);
                      setFontFamily(term.fontFamily);
                      return;
                    }
                    setCustomFont(false);
                    setFontFamily(v);
                    set({ "terminal.fontFamily": v });
                  }}
                />
              </Row>
              {(customFont || !fontOption) && (
                <Row
                  label="Custom font family"
                  help="A CSS font-family list. Ends up in a stylesheet, so { } ; < > are refused."
                >
                  <TextInput
                    size="xs"
                    w={240}
                    value={fontFamily}
                    disabled={locked}
                    onChange={(e) => setFontFamily(e.currentTarget.value)}
                    onBlur={() => {
                      const v = fontFamily.trim();
                      // An empty family would render as the browser default and read as
                      // a bug, so treat clearing the box as "reset" rather than sending
                      // a value the daemon must reject.
                      if (!v) {
                        setFontFamily(term.fontFamily);
                        return;
                      }
                      if (v !== term.fontFamily) set({ "terminal.fontFamily": v });
                    }}
                  />
                </Row>
              )}
              <Row label="Cursor">
                <NativeSelect
                  size="xs"
                  w={140}
                  value={term.cursorStyle}
                  disabled={locked}
                  data={[
                    { value: "block", label: "Block" },
                    { value: "underline", label: "Underline" },
                    { value: "bar", label: "Bar" },
                  ]}
                  onChange={(e) =>
                    set({ "terminal.cursorStyle": e.currentTarget.value })
                  }
                />
              </Row>
              <Row label="Blinking cursor">
                <Checkbox
                  size="xs"
                  checked={term.cursorBlink}
                  disabled={locked}
                  onChange={(e) =>
                    set({ "terminal.cursorBlink": e.currentTarget.checked })
                  }
                />
              </Row>

              <SectionTitle>Behaviour</SectionTitle>
              <Row
                label="Shell"
                help={
                  shellValue === "auto"
                    ? "Your login shell. Pick another if your aliases and integrations live in a different shell's startup files — a terminal already open keeps the shell it started with."
                    : "A terminal already open keeps the shell it started with."
                }
              >
                <NativeSelect
                  size="xs"
                  w={220}
                  value={
                    customShell
                      ? CUSTOM_SHELL
                      : // The stored value is always one of the options below, so a
                        // shell that is not on this machine's list — uninstalled, or
                        // somewhere unusual — still shows as chosen rather than
                        // silently reading as "Automatic".
                        shellValue
                  }
                  disabled={locked}
                  data={[
                    {
                      value: "auto",
                      // Named, not just "Automatic": the whole question this setting
                      // answers is "which shell am I actually getting?", and the
                      // client cannot work that out itself.
                      label: shells
                        ? `Automatic (${shells.auto.split("/").pop()})`
                        : "Automatic",
                    },
                    ...(shells?.shells ?? []).map((s) => ({
                      value: s.path,
                      label: `${s.name} (${s.path})`,
                    })),
                    ...(shellValue !== "auto" &&
                    !(shells?.shells ?? []).some((s) => s.path === shellValue)
                      ? [{ value: shellValue, label: shellValue }]
                      : []),
                    { value: CUSTOM_SHELL, label: "Custom path…" },
                  ]}
                  onChange={(e) => {
                    const v = e.currentTarget.value;
                    if (v === CUSTOM_SHELL) {
                      // Seed the field with what is in effect, so "Custom" starts
                      // from the current shell rather than from empty.
                      setCustomShell(true);
                      setShellPath(shellValue === "auto" ? "" : shellValue);
                      return;
                    }
                    setCustomShell(false);
                    setShellPath(v);
                    set({ "terminal.shell": v });
                  }}
                />
              </Row>
              {shimStatus?.enabled && shimStatus.works === false && (
                <Stack gap={4}>
                  <Text size="xs" c="var(--warning, #d08770)">
                    Programs in {shimStatus.name} that call{" "}
                    <code>open</code> directly — an agent running{" "}
                    <code>open https://…</code>, for instance — will use your
                    system browser rather than a Veld pane.{" "}
                    {shimStatus.resolved
                      ? `open resolves to ${shimStatus.resolved}.`
                      : ""}{" "}
                    Links you click, and anything reading <code>$BROWSER</code>,
                    are unaffected.
                  </Text>
                  {shimStatus.hint && (
                    <>
                      <Text size="xs" c="dimmed">
                        To catch them too, add this to {shimStatus.hint.file}:
                      </Text>
                      <Text
                        size="xs"
                        ff="monospace"
                        style={{
                          background: "var(--surface-2, rgba(127,127,127,0.12))",
                          padding: "4px 8px",
                          borderRadius: 4,
                          // A shell line is wide and must not widen the panel;
                          // the file's own flexbox note explains why.
                          overflowX: "auto",
                          whiteSpace: "pre",
                        }}
                      >
                        {shimStatus.hint.line}
                      </Text>
                    </>
                  )}
                </Stack>
              )}
              {showCustomShell && (
                <Row
                  label="Custom shell path"
                  help="An absolute path — a bare name would be looked up on the daemon's own PATH, which is not your terminal's."
                >
                  <TextInput
                    size="xs"
                    w={240}
                    placeholder="/opt/homebrew/bin/fish"
                    value={shellPath}
                    disabled={locked}
                    onChange={(e) => setShellPath(e.currentTarget.value)}
                    onBlur={() => {
                      const v = shellPath.trim();
                      // Clearing the box is "never mind", not a value to send: the
                      // daemon would refuse an empty string and the user would see
                      // an error for having deleted their own typing.
                      if (!v) {
                        setShellPath(shellValue === "auto" ? "" : shellValue);
                        return;
                      }
                      if (v !== shellValue) set({ "terminal.shell": v });
                    }}
                  />
                </Row>
              )}
              <Row
                label="Scrollback"
                help="Lines kept per terminal. Lowering this drops the oldest lines from every live terminal immediately."
              >
                <NumberInput
                  size="xs"
                  w={120}
                  min={0}
                  max={100000}
                  value={scrollback}
                  disabled={locked}
                  onChange={setScrollback}
                  onBlur={() =>
                    commitNumber(
                      "terminal.scrollback",
                      scrollback,
                      term.scrollback,
                      setScrollback,
                    )
                  }
                />
              </Row>
              <Row
                label="Shift+Enter inserts a newline"
                help="Sends ESC CR, which is what Claude Code's /terminal-setup configures. Turn off if a TUI you use binds meta-Enter."
              >
                <Checkbox
                  size="xs"
                  checked={term.shiftEnterNewline}
                  disabled={locked}
                  onChange={(e) =>
                    set({ "terminal.shiftEnterNewline": e.currentTarget.checked })
                  }
                />
              </Row>
              <Row
                label="Bell volume"
                help="How loud the terminal bell rings when a process sends a BEL — the baseline 'something finished' signal. 0 is silent. Takes effect immediately for new bells."
              >
                <Slider
                  size="sm"
                  w={200}
                  min={0}
                  max={100}
                  step={5}
                  label={(v) => `${v}%`}
                  value={bellVolume}
                  disabled={locked}
                  onChange={setBellVolume}
                  onChangeEnd={(v) => set({ "terminal.bellVolume": v })}
                />
              </Row>
              <Row
                label="Keep detached shells for"
                help="Minutes a terminal with nobody attached keeps running before it is collected. Takes effect for new shells and for the next collection pass; shells already running keep the value they started with."
              >
                <NumberInput
                  size="xs"
                  w={120}
                  min={1}
                  max={10080}
                  suffix=" min"
                  value={grace}
                  disabled={locked}
                  onChange={setGrace}
                  onBlur={() =>
                    commitNumber(
                      "terminal.detachGraceMinutes",
                      grace,
                      graceValue,
                      setGrace,
                    )
                  }
                />
              </Row>
              <SectionTitle>Auto-reconnect</SectionTitle>
              <Row
                label="Reconnect attempts on drop"
                help="How many times a dropped connection reattaches to the running shell before it gives up and shows the Reconnect button (the machine slept, the daemon restarted mid-update, a proxy timed out — the shell keeps running, which is what the holder process is for). 0 turns it off: a dropped socket always waits for a click. Each attempt reattaches to the same shell, never starts a new one."
              >
                <NumberInput
                  size="xs"
                  w={100}
                  min={0}
                  max={20}
                  value={reconnectTries}
                  disabled={locked}
                  onChange={setReconnectTries}
                  onBlur={() =>
                    commitNumber(
                      "terminal.reconnectTries",
                      reconnectTries,
                      reconnectTriesValue,
                      setReconnectTries,
                    )
                  }
                />
              </Row>
              <Row
                label="First attempt after"
                help="Seconds before the first auto-reconnect fires. The first reconnect is the near-immediate one that fixes a sleep or a dropped proxy, so it defaults small — 1s — and this is the setting to raise if a flaky network is racing every blip."
              >
                <NumberInput
                  size="xs"
                  w={100}
                  min={1}
                  max={30}
                  suffix=" s"
                  value={reconnectFirst}
                  disabled={locked}
                  onChange={setReconnectFirst}
                  onBlur={() =>
                    commitNumber(
                      "terminal.reconnectFirstDelaySeconds",
                      reconnectFirst,
                      reconnectFirstValue,
                      setReconnectFirst,
                    )
                  }
                />
              </Row>
              <Row
                label="Wait between later attempts"
                help="Seconds between attempts after the first. This is the backoff: a connection still failing is not hammering a daemon that is itself coming back, so later attempts space out to this interval."
              >
                <NumberInput
                  size="xs"
                  w={100}
                  min={1}
                  max={300}
                  suffix=" s"
                  value={reconnectBackoff}
                  disabled={locked}
                  onChange={setReconnectBackoff}
                  onBlur={() =>
                    commitNumber(
                      "terminal.reconnectBackoffSeconds",
                      reconnectBackoff,
                      reconnectBackoffValue,
                      setReconnectBackoff,
                    )
                  }
                />
              </Row>
            </Stack>
          </Tabs.Panel>

          <Tabs.Panel
            value="activity"
            style={PANEL_STYLE}
            pl={{ base: 0, sm: "lg" }}
          >
            <Stack gap="md">
              <SectionTitle>Noticing</SectionTitle>
              <Row
                label="Notice when a command finishes"
                help="Veld registers two hooks in the shell it opens, which print an invisible marker when a command starts and when it ends. A command that finishes in a terminal you are not looking at then marks its worktree in the rail — and its pane's tab, so you can tell which one. A shell sitting at a prompt never counts, and neither does a watcher that has not ended, so `pnpm dev` stays silent. Nothing of yours is edited: the hooks live in a file Veld owns and rewrites on every start. zsh, and bash 4.4 or newer — macOS's own /bin/bash is 3.2 and cannot carry it. Takes effect for new terminals; a running shell keeps the environment it started with."
              >
                <Checkbox
                  size="xs"
                  checked={shellIntegration}
                  disabled={locked}
                  onChange={(e) =>
                    set({ "terminal.shellIntegration": e.currentTarget.checked })
                  }
                />
              </Row>
              <Row
                label="Notice when a coding agent is waiting for you"
                help="Works with Claude Code, Codex CLI and Pi. None of the three's output says whether it is thinking or waiting — measured against Claude Code, which emits a title sequence, hyperlinks and a progress report, and nothing about its state. So Veld puts a wrapper on the terminal's PATH that hands the real binary an extra, throwaway hook configuration: a settings file for Claude, a `-c notify=[...]` override for Codex, an extension module loaded with `-e` for Pi. Your own ~/.claude/settings.json is merged into, not replaced; ~/.codex/config.toml is never touched, but a notify you configured there does not fire in a Veld terminal — Codex's -c overrides that key rather than merging it; ~/.pi/agent/settings.json and ~/.pi/agent/extensions/ are never touched either, and Pi's own -e is documented repeatable, so nothing of yours is replaced. Codex tells Veld less than Claude on purpose: it has a richer hooks system too, but using it means an interactive trust prompt or a flag that waives trust for every hook, not just this one, so Veld uses the older notify hook instead, which only fires at the end of a turn — a Codex pane goes straight from launched to finished with no waiting signal in between, and its spinner (Show what is working) stays blank the whole time it is genuinely working. Pi has no waiting signal for a different reason — it has no permission prompts or plan mode at all — but it does report the start of each turn, so its spinner is accurate the way Claude's is. Anything that is not a plain interactive launch — `claude mcp`, `claude -p …`, `codex exec …`, `pi install …`, `pi -p …`, or a settings/config/extension flag of your own — is passed through untouched (`codex resume`/`fork` count as interactive). Takes effect for new terminals."
              >
                <Checkbox
                  size="xs"
                  checked={agentIntegration}
                  disabled={locked}
                  onChange={(e) =>
                    set({ "terminal.agentIntegration": e.currentTarget.checked })
                  }
                />
              </Row>
              <Row
                label="Show what is working"
                help="A spinner on any worktree with a command running in it. Off by default because the signal is uneven: for a shell command it is exact — a start marker with no end marker yet genuinely means 'running here'. A Claude or Pi session reports it per turn, from the prompt you sent, so a turn that starts any other way — an agent picking work back up after a background command it left running — spins down early; Codex reports no start of turn at all, and an agent Veld has no integration for reports nothing. Useful if you mainly want to know whether a build is still going. It is the quietest thing the rail shows either way: it loses to every unseen event, so a worktree with an agent waiting for you still reads as waiting."
              >
                <Checkbox
                  size="xs"
                  checked={activity.showWorking}
                  disabled={locked}
                  onChange={(e) =>
                    set({ "activity.showWorking": e.currentTarget.checked })
                  }
                />
              </Row>

              <SectionTitle>Notifying</SectionTitle>
              <Text size="xs" c="dimmed">
                System notifications, and only while Veld is not the focused window —
                nothing interrupts you about a pane you could be looking at. The rail
                still marks everything either way.
              </Text>
              {/* A table and not one switch: "a command finished" and "a coding agent
                  is waiting for you" are different enough events that a single answer
                  for both is wrong in one direction or the other. Four rows, four keys —
                  each validated by the same boolean path as every other switch here. */}
              {NOTIFY_ROWS.map((row) => (
                <Row key={row.key} label={row.label} help={row.help}>
                  <Checkbox
                    size="xs"
                    checked={activity.notify[row.key] ?? false}
                    disabled={locked}
                    onChange={(e) => set({ [row.key]: e.currentTarget.checked })}
                  />
                </Row>
              ))}
            </Stack>
          </Tabs.Panel>

          <Tabs.Panel
            value="links"
            style={PANEL_STYLE}
            pl={{ base: 0, sm: "lg" }}
          >
            <Stack gap="md">
              <Row
                label="Open links from the terminal in Veld"
                help="A URL you click in the terminal output, and a URL a program in it opens (Veld points $BROWSER at itself, which is what Claude Code, gh, git, vite and next all use), become a browser pane beside that terminal. Off sends both to your system browser and puts nothing in the shell at all. Clicking a link responds immediately; the rest takes effect for new terminals, since a running shell keeps the environment it started with. Hold ⌘/Ctrl while clicking a link to go to your browser just once."
              >
                <Checkbox
                  size="xs"
                  checked={openInApp}
                  disabled={locked}
                  onChange={(e) =>
                    set({ "terminal.openUrlsInApp": e.currentTarget.checked })
                  }
                />
              </Row>
              <Row
                label="Also catch programs that call open / xdg-open"
                help="Most tools read $BROWSER, but some call the system opener directly — including an agent's shell tool (Bash(open “https://…”)). For those, Veld puts a small shim directory on the PATH of each terminal. It needs the last word after your shell's startup files, so Veld points ZDOTDIR at a directory of its own holding one .zshenv: that file hands ZDOTDIR straight back, sources your real .zshenv, and registers a hook. Your .zprofile, .zshrc and .zlogin are read normally, in order, and nothing of yours is edited. In bash it uses the equivalent seam — posix mode’s $ENV, the only startup file an interactive --posix bash reads — replaying your own startup itself; that is probed per binary, because macOS ships bash 3.2 as /bin/bash and 3.2 ignores $ENV. Other shells keep $BROWSER and can add $VELD_SHIM_DIR to PATH by hand. The Shell row above reports whether it actually worked. Takes effect for new terminals."
              >
                <Checkbox
                  size="xs"
                  checked={intercept}
                  disabled={locked || !openInApp}
                  onChange={(e) =>
                    set({ "terminal.interceptSystemOpen": e.currentTarget.checked })
                  }
                />
              </Row>
              {/* A full-width field rather than a `Row`, whose label and control sit
                  side by side: a list needs the width, and its explanation is longer
                  than a row's trailing help line. */}
              <Stack gap={2}>
                <Text size="sm">Always open these in the system browser</Text>
                <Text size="xs" c="dimmed">
                  One origin per line — <code>https://accounts.google.com</code>,{" "}
                  <code>https://*.okta.com</code>, <code>http://localhost:*</code> —
                  for the sign-ins that need the browser you are already logged into.
                  A pane has its own cookie jar, so an SSO flow in one starts from
                  scratch. A project can add to this list without touching your
                  settings: <code>ide.externalOrigins</code> in its veld.json.
                </Text>
              </Stack>
              <Textarea
                size="xs"
                autosize
                minRows={2}
                maxRows={8}
                spellCheck={false}
                placeholder="https://accounts.google.com"
                value={exempt}
                disabled={locked || !openInApp}
                onChange={(e) => setExempt(e.currentTarget.value)}
                onBlur={() => {
                  const lines = exempt
                    .split("\n")
                    .map((line) => line.trim())
                    .filter((line) => line !== "");
                  // Nothing to say if the list is unchanged — including the common case
                  // of opening the dialog, clicking through the box and leaving.
                  if (lines.join("\n") === exemptValue.join("\n")) return;
                  set({ "browser.externalOrigins": lines });
                }}
              />
            </Stack>
          </Tabs.Panel>

          <Tabs.Panel
            value="browser"
            style={PANEL_STYLE}
            pl={{ base: 0, sm: "lg" }}
          >
            <Stack gap="md">
              {/* Both on by default. They are shortcuts into the device menu, which
                  keeps every one of these controls — so turning a switch off costs
                  reach, never capability. Global, like every other row here: this is a
                  standing choice about whether you want the shortcut, not an answer to
                  one narrow pane, since pane width changes on every split. These say
                  whether the *button* is shown; what each pane is emulating lives in
                  that pane's layout. */}
              <Row
                label="Responsive switch in the pane toolbar"
                help="One click into the resizable viewport, whose edges you drag to find where a layout breaks. The switch's own off state is no emulation at all — it does not go back to a device you picked earlier. Unchecking this hides the button; it changes nothing a pane is currently emulating."
              >
                <Checkbox
                  size="xs"
                  checked={quick.responsive}
                  disabled={locked}
                  onChange={(e) =>
                    set({
                      "browser.quickSwitch.responsive": e.currentTarget.checked,
                    })
                  }
                />
              </Row>
              <Row
                label="Colour-scheme switch in the pane toolbar"
                help="Cycles the page's prefers-color-scheme through System, Dark and Light — the page in the pane, not Veld itself. System is the absence of an override rather than a third value. Needs Veld Desktop; in a browser tab the button is shown inert and its tooltip says why."
              >
                <Checkbox
                  size="xs"
                  checked={quick.colorScheme}
                  disabled={locked}
                  onChange={(e) =>
                    set({
                      "browser.quickSwitch.colorScheme": e.currentTarget.checked,
                    })
                  }
                />
              </Row>
              {/* Full width rather than a `Row`: a URL needs the room, and the
                  explanation carries two things a trailing help line cannot — what
                  %s means, and that empty is a supported answer. */}
              <Stack gap={2}>
                <Text size="sm">Search from the address bar</Text>
                <Text size="xs" c="dimmed">
                  Where a browser pane sends words that are not an address, so you
                  can look something up in the pane you are working in.{" "}
                  <code>%s</code> is where the words go — the same spelling a
                  browser's own custom-engine field uses, so a URL copied from one
                  works here. Leave it empty to turn search off; the address bar then
                  only accepts addresses.
                </Text>
              </Stack>
              <TextInput
                size="xs"
                spellCheck={false}
                placeholder="https://www.google.com/search?q=%s"
                value={search}
                disabled={locked}
                onChange={(e) => setSearch(e.currentTarget.value)}
                error={searchBroken ? "Veld cannot build a search URL from this" : undefined}
                onBlur={() => {
                  const next = search.trim();
                  // Nothing to say if it is unchanged — including opening the dialog,
                  // clicking through the field and leaving.
                  if (next === searchValue) return;
                  // **Validated here with the parser that will actually use it.** The
                  // daemon has its own rules, but they are hand-written and review found
                  // a hole in them three rounds running; this runs `searchTarget` — the
                  // same function a pane calls, so the same `new URL()` — against a probe
                  // query, which makes "would this template ever work" a question answered
                  // by the thing that has to answer it. Without it, a template that parses
                  // nowhere is stored happily and then fails on the user's next query,
                  // blaming the query.
                  if (next !== "" && searchTarget(next, "veld") === null) return;
                  set({ "browser.searchUrl": next });
                }}
              />
            </Stack>
          </Tabs.Panel>
        </Tabs>

        <Text size="xs" c="dimmed">
          Settings are stored by the veld daemon, so they are shared between Veld
          Desktop and a browser tab — and between every window. Changes save as you
          make them; there is no Save button.
        </Text>
      </Stack>
    </Modal>
  );
}

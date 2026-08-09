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
  Checkbox,
  Group,
  NativeSelect,
  NumberInput,
  Stack,
  Tabs,
  Text,
  TextInput,
  Textarea,
} from "@mantine/core";
import {
  IconAdjustments,
  IconAppWindow,
  IconLink,
  IconTerminal2,
} from "@tabler/icons-react";

import type { SettingsDoc } from "../api";
import { Modal } from "./dialogs";
import {
  availableFonts,
  matchFont,
} from "../shared/terminalFonts";
import {
  detachGraceMinutes,
  externalOrigins,
  hideDisabledActions,
  logsTimeZone,
  terminalInterceptSystemOpen,
  runHistoryDays,
  terminalOpenUrlsInApp,
  trashRetentionDays,
  markerStyle,
  quickSwitchPrefs,
  terminalPrefs,
  type LogTimeZone,
  type MarkerStyle,
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

type GroupId = "general" | "terminal" | "links" | "browser";

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
 */
const GROUPS: { id: GroupId; label: string; icon: ReactNode }[] = [
  { id: "general", label: "General", icon: <IconAdjustments size={15} /> },
  { id: "terminal", label: "Terminal", icon: <IconTerminal2 size={15} /> },
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
  const logsTz = logsTimeZone(settings ?? {});
  const hideDisabled = hideDisabledActions(settings ?? {});

  // Not persisted, and deliberately so: the dialog is remounted on every open, so
  // it always opens on General rather than wherever a previous visit ended up.
  const [group, setGroup] = useState<GroupId>("general");

  // Number inputs are held locally while being typed and committed on blur —
  // see the file header. Re-seeded whenever the daemon's value changes so a
  // clamp lands in the box rather than leaving the rejected number on screen.
  const [fontSize, setFontSize] = useState<number | string>(term.fontSize);
  const [scrollback, setScrollback] = useState<number | string>(term.scrollback);
  const graceValue = detachGraceMinutes(settings ?? {});
  const [grace, setGrace] = useState<number | string>(graceValue);
  const retentionValue = trashRetentionDays(settings ?? {});
  const [retention, setRetention] = useState<number | string>(retentionValue);
  const historyValue = runHistoryDays(settings ?? {});
  const [history, setHistory] = useState<number | string>(historyValue);
  const [fontFamily, setFontFamily] = useState(term.fontFamily);
  // One origin per line, which is what an exempt list reads as. Held locally and
  // committed on blur like every other text field here — the daemon refuses the
  // whole list if one entry is not an origin, and its error lands in `props.error`.
  const exemptValue = externalOrigins(settings ?? {});
  const [exempt, setExempt] = useState(exemptValue.join("\n"));
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
    setFontFamily(term.fontFamily);
    setGrace(graceValue);
    setRetention(retentionValue);
    setHistory(historyValue);
    setExempt(exemptValue.join("\n"));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settings]);

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
                help="Most tools read $BROWSER, but some call the system opener directly — including an agent's shell tool (Bash(open “https://…”)). For those, Veld puts a small shim directory on the PATH of each terminal. It needs the last word after your shell's startup files, so Veld points ZDOTDIR at a directory of its own holding one .zshenv: that file hands ZDOTDIR straight back, sources your real .zshenv, and registers a hook. Your .zprofile, .zshrc and .zlogin are read normally, in order, and nothing of yours is edited. zsh only; other shells keep $BROWSER and can add $VELD_SHIM_DIR to PATH by hand. Takes effect for new terminals."
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

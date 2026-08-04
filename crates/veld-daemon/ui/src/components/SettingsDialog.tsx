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
 */

import { useEffect, useMemo, useState } from "react";
import {
  Checkbox,
  Group,
  NativeSelect,
  NumberInput,
  Stack,
  Text,
  TextInput,
} from "@mantine/core";

import type { SettingsDoc } from "../api";
import { Modal } from "./dialogs";
import {
  availableFonts,
  matchFont,
} from "../shared/terminalFonts";
import {
  detachGraceMinutes,
  markerStyle,
  quickSwitchPrefs,
  terminalPrefs,
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

function SectionTitle(props: { children: React.ReactNode }) {
  return (
    <Text size="xs" fw={600} c="dimmed" tt="uppercase">
      {props.children}
    </Text>
  );
}

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

  // Number inputs are held locally while being typed and committed on blur —
  // see the file header. Re-seeded whenever the daemon's value changes so a
  // clamp lands in the box rather than leaving the rejected number on screen.
  const [fontSize, setFontSize] = useState<number | string>(term.fontSize);
  const [scrollback, setScrollback] = useState<number | string>(term.scrollback);
  const graceValue = detachGraceMinutes(settings ?? {});
  const [grace, setGrace] = useState<number | string>(graceValue);
  const [fontFamily, setFontFamily] = useState(term.fontFamily);
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
    <Modal title="Settings" onClose={props.onClose}>
      <Stack gap="lg">
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

        <Stack gap="xs">
          <SectionTitle>Appearance</SectionTitle>
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
                set({ "worktree.markerStyle": e.currentTarget.value as MarkerStyle })
              }
            />
          </Row>
        </Stack>

        <Stack gap="xs">
          <SectionTitle>Browser panes</SectionTitle>
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
                set({ "browser.quickSwitch.responsive": e.currentTarget.checked })
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
                set({ "browser.quickSwitch.colorScheme": e.currentTarget.checked })
              }
            />
          </Row>
        </Stack>

        <Stack gap="xs">
          <SectionTitle>Terminal</SectionTitle>
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
                commitNumber("terminal.fontSize", fontSize, term.fontSize, setFontSize)
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

        <Text size="xs" c="dimmed">
          Settings are stored by the veld daemon, so they are shared between Veld
          Desktop and a browser tab — and between every window. Changes save as you
          make them; there is no Save button.
        </Text>
      </Stack>
    </Modal>
  );
}

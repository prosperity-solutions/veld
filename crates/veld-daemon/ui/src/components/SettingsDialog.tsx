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

import { useEffect, useState } from "react";
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
  markerStyle,
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
  const term = terminalPrefs(settings ?? {});
  const marker = markerStyle(settings ?? {});

  // Number inputs are held locally while being typed and committed on blur —
  // see the file header. Re-seeded whenever the daemon's value changes so a
  // clamp lands in the box rather than leaving the rejected number on screen.
  const [fontSize, setFontSize] = useState<number | string>(term.fontSize);
  const [scrollback, setScrollback] = useState<number | string>(term.scrollback);
  const [grace, setGrace] = useState<number | string>(
    typeof settings?.["terminal.detachGraceMinutes"] === "number"
      ? (settings["terminal.detachGraceMinutes"] as number)
      : 30,
  );
  const [fontFamily, setFontFamily] = useState(term.fontFamily);

  useEffect(() => setFontSize(term.fontSize), [term.fontSize]);
  useEffect(() => setScrollback(term.scrollback), [term.scrollback]);
  useEffect(() => setFontFamily(term.fontFamily), [term.fontFamily]);
  useEffect(() => {
    const v = settings?.["terminal.detachGraceMinutes"];
    if (typeof v === "number") setGrace(v);
  }, [settings]);

  const set = (patch: SettingsDoc) => {
    // Fire and forget: the hook holds the error, and awaiting here would freeze
    // the control that was just clicked for a loopback round trip.
    void props.onSave(patch).catch(() => {});
  };

  const commitNumber = (key: string, value: number | string, fallback: number) => {
    // An empty or half-typed box must not be sent as NaN — the daemon would
    // reject it and the user would see an error for having selected the text.
    const n = typeof value === "number" ? value : Number(value);
    if (!Number.isFinite(n)) {
      setFontSize(term.fontSize);
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
              disabled={!settings}
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
          <SectionTitle>Terminal</SectionTitle>
          <Row label="Font size">
            <NumberInput
              size="xs"
              w={100}
              min={6}
              max={72}
              value={fontSize}
              disabled={!settings}
              onChange={setFontSize}
              onBlur={() =>
                commitNumber("terminal.fontSize", fontSize, term.fontSize)
              }
            />
          </Row>
          <Row
            label="Font family"
            help="A CSS font-family list. Falls back to the bundled JetBrains Mono."
          >
            <TextInput
              size="xs"
              w={240}
              value={fontFamily}
              disabled={!settings}
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
          <Row label="Cursor">
            <NativeSelect
              size="xs"
              w={140}
              value={term.cursorStyle}
              disabled={!settings}
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
              disabled={!settings}
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
              max={500000}
              value={scrollback}
              disabled={!settings}
              onChange={setScrollback}
              onBlur={() =>
                commitNumber("terminal.scrollback", scrollback, term.scrollback)
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
              disabled={!settings}
              onChange={(e) =>
                set({ "terminal.shiftEnterNewline": e.currentTarget.checked })
              }
            />
          </Row>
          <Row label="Selecting text copies it">
            <Checkbox
              size="xs"
              checked={term.copyOnSelect}
              disabled={!settings}
              onChange={(e) =>
                set({ "terminal.copyOnSelect": e.currentTarget.checked })
              }
            />
          </Row>
          <Row label="Middle click pastes">
            <Checkbox
              size="xs"
              checked={term.middleClickPaste}
              disabled={!settings}
              onChange={(e) =>
                set({ "terminal.middleClickPaste": e.currentTarget.checked })
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
              disabled={!settings}
              onChange={setGrace}
              onBlur={() => {
                const n = typeof grace === "number" ? grace : Number(grace);
                if (!Number.isFinite(n)) return;
                set({ "terminal.detachGraceMinutes": n });
              }}
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

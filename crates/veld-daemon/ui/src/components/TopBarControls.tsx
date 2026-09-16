/**
 * The controls at the end of the top bar: Next, search, keep-awake, focus mode.
 *
 * **One component, mounted by every mode**, which is the whole point. These four
 * are not *about* whatever the bar is showing — they are about this machine and
 * this person: where you are needed, what you are looking for, whether the laptop
 * may sleep, and whether anything is allowed to interrupt you. Three of them were
 * IDE-only, and
 * keep-awake is what made that untenable: once a live share can hold the machine
 * awake on its own, Runs mode was a screen where sharing happens, the machine
 * gets held awake, and there is no control anywhere on it saying so or switching
 * it off. A hold you cannot see is bad; a hold you cannot see *or reach* is the
 * shape of a support ticket.
 *
 * Two of the three own no state — focus mode is a settings write, and search is
 * the caller's handler — while keep-awake owns the machine's, because it is the
 * machine's and not a tab's. That asymmetry is why this takes a settings document
 * plus a writer rather than a bag of booleans: the two settings-backed controls
 * read the same document the rest of the app already holds, and a second
 * `useSettings` here would be another fetch and another focus listener for it.
 *
 * The order is deliberate: Next first, then search, then keep-awake, then focus
 * mode, then whatever the caller puts after (the ⋯ menu, always last, because
 * everything Veld-level lives inside it). Next leads because it is the only one
 * of the four that is *ever urgent* — the other three are reached when you decide
 * to reach for them, and this one is reached because something is waiting — and
 * because leading the cluster puts it immediately right of the flexible gap,
 * which is the seam between what this worktree is doing and what you have to do.
 * It is also the only one that is text rather than a glyph, on the maintainer's
 * call: every icon in this bar acts on the thing already on screen, and a control
 * that throws the whole window somewhere else should not look like one of them.
 */
import { ActionIcon, Button, Tooltip } from "@mantine/core";
import { IconBell, IconBellOff, IconSearch } from "@tabler/icons-react";

import type { SettingsDoc } from "../api";
import type { UnseenKind } from "../inbox/inbox";
import { focusPrefs, hideDisabledActions } from "../shared/settings";
import { KeepAwakeButton } from "./KeepAwakeButton";

/** Where the Next button would take you, ready to render. */
export interface NextDestination {
  /** Colours the label, from the same three CSS vars the rail glyph uses. */
  kind: UnseenKind;
  /** The whole tooltip line, built by the app — it owns the projects and panes. */
  tooltip: string;
}

export function TopBarControls(props: {
  settings: SettingsDoc;
  /** Writes a settings patch. The app's own `saveSettings`. */
  onSetting: (patch: SettingsDoc) => void;
  /**
   * Opens the command palette.
   *
   * In Runs mode the app hands a handler that switches to the IDE first: what
   * search finds — worktrees, panes, run actions — only exists there, so a
   * palette opened over Runs would list things whose selection changed nothing
   * visible. Switching first means every item's handler behaves exactly as it
   * always has, with no per-item special case.
   */
  onSearch: () => void;
  /**
   * Where Next would take you, or `null` when nothing anywhere has anything
   * unread. Computed across *every* project, which is the point of the control.
   */
  next: NextDestination | null;
  /** Go there — select the worktree in whatever project it is in, and focus its
   *  pane. The app owns it, because only it can do either. */
  onNext: () => void;
}) {
  const focus = focusPrefs(props.settings);

  return (
    <>
      {props.next === null ? (
        // Nothing waiting. Hidden under `ui.hideDisabledActions` — which is **on by
        // default**, so the quiet state is normally no button at all: the bar is the
        // densest row in the app and a permanently dead word in it earns less than
        // the pixels it costs. With the setting off it is greyed with the reason
        // instead, the same call `KeepAwakeButton` makes, because a control that
        // vanishes teaches nobody it exists.
        hideDisabledActions(props.settings) ? null : (
          <Tooltip label="Nothing is waiting for you">
            {/* The `<span>` is load-bearing: a disabled Mantine control has
                `pointer-events: none`, so the tooltip explaining why would never
                open — the #205 trap. */}
            <span style={{ display: "inline-flex" }}>
              <Button size="xs" variant="default" className="topbar-next" disabled>
                Next
              </Button>
            </span>
          </Tooltip>
        )
      ) : (
        <Tooltip label={props.next.tooltip}>
          <Button
            size="xs"
            variant="default"
            className={`topbar-next ${props.next.kind}`}
            // The visible word is "Next", which on its own answers "next what?"
            // with nothing. The tooltip's sentence is the accessible name, and it
            // starts with that word so the visible label is still contained in it
            // (WCAG 2.5.3) — a voice-control user saying "click Next" still lands.
            aria-label={`Next: ${props.next.tooltip}`}
            onClick={props.onNext}
          >
            Next
          </Button>
        </Tooltip>
      )}
      <Tooltip label="Search (⌘K)">
        <ActionIcon size="md" variant="default" onClick={props.onSearch}>
          <IconSearch size={14} />
        </ActionIcon>
      </Tooltip>
      <KeepAwakeButton
        hideDisabled={hideDisabledActions(props.settings)}
        settings={props.settings}
        onSetting={props.onSetting}
      />
      <Tooltip
        label={
          focus.enabled ? "Focus mode: on — click to turn off" : "Focus mode: off — click to turn on"
        }
      >
        <ActionIcon
          size="md"
          variant={focus.enabled ? "filled" : "default"}
          color={focus.enabled ? "teal" : undefined}
          aria-label="Focus mode"
          aria-pressed={focus.enabled}
          onClick={() => props.onSetting({ "focus.enabled": !focus.enabled })}
        >
          {focus.enabled ? <IconBellOff size={14} /> : <IconBell size={14} />}
        </ActionIcon>
      </Tooltip>
    </>
  );
}

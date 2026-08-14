/**
 * The controls at the end of the top bar: search, keep-awake, focus mode.
 *
 * **One component, mounted by every mode**, which is the whole point. These three
 * are not *about* whatever the bar is showing — they are about this machine and
 * this person: what you are looking for, whether the laptop may sleep, and
 * whether anything is allowed to interrupt you. All three were IDE-only, and
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
 * The order is deliberate and matches what it replaced: search, then keep-awake,
 * then focus mode, then whatever the caller puts after (the ⋯ menu, always last,
 * because everything Veld-level lives inside it).
 */
import { ActionIcon, Tooltip } from "@mantine/core";
import { IconBell, IconBellOff, IconSearch } from "@tabler/icons-react";

import type { SettingsDoc } from "../api";
import { focusPrefs, hideDisabledActions } from "../shared/settings";
import { KeepAwakeButton } from "./KeepAwakeButton";

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
}) {
  const focus = focusPrefs(props.settings);

  return (
    <>
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

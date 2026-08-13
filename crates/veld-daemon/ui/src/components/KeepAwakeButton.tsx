/**
 * The top bar's keep-awake switch, beside the search icon.
 *
 * A run that outlives your attention — an overnight agent, a long build, a
 * watcher you walked away from — dies when the laptop suspends. This is the
 * one-click "not for the next four hours", and the state it shows is the
 * *machine's*: every window of every client sees the same answer, because the
 * thing being held awake is the laptop and not a tab.
 *
 * A `Menu` rather than a plain toggle because "how long" is the interesting
 * half of the question. Turning it off is one click from the same menu rather
 * than a second gesture on the icon, so the button has exactly one behaviour
 * whatever state it is in — a click that sometimes toggles and sometimes opens
 * a menu is the kind of control people learn to distrust.
 *
 * Coverage is not uniform, and the menu says so rather than leaving the user to
 * find out by shutting the lid. On Linux one unprivileged inhibitor covers
 * everything. On macOS the unprivileged half stops at mains power, and the
 * battery/lid-closed half needs the privileged helper — so the footer states
 * which of the three cases this machine is in. See `LidNote`.
 */
import { ActionIcon, Menu, Tooltip } from "@mantine/core";
import { IconCoffee, IconCoffeeOff } from "@tabler/icons-react";

import type { CaffeinateState } from "../api";
import { formatRemaining, useCaffeinate } from "../shared/useCaffeinate";

/**
 * The offered limits.
 *
 * Short enough at the bottom that "just this build" is a real answer, and
 * stopping at 8 hours because past that the honest choice is the unlimited one
 * rather than a number somebody guessed at.
 */
const DURATIONS: Array<{ label: string; secs: number }> = [
  { label: "30 minutes", secs: 30 * 60 },
  { label: "1 hour", secs: 60 * 60 },
  { label: "2 hours", secs: 2 * 60 * 60 },
  { label: "4 hours", secs: 4 * 60 * 60 },
  { label: "8 hours", secs: 8 * 60 * 60 },
];

/**
 * What the menu says about a closed lid on battery — the one case the
 * unprivileged hold cannot reach.
 *
 * Three different facts, and they must not be collapsed. *Not capable* is
 * actionable (`veld setup privileged` fixes it) and is the common case. *Capable
 * but not covering* means a lease was asked for and not held — a real fault,
 * worth saying so the user does not shut the lid on a promise. *Covering* earns
 * one line of confirmation, because "will this survive me closing the laptop"
 * is the actual question somebody opens this menu with.
 *
 * Nothing at all on Linux, where the inhibitor already covers battery, and
 * nothing on macOS while idle-but-capable — there is no caveat to give somebody
 * who has not switched it on yet. Deliberately not a promise that turning it on
 * *will* cover the lid: the `active && !covers_battery` branch below exists
 * precisely for the take that does not land.
 */
function LidNote(props: { state: CaffeinateState | null; active: boolean }) {
  const { state, active } = props;
  if (state?.platform !== "macos") return null;

  const note = !state.battery_capable
    ? // No backticks: a Menu.Label renders text, not markdown, so they would
      // show up as literal characters.
      "A closed lid on battery still sleeps. Run veld setup privileged to cover that too."
    : active && !state.covers_battery
      ? "A closed lid on battery still sleeps — the privileged helper didn’t take the lease."
      : active
        ? "Covers a closed lid, on battery too."
        : null;
  if (!note) return null;

  return (
    <>
      <Menu.Divider />
      {/* `whiteSpace: normal` because a Menu.Label is a single line by default
          and this is a sentence; the dropdown is fixed-width, so it wraps. */}
      <Menu.Label style={{ whiteSpace: "normal" }}>{note}</Menu.Label>
    </>
  );
}

export function KeepAwakeButton(props: { hideDisabled: boolean }) {
  const { state, start, stop } = useCaffeinate();

  // Optimistic until the first answer lands: the supported case is the common
  // one, and a control that appears a beat late reads as jank. A click before
  // then still tells the truth — the daemon answers 501 with the reason, which
  // surfaces as a toast.
  const supported = state?.supported ?? true;
  const active = state?.active ?? false;

  if (!supported) {
    // Not a failure — this machine has no way to ask (a Linux box without
    // systemd). Hidden under `ui.hideDisabledActions`, otherwise greyed with the
    // reason, because a control that vanishes teaches nobody it exists.
    if (props.hideDisabled) return null;
    return (
      <Tooltip label={state?.unsupported_reason ?? "Not available on this machine"}>
        {/* The `<span>` is load-bearing: a disabled Mantine control has
            `pointer-events: none`, so the tooltip explaining *why* it is
            disabled would never open — the #205 trap. */}
        <span style={{ display: "inline-flex" }}>
          <ActionIcon size="md" variant="default" aria-label="Keep this machine awake" disabled>
            <IconCoffeeOff size={14} />
          </ActionIcon>
        </span>
      </Tooltip>
    );
  }

  const remaining = state?.remaining_secs;
  // Not named `window` — that shadows the global, and this file is one
  // `window.addEventListener` away from a confusing bug.
  const left =
    typeof remaining === "number" ? `${formatRemaining(remaining)} left` : "no time limit";
  // Only macOS has a gap to qualify: on Linux the unprivileged inhibitor already
  // covers a closed lid on battery, so a "mains power only" caveat there would
  // be advice about a limitation that does not exist.
  const mainsOnly = state?.platform === "macos" && !state.covers_battery;
  const tooltip = !active
    ? "This machine may sleep — click to keep it awake"
    : mainsOnly
      ? `Keeping this machine awake on mains power — ${left}`
      : `Keeping this machine awake — ${left}`;

  return (
    <Menu position="bottom-end" width={260}>
      <Menu.Target>
        <Tooltip label={tooltip}>
          <ActionIcon
            size="md"
            variant={active ? "filled" : "default"}
            color={active ? "teal" : undefined}
            aria-label="Keep this machine awake"
            aria-pressed={active}
          >
            {active ? <IconCoffee size={14} /> : <IconCoffeeOff size={14} />}
          </ActionIcon>
        </Tooltip>
      </Menu.Target>
      <Menu.Dropdown>
        {active ? (
          <>
            <Menu.Label>{`On — ${left}`}</Menu.Label>
            <Menu.Item
              color="red"
              leftSection={<IconCoffeeOff size={14} />}
              onClick={() => void stop()}
            >
              Let this machine sleep
            </Menu.Item>
            <Menu.Divider />
            <Menu.Label>Change to</Menu.Label>
          </>
        ) : (
          <Menu.Label>Keep this machine awake for</Menu.Label>
        )}
        {DURATIONS.map((d) => (
          <Menu.Item key={d.secs} onClick={() => void start(d.secs)}>
            {d.label}
          </Menu.Item>
        ))}
        <Menu.Item onClick={() => void start(null)}>Until I turn it off</Menu.Item>
        <LidNote state={state} active={active} />
      </Menu.Dropdown>
    </Menu>
  );
}

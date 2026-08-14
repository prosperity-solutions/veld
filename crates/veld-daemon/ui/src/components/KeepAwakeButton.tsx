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
 *
 * **The machine can also be held awake by something nobody clicked**: a live
 * share arms it (see the daemon's `caffeinate` module). That is the one state
 * this control has to work hardest at, because a lit cup the user did not light
 * is the whole cost of that default. So the reason is named in the tooltip, in
 * the menu's first line, and — when the automatic allowance has been spent while
 * a share is still up — in a line that says so rather than leaving somebody to
 * notice the cup went out.
 */
import { ActionIcon, Menu, Switch, Tooltip } from "@mantine/core";
import { IconCoffee, IconCoffeeOff } from "@tabler/icons-react";

import type { CaffeinateState, SettingsDoc } from "../api";
import { autoWhileSharingKey } from "../shared/settings";
import { attributesToShares, formatRemaining, useCaffeinate } from "../shared/useCaffeinate";

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
 * What the menu says about a closed lid — the coverage that is not uniform.
 *
 * Four different facts, and they must not be collapsed. The daemon tells them
 * apart in `lid_gap` precisely so this note cannot report a fault for a lease
 * that was never asked for:
 *
 * - `automatic` — an automatic hold on battery deliberately holds idle sleep
 *   only. Not a fault and not a missing install; one click on a duration buys
 *   the rest, so the note says that instead of naming a command.
 * - `setting` — *Settings → Keep awake* says not to. Points at the setting.
 * - `no_helper` — veld asked and could not get it. Two sentences, split on
 *   `battery_capable`: no helper installed names the command that installs one,
 *   and a helper that refused the lease is a fault with nothing to suggest. This
 *   is the only note that should ever mention `veld setup privileged`.
 * - none, while active — one line of confirmation, because "will this survive me
 *   closing the laptop" is the actual question somebody opens this menu with.
 *
 * Nothing at all while idle: there is no caveat to give somebody who has not
 * switched it on yet, and no promise to make about a take that has not landed.
 */
function LidNote(props: { state: CaffeinateState | null }) {
  const { state } = props;
  if (!state?.active) return null;

  // No backticks anywhere below: a Menu.Label renders text, not markdown, so
  // they would show up as literal characters.
  // A `lid_gap` this bundle does not know about renders nothing rather than
  // falling through to the "covers a closed lid" reassurance below it. The Rust
  // side's match is exhaustive and the compiler enforces it; nothing enforces
  // the pair, so an older bundle meeting a newer daemon has to fail quiet rather
  // than fail confident.
  const known = new Set(["no_helper", "setting", "automatic"]);
  if (state.lid_gap && !known.has(state.lid_gap)) return null;

  const note =
    state.lid_gap === "no_helper"
      ? // Two cases behind one `lid_gap`, and only the daemon's `battery_capable`
        // tells them apart: *no helper installed* is the common one and has a
        // command that fixes it, while *installed but the lease was refused* is a
        // fault with no user action. Collapsing them dropped the instruction
        // entirely — the one actionable sentence this menu ever had.
        !state.battery_capable
        ? "A closed lid on battery still sleeps. Run veld setup privileged to cover that too."
        : "A closed lid on battery still sleeps — the privileged helper didn’t take the lease."
      : state.lid_gap === "setting"
        ? "A closed lid on battery still sleeps — that’s off in Settings → Keep awake."
        : state.lid_gap === "automatic"
          ? "A closed lid still sleeps this machine. Pick a length above to cover that too."
          : state.covers_lid
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

export function KeepAwakeButton(props: {
  hideDisabled: boolean;
  settings: SettingsDoc;
  /** Writes a settings patch. The app's own `saveSettings`. */
  onSetting: (patch: SettingsDoc) => void;
}) {
  const { state, start, stop } = useCaffeinate();

  // Which of the two automatic switches the menu offers is decided by the power
  // source the *daemon* reports, not by anything this client knows — which is
  // why it is read here rather than passed in. Showing both switches in a
  // dropdown would be the settings dialog's job done badly; showing the one that
  // is in force answers the question somebody opened the menu with.
  const autoKey = autoWhileSharingKey(state?.power_source ?? "mains");
  const autoWhileSharing = props.settings[autoKey] !== false;

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
  // Whether a human asked for any of this. `"sharing"` alone means nobody did,
  // which changes what every string below should say.
  const automatic = state?.reason === "sharing";
  // The countdown above is the shares' own life ending, not the "For at most"
  // cap somebody configured. Not the default case — the default pair puts the cap
  // in front — but it is what a `--ttl`, a project override or a raised cap
  // produces, and there the cup would otherwise claim a number it did not
  // compute.
  //
  // The shared predicate rather than the condition written out again: the other
  // consumer wrote it out and got the `"both"` case wrong. See
  // `attributesToShares`.
  const boundByShare = attributesToShares(state);
  // Sharing is live and the automatic hold has had its allowance for this share.
  // Worth its own line: the cup going out mid-share is otherwise something the
  // user has to notice rather than be told.
  const spent = (state?.sharing_spent ?? false) && !active;
  // Not the same state as `spent`, and it must not borrow its copy: this machine
  // could not start an inhibitor, so "it comes back with the next share" would
  // be a promise nothing is keeping.
  const failed = (state?.hold_failed ?? false) && !active;
  const lidCaveat = active && !state?.covers_lid;

  const tooltip = !active
    ? failed
      ? "This machine may sleep — Veld could not start the keep-awake"
      : spent
        ? "This machine may sleep — its automatic time for this share is used up"
        : "This machine may sleep — click to keep it awake"
    : automatic
      ? lidCaveat
        ? `Keeping this machine awake while you're sharing — ${left}${boundByShare ? " (your sharing ending, not your keep-awake setting)" : ""}. A shut lid still sleeps it.`
        : `Keeping this machine awake while you're sharing — ${left}${boundByShare ? " (your sharing ending, not your keep-awake setting)" : ""}`
      : lidCaveat
        ? `Keeping this machine awake — ${left}. A shut lid still sleeps it.`
        : `Keeping this machine awake — ${left}`;

  return (
    <Menu position="bottom-end" width={272} closeOnItemClick={false}>
      <Menu.Target>
        <Tooltip label={tooltip}>
          <ActionIcon
            size="md"
            variant={active ? "filled" : "default"}
            color={active ? "teal" : undefined}
            aria-label={
              automatic ? "Keeping this machine awake while sharing" : "Keep this machine awake"
            }
            // Only a hold somebody *pressed* is a pressed toggle. Asserting it for
            // an automatic one tells a screen-reader user they did something they
            // did not do.
            aria-pressed={active && !automatic}
          >
            {active ? <IconCoffee size={14} /> : <IconCoffeeOff size={14} />}
          </ActionIcon>
        </Tooltip>
      </Menu.Target>
      <Menu.Dropdown>
        {active ? (
          <>
            <Menu.Label>
              {automatic ? `On while you're sharing — ${left}` : `On — ${left}`}
            </Menu.Label>
            {boundByShare && (
              // Wrapped like `LidNote`'s: a `Menu.Label` is single-line by
              // default and this is a sentence.
              //
              // Says which deadline the number above is, and stops there. It
              // deliberately does NOT point at *Settings → Sharing*: a share's
              // expiry is stamped when it is minted, so nothing in that dialog
              // shortens or extends the hold this label sits under — naming it
              // here would send somebody to a control that cannot move the number
              // they are reading, which is a new version of the same defect.
              // Changing the durations below, or ending the sharing, are the two
              // things that do act on it.
              <Menu.Label style={{ whiteSpace: "normal" }}>
                That's your sharing ending, not this limit.
              </Menu.Label>
            )}
            <Menu.Item
              color="red"
              leftSection={<IconCoffeeOff size={14} />}
              onClick={() => void stop()}
            >
              Let this machine sleep
            </Menu.Item>
            <Menu.Divider />
            <Menu.Label>{automatic ? "Keep it awake myself, for" : "Change to"}</Menu.Label>
          </>
        ) : (
          <>
            {failed ? (
              <Menu.Label style={{ whiteSpace: "normal" }}>
                Veld could not start the keep-awake on this machine. See the daemon log.
              </Menu.Label>
            ) : spent ? (
              <Menu.Label style={{ whiteSpace: "normal" }}>
                Automatic keep-awake is used up for this share. It comes back with the next one.
              </Menu.Label>
            ) : null}
            <Menu.Label>Keep this machine awake for</Menu.Label>
          </>
        )}
        {DURATIONS.map((d) => (
          <Menu.Item key={d.secs} onClick={() => void start(d.secs)}>
            {d.label}
          </Menu.Item>
        ))}
        <Menu.Item onClick={() => void start(null)}>Until I turn it off</Menu.Item>
        <LidNote state={state} />
        <Menu.Divider />
        {/* The setting lives here as well as in Settings, and this is the copy
            that matters more: nobody's route to this feature is the settings
            dialog. It is "why is my machine awake" → the cup → its menu, so the
            switch that stops it has to be reachable at the end of that route.
            `closeOnItemClick={false}` on the Menu is what lets it be flipped
            without the dropdown vanishing under the pointer. */}
        <Menu.Item closeMenuOnClick={false} component="div">
          <Switch
            size="xs"
            checked={autoWhileSharing}
            // Both labels name their power source. Only the battery one did,
            // which left the mains switch reading as the whole feature — so
            // somebody who turned it off on mains, unplugged, and shared got a
            // machine held awake by a switch they believed they had turned off.
            // Naming both is what makes the *other* one discoverable, and
            // Settings is where both are visible at once.
            label={
              state?.power_source === "battery"
                ? "Do this whenever I share, on battery"
                : "Do this whenever I share, on mains power"
            }
            onChange={(e) => props.onSetting({ [autoKey]: e.currentTarget.checked })}
          />
        </Menu.Item>
      </Menu.Dropdown>
    </Menu>
  );
}

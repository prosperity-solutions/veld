/**
 * The shown worktree's outfit profile: an icon in the top bar, beside the project selector.
 *
 * Absent unless the daemon says outfit is `ready` for this checkout — a person
 * without outfit never sees it. The state is read when the worktree changes, when
 * the menu opens (something else may have switched it), and through a switch
 * (which answers with the new state); the 5s poll does not touch it, because
 * every read spawns a process in the daemon.
 *
 * Mounted with `key={path}` so a switch finishing after the user moved on lands
 * in an unmounted component rather than labelling the wrong worktree.
 */
import { ActionIcon, Menu, Text, Tooltip } from "@mantine/core";
import { IconCheck, IconShirt } from "@tabler/icons-react";
import { useState } from "react";
import { api } from "../api";
import { isCurrent, outfitReady, outfitTone, outfitTooltip, profileSwaps } from "../outfit/model";
import { useOutfitState } from "../outfit/useOutfitState";
import { notifyError } from "../shared/notify";

export function OutfitMenu(props: { path: string }) {
  const { state, loading, set, refresh } = useOutfitState(props.path);
  const [busy, setBusy] = useState(false);
  // The first answer still in flight: the same-size icon, resolving, so the slot
  // is not empty. Once answered and not ready, nothing — no outfit, no control.
  if (!outfitReady(state)) {
    if (!loading) return null;
    return (
      <ActionIcon size="md" variant="default" loading aria-label="Outfit profile" />
    );
  }

  const pick = async (profile: string) => {
    // Not skipped for the profile shown as current: that label can be stale (an agent
    // switched since), and outfit answers a no-op `use` with "nothing changed".
    if (busy) return;
    setBusy(true);
    try {
      set(await api.outfitUse(props.path, profile));
    } catch (e) {
      // outfit's own sentence, fix included — the message is already the whole
      // explanation, so it is shown as the daemon sent it.
      notifyError("Could not switch outfit profile", e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Menu position="bottom-start" withinPortal width={300} onOpen={refresh}>
      <Menu.Target>
        <Tooltip label={outfitTooltip(state)}>
          <ActionIcon
            size="md"
            variant="default"
            // Green for a profile other than `base`; neutral for base.
            color={outfitTone(state) === "worn" ? "green" : undefined}
            c={outfitTone(state) === "worn" ? "green" : "dimmed"}
            loading={busy}
            aria-label={outfitTooltip(state)}
          >
            <IconShirt size={14} />
          </ActionIcon>
        </Tooltip>
      </Menu.Target>
      <Menu.Dropdown>
        <Menu.Label>
          Outfit profile
          {state.current && (state.current.pinned ? " (pinned)" : " (following the clone)")}
        </Menu.Label>
        {state.profiles.map((p) => (
          <Menu.Item
            key={p.name}
            onClick={() => void pick(p.name)}
            rightSection={isCurrent(state, p) ? <IconCheck size={14} /> : null}
          >
            <Text size="sm">{p.name}</Text>
            <Text size="xs" c="dimmed">
              {profileSwaps(p.files)}
            </Text>
          </Menu.Item>
        ))}
        <Menu.Divider />
        <Text size="xs" c="dimmed" px="sm" py={6}>
          Agents already running keep the old files until restarted — an agent can
          run <code>veld agent restart</code>.
        </Text>
      </Menu.Dropdown>
    </Menu>
  );
}

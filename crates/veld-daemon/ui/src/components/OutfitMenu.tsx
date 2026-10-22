/**
 * The shown worktree's outfit profile, in the top bar.
 *
 * Absent unless the daemon says outfit is `ready` for this checkout — a person
 * without outfit never sees it. The state is read when the worktree changes and
 * again only through a switch (which answers with the new state); the 5s poll
 * does not touch it, because every read spawns a process in the daemon.
 *
 * Mounted with `key={path}` so a switch finishing after the user moved on lands
 * in an unmounted component rather than labelling the wrong worktree.
 */
import { Button, Menu, Text } from "@mantine/core";
import { IconCheck, IconShirt } from "@tabler/icons-react";
import { useState } from "react";
import { api } from "../api";
import { currentLabel, isCurrent, outfitReady, profileSwaps } from "../outfit/model";
import { useOutfitState } from "../outfit/useOutfitState";
import { notifyError } from "../shared/notify";

export function OutfitMenu(props: { path: string }) {
  const { state, set } = useOutfitState(props.path);
  const [busy, setBusy] = useState(false);
  if (!outfitReady(state)) return null;

  const pick = async (profile: string) => {
    if (busy || state.current?.name === profile) return;
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
    <Menu position="bottom-start" withinPortal width={300}>
      <Menu.Target>
        <Button
          size="xs"
          variant="default"
          loading={busy}
          leftSection={<IconShirt size={14} />}
          aria-label="Outfit profile"
        >
          {currentLabel(state)}
        </Button>
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

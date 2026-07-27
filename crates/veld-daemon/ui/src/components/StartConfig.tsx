import { useState } from "react";
import {
  Button,
  Checkbox,
  Grid,
  Group,
  Modal,
  NativeSelect,
  Radio,
  ScrollArea,
  Stack,
  Text,
} from "@mantine/core";
import { IconChevronDown } from "@tabler/icons-react";
import type { Worktree } from "../api";

/**
 * What ▶ starts: a preset, or an explicit set of `node:variant` selections.
 * A non-TTY `veld start` with neither fails ("No selections provided"), so
 * the UI always resolves to one of the two before starting.
 */
export type StartSelection =
  | { kind: "preset"; name: string }
  | { kind: "nodes"; selections: string[] };

export function defaultStartSelection(w: Worktree): StartSelection | null {
  if (w.presets.length > 0) return { kind: "preset", name: w.presets[0] };
  if (w.nodes.length > 0) {
    return {
      kind: "nodes",
      selections: w.nodes.map(
        (n) => `${n.name}:${n.default_variant ?? n.variants[0]}`,
      ),
    };
  }
  return null;
}

export function startSelectionLabel(sel: StartSelection | null): string {
  if (!sel) return "nothing to start";
  if (sel.kind === "preset") return sel.name;
  const n = sel.selections.length;
  return n === 1 ? sel.selections[0] : `${n} nodes`;
}

/**
 * Start-configuration modal: presets as one-click picks (left panel), custom
 * per-node variant selection (right panel). A modal, not a popover — real
 * configs carry dozens of presets/nodes and need independent scrolling.
 */
export function StartConfig(props: {
  worktree: Worktree;
  value: StartSelection | null;
  onChange: (sel: StartSelection) => void;
}) {
  const [opened, setOpened] = useState(false);
  const w = props.worktree;
  const sel = props.value ?? defaultStartSelection(w);
  const hasPresets = w.presets.length > 0;

  const selectedVariant = (node: string): string | null => {
    if (sel?.kind !== "nodes") return null;
    const hit = sel.selections.find((s) => s.split(":")[0] === node);
    return hit ? (hit.split(":")[1] ?? null) : null;
  };

  const toggleNode = (node: string, variant: string, on: boolean) => {
    const current =
      sel?.kind === "nodes"
        ? sel.selections.filter((s) => s.split(":")[0] !== node)
        : [];
    props.onChange({
      kind: "nodes",
      selections: on ? [...current, `${node}:${variant}`] : current,
    });
  };

  const customPanel = (
    <Stack gap={0} style={{ minWidth: 0 }}>
      <Text size="xs" fw={600} c="dimmed" tt="uppercase" pb={6}>
        Custom selection
      </Text>
      <ScrollArea.Autosize mah={380}>
        <Stack gap={8} pr={8}>
          {w.nodes.length === 0 && (
            <Text size="xs" c="dimmed">
              No startable nodes in this config.
            </Text>
          )}
          {w.nodes.map((n) => {
            const variant = selectedVariant(n.name);
            const fallback = n.default_variant ?? n.variants[0];
            return (
              <Group key={n.name} gap="xs" wrap="nowrap">
                <Checkbox
                  size="xs"
                  label={n.name}
                  checked={variant !== null}
                  onChange={(e) =>
                    toggleNode(n.name, fallback, e.currentTarget.checked)
                  }
                  styles={{
                    label: {
                      fontFamily: "var(--mantine-font-family-monospace)",
                    },
                  }}
                  style={{ flex: 1, minWidth: 0 }}
                />
                {n.variants.length > 1 && (
                  <NativeSelect
                    size="xs"
                    value={variant ?? fallback}
                    disabled={variant === null}
                    onChange={(e) =>
                      toggleNode(n.name, e.currentTarget.value, true)
                    }
                    data={n.variants}
                    styles={{
                      input: {
                        fontFamily: "var(--mantine-font-family-monospace)",
                      },
                    }}
                  />
                )}
              </Group>
            );
          })}
        </Stack>
      </ScrollArea.Autosize>
    </Stack>
  );

  return (
    <>
      <Button
        size="compact-sm"
        variant="default"
        rightSection={<IconChevronDown size={12} />}
        onClick={() => setOpened(true)}
        styles={{
          label: { fontFamily: "var(--mantine-font-family-monospace)" },
        }}
      >
        {startSelectionLabel(sel)}
      </Button>
      <Modal
        opened={opened}
        onClose={() => setOpened(false)}
        title="Start configuration"
        size={hasPresets ? 680 : 440}
        yOffset={88}
        radius="lg"
        overlayProps={{ backgroundOpacity: 0.42 }}
      >
        {hasPresets ? (
          <Grid gap="lg">
            <Grid.Col span={6}>
              <Stack gap={0}>
                <Text size="xs" fw={600} c="dimmed" tt="uppercase" pb={6}>
                  Presets
                </Text>
                <ScrollArea.Autosize mah={380}>
                  <Radio.Group
                    value={sel?.kind === "preset" ? sel.name : null}
                    onChange={(name) => {
                      props.onChange({ kind: "preset", name });
                      setOpened(false);
                    }}
                  >
                    <Stack gap={7} pr={8}>
                      {w.presets.map((p) => (
                        <Radio
                          key={p}
                          value={p}
                          label={p}
                          size="xs"
                          styles={{
                            label: {
                              fontFamily:
                                "var(--mantine-font-family-monospace)",
                            },
                          }}
                        />
                      ))}
                    </Stack>
                  </Radio.Group>
                </ScrollArea.Autosize>
              </Stack>
            </Grid.Col>
            <Grid.Col span={6}>{customPanel}</Grid.Col>
          </Grid>
        ) : (
          customPanel
        )}
        <Group justify="end" pt="md">
          <Button size="compact-sm" onClick={() => setOpened(false)}>
            Done
          </Button>
        </Group>
      </Modal>
    </>
  );
}

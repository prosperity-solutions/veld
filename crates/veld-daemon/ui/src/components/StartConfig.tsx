import { useState } from "react";
import {
  Button,
  Checkbox,
  Divider,
  Group,
  NativeSelect,
  Popover,
  Radio,
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
 * Compact popover choosing between presets (quick picks) and a custom
 * per-node variant selection — covers configs with no presets at all.
 */
export function StartConfig(props: {
  worktree: Worktree;
  value: StartSelection | null;
  onChange: (sel: StartSelection) => void;
}) {
  const [opened, setOpened] = useState(false);
  const w = props.worktree;
  const sel = props.value ?? defaultStartSelection(w);

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

  return (
    <Popover
      opened={opened}
      onChange={setOpened}
      position="bottom-start"
      shadow="md"
      width={280}
    >
      <Popover.Target>
        <Button
          size="compact-sm"
          variant="default"
          rightSection={<IconChevronDown size={12} />}
          onClick={() => setOpened((v) => !v)}
          styles={{
            label: { fontFamily: "var(--mantine-font-family-monospace)" },
          }}
        >
          {startSelectionLabel(sel)}
        </Button>
      </Popover.Target>
      <Popover.Dropdown p="sm">
        <Stack gap="sm">
          {w.presets.length > 0 && (
            <>
              <Radio.Group
                label="Preset"
                value={sel?.kind === "preset" ? sel.name : null}
                onChange={(name) => {
                  props.onChange({ kind: "preset", name });
                  setOpened(false);
                }}
              >
                <Stack gap={6} pt={4}>
                  {w.presets.map((p) => (
                    <Radio key={p} value={p} label={p} size="xs" />
                  ))}
                </Stack>
              </Radio.Group>
              <Divider label="or custom" labelPosition="center" />
            </>
          )}
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
                  style={{ flex: 1 }}
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
      </Popover.Dropdown>
    </Popover>
  );
}

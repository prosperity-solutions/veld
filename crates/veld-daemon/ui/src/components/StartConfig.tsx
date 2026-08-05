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
import type { Preset, Worktree } from "../api";

/**
 * What ▶ starts: a preset, or an explicit set of `node:variant` selections.
 * The UI always resolves to one of the two before starting — sending neither is
 * not a no-op, since a bare `veld start` falls back to the project's
 * `default_preset` when one is declared and only fails ("No selections provided")
 * when there isn't one.
 */
export type StartSelection =
  | { kind: "preset"; name: string }
  | { kind: "nodes"; selections: string[] };

/**
 * The presets this surface can offer.
 *
 * `Worktree.presets` is `null` when the config could not be read and `[]` when it
 * declares none — a distinction that matters for *provenance* (an empty list means
 * a run's recorded preset was deleted; unreadable means nobody knows) and does not
 * matter here: either way there is nothing to pick, and the fallback is node
 * selections. Collapsed once, in one named place, so the difference is not silently
 * flattened at seven call sites.
 */
function offered(w: Worktree): Preset[] {
  return w.presets ?? [];
}

export function defaultStartSelection(w: Worktree): StartSelection | null {
  if (offered(w).length > 0) {
    // `default_preset` first — the config author said which one this is, and
    // "the first preset in the file" is a guess by comparison.
    const list = offered(w);
    const preferred = list.find((p) => p.is_default) ?? list[0];
    return { kind: "preset", name: preferred.name };
  }
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

/** Parse a stored selection, tolerating anything that isn't one. */
export function parseStartSelection(raw: string): StartSelection | null {
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as StartSelection;
    if (parsed?.kind === "preset" && typeof parsed.name === "string") {
      return parsed;
    }
    if (parsed?.kind === "nodes" && Array.isArray(parsed.selections)) {
      return parsed;
    }
    return null;
  } catch {
    return null;
  }
}

/**
 * Drop stored choices the worktree's config no longer offers (preset renamed
 * away, node removed) — `veld start` would reject them. `null` means nothing
 * usable survived and the caller should fall back to
 * [`defaultStartSelection`].
 */
export function pruneStartSelection(
  w: Worktree,
  sel: StartSelection | null,
): StartSelection | null {
  if (!sel) return null;
  if (sel.kind === "preset") {
    return offered(w).some((p) => p.name === sel.name) ? sel : null;
  }
  const valid = new Set(
    w.nodes.flatMap((n) => n.variants.map((v) => `${n.name}:${v}`)),
  );
  const selections = sel.selections.filter((s) => valid.has(s));
  return selections.length > 0 ? { kind: "nodes", selections } : null;
}

/** localStorage key holding a worktree's remembered start configuration. */
export function startStorageKey(path: string): string {
  return `veld.start.${path}`;
}

/**
 * What ▶ would start for `w`, resolved straight from localStorage. The rail's
 * per-row controls need this for worktrees other than the selected one, where
 * the `usePersisted` hook backing the top bar isn't available.
 */
export function resolveStartSelection(w: Worktree): StartSelection | null {
  let raw = "";
  try {
    // `globalThis`, not `window`: identical in the browser, but it also lets
    // this run under the node test environment without pulling in a DOM.
    raw = globalThis.localStorage?.getItem(startStorageKey(w.path)) ?? "";
  } catch {
    // Private-mode / disabled storage: fall through to the default.
  }
  return (
    pruneStartSelection(w, parseStartSelection(raw)) ?? defaultStartSelection(w)
  );
}

/**
 * The group heading to render above `presets[i]`, or `null` for none.
 *
 * Presets arrive already in display order, so a heading belongs exactly where the
 * group changes. Ungrouped presets get an explicit **"Other"** heading rather than
 * none: the daemon's resolver can place the ungrouped bucket *between* two groups,
 * and a heading-less run of radios there renders under the previous group's title
 * and reads as members of it. The CLI labels the same bucket "Other"
 * (`crates/veld/src/commands/presets.rs`), and the two surfaces must not describe
 * one payload differently.
 *
 * A config that never mentions `group` gets no headings at all.
 */
export function presetHeading(presets: Preset[], i: number): string | null {
  if (!presets.some((p) => p.group != null)) return null;
  const heading = presets[i].group ?? "Other";
  const previous = i === 0 ? undefined : (presets[i - 1].group ?? "Other");
  return heading === previous ? null : heading;
}

/** Request body for `POST /api/worktrees/{id}/start`. */
export function startBody(sel: StartSelection): {
  preset?: string;
  selections?: string[];
} {
  return sel.kind === "preset"
    ? { preset: sel.name }
    : { selections: sel.selections };
}

/**
 * Button text for a selection. Given the worktree, a preset renders its
 * human-readable `label`; without one it falls back to the config key, which is
 * what a config with no labels has to show anyway.
 */
export function startSelectionLabel(
  sel: StartSelection | null,
  w?: Worktree,
): string {
  if (!sel) return "nothing to start";
  if (sel.kind === "preset") {
    const hit = w && offered(w).find((p) => p.name === sel.name);
    return hit?.label ?? sel.name;
  }
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
  const hasPresets = offered(w).length > 0;

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
                    body: { alignItems: "center" },
                    labelWrapper: { minWidth: 0 },
                    label: {
                      fontFamily: "var(--mantine-font-family-monospace)",
                      whiteSpace: "nowrap",
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      display: "block",
                    },
                  }}
                  style={{ flex: 1, minWidth: 0, overflow: "hidden" }}
                />
                {n.variants.length > 1 && (
                  <NativeSelect
                    size="xs"
                    w={130}
                    style={{ flex: "none" }}
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
      {/* The `Start:` prefix is the whole fix for a real confusion, not
          decoration. This label is a *client-side* choice — what ▶ will start
          next, remembered per worktree — and it sits beside the run selector,
          which reports what the daemon says is actually running. Unprefixed, the
          two read as one statement: a stale preset name next to a green dot was
          taken to mean "that preset is running", when the live run had come from
          the CLI or an agent. The run's own origin now renders in the selector
          (`startOriginLabel`), so these are two labelled answers to two
          questions rather than one ambiguous pair. */}
      <Button
        size="compact-sm"
        variant="default"
        rightSection={<IconChevronDown size={12} />}
        onClick={() => setOpened(true)}
        title="What ▶ will start next — not what is running now"
        styles={{
          label: { fontFamily: "var(--mantine-font-family-monospace)" },
        }}
      >
        Start: {startSelectionLabel(sel, w)}
      </Button>
      <Modal
        opened={opened}
        onClose={() => setOpened(false)}
        title="Start configuration"
        size={hasPresets ? 680 : 440}
        yOffset={88}
        radius="lg"
        overlayProps={{ backgroundOpacity: 0.42 }}
        styles={{
          header: { borderBottom: "1px solid var(--border)" },
          body: { paddingTop: "var(--mantine-spacing-md)" },
        }}
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
                      {offered(w).map((p, i) => {
                        const heading = presetHeading(offered(w), i);
                        return (
                          <div key={p.name}>
                            {heading != null && (
                              <Text
                                size="xs"
                                fw={600}
                                c="dimmed"
                                pt={i === 0 ? 0 : 8}
                                pb={4}
                              >
                                {heading}
                              </Text>
                            )}
                            <Radio
                              value={p.name}
                              size="xs"
                              label={
                                <Stack gap={0}>
                                  <Group gap={6} wrap="nowrap">
                                    <Text size="xs" c="dimmed" ff="monospace">
                                      {p.key}
                                    </Text>
                                    <Text size="xs">
                                      {p.label ?? p.name}
                                    </Text>
                                    {p.is_default && (
                                      <Text size="xs" c="dimmed">
                                        default
                                      </Text>
                                    )}
                                  </Group>
                                  {p.when_to_use && (
                                    <Text size="xs" c="dimmed">
                                      {p.when_to_use}
                                    </Text>
                                  )}
                                </Stack>
                              }
                            />
                          </div>
                        );
                      })}
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
        <Group
          justify="end"
          pt="md"
          mt="md"
          style={{ borderTop: "1px solid var(--border)" }}
        >
          <Button size="compact-sm" onClick={() => setOpened(false)}>
            Done
          </Button>
        </Group>
      </Modal>
    </>
  );
}

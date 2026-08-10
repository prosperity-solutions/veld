import { useEffect, useState } from "react";
import {
  Button,
  Checkbox,
  Group,
  Modal,
  NativeSelect,
  Radio,
  ScrollArea,
  SegmentedControl,
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

/** Which panel the modal is showing. `"choose"` is the two-card choice a
 *  worktree with no selection yet opens on. */
export type StartMode = "choose" | "preset" | "nodes";

/** The panel a committed selection opens on. */
export function modeForSelection(sel: StartSelection | null): StartMode {
  if (sel === null) return "choose";
  return sel.kind === "preset" ? "preset" : "nodes";
}

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
 * The user's *stored* choice for `w` — without the fallback to the default.
 *
 * `null` means the user has not deliberately picked a preset/node set yet. The
 * top bar's ▶ (and the rail's) open the picker on this state rather than
 * guessing at the default — the point of the first-user-test change.
 */
export function resolveStoredSelection(w: Worktree): StartSelection | null {
  let raw = "";
  try {
    // `globalThis`, not `window`: identical in the browser, but it also lets
    // this run under the node test environment without pulling in a DOM.
    raw = globalThis.localStorage?.getItem(startStorageKey(w.path)) ?? "";
  } catch {
    // Private-mode / disabled storage: fall through to the default.
  }
  return pruneStartSelection(w, parseStartSelection(raw));
}

/**
 * What ▶ would start for `w`, resolved straight from localStorage. The rail's
 * per-row controls need this for worktrees other than the selected one, where
 * the `usePersisted` hook backing the top bar isn't available.
 */
export function resolveStartSelection(w: Worktree): StartSelection | null {
  return resolveStoredSelection(w) ?? defaultStartSelection(w);
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

/** The fixed height of the two scrollable lists, so the scrollbar is always
 *  visible (`ScrollArea type="always"`) — the first test's user did not notice
 *  the lists scrolled, so this makes the scroll affordance impossible to miss. */
const LIST_HEIGHT = 340;

/**
 * Start-configuration modal.
 *
 * The design answers the first user test: on a worktree with **no selection yet**
 * it opens on two big cards — "Select a preset" (the simple, curated path) and
 * "Custom node selection" (advanced) — so nobody is handed a pre-selected preset
 * they did not choose. Once something is selected the modal shows only the
 * relevant panel, with a segmented control to switch to the other. The preset and
 * node lists are tall always-scrollable regions so it is obvious they scroll.
 *
 * The modal is **controlled** (`opened`/`onOpen`/`onClose` live in the app): the
 * ▶ start button opens the same modal on a worktree with no selection, so
 * "choose, then Done" is the one gesture for a first start.
 */
export function StartConfig(props: {
  worktree: Worktree;
  /** The committed selection, or `null` when the user has not picked one yet. */
  value: StartSelection | null;
  opened: boolean;
  onOpen: () => void;
  onClose: () => void;
  onChange: (sel: StartSelection) => void;
  /** Called when the user clicks Done. The app decides whether that also starts
   *  (it does when the modal was opened from the ▶ start button). */
  onDone: () => void;
}) {
  const w = props.worktree;
  const hasPresets = offered(w).length > 0;
  const hasNodes = w.nodes.length > 0;
  // A local draft, committed only on Done — picking inside the modal must not
  // touch storage (or the rail's rows) until the user confirms.
  const [draft, setDraft] = useState<StartSelection | null>(props.value);
  const [mode, setMode] = useState<StartMode>(modeForSelection(props.value));
  // Re-seed the draft each time the modal opens: the committed value may have
  // changed (another window, a re-read) while it was closed.
  useEffect(() => {
    if (props.opened) {
      setDraft(props.value);
      setMode(modeForSelection(props.value));
    }
    // `props.value` is deliberately not a dependency — the draft is reset on
    // open, not on every change while the modal is showing.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.opened]);

  const selectedVariant = (node: string): string | null => {
    if (draft?.kind !== "nodes") return null;
    const hit = draft.selections.find((s) => s.split(":")[0] === node);
    return hit ? (hit.split(":")[1] ?? null) : null;
  };

  const toggleNode = (node: string, variant: string, on: boolean) => {
    const current =
      draft?.kind === "nodes" ? draft.selections.filter((s) => s.split(":")[0] !== node) : [];
    const selections = on ? [...current, `${node}:${variant}`] : current;
    // Empty custom selection means "nothing picked" again — back to the cards.
    if (selections.length === 0) {
      setDraft(null);
      setMode("choose");
    } else {
      setDraft({ kind: "nodes", selections });
    }
  };

  const customPanel = (
    <Stack gap={0} style={{ minWidth: 0 }}>
      <Text size="xs" fw={600} c="dimmed" tt="uppercase" pb={6}>
        Custom node selection
      </Text>
      {hasNodes ? (
        <ScrollArea type="always" offsetScrollbars style={{ height: LIST_HEIGHT }}>
          <Stack gap={8} pr={10}>
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
        </ScrollArea>
      ) : (
        <Text size="xs" c="dimmed">
          No startable nodes in this config.
        </Text>
      )}
    </Stack>
  );

  const presetsPanel = (
    <Stack gap={0} style={{ minWidth: 0 }}>
      <Text size="xs" fw={600} c="dimmed" tt="uppercase" pb={6}>
        Presets
      </Text>
      <ScrollArea type="always" offsetScrollbars style={{ height: LIST_HEIGHT }}>
        <Radio.Group
          value={draft?.kind === "preset" ? draft.name : null}
          onChange={(name) => setDraft({ kind: "preset", name })}
        >
          <Stack gap={7} pr={10}>
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
                          <Text size="xs">{p.label ?? p.name}</Text>
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
      </ScrollArea>
    </Stack>
  );

  /** The segmented control shown once something is selected, switching between
   *  the preset and custom panels without losing the choice. */
  const tabs = (hasPresets || hasNodes) && mode !== "choose" && (
    <SegmentedControl
      size="xs"
      fullWidth
      value={mode}
      onChange={(v) => setMode(v as StartMode)}
      data={[
        ...(hasPresets ? [{ value: "preset", label: "Presets" }] : []),
        ...(hasNodes ? [{ value: "nodes", label: "Custom nodes" }] : []),
      ]}
    />
  );

  /** The two-card choice shown when nothing has been picked yet. */
  const choiceCards = (
    <Stack gap="md" pt="xs">
      <Text size="sm" c="dimmed">
        Choose how to start this worktree. Nothing is pre-selected — pick the
        option that fits, or close to decide later.
      </Text>
      <Group grow align="stretch" wrap="nowrap">
        {hasPresets && (
          <button
            type="button"
            className="start-mode-card"
            onClick={() => setMode("preset")}
          >
            <Text size="sm" fw={700}>
              Select a preset
            </Text>
            <Text size="xs" c="dimmed">
              The simplest option. The project&apos;s author curated ready-made
              setups — pick one and go.
            </Text>
          </button>
        )}
        {hasNodes && (
          <button
            type="button"
            className="start-mode-card"
            onClick={() => setMode("nodes")}
          >
            <Text size="sm" fw={700}>
              Custom node selection
            </Text>
            <Text size="xs" c="dimmed">
              For advanced use. Choose exactly which nodes run and which variant
              each uses.
            </Text>
          </button>
        )}
      </Group>
    </Stack>
  );

  const nothingToStart = !hasPresets && !hasNodes;

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
        onClick={props.onOpen}
        title="What ▶ will start next — not what is running now"
      >
        {props.value ? `Start: ${startSelectionLabel(props.value, w)}` : "Choose preset or nodes"}
      </Button>
      <Modal
        opened={props.opened}
        onClose={props.onClose}
        title="Start configuration"
        size={760}
        yOffset={88}
        radius="lg"
        overlayProps={{ backgroundOpacity: 0.42 }}
        styles={{
          header: { borderBottom: "1px solid var(--border)" },
          body: { paddingTop: "var(--mantine-spacing-md)" },
        }}
      >
        {nothingToStart ? (
          <Text size="xs" c="dimmed" py="sm">
            Nothing to start — this config declares no presets and no startable
            nodes.
          </Text>
        ) : mode === "choose" ? (
          choiceCards
        ) : (
          <Stack gap="md">
            {tabs}
            <div style={{ minHeight: LIST_HEIGHT }}>
              {mode === "preset" ? presetsPanel : customPanel}
            </div>
          </Stack>
        )}
        <Group
          justify="space-between"
          pt="md"
          mt="md"
          style={{ borderTop: "1px solid var(--border)" }}
        >
          <Text size="xs" c="dimmed">
            {draft
              ? `Will start: ${startSelectionLabel(draft, w)}`
              : "Nothing selected yet."}
          </Text>
          <Group gap="sm">
            <Button size="compact-sm" variant="default" onClick={props.onClose}>
              Cancel
            </Button>
            <Button
              size="compact-sm"
              disabled={draft === null}
              onClick={() => {
                if (!draft) return;
                props.onChange(draft);
                // `onDone` before `onClose`: the app reads a flag that `onClose`
                // clears, so starting must happen before the modal shuts down.
                props.onDone();
                props.onClose();
              }}
            >
              Done
            </Button>
          </Group>
        </Group>
      </Modal>
    </>
  );
}

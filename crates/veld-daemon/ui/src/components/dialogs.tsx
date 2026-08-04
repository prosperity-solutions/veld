import { type FormEvent, type ReactNode, useEffect, useState } from "react";
import {
  Button,
  Checkbox,
  Group,
  Loader,
  Modal as MantineModal,
  Stack,
  Text,
  TextInput,
} from "@mantine/core";
import { api, type EmojiHolder, type Repo } from "../api";
import type { MarkerStyle } from "../shared/settings";

/**
 * Shared dialog shell on Mantine's Modal (scrim, esc, focus trap, a11y) —
 * kept as a local wrapper so call sites stay stable and the design-token
 * offset/size match the handoff.
 */
export function Modal(props: {
  title: string;
  onClose: () => void;
  children: ReactNode;
}) {
  return (
    <MantineModal
      opened
      onClose={props.onClose}
      title={props.title}
      yOffset={88}
      size={560}
      radius="lg"
      overlayProps={{ backgroundOpacity: 0.42 }}
    >
      {props.children}
    </MantineModal>
  );
}

/** Shared submit plumbing: disables the button, surfaces the API error. */
function useSubmit(action: () => Promise<void>) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await action();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setBusy(false);
    }
  };
  return { busy, error, submit };
}

function ErrorText(props: { error: string | null }) {
  if (!props.error) return null;
  return (
    <Text size="sm" c="red" style={{ whiteSpace: "pre-wrap" }}>
      {props.error}
    </Text>
  );
}

export function ImportRepoDialog(props: {
  onImport: (path: string) => Promise<void>;
  onClose: () => void;
}) {
  const [path, setPath] = useState("");
  const [pickError, setPickError] = useState<string | null>(null);
  const [picking, setPicking] = useState(false);
  const { busy, error, submit } = useSubmit(() => props.onImport(path.trim()));

  const browse = async () => {
    setPicking(true);
    setPickError(null);
    try {
      const picked = await api.pickDirectory();
      if (picked) setPath(picked);
    } catch (e) {
      setPickError(e instanceof Error ? e.message : String(e));
    } finally {
      setPicking(false);
    }
  };

  return (
    <Modal title="Import repository" onClose={props.onClose}>
      <form onSubmit={submit}>
        <Stack gap="sm">
          <Group gap="xs" align="end">
            <TextInput
              label="Repository directory"
              placeholder="/Users/you/git/my-project"
              value={path}
              onChange={(e) => setPath(e.currentTarget.value)}
              style={{ flex: 1 }}
              styles={{ input: { fontFamily: "var(--mantine-font-family-monospace)" } }}
              data-autofocus
            />
            <Button variant="default" onClick={browse} loading={picking}>
              Browse…
            </Button>
          </Group>
          <Text size="xs" c="dimmed">
            Any directory inside the repo works — the main checkout and
            existing worktrees are discovered automatically.
          </Text>
          <ErrorText error={pickError} />
          <ErrorText error={error} />
          <Button
            type="submit"
            loading={busy}
            disabled={picking || !path.trim()}
          >
            Import
          </Button>
        </Stack>
      </form>
    </Modal>
  );
}

export function RemoveRepoDialog(props: {
  repo: Repo;
  onRemove: () => Promise<void>;
  onClose: () => void;
}) {
  const { busy, error, submit } = useSubmit(() => props.onRemove());
  return (
    <Modal title={`Remove ${props.repo.name}?`} onClose={props.onClose}>
      <form onSubmit={submit}>
        <Stack gap="sm">
          <Text size="sm" c="dimmed">
            Removes the project (and its worktree list) from Veld Desktop only
            — nothing on disk is touched. You can re-import it anytime.
          </Text>
          <Text size="xs" c="dimmed" ff="monospace">
            {props.repo.root}
          </Text>
          <ErrorText error={error} />
          <Button type="submit" color="red" variant="light" loading={busy}>
            Remove project
          </Button>
        </Stack>
      </form>
    </Modal>
  );
}

export function NewWorktreeDialog(props: {
  onCreate: (body: {
    branch: string;
    create_branch: boolean;
    alias?: string;
  }) => Promise<void>;
  onClose: () => void;
}) {
  const [branch, setBranch] = useState("");
  const [createBranch, setCreateBranch] = useState(true);
  const [alias, setAlias] = useState("");
  const { busy, error, submit } = useSubmit(() =>
    props.onCreate({
      branch: branch.trim(),
      create_branch: createBranch,
      alias: alias.trim() || undefined,
    }),
  );
  return (
    <Modal title="New worktree" onClose={props.onClose}>
      <form onSubmit={submit}>
        <Stack gap="sm">
          <TextInput
            label="Branch"
            placeholder="feat/checkout-v2"
            value={branch}
            onChange={(e) => setBranch(e.currentTarget.value)}
            styles={{ input: { fontFamily: "var(--mantine-font-family-monospace)" } }}
            data-autofocus
          />
          <Checkbox
            label="Create this branch (from the repo's current HEAD)"
            checked={createBranch}
            onChange={(e) => setCreateBranch(e.currentTarget.checked)}
          />
          <TextInput
            label="Alias (optional)"
            placeholder="derived from the branch name"
            value={alias}
            onChange={(e) => setAlias(e.currentTarget.value)}
            styles={{ input: { fontFamily: "var(--mantine-font-family-monospace)" } }}
          />
          <ErrorText error={error} />
          <Button type="submit" loading={busy} disabled={!branch.trim()}>
            Create worktree
          </Button>
        </Stack>
      </form>
    </Modal>
  );
}

/**
 * Marker picker for a worktree's rail identifier.
 *
 * A marker has two faces — a colour and a glyph — and `worktree.markerStyle`
 * decides which one the rail renders. Both are editable here regardless of that
 * setting, because both are stored: a user who prefers colours can still choose
 * their glyph, and switching the setting later is then lossless in either
 * direction rather than a re-pick.
 *
 * The choices come from the daemon (`/api/worktree-emoji`) rather than a
 * TypeScript copy, because the same list is the server-side allowlist. Glyphs
 * already in use by another checkout of the same repo stay selectable — the assigner
 * avoids duplicates within a repo, but an explicit choice is the user's to make —
 * and are only labelled, so the ambiguity is visible before it's created.
 */
export function ChangeMarkerDialog(props: {
  current: string;
  currentColor: string;
  alias: string;
  /** Identifies "this worktree" among the holders — aliases can't, since
   *  they are unique only within one repo. */
  worktreeId: number;
  /** emoji → the worktree's own repo siblings holding it. Scoped to one repo
   *  because the assigner is: a glyph repeating across repos is expected, not a
   *  collision, and the rail only ever renders one repo. */
  usedBy: Record<string, EmojiHolder[]>;
  /** The same, for the colour face — so both halves of a marker warn alike. */
  colorUsedBy: Record<string, EmojiHolder[]>;
  /** Which face the rail is currently rendering, so the dialog can say which
   *  half of the choice is the one being shown right now. Both halves stay
   *  editable regardless — see the note at the bottom of the dialog. */
  style: MarkerStyle;
  onPick: (patch: { emoji?: string; marker_color?: string }) => Promise<void>;
  onClose: () => void;
}) {
  const [choices, setChoices] = useState<string[] | null>(null);
  const [colors, setColors] = useState<string[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void api
      .worktreeEmoji()
      .then((r) => {
        if (cancelled) return;
        // A malformed payload would otherwise store `undefined` and leave the
        // Loader spinning forever with no error and no way out.
        if (Array.isArray(r?.emoji)) setChoices(r.emoji);
        else setLoadError("The daemon returned an unexpected emoji list.");
        // The palette comes from the daemon for the same reason the glyphs do: it
        // is the set the server offers, and a TypeScript copy would drift. A
        // malformed payload leaves the colour grid out rather than rendering
        // swatches with no fill.
        if (Array.isArray(r?.colors)) setColors(r.colors);
      })
      .catch((e: unknown) => {
        if (!cancelled) {
          setLoadError(e instanceof Error ? e.message : String(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const pick = async (
    key: string,
    patch: { emoji?: string; marker_color?: string },
  ) => {
    setBusy(key);
    setError(null);
    try {
      await props.onPick(patch);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(null);
    }
  };

  return (
    <Modal title={`Marker for ${props.alias}`} onClose={props.onClose}>
      <Stack gap="sm">
        {loadError && <ErrorText error={loadError} />}
        {colors !== null && (
          <>
            <Text size="xs" fw={600} c="dimmed">
              Colour{props.style === "color" ? " (shown in the rail)" : ""}
            </Text>
            <div className="swatch-grid">
              {colors.map((color) => {
                const isCurrent = color === props.currentColor;
                // Same treatment as the glyph grid. It matters more here: eight
                // colours against a repo that can hold more checkouts than that
                // means a within-repo duplicate is likely, and within-repo is the
                // only scope where distinctness is claimed.
                const others = (props.colorUsedBy[color] ?? []).filter(
                  (h) => h.id !== props.worktreeId,
                );
                const taken = others.map((h) => h.alias).join(", ");
                return (
                  <button
                    key={color}
                    type="button"
                    className={`swatch-cell${isCurrent ? " current" : ""}`}
                    disabled={busy !== null}
                    aria-pressed={isCurrent}
                    aria-label={
                      taken
                        ? `Colour ${color} — in use by ${taken}`
                        : `Colour ${color}${isCurrent ? " — current" : ""}`
                    }
                    title={
                      [
                        isCurrent ? "Current" : color,
                        taken ? `In use by ${taken}` : "",
                      ]
                        .filter(Boolean)
                        .join(" · ") || undefined
                    }
                    onClick={() => void pick(color, { marker_color: color })}
                  >
                    {busy === color ? (
                      <Loader size={14} />
                    ) : (
                      <span style={{ background: color }} />
                    )}
                    {taken && <span className="emoji-taken" />}
                  </button>
                );
              })}
            </div>
            <Text size="xs" fw={600} c="dimmed">
              Glyph{props.style === "emoji" ? " (shown in the rail)" : ""}
            </Text>
          </>
        )}
        {!choices && !loadError && (
          <Group justify="center" py="lg">
            <Loader size="sm" aria-label="Loading emoji" />
          </Group>
        )}
        {choices && (
          <div className="emoji-grid">
            {choices.map((e) => {
              const isCurrent = e === props.current;
              // Every holder that isn't this worktree. Compared by id, not
              // alias, and covering all holders rather than the first: a
              // collision with another project's identically-named worktree
              // is precisely what the picker exists to surface.
              const others = (props.usedBy[e] ?? []).filter(
                (h) => h.id !== props.worktreeId,
              );
              const taken = others.map((h) => h.alias).join(", ");
              return (
                <button
                  key={e}
                  type="button"
                  className={`emoji-cell${isCurrent ? " current" : ""}`}
                  disabled={busy !== null}
                  aria-pressed={isCurrent}
                  aria-label={taken ? `${e} — in use by ${taken}` : e}
                  title={
                    [
                      isCurrent ? "Current" : "",
                      taken ? `In use by ${taken}` : "",
                    ]
                      .filter(Boolean)
                      .join(" · ") || undefined
                  }
                  onClick={() => void pick(e, { emoji: e })}
                >
                  {busy === e ? (
                    <Loader size={14} />
                  ) : (
                    <span aria-hidden="true">{e}</span>
                  )}
                  {taken && <span className="emoji-taken" />}
                </button>
              );
            })}
          </div>
        )}
        <ErrorText error={error} />
        <Text size="xs" c="dimmed">
          A dot marks a colour or glyph another checkout of this repo already uses.
          Picking it is allowed — the rail just won&apos;t identify them apart.
        </Text>
        <Text size="xs" c="dimmed">
          Both halves are always saved, so you can set the one you aren&apos;t
          currently showing and it will be waiting if you switch. Which one the
          rail renders is under Settings → Appearance.
        </Text>
      </Stack>
    </Modal>
  );
}

export function RenameWorktreeDialog(props: {
  current: string;
  onRename: (alias: string) => Promise<void>;
  onDelete: (force: boolean) => Promise<void>;
  isMain: boolean;
  /** Open with the remove confirmation already expanded (context menu). */
  deleteFocus: boolean;
  onClose: () => void;
}) {
  const [alias, setAlias] = useState(props.current);
  const [confirmDelete, setConfirmDelete] = useState(props.deleteFocus);
  const [force, setForce] = useState(false);
  const rename = useSubmit(() => props.onRename(alias.trim()));
  const del = useSubmit(() => props.onDelete(force));
  return (
    <Modal title="Edit worktree" onClose={props.onClose}>
      <form onSubmit={rename.submit}>
        <Stack gap="sm">
          <TextInput
            label="Alias"
            value={alias}
            onChange={(e) => setAlias(e.currentTarget.value)}
            styles={{ input: { fontFamily: "var(--mantine-font-family-monospace)" } }}
            data-autofocus={!props.deleteFocus}
          />
          <ErrorText error={rename.error} />
          <Button type="submit" loading={rename.busy} disabled={!alias.trim()}>
            Save
          </Button>
        </Stack>
      </form>
      {!props.isMain && (
        <Stack
          gap="sm"
          mt="md"
          pt="md"
          style={{ borderTop: "1px solid var(--border)" }}
        >
          {confirmDelete ? (
            <>
              <Text size="sm" c="dimmed">
                Removes the checkout from disk (git refuses if it has
                uncommitted changes). The branch itself is kept. Stop any
                running environment in this worktree first — removing pulls
                the directory out from under it.
              </Text>
              <ErrorText error={del.error} />
              {del.error && (
                <Checkbox
                  color="red"
                  label="Force remove — discards uncommitted changes"
                  checked={force}
                  onChange={(e) => setForce(e.currentTarget.checked)}
                />
              )}
              <Button
                color="red"
                variant="light"
                loading={del.busy}
                onClick={(e) => {
                  e.preventDefault();
                  void del.submit(e);
                }}
              >
                Really remove worktree
              </Button>
            </>
          ) : (
            <Button
              color="red"
              variant="subtle"
              onClick={(e) => {
                e.preventDefault();
                setConfirmDelete(true);
              }}
            >
              Remove worktree…
            </Button>
          )}
        </Stack>
      )}
    </Modal>
  );
}

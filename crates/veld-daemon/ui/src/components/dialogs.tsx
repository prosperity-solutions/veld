import { type FormEvent, type ReactNode, useEffect, useRef, useState } from "react";
import {
  Alert,
  Badge,
  Button,
  Checkbox,
  Group,
  Loader,
  Modal as MantineModal,
  Radio,
  ScrollArea,
  SegmentedControl,
  Stack,
  Text,
  TextInput,
  Tooltip,
} from "@mantine/core";
import {
  api,
  MAX_LANE_NAME_LEN,
  type DirtyFile,
  type EmojiHolder,
  type Repo,
  type WorktreeGitStatus,
} from "../api";
import type { GitCreateFrom, MarkerStyle } from "../shared/settings";
import {
  aliasCollides,
  deriveAlias,
  deriveBranch,
  deriveDisplayName,
  takenExcluding,
} from "../shared/worktreeName";
import { randomMarker } from "../shared/markerPick";

/**
 * Shared dialog shell on Mantine's Modal (scrim, esc, focus trap, a11y) —
 * kept as a local wrapper so call sites stay stable and the design-token
 * offset/size match the handoff.
 */
export function Modal(props: {
  title: string;
  onClose: () => void;
  /**
   * Width, for the one dialog that is not a form. 560 is the handoff's dialog
   * width and stays the default; settings opts out because it is a two-column
   * surface, where 560 leaves a group panel narrower than the help text under
   * every control. Mantine caps this at the viewport, so a wide value degrades
   * to a full-width modal on a small screen rather than overflowing.
   */
  size?: number | string;
  children: ReactNode;
}) {
  return (
    <MantineModal
      opened
      onClose={props.onClose}
      title={props.title}
      yOffset={88}
      size={props.size ?? 560}
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

/**
 * Create a worktree, name first.
 *
 * The field you land in is the **name** — what this checkout is called in the rail —
 * and the alias and branch are derived from it (`shared/worktreeName.ts`). That is
 * the inversion §20 asked for: the old dialog led with the branch and treated the
 * name as an optional afterthought, which is backwards for the way this app is used.
 * Most checkouts here are "the thing I'm working on", not "a ref".
 *
 * Both derived names are rendered **before** anything is created, because both
 * derivations are lossy — the alias cannot hold a space, the branch cannot hold two
 * consecutive slashes — and a field that silently renames what you typed is a field
 * you stop trusting. The branch stays editable: it follows the name until you touch
 * it, and clearing it hands it back to the derivation.
 *
 * What the name *itself* becomes is no longer lossy, though: it is stored as the
 * worktree's `display_name` and the rail renders it verbatim. The slug is still
 * derived and still shown, because it is the identifier the run name and hostname
 * are built from — but it is now a receipt for something happening underneath
 * rather than the only name the checkout will ever have. Before this, typing
 * "Hello test" produced a rail row reading `hello-test` and no way to get the
 * capital or the space back.
 *
 * The marker is picked here too, rather than being a second trip through the context
 * menu after the checkout exists. A free colour and glyph are drawn at random when the
 * dialog opens and **sent explicitly**, so the checkout wears what the dialog showed;
 * the daemon's own assignment is now only the fallback for the frame before the choice
 * lists resolve. Picking nothing is still a valid way to use this — it just means
 * accepting the draw rather than reaching a different code path.
 */
export function NewWorktreeDialog(props: {
  onCreate: (body: {
    branch: string;
    create_branch: boolean;
    alias?: string;
    display_name?: string;
    emoji?: string;
    marker_color?: string;
  }) => Promise<void>;
  /** The repo's existing aliases, for the collision check. Courtesy only — the
   *  daemon's transaction is the authority (see `aliasCollides`). */
  takenAliases: string[];
  /** Which rail section the "＋" was clicked in — `""` for ungrouped. Shown, not
   *  editable: the click already chose it, and a second control saying the same
   *  thing is one more thing to disagree with. */
  lane: string;
  usedBy: Record<string, EmojiHolder[]>;
  colorUsedBy: Record<string, EmojiHolder[]>;
  /** Which marker face the rail renders, so the grids can label it. */
  markerStyle: MarkerStyle;
  /** Switch the rail's marker face — a settings change, offered right in the
   *  dialog so a user who sees one face never has to find Settings to reach the
   *  other (the first test's confusion). */
  onStyleChange: (style: MarkerStyle) => void;
  /** Where a new branch is cut from (`git.createFrom`) — shown so the create
   *  states where the worktree starts, rather than leaving it a guess. */
  createFrom: GitCreateFrom;
  onClose: () => void;
}) {
  const [name, setName] = useState("");
  const [createBranch, setCreateBranch] = useState(true);
  /** A branch the user typed, or `null` while it still follows the name. */
  const [branchEdit, setBranchEdit] = useState<string | null>(null);
  const loaded = useMarkerChoices();
  /**
   * The marker: drawn once, when the choice lists arrive, and then owned by the user.
   *
   * Drawn in an effect rather than computed during render, because the draw is
   * **random** — a value recomputed on every render would reshuffle the selection
   * while the cursor is on it, and `usedBy` changes identity on every 5s poll, so
   * "memoise it" would only have narrowed the window. Empty until then, which reads
   * as nothing picked and leaves the daemon's own assignment in charge if the fetch
   * never resolves.
   */
  const [marker, setMarker] = useState<{ emoji: string; color: string }>({
    emoji: "",
    color: "",
  });
  const drawn = useRef(false);
  useEffect(() => {
    if (drawn.current || !loaded.choices || !loaded.colors) return;
    drawn.current = true;
    setMarker(
      randomMarker(loaded.choices, loaded.colors, props.usedBy, props.colorUsedBy),
    );
    // Deliberately keyed on the lists alone: `usedBy` is a fresh object per poll, and
    // depending on it would re-run this — the `drawn` guard already makes that safe,
    // but the honest statement is that the draw happens once per dialog.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loaded.choices, loaded.colors]);
  const chosen = marker;

  const alias = deriveAlias(name);
  const displayName = deriveDisplayName(name);
  const derivedBranch = deriveBranch(name);
  const branch = branchEdit ?? derivedBranch;
  /** The alias this dialog has submitted and is waiting on. While a create is
   *  in flight the 5s `refresh()` poll can already surface the checkout it is
   *  creating (the daemon registers the row before its response returns), so
   *  `takenAliases` starts containing exactly the alias being made and the
   *  courtesy check below lights up against the dialog's *own* creation — a
   *  slow `git fetch origin` make that a seconds-long, confusing red error on
   *  a create that then finishes fine. Set at submit, cleared only when the
   *  create *fails* (the dialog stays open; a poll may then legitimately show
   *  a sibling that really does collide). On success the dialog closes, so it
   *  staying set across the reorder/refresh that follow is exactly the window
   *  it exists to silence. */
  const [pendingAlias, setPendingAlias] = useState<string | null>(null);
  const taken = takenExcluding(props.takenAliases, pendingAlias);
  const collides = aliasCollides(alias, taken);
  // An existing branch is named exactly, not slugged: `deriveBranch` would happily
  // turn `feature/JIRA-12` into `feature/jira-12`, which is a different ref and would
  // fail `git worktree add` with a confusing "invalid reference".
  const branchRequired = !createBranch;
  const ready = alias !== "" && branch !== "" && !collides;

  const { busy, error, submit } = useSubmit(() => {
    // Captured at submit, before the daemon's response: this is the alias the
    // in-flight create is about to make, and it must not read as a collision
    // when the poll catches up with the daemon mid-request.
    setPendingAlias(alias);
    return props
      .onCreate({
        branch,
        create_branch: createBranch,
        // Always explicit, never left to the daemon's branch-derived default: the name
        // is the primary field, so a checkout must end up called what the dialog said
        // it would be. It also means a collision is a 409 that created nothing rather
        // than a silent `-2` suffix.
        alias,
        // What the rail will show. Sent even when it happens to equal the alias:
        // "the name is what you typed" is one rule, and a dialog that silently
        // stops storing it once you type something already slug-shaped is two.
        display_name: displayName,
        // Always sent when a list resolved, so the checkout wears what the dialog
        // showed. Empty only while the fetch is in flight, where the daemon's own
        // assignment is the honest fallback.
        emoji: chosen.emoji || undefined,
        marker_color: chosen.color || undefined,
      })
      .catch((e) => {
        // The create is no longer in flight. The dialog stays open on failure,
        // so the courtesy check must be truthful again: if the daemon's own
        // refresh now lists a sibling, that is a real collision worth saying.
        setPendingAlias(null);
        throw e;
      });
  });

  return (
    <Modal title="New worktree" onClose={props.onClose}>
      <form onSubmit={submit}>
        {/* The fields scroll, the footer does not. The glyph grid is 64 animals and
            pushed the Create button below the fold on a laptop, so the dialog's only
            action was reachable exactly when you had stopped scrolling to look for it.
            An inner scroll region rather than `position: sticky` on the footer: the
            modal body is the scroller otherwise, and a sticky child of a scrolling
            ancestor sticks to the wrong box. */}
        <div
          style={{
            maxHeight: "min(58vh, 520px)",
            overflowY: "auto",
            // Keeps the focus ring of a field flush against the scrollbar from being
            // clipped, and stops the grids touching the edge.
            paddingRight: 8,
            marginRight: -8,
          }}
        >
        <Stack gap="sm">
          <TextInput
            label="Name"
            placeholder="Checkout V2"
            value={name}
            onChange={(e) => setName(e.currentTarget.value)}
            /* Both blocking states go on `error`, not only the collision. An
               unusable name disables Create exactly as a collision does, so
               rendering it as dimmed prose below the field made the one thing
               stopping you the quietest thing on screen. */
            error={
              collides
                ? "This repo already has a checkout with that name"
                : name.trim() !== "" && alias === ""
                  ? "Nothing in that name can be used as an identifier — add a letter or a digit"
                  : null
            }
            data-autofocus
          />
          {/* The receipt for the lossy derivation. The rail shows the name you
              typed, verbatim — but the *identifier* underneath it cannot hold a
              space or a capital (it defaults the run name, which becomes a
              hostname), so it is still worth showing before anything is created.
              Monospace because it is an identifier. */}
          {alias !== "" && (
            <Text size="xs" c="dimmed">
              Shown as <b>{displayName}</b>; identified as{" "}
              <Text span ff="monospace">
                {alias}
              </Text>
              {/* Why the two differ, on the screen where they first differ. The
                  old single-name receipt carried this and dropping it left a
                  first-time creator watching their name change with no reason
                  given — the rule now lives only in the *rename* dialog, which
                  is a different screen they have not seen yet. */}
              {alias !== displayName ? " (letters, digits and dashes only)" : ""}
            </Text>
          )}
          {/* Where it lands. Stated rather than assumed: the create was started
              from one specific rail section and a dialog that says nothing about
              it makes the destination a thing you have to remember clicking. */}
          {props.lane !== "" && (
            <Text size="xs" c="dimmed">
              Filed under <b>{props.lane}</b>.
            </Text>
          )}
          <Checkbox
            label={
              props.createFrom === "origin"
                ? "Create the branch (from the latest origin/main)"
                : "Create the branch (from the repo's current HEAD)"
            }
            checked={createBranch}
            onChange={(e) => {
              setCreateBranch(e.currentTarget.checked);
              // Switching to an existing branch clears a derived value that was only
              // ever a guess at a *new* ref — keeping it would offer to check out a
              // branch that does not exist.
              if (!e.currentTarget.checked && branchEdit === null) setBranchEdit("");
            }}
          />
          {/* The receipt for where the branch starts. `git.createFrom` is the
              project-wide policy (Settings → Git); saying it here means a new
              worktree is never born behind the remote without the dialog having
              said so. Offline, or no remote, the daemon falls back to local HEAD. */}
          {createBranch && props.createFrom === "origin" && (
            <Text size="xs" c="dimmed">
              The branch is fetched from the remote first, so it starts from the
              latest <Text span ff="monospace">origin/main</Text> — change this in
              Settings → Git.
            </Text>
          )}
          <TextInput
            label={branchRequired ? "Existing branch" : "Branch"}
            placeholder={branchRequired ? "feat/checkout-v2" : derivedBranch || "feat/checkout-v2"}
            description={
              branchRequired
                ? "Named exactly as git has it — this one is checked out, not created."
                : branchEdit === null
                  ? "Derived from the name. Type here to use something else."
                  : "Custom. Clear the field to go back to the derived name."
            }
            value={branch}
            onChange={(e) => setBranchEdit(e.currentTarget.value)}
            onBlur={() => {
              // An empty box means "follow the name again" rather than "create a
              // branch called nothing".
              if (branchEdit !== null && branchEdit.trim() === "" && !branchRequired) {
                setBranchEdit(null);
              }
            }}
            styles={{ input: { fontFamily: "var(--mantine-font-family-monospace)" } }}
          />
          <Stack gap={6}>
            <Text size="xs" fw={600} c="dimmed" tt="uppercase">
              Marker
            </Text>
            <MarkerGrids
              emoji={chosen.emoji}
              color={chosen.color}
              usedBy={props.usedBy}
              colorUsedBy={props.colorUsedBy}
              style={props.markerStyle}
              onStyleChange={props.onStyleChange}
              // Local until the worktree exists, so nothing is ever in flight.
              busy={null}
              loaded={loaded}
              onPick={(patch) =>
                setMarker((m) => ({
                  emoji: patch.emoji ?? m.emoji,
                  color: patch.marker_color ?? m.color,
                }))
              }
            />
            <Text size="xs" c="dimmed">
              A free one is picked at random — change it, or leave it. The other
              face is saved too, so it is there if you switch.
            </Text>
          </Stack>
        </Stack>
        </div>
        <Stack
          gap="sm"
          pt="sm"
          mt="sm"
          style={{ borderTop: "1px solid var(--border)" }}
        >
          <ErrorText error={error} />
          <Button type="submit" loading={busy} disabled={!ready}>
            Create worktree
          </Button>
        </Stack>
      </form>
    </Modal>
  );
}

/**
 * The colour and glyph grids of a worktree marker, without a dialog around them.
 *
 * Shared by the change-marker dialog (where a pick is written immediately) and the
 * create dialog (where it is held until the worktree exists), because the grids
 * carry more decisions than they look like they do — the choices come from the
 * daemon, "in use" is scoped to one repo, a taken glyph stays selectable — and two
 * copies would eventually disagree about one of them.
 *
 * A marker has two faces: a colour and a glyph, with `worktree.markerStyle` deciding
 * which the rail renders. Both are always shown here, because both are stored — a
 * user who prefers colours can still choose their glyph, and switching the setting
 * later is then lossless rather than a re-pick.
 *
 * The choices come from `/api/worktree-emoji` rather than a TypeScript copy, because
 * that list is the server-side allowlist. Glyphs and colours already used by another
 * checkout of the same repo stay selectable — the assigner avoids duplicates, but an
 * explicit choice is the user's to make — and are only marked, so the ambiguity is
 * visible before it is created.
 */
/**
 * The marker choices the daemon offers: the glyph allowlist and the colour palette.
 *
 * A hook rather than state inside `MarkerGrids`, because the create dialog needs the
 * lists *before* it renders them — it preselects a free colour and glyph, and a
 * preselection cannot be computed from lists only the grid can see.
 *
 * Fetched once per open, not on the 5s poll: both lists are compile-time constants
 * on the daemon side.
 */
export function useMarkerChoices(): {
  choices: string[] | null;
  colors: string[] | null;
  loadError: string | null;
} {
  const [choices, setChoices] = useState<string[] | null>(null);
  const [colors, setColors] = useState<string[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

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

  return { choices, colors, loadError };
}

export function MarkerGrids(props: {
  /** The selected glyph, or `""` for "none picked". */
  emoji: string;
  /** The selected colour, or `""`. */
  color: string;
  /** emoji → the repo's siblings holding it. Scoped to one repo because the
   *  assigner is: a glyph repeating across repos is expected, not a collision, and
   *  the rail only ever renders one repo. */
  usedBy: Record<string, EmojiHolder[]>;
  /** The same, for the colour face — so both halves of a marker warn alike. */
  colorUsedBy: Record<string, EmojiHolder[]>;
  /** Which worktree to exclude from "in use", or `undefined` when it does not exist
   *  yet. Compared by id, not alias: aliases are unique only within one repo. */
  worktreeId?: number;
  /** Which face the rail is currently rendering, so the labels can say which half of
   *  the choice is the one being shown. Both stay editable regardless. */
  style: MarkerStyle;
  /** The choice currently being written, for its spinner, or `null` when a pick is
   *  local and instant. Non-null also disables the whole grid. */
  busy: string | null;
  /** The lists from [`useMarkerChoices`], hoisted so a caller can preselect. */
  loaded: ReturnType<typeof useMarkerChoices>;
  /** Switch the rail's marker face — a settings change, offered *inside* the picker
   *  so a user who sees one face never has to find the setting to reach the other. */
  onStyleChange: (style: MarkerStyle) => void;
  onPick: (patch: { emoji?: string; marker_color?: string }) => void;
}) {
  const busy = props.busy;
  const { choices, colors, loadError } = props.loaded;

  const pick = (patch: { emoji?: string; marker_color?: string }) =>
    props.onPick(patch);

  const colourGrid = colors !== null ? (
    <>
      <Text size="xs" fw={600} c="dimmed">
        Colour
      </Text>
      <div className="swatch-grid">
        {colors.map((color) => {
          const isCurrent = color === props.color;
          // Same treatment as the glyph grid. It matters more here: eight
          // colours against a repo that can hold more checkouts than that
          // means a within-repo duplicate is likely, and within-repo is the
          // only scope where distinctness is claimed.
          const others = (props.colorUsedBy[color] ?? []).filter(
            (h) => h.id !== props.worktreeId,
          );
          const taken = others.map((h) => h.label).join(", ");
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
              onClick={() => pick({ marker_color: color })}
            >
              {busy === color ? (
                <Loader size={14} />
              ) : (
                /* Classed, not styled by position. `.swatch-cell > span`
                   also matched the `.marker-taken` marker below and — being
                   the more specific selector — inflated that 4px dot to a
                   16px circle. */
                <span className="swatch-dot" style={{ background: color }} />
              )}
              {taken && <span className="marker-taken" />}
            </button>
          );
        })}
      </div>
    </>
  ) : null;

  const emojiGrid = choices ? (
    <>
      <Text size="xs" fw={600} c="dimmed">
        Glyph
      </Text>
      <div className="emoji-grid">
        {choices.map((e) => {
          const isCurrent = e === props.emoji;
          // Every holder that isn't this worktree. Compared by id, not
          // alias, and covering all holders rather than the first: a
          // collision with another project's identically-named worktree
          // is precisely what the picker exists to surface.
          const others = (props.usedBy[e] ?? []).filter(
            (h) => h.id !== props.worktreeId,
          );
          const taken = others.map((h) => h.label).join(", ");
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
              onClick={() => pick({ emoji: e })}
            >
              {busy === e ? (
                <Loader size={14} />
              ) : (
                <span aria-hidden="true">{e}</span>
              )}
              {taken && <span className="marker-taken" />}
            </button>
          );
        })}
      </div>
    </>
  ) : null;

  return (
    <>
      {loadError && <ErrorText error={loadError} />}
      {/* The two faces, switchable right here. The first user test confused the
          two marker types when both grids sat in one dialog, so only the face
          the rail renders is shown — and the other face is switched to by
          changing the setting in place, so nobody has to go find Settings. */}
      <SegmentedControl
        size="xs"
        fullWidth
        value={props.style}
        onChange={(v) => props.onStyleChange(v as MarkerStyle)}
        data={[
          { value: "color", label: "Colour" },
          { value: "emoji", label: "Emoji" },
        ]}
      />
      <Text size="xs" c="dimmed">
        {props.style === "color"
          ? "This is what the rail shows. The glyph is still saved, so it is there if you switch."
          : "This is what the rail shows. The colour is still saved, so it is there if you switch."}
      </Text>
      {props.style === "color" ? colourGrid : emojiGrid}
      {props.style === "emoji" && !choices && !loadError && (
        <Group justify="center" py="lg">
          <Loader size="sm" aria-label="Loading emoji" />
        </Group>
      )}
      <Text size="xs" c="dimmed">
        A dot marks a colour or glyph another checkout of this repo already uses.
        Picking it is allowed — the rail just won&apos;t identify them apart.
      </Text>
    </>
  );
}

/**
 * Change an existing worktree's marker.
 *
 * Every pick writes immediately and closes — there is no Save button, because a
 * marker is one value and a dialog that made you confirm a swatch would be a worse
 * version of clicking it.
 */
export function ChangeMarkerDialog(props: {
  current: string;
  currentColor: string;
  /** The worktree's rendered name (`worktreeLabel`), for the dialog title. */
  label: string;
  worktreeId: number;
  usedBy: Record<string, EmojiHolder[]>;
  colorUsedBy: Record<string, EmojiHolder[]>;
  style: MarkerStyle;
  /** Switch the rail's marker face — see `MarkerGrids`. */
  onStyleChange: (style: MarkerStyle) => void;
  onPick: (patch: { emoji?: string; marker_color?: string }) => Promise<void>;
  onClose: () => void;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const loaded = useMarkerChoices();

  const pick = async (patch: { emoji?: string; marker_color?: string }) => {
    // The key the grid spins is the value being written, whichever face it is.
    setBusy(patch.emoji ?? patch.marker_color ?? "");
    setError(null);
    try {
      await props.onPick(patch);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(null);
    }
  };

  return (
    <Modal title={`Marker for ${props.label}`} onClose={props.onClose}>
      <Stack gap="sm">
        <MarkerGrids
          emoji={props.current}
          color={props.currentColor}
          usedBy={props.usedBy}
          colorUsedBy={props.colorUsedBy}
          worktreeId={props.worktreeId}
          style={props.style}
          onStyleChange={props.onStyleChange}
          busy={busy}
          loaded={loaded}
          onPick={(patch) => void pick(patch)}
        />
        <ErrorText error={error} />
        <Text size="xs" c="dimmed">
          Both halves are always saved, so the one you aren&apos;t currently
          showing is waiting if you switch the face up top.
        </Text>
      </Stack>
    </Modal>
  );
}

/**
 * Edit a worktree's two names, and bin it.
 *
 * Two fields because a worktree has two names and they answer different
 * questions. **Name** is what the rail shows and is free text; **Alias** is the
 * identifier — it defaults the run name, so it reaches a hostname, and the daemon
 * bounds it to `[A-Za-z0-9._-]` and refuses one a sibling already holds.
 *
 * The Name field is clearable, and clearing it is a real operation rather than a
 * no-op: an empty name takes the row back to rendering its alias, which is the
 * state every checkout created before v13 is already in and the only way back to
 * it. The placeholder says which alias it would fall back to, so "empty" never
 * looks like "nameless".
 */
/**
 * One file that would stop `git worktree remove`, with a stable label.
 *
 * Used by both the trash confirmation and the delete confirmation, so the two
 * surfaces cannot drift apart in how they present the same list.
 */
function DirtyFileList(props: { files: DirtyFile[] }) {
  return (
    <ScrollArea.Autosize mah={150} type="hover" scrollbarSize={6}>
      <Stack gap={2}>
        {props.files.map((f) => (
          <Group key={f.path} gap={8} wrap="nowrap" align="center">
            <Badge
              size="xs"
              variant="light"
              color={kindColor(f.kind)}
              style={{ flex: "none" }}
            >
              {f.kind}
            </Badge>
            <Text
              size="xs"
              style={{
                fontFamily: "var(--mantine-font-family-monospace)",
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
            >
              {f.path}
            </Text>
          </Group>
        ))}
      </Stack>
    </ScrollArea.Autosize>
  );
}

/** Colour the change kind so untracked throwaways read differently from edits. */
function kindColor(kind: string): string {
  switch (kind) {
    case "modified":
    case "deleted":
      return "red";
    case "untracked":
      return "orange";
    case "added":
      return "blue";
    case "renamed":
    case "copied":
      return "teal";
    case "conflicted":
      return "grape";
    default:
      return "gray";
  }
}

/**
 * The destructive confirm for deleting a *trashed* worktree that is dirty.
 *
 * Deletion is where the dirty state actually bites: `git worktree remove`
 * refuses on uncommitted files, so today the user only found out *after* the
 * attempt failed and the row came back with `trash_error`. This surfaces the
 * files up front and turns the refusal into a choice — delete and discard, or
 * revert first and delete cleanly.
 */
export function ConfirmDeleteWorktreeDialog(props: {
  /** The rail label, so the dialog says which worktree it means. */
  label: string;
  status: WorktreeGitStatus;
  onClose: () => void;
  /** Delete now, discarding the uncommitted changes (`git worktree remove --force`). */
  onDeleteDiscard: () => Promise<void>;
  /** Revert the changes, then delete the now-clean worktree. */
  onRevertThenDelete: () => Promise<void>;
}) {
  const discard = useSubmit(props.onDeleteDiscard);
  const revert = useSubmit(props.onRevertThenDelete);
  const count = props.status.files.length;
  const error = discard.error ?? revert.error;
  return (
    <Modal title="Delete worktree" onClose={props.onClose}>
      <Stack gap="md">
        <Alert color="yellow" variant="light" p="sm">
          <Text size="sm" fw={600}>
            “{props.label}” has {count} uncommitted change
            {count === 1 ? "" : "s"} a delete would discard.
          </Text>
          <Text size="xs" c="dimmed" mt={2}>
            These files are not saved to git yet.
          </Text>
          <div style={{ marginTop: 8 }}>
            <DirtyFileList files={props.status.files} />
          </div>
        </Alert>
        <Text size="sm" c="dimmed">
          How do you want to proceed?
        </Text>
        <Group>
          <Tooltip
            label="Deletes the worktree now and discards these uncommitted changes. This cannot be undone."
            multiline
            w={260}
          >
            <Button
              color="red"
              loading={discard.busy}
              onClick={(e) => {
                e.preventDefault();
                void discard.submit(e);
              }}
            >
              Delete and discard changes
            </Button>
          </Tooltip>
          <Tooltip
            label="Reverts these changes first (reset to the last commit, remove untracked files), then deletes the worktree."
            multiline
            w={260}
          >
            <Button
              color="yellow"
              variant="light"
              loading={revert.busy}
              onClick={(e) => {
                e.preventDefault();
                void revert.submit(e);
              }}
            >
              Revert, then delete
            </Button>
          </Tooltip>
        </Group>
        <ErrorText error={error} />
      </Stack>
    </Modal>
  );
}

/**
 * Warn before fast-forwarding main over a dirty repo root.
 *
 * `merge --ff-only` refuses on uncommitted changes the same way
 * `git worktree remove` does, so this mirrors the trash flow's dirty confirm:
 * show what is in the way and let the user revert first instead of hitting
 * the daemon's refusal blind. There is no "update anyway" — reverting is the
 * only path that makes the fast-forward possible, unlike the trash flow's
 * discard-and-proceed option.
 */
export function UpdateMainDirtyDialog(props: {
  status: WorktreeGitStatus;
  onClose: () => void;
  /** Revert the repo root's changes, then fetch and fast-forward main. */
  onRevertThenUpdate: () => Promise<void>;
}) {
  const revert = useSubmit(props.onRevertThenUpdate);
  const count = props.status.files.length;
  return (
    <Modal title="Update main" onClose={props.onClose}>
      <Stack gap="md">
        <Alert color="yellow" variant="light" p="sm">
          <Text size="sm" fw={600}>
            The main checkout has {count} uncommitted change
            {count === 1 ? "" : "s"} a fast-forward would refuse on.
          </Text>
          <Text size="xs" c="dimmed" mt={2}>
            These files are not saved to git yet.
          </Text>
          <div style={{ marginTop: 8 }}>
            <DirtyFileList files={props.status.files} />
          </div>
        </Alert>
        <Text size="sm" c="dimmed">
          Revert these changes and update main, or cancel and handle them
          yourself first.
        </Text>
        <Group>
          <Tooltip
            label="Reverts these changes (reset to the last commit, remove untracked files), then fetches and fast-forwards main. This cannot be undone."
            multiline
            w={260}
          >
            <Button
              color="yellow"
              variant="light"
              loading={revert.busy}
              onClick={(e) => {
                e.preventDefault();
                void revert.submit(e);
              }}
            >
              Revert changes, then update main
            </Button>
          </Tooltip>
          <Button variant="default" onClick={props.onClose}>
            Cancel
          </Button>
        </Group>
        <ErrorText error={revert.error} />
      </Stack>
    </Modal>
  );
}

/**
 * Edit a worktree's name and alias. Purely editing — trashing/deletion has its
 * own dialog ([`TrashWorktreeDialog`]), so a rename action can never
 * accidentally read as a delete, and the trash flow is never buried under a
 * rename form.
 */
export function RenameWorktreeDialog(props: {
  currentAlias: string;
  /** The stored `display_name`, `""` when the row renders its alias. */
  currentName: string;
  /** Only the fields the user actually changed; an absent key is "leave it". */
  onRename: (patch: {
    alias?: string;
    display_name?: string;
  }) => Promise<void>;
  onClose: () => void;
}) {
  const [alias, setAlias] = useState(props.currentAlias);
  const [name, setName] = useState(props.currentName);
  const rename = useSubmit(() => {
    // **Only the fields that changed.** Both values are a snapshot taken when
    // the dialog opened, and this app runs up to eight windows against one
    // daemon — so sending both unconditionally means opening Edit here, renaming
    // in another window, then changing only the Alias reverts the other window's
    // rename with a value that was already stale when it was read. Omitting an
    // untouched field turns that into the no-op it should be, because the
    // daemon's `COALESCE` leaves an absent column alone.
    const patch: { alias?: string; display_name?: string } = {};
    if (alias.trim() !== props.currentAlias) patch.alias = alias.trim();
    const derived = deriveDisplayName(name);
    if (derived !== props.currentName) patch.display_name = derived;
    // An empty patch is a 400 ("nothing to update"), and it is also just a Save
    // with nothing typed — close rather than report a rejection for it.
    if (patch.alias === undefined && patch.display_name === undefined) {
      props.onClose();
      return Promise.resolve();
    }
    return props.onRename(patch);
  });
  return (
    <Modal title="Edit worktree" onClose={props.onClose}>
      <form onSubmit={rename.submit}>
        <Stack gap="sm">
          <TextInput
            label="Name"
            description="What the rail shows. Clear it to show the alias instead."
            placeholder={props.currentAlias}
            value={name}
            onChange={(e) => setName(e.currentTarget.value)}
            data-autofocus
          />
          <TextInput
            label="Alias"
            description="The identifier: letters, digits, '-', '_', '.'. Defaults the run name, so it ends up in a hostname."
            value={alias}
            onChange={(e) => setAlias(e.currentTarget.value)}
            styles={{ input: { fontFamily: "var(--mantine-font-family-monospace)" } }}
          />
          <ErrorText error={rename.error} />
          <Button type="submit" loading={rename.busy} disabled={!alias.trim()}>
            Save
          </Button>
        </Stack>
      </form>
    </Modal>
  );
}

/**
 * The dedicated trash confirmation, split out of the edit dialog.
 *
 * Opens straight into the decision (no collapsed button): it exists to be shown
 * when the user chooses to trash a checkout, so it fetches the dirty state on
 * mount and turns the refusal the user would otherwise hit later into a choice
 * now — trash anyway, or revert first.
 */
export function TrashWorktreeDialog(props: {
  /** The row's id, to fetch this worktree's git dirty state on demand. */
  worktreeId: number;
  /** Fetch the worktree's git dirty state (the files blocking deletion). */
  onStatus: (id: number) => Promise<WorktreeGitStatus>;
  /** Discard the worktree's uncommitted changes; returns the new status. */
  onRevert: (id: number) => Promise<WorktreeGitStatus>;
  /** Bin the worktree, or with `force` delete it outright. */
  onTrash: (force: boolean) => Promise<void>;
  /**
   * Why the last deletion failed, or `""`.
   *
   * Load-bearing, not decoration. Deletion happens later than the click — on the
   * retention sweep or from the trash — so git's refusal arrives on the row and can
   * never land in this dialog's own `useSubmit` error. Gating the force checkbox on
   * that error made `?force=true` unreachable: click → 202 → dialog closes →
   * reopening gives a fresh, empty error. This is the durable record of the refusal,
   * and it is what keeps forcing an answer to something the user has been shown
   * rather than a checkbox offered up front.
   */
  trashError: string;
  onClose: () => void;
}) {
  const [force, setForce] = useState(false);
  const [dirty, setDirty] = useState<WorktreeGitStatus | null>(null);
  const [statusLoading, setStatusLoading] = useState(true);
  useEffect(() => {
    let cancelled = false;
    props
      .onStatus(props.worktreeId)
      .then((s) => {
        if (!cancelled) setDirty(s);
      })
      // An unavailable status (git error, checkout gone) must not block the
      // trash: binning is non-destructive, so the panel degrades to the plain
      // "Move to trash" flow and any refusal surfaces later on the row.
      .catch(() => {})
      .finally(() => {
        if (!cancelled) setStatusLoading(false);
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.worktreeId]);

  const del = useSubmit(() => props.onTrash(force));
  // Either source of a refusal: this attempt's own (a 4xx from the forced path, or
  // a rejected precondition) or the last background attempt's.
  const refusal = del.error ?? (props.trashError || null);

  // Discard the worktree's changes, then bin it. Destructive by nature, which
  // is exactly what the tooltip on the button spells out — see the button below.
  const revertAndTrash = useSubmit(async () => {
    await props.onRevert(props.worktreeId); // throws on failure; dialog stays open
    await props.onTrash(false); // bin the now-clean worktree
  });

  const dirtyFiles = dirty && dirty.dirty ? dirty.files : [];
  return (
    <Modal title="Move to trash" onClose={props.onClose}>
      <Stack gap="sm">
        <Text size="sm" c="dimmed">
          Moves the checkout to the trash. Nothing is deleted yet — it stays on
          disk and you can restore it from the rail. It is deleted for good when
          its retention period runs out (Settings → General, off by default) or
          when you delete it from the trash. The branch itself is always kept.
          {force
            ? " Forcing deletes it right now and discards uncommitted changes; it will not start if an environment is still running."
            : ""}
        </Text>

        {statusLoading && (
          <Group gap="xs">
            <Loader size="xs" />
            <Text size="sm" c="dimmed">
              Checking for uncommitted changes…
            </Text>
          </Group>
        )}

        {!statusLoading && dirtyFiles.length > 0 && (
          <Alert color="yellow" variant="light" p="sm">
            <Text size="sm" fw={600}>
              {dirtyFiles.length} uncommitted change
              {dirtyFiles.length === 1 ? "" : "s"} would block a permanent
              delete
            </Text>
            <Text size="xs" c="dimmed" mt={2}>
              Trashing keeps these on disk — you can still change your mind.
              But a later permanent delete will refuse until they are reverted
              or discarded.
            </Text>
            <div style={{ marginTop: 8 }}>
              <DirtyFileList files={dirtyFiles} />
            </div>
          </Alert>
        )}

        <ErrorText error={refusal} />
        {refusal && (
          <Checkbox
            color="red"
            label="Delete now, discarding uncommitted changes"
            checked={force}
            onChange={(e) => setForce(e.currentTarget.checked)}
          />
        )}

        {!statusLoading && dirtyFiles.length > 0 && (
          <Tooltip
            label="Discards this worktree's uncommitted changes: tracked files are reset to the last commit and untracked files are removed. This cannot be undone — but it is what lets a later delete succeed cleanly."
            multiline
            w={280}
          >
            <Button
              color="yellow"
              variant="light"
              loading={revertAndTrash.busy}
              onClick={(e) => {
                e.preventDefault();
                void revertAndTrash.submit(e);
              }}
            >
              Revert changes, then trash
            </Button>
          </Tooltip>
        )}
        {revertAndTrash.error && <ErrorText error={revertAndTrash.error} />}

        <Button
          color="red"
          variant="light"
          loading={del.busy}
          onClick={(e) => {
            e.preventDefault();
            void del.submit(e);
          }}
        >
          {force
            ? "Delete permanently"
            : dirtyFiles.length > 0
              ? "Trash anyway"
              : "Move to trash"}
        </Button>
      </Stack>
    </Modal>
  );
}

/**
 * Create or rename a rail lane.
 *
 * One component for both because the only difference is the initial value and
 * the button label — and because the collision rule has to be identical in
 * both, which two components would eventually get wrong.
 *
 * The taken-name check here is a courtesy, not the enforcement: `create_lane`
 * and `rename_lane` decide inside their transaction, so two windows creating
 * "review" at once still resolve correctly. Checking here only means the user
 * finds out before pressing the button.
 */
export function LaneNameDialog(props: {
  title: string;
  confirmLabel: string;
  initial: string;
  /** Existing lane names, excluding the one being renamed. */
  taken: string[];
  onSubmit: (name: string) => Promise<void>;
  onClose: () => void;
}) {
  const [name, setName] = useState(props.initial);
  const trimmed = name.trim();
  // Case-insensitive, matching the daemon: two rail headers differing only in
  // case is a mistake every time.
  const collides = props.taken.some(
    (n) => n.toLowerCase() === trimmed.toLowerCase(),
  );
  const { busy, error, submit } = useSubmit(() => props.onSubmit(trimmed));
  return (
    <Modal title={props.title} onClose={props.onClose}>
      <form onSubmit={submit}>
        <Stack gap="sm">
          <TextInput
            label="Group name"
            placeholder="review"
            value={name}
            maxLength={MAX_LANE_NAME_LEN}
            onChange={(e) => setName(e.currentTarget.value)}
            error={collides ? "This repo already has a lane with that name" : null}
            data-autofocus
          />
          <Text size="sm" c="dimmed">
            Groups organise the worktrees in the rail. They are yours, not the
            repository&apos;s — nothing is written to the project&apos;s config, so
            your groups never show up in someone else&apos;s checkout.
          </Text>
          <ErrorText error={error} />
          <Button type="submit" loading={busy} disabled={!trimmed || collides}>
            {props.confirmLabel}
          </Button>
        </Stack>
      </form>
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// Batch actions on a whole rail section
// ---------------------------------------------------------------------------

/**
 * The picker value meaning "no destination chosen yet".
 *
 * The batch move has no preselected target on purpose — moving a whole group
 * somewhere is not a gesture that should have a default sitting under the cursor
 * — and `""` is a *real* value here (ungrouped), so "nothing chosen" cannot be
 * spelled with an empty string. A leading NUL can never be a lane name
 * (`valid_lane_name` rejects control characters), the same argument the rail's
 * own sentinel lanes make, so this can never collide with a destination.
 */
const NO_TARGET = "\u0000none";

/** The scroll box both batch dialogs show their "what is about to happen" list in. */
function BatchList(props: { children: ReactNode }) {
  return (
    <ScrollArea.Autosize mah={168} type="auto">
      <Stack gap={4}>{props.children}</Stack>
    </ScrollArea.Autosize>
  );
}

/**
 * Move every worktree in one rail section into another group.
 *
 * What will move is listed before anything happens, for the same reason
 * `NewWorktreeDialog` renders its derived names: a batch action whose size you
 * learn afterwards is one you stop using. The caller reads that list from live
 * state, so a checkout the 5s poll adds while this is open appears here too.
 */
export function MoveLaneWorktreesDialog(props: {
  /** Header text of the section the worktrees are leaving. */
  from: string;
  /** Rendered labels of the worktrees that will move, in rail order. */
  worktrees: string[];
  /**
   * Destinations, in rail order — see `bulkMoveTargets`. Never empty here: the
   * menu entry that opens this dialog is disabled when there is nowhere to go.
   */
  targets: Array<{ value: string; label: string }>;
  onMove: (lane: string) => Promise<void>;
  onClose: () => void;
}) {
  const [to, setTo] = useState(NO_TARGET);
  const chosen = to === NO_TARGET ? null : to;
  const { busy, error, submit } = useSubmit(() => props.onMove(chosen ?? ""));
  const n = props.worktrees.length;
  return (
    <Modal title={`Move everything in ${props.from}`} onClose={props.onClose}>
      <form onSubmit={submit}>
        <Stack gap="sm">
          <Text size="sm" c="dimmed">
            Moves {n === 1 ? "this worktree" : `all ${n} worktrees`} into another
            group. Nothing is created, deleted or checked out — only where the rows
            sit in the rail changes. Each one loses the position you dragged it to,
            because a position only means something inside one group.
          </Text>
          <Radio.Group value={to} onChange={setTo} label="Move to">
            <Stack gap={6} mt={6}>
              {props.targets.map((t) => (
                <Radio key={t.value} value={t.value} label={t.label} />
              ))}
            </Stack>
          </Radio.Group>
          <Text size="xs" c="dimmed">
            {n === 1 ? "Moving:" : `Moving ${n}:`}
          </Text>
          <BatchList>
            {props.worktrees.map((label) => (
              <Text key={label} size="xs" c="dimmed">
                {label}
              </Text>
            ))}
          </BatchList>
          <ErrorText error={error} />
          <Button type="submit" loading={busy} disabled={chosen === null}>
            {n === 1 ? "Move 1 worktree" : `Move ${n} worktrees`}
          </Button>
        </Stack>
      </form>
    </Modal>
  );
}

/**
 * Move every worktree in one rail section to the trash.
 *
 * Confirmed, unlike the Detached lane's "trash all" button beside it: that lane
 * holds throwaways by definition, and a group holds whatever the user filed into
 * it.
 *
 * The uncommitted-changes check is why this is more than a yes/no. Binning never
 * refuses — `DELETE /api/worktrees/{id}` without `force` marks the row and
 * returns, leaving the checkout on disk — so a dirty worktree bins silently, and
 * the single-row flows in this app deliberately never let that happen. The
 * statuses are fetched while the dialog is open and reported per row, so the
 * batch says which checkouts carry work before it takes them out of the rail.
 */
export function TrashLaneWorktreesDialog(props: {
  /** Header text of the section being emptied. */
  from: string;
  /** Exactly what will be binned — `bulkTrashable`, so main is already out. */
  worktrees: Array<{ id: number; label: string }>;
  /** Whether the section also holds the main checkout, which is never binned. */
  mainExcluded: boolean;
  /** Fetch one worktree's git dirty state (the files blocking a later delete). */
  onStatus: (id: number) => Promise<WorktreeGitStatus>;
  onTrash: () => Promise<void>;
  onClose: () => void;
}) {
  const [dirty, setDirty] = useState<ReadonlySet<number>>(new Set());
  const [checking, setChecking] = useState(true);
  // The ids as a stable dependency: `props.worktrees` is derived by the caller on
  // every render, so depending on the array itself would refetch forever.
  const idKey = props.worktrees.map((w) => w.id).join(",");
  useEffect(() => {
    let cancelled = false;
    const ids = idKey === "" ? [] : idKey.split(",").map(Number);
    setChecking(true);
    // In parallel, and failures ignored: an unavailable status (git error,
    // checkout gone) must not block a bin, which is non-destructive anyway. That
    // row simply carries no badge — the same degradation `TrashWorktreeDialog`
    // takes for a single checkout.
    void Promise.allSettled(ids.map((id) => props.onStatus(id))).then((results) => {
      if (cancelled) return;
      const found = new Set<number>();
      results.forEach((r, i) => {
        if (r.status === "fulfilled" && r.value.dirty) found.add(ids[i]);
      });
      setDirty(found);
      setChecking(false);
    });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [idKey]);

  const { busy, error, submit } = useSubmit(() => props.onTrash());
  const n = props.worktrees.length;
  return (
    <Modal
      title={
        n === 1
          ? `Move 1 worktree from ${props.from} to the trash?`
          : `Move ${n} worktrees from ${props.from} to the trash?`
      }
      onClose={props.onClose}
    >
      <form onSubmit={submit}>
        <Stack gap="sm">
          <Text size="sm" c="dimmed">
            Nothing is deleted. Every checkout stays on disk and can be restored
            from the trash in the rail. They are deleted for good when the
            retention period runs out (Settings → General, off by default) or when
            you delete them from the trash. The branches are always kept.
          </Text>
          {props.mainExcluded && (
            <Text size="sm" c="dimmed">
              The main checkout stays where it is — it is the repository itself, so
              it is never trashed.
            </Text>
          )}
          {checking && (
            <Group gap="xs">
              <Loader size="xs" />
              <Text size="sm" c="dimmed">
                Checking for uncommitted changes…
              </Text>
            </Group>
          )}
          {!checking && dirty.size > 0 && (
            <Alert color="yellow" variant="light" p="sm">
              <Text size="sm" fw={600}>
                {dirty.size} of these {dirty.size === 1 ? "has" : "have"}{" "}
                uncommitted changes
              </Text>
              <Text size="xs" c="dimmed" mt={2}>
                Trashing keeps them — nothing is lost now. But a later permanent
                delete refuses until those changes are reverted or discarded, so
                the rows sit in the trash until you deal with them.
              </Text>
            </Alert>
          )}
          <BatchList>
            {props.worktrees.map((w) => (
              <Group key={w.id} gap={6} wrap="nowrap">
                <Text size="xs" c="dimmed">
                  {w.label}
                </Text>
                {dirty.has(w.id) && (
                  <Badge size="xs" color="yellow" variant="light">
                    uncommitted changes
                  </Badge>
                )}
              </Group>
            ))}
          </BatchList>
          <ErrorText error={error} />
          <Button type="submit" color="red" variant="light" loading={busy}>
            {n === 1
              ? "Move 1 worktree to trash"
              : `Move ${n} worktrees to trash`}
          </Button>
        </Stack>
      </form>
    </Modal>
  );
}

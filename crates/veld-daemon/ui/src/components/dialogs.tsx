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
  type DbHealth,
  type DirtyFile,
  type EmojiHolder,
  type Repo,
  type WorktreeGitStatus,
} from "../api";
import { describeAge } from "../dbhealth/model";
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

/**
 * What veld knows about its own database, and the one button that puts a backup
 * back.
 *
 * **This dialog's job is to be honest about the cost.** Restoring is not a
 * repair — it replaces the live file with an older copy, so everything veld
 * learned since that copy was taken is gone: worktrees created, lanes arranged,
 * settings changed, run history. The age of the candidate is therefore the most
 * important thing on the screen, and it is stated in the button's own sentence
 * rather than buried in prose above it.
 */
export function DatabaseHealthDialog(props: {
  health: DbHealth;
  /** The command that brings this daemon back, when nothing will do it for you.
   *  Named here as well as in the toast: this is the screen where somebody
   *  decides whether to go ahead. */
  restartHint?: string;
  /** `undefined` when the action is not being offered — either there is nothing
   *  restorable, or the fault does not call for it (a failing backup schedule
   *  over a healthy database). The two produce different copy below, because
   *  telling somebody no copy passed its check when one did is a lie. */
  onRestore?: () => Promise<void>;
  onClose: () => void;
}) {
  const { busy, error, submit } = useSubmit(() => props.onRestore?.() ?? Promise.resolve());
  const { database, backups, restore } = props.health;
  const candidate = restore?.candidate ?? null;

  return (
    <Modal title="Veld's database" onClose={props.onClose} size={620}>
      <Stack gap="sm">
        {database.state !== "ok" && (
          <Alert color="red" variant="light" p="sm">
            <Text size="sm">
              SQLite reports this database as damaged. Veld keeps working for
              everything that does not touch the damaged part, which is why this
              can go unnoticed — but writes that reach it fail, and the next
              thing to fail may be one you care about.
            </Text>
            <Text size="sm" mt={6}>
              While it reads as damaged, Veld stops pruning old logs and stops
              reclaiming free pages: that housekeeping relocates pages, and this
              file's page map can no longer be trusted. Running environments
              keep writing their logs, so the database may grow until you
              restore it.
            </Text>
          </Alert>
        )}

        <Stack gap={2}>
          <Text size="xs" c="dimmed">
            Database
          </Text>
          <Text size="xs" ff="monospace">
            {database.path || "unknown"}
          </Text>
        </Stack>

        {database.detail && (
          <Stack gap={2}>
            <Text size="xs" c="dimmed">
              What SQLite said
            </Text>
            <Text size="xs" ff="monospace" style={{ whiteSpace: "pre-wrap" }}>
              {database.detail}
            </Text>
            {database.firstSeen && (
              <Text size="xs" c="dimmed">
                First seen {describeAge(database.firstSeen)}
                {database.hits > 1 ? ` · ${database.hits} occurrences since` : ""}
              </Text>
            )}
          </Stack>
        )}

        <Stack gap={2}>
          <Text size="xs" c="dimmed">
            Backups
          </Text>
          <Text size="sm">
            {backups.lastOk
              ? `Last successful backup taken ${describeAge(backups.lastOk)}`
              : backups.newest
                ? `Newest copy on disk taken ${describeAge(backups.newest.takenAt)}`
                : "No backups have been written"}
            {backups.state === "off" ? " · switched off in settings" : ""}
          </Text>
          {backups.lastError && (
            <Text size="xs" c="red" style={{ whiteSpace: "pre-wrap" }}>
              {backups.lastError}
            </Text>
          )}
        </Stack>

        <ErrorText error={error} />

        {candidate && props.onRestore ? (
          <form onSubmit={submit}>
            <Stack gap="xs">
              <Text size="sm">
                Restoring replaces the database with the copy taken{" "}
                <strong>{describeAge(candidate.takenAt)}</strong>. Anything veld
                learned since then — worktrees, lanes, settings, run history — is
                lost. The database that is there now is kept, renamed, not
                deleted.
              </Text>
              {/* Verified against a real artifact: a backup carries `runs`,
                  `nodes` and `environments`, so this is not just history. A run
                  started after that copy was taken has no row in it, and its
                  processes keep going while veld no longer knows about them —
                  which is a leak the user can avoid by stopping first, and
                  cannot avoid if nobody tells them. */}
              <Text size="sm">
                <strong>Stop your running environments first.</strong> A run
                started since that copy was taken is not in it, so veld would
                lose track of one that is still running.
              </Text>
              <Text size="xs" c="dimmed">
                Your machine will ask you to confirm in a dialog of its own —
                replacing the database is not something a page is allowed to do
                on its own.{" "}
                {restore.restartsAutomatically
                  ? "Veld's daemon restarts itself afterwards; your terminals are left alone."
                  : props.restartHint
                    ? `Nothing is managing this daemon, so it will not come back on its own — start it again afterwards with \`${props.restartHint}\`.`
                    : // A dev instance. Deliberately no command: the daemon here is
                      // a node of a veld run, and the presets that start it often
                      // recreate this database (`dev-db:fresh`, `dev-db:from-real`),
                      // which would replace the copy being restored right now.
                      "This daemon is part of a veld run, so the run stops with it. Start that run again the way you started it — and check its selections first: `dev-db:fresh` and `dev-db:from-real` recreate this database, which would undo the restore."}
              </Text>
              <Button type="submit" color="red" variant="light" loading={busy}>
                Restore the backup from {describeAge(candidate.takenAt)}
              </Button>
            </Stack>
          </form>
        ) : candidate ? (
          // **A candidate exists and the action is still withheld**, which is a
          // different sentence from "there is nothing to restore". This is the
          // backups-failing state: the live database is fine, so putting an old
          // copy over it would *lose* state rather than recover any. Saying
          // "none of the copies passed their integrity check" here — which the
          // single fallback used to say — is simply false, and one of them is
          // sitting in `candidate`.
          <Text size="sm" c="dimmed">
            Nothing here needs restoring — the database itself is intact. The
            newest usable backup was taken {describeAge(candidate.takenAt)}; use{" "}
            <Text span ff="monospace" size="sm">
              veld backup restore
            </Text>{" "}
            if you deliberately want to go back to it.
          </Text>
        ) : backups.newest ? (
          <Text size="sm" c="dimmed">
            There is no backup that can be restored — none of the copies on disk
            passed their integrity check. Copy the database aside before doing
            anything else, then see <Text span ff="monospace" size="sm">veld backup</Text>.
          </Text>
        ) : (
          // **"No copy passed the check" is false when there is no copy.** Every
          // attempt failing since install (an unwritable `backup.dir`) leaves
          // `newest` empty and `candidate` empty, which landed on the sentence
          // above and blamed a check that never ran — the same false-claim class
          // the branch above this one was added to remove.
          <Text size="sm" c="dimmed">
            There are no backups to restore from — none has been written yet. Copy
            the database aside before doing anything else, then see{" "}
            <Text span ff="monospace" size="sm">
              veld backup
            </Text>{" "}
            for why the schedule is not producing any.
          </Text>
        )}
      </Stack>
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

/** The picker value for "make a new group and move them into that". */
const NEW_TARGET = "\u0000new";

/** How many `git status` calls the trash confirmation runs at once. */
const STATUS_CONCURRENCY = 4;

/**
 * How long the trash confirmation's submit waits for those calls.
 *
 * A gate, **not** a give-up: the checks carry on past it and their results keep
 * landing, so the badge list only ever gains rows. That is what makes a short
 * value safe — and a give-up was the wrong shape twice over. `api.request` has no
 * timeout, so one hung `git status` (an `index.lock` a dead rebase left behind, a
 * stale network mount) must not hold the batch hostage; but abandoning the check
 * on a fixed deadline made a *large* group — the case where the warning matters
 * most, and the slowest to answer — the one that quietly stopped getting it.
 */
const STATUS_GATE_MS = 5000;

/**
 * Freeze a derived list for as long as an action on it is in flight.
 *
 * Both batch dialogs render a list their caller re-derives from live state on
 * every 5s poll — which is what makes a checkout added while the dialog is open
 * appear in it. That is right up until the batch actually runs: the poll then
 * shrinks the list *underneath the request that is emptying it*, so the dialog
 * swapped to "the last worktree left while this was open" halfway through the
 * batch doing exactly that, and the loading button unmounted with it — a long
 * batch looked idle and then looked like it had never run.
 *
 * A render-phase ref write, deliberately: this is a cache of the last value seen
 * while idle, not state anything renders differently, so there is nothing to
 * schedule and an effect would land one render too late.
 */
function useLatched<T>(value: T, frozen: boolean): T {
  const held = useRef(value);
  if (!frozen) held.current = value;
  return held.current;
}

/** The scroll box both batch dialogs show their "what is about to happen" list in. */
function BatchList(props: { children: ReactNode }) {
  return (
    <ScrollArea.Autosize mah={168} type="auto">
      <Stack gap={4}>{props.children}</Stack>
    </ScrollArea.Autosize>
  );
}

/**
 * The line both batch dialogs show when the section holds detached checkouts.
 *
 * They are filed into this section but are not members of it — a detached HEAD
 * moves a row into the virtual Detached lane — so a batch cannot act on them,
 * and their `lane` keeps pointing here. Said up front, because the surprise
 * otherwise arrives much later: check a branch out again and the row is back in
 * a group you emptied. See `detachedInSection`.
 */
function DetachedNote(props: { count: number; verb: string }) {
  if (props.count === 0) return null;
  return (
    <Text size="sm" c="dimmed">
      {props.count === 1
        ? "One detached checkout is filed here"
        : `${props.count} detached checkouts are filed here`}{" "}
      but listed under <b>Detached</b> instead, so this cannot {props.verb}{" "}
      {props.count === 1 ? "it" : "them"}. Check a branch out again and{" "}
      {props.count === 1 ? "it comes" : "they come"} back here.
    </Text>
  );
}

/** What a batch move is aimed at: an existing group, or one to be created. */
export type BatchMoveTarget = { lane: string } | { newLane: string };

/**
 * Move every worktree in one rail section into another group.
 *
 * What will move is listed before anything happens, for the same reason
 * `NewWorktreeDialog` renders its derived names: a batch action whose size you
 * learn afterwards is one you stop using. The caller reads that list from live
 * state, so a checkout the 5s poll adds while this is open appears here too —
 * and if the poll empties the section entirely, the dialog says so and offers no
 * button rather than a "Move 0 worktrees" that does nothing.
 *
 * **"New group…" is one of the destinations**, mirroring the single-row *Move to
 * group* submenu. Without it the batch refused exactly the repo it is most useful
 * in: one that has never defined a group, and therefore has nothing to offer as
 * an existing target.
 */
export function MoveLaneWorktreesDialog(props: {
  /** Header text of the section the worktrees are leaving. */
  from: string;
  /** Exactly what will move, in rail order. */
  worktrees: Array<{ id: number; label: string }>;
  /** Existing destinations, in rail order — see `bulkMoveTargets`. May be empty. */
  targets: Array<{ value: string; label: string }>;
  /** Every lane name in the repo — the collision check for a new group. */
  taken: string[];
  /** Detached checkouts filed here that this cannot move — see `DetachedNote`. */
  detached: number;
  onMove: (target: BatchMoveTarget) => Promise<void>;
  onClose: () => void;
}) {
  const [to, setTo] = useState(NO_TARGET);
  const [newName, setNewName] = useState("");
  const making = to === NEW_TARGET;
  const trimmed = newName.trim();
  // Case-insensitive, matching the daemon and `LaneNameDialog`: two rail headers
  // differing only in case is a mistake every time. A courtesy check — the
  // daemon decides inside its transaction.
  const collides = props.taken.some(
    // Not `n`: that names the row count a few lines down, and a later read of it
    // inside this callback would silently get a lane name instead.
    (taken) => taken.toLowerCase() === trimmed.toLowerCase(),
  );
  const { busy, error, submit } = useSubmit(() =>
    props.onMove(making ? { newLane: trimmed } : { lane: to }),
  );
  const rows = useLatched(props.worktrees, busy);
  const n = rows.length;
  // `bulkMoveTargets` offers "No group" only when the source *is* a group, so on
  // the ungrouped section — the one most repos live in — there is no such
  // destination and the copy must not advertise one.
  const canUngroup = props.targets.some((t) => t.value === "");
  const ready =
    n > 0 &&
    (making
      ? trimmed !== "" && !collides
      : // Not merely "something is selected": `targets` is recomputed from live
        // lanes on every poll, so another window deleting or renaming the chosen
        // destination leaves `to` naming a lane that is no longer offered. The
        // radio then renders nothing selected while the button stayed enabled,
        // and every PATCH answered 400 `no such lane in this repo`.
        props.targets.some((t) => t.value === to));
  return (
    <Modal
      title={`Move everything in ${props.from}`}
      /* A no-op while the batch runs: Escape, the scrim and the ✕ all land here,
         and the loop firing the requests lives in the app rather than in this
         component — so closing would read as a cancel while every remaining
         request kept going. */
      onClose={busy ? () => {} : props.onClose}
    >
      <form onSubmit={submit}>
        <Stack gap="sm">
          {n === 0 ? (
            <Text size="sm" c="dimmed">
              There is nothing in {props.from} any more — the last worktree left
              it while this was open.
            </Text>
          ) : (
            <>
              <Text size="sm" c="dimmed">
                Moves {n === 1 ? "this worktree" : `all ${n} worktrees`}{" "}
                somewhere else in the rail — into another group
                {canUngroup ? ", or out of any group" : ""}. Nothing is created,
                deleted or checked out; only where the rows sit changes. Each one loses the
                position you dragged it to, because a position only means
                something inside one group.
              </Text>
              <DetachedNote count={props.detached} verb="move" />
              <Radio.Group value={to} onChange={setTo} label="Move to">
                <Stack gap={6} mt={6}>
                  {props.targets.map((t) => (
                    <Radio key={t.value} value={t.value} label={t.label} />
                  ))}
                  <Radio value={NEW_TARGET} label="New group…" />
                </Stack>
              </Radio.Group>
              {making && (
                <TextInput
                  label="Group name"
                  placeholder="review"
                  value={newName}
                  maxLength={MAX_LANE_NAME_LEN}
                  onChange={(e) => setNewName(e.currentTarget.value)}
                  error={
                    collides ? "This repo already has a group with that name" : null
                  }
                  /* `autoFocus`, not `data-autofocus`: this field mounts when
                     "New group…" is picked, long after the modal's focus trap has
                     already chosen where to land, so the trap's marker would
                     never fire for it. */
                  autoFocus
                />
              )}
              <Text size="xs" c="dimmed">
                {n === 1 ? "Moving:" : `Moving ${n}:`}
              </Text>
              <BatchList>
                {rows.map((w) => (
                  /* Keyed by id, not by the label: a display name is free text
                     with no uniqueness constraint, so two rows can render the
                     same string. */
                  <Text key={w.id} size="xs" c="dimmed">
                    {w.label}
                  </Text>
                ))}
              </BatchList>
            </>
          )}
          <ErrorText error={error} />
          {n > 0 && (
            <Button type="submit" loading={busy} disabled={!ready}>
              {n === 1 ? "Move 1 worktree" : `Move ${n} worktrees`}
            </Button>
          )}
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
 * statuses are fetched while the dialog is open and reported per row, and **the
 * button waits for them**: a group large enough for the fan-out to be slow is
 * exactly the one where binning before the badges arrive loses the warning this
 * dialog exists to give.
 */
export function TrashLaneWorktreesDialog(props: {
  /** Header text of the section being emptied. */
  from: string;
  /** Exactly what will be binned — `bulkTrashable`, so main is already out. */
  worktrees: Array<{ id: number; label: string }>;
  /** Whether the section also holds the main checkout, which is never binned. */
  mainExcluded: boolean;
  /** Detached checkouts filed here that this cannot bin — see `DetachedNote`. */
  detached: number;
  /** Fetch one worktree's git dirty state (the files blocking a later delete). */
  onStatus: (id: number) => Promise<WorktreeGitStatus>;
  onTrash: () => Promise<void>;
  onClose: () => void;
}) {
  const [dirty, setDirty] = useState<ReadonlySet<number>>(new Set());
  const [checking, setChecking] = useState(true);
  /** Past the gate, with checks still outstanding — see [`STATUS_GATE_MS`]. */
  const [pending, setPending] = useState(false);
  const { busy, error, submit } = useSubmit(() => props.onTrash());
  const rows = useLatched(props.worktrees, busy);
  // The ids as a stable dependency: `props.worktrees` is derived by the caller on
  // every render, so depending on the array itself would refetch forever. Read
  // off the latched rows, so a batch in flight cannot re-arm the whole check
  // against checkouts it is currently binning.
  const idKey = rows.map((w) => w.id).join(",");
  useEffect(() => {
    let cancelled = false;
    const ids = idKey === "" ? [] : idKey.split(",").map(Number);
    setChecking(true);
    setPending(false);
    setDirty(new Set());
    const found = new Set<number>();
    // Bounded, not `Promise.allSettled` over the whole list. Each call runs git
    // in a checkout on a machine that is already running this project's
    // environments, and a thirty-worktree group would start thirty at once — the
    // same contention argument that keeps the write loop in `App.tsx` sequential.
    let next = 0;
    const worker = async () => {
      while (next < ids.length && !cancelled) {
        const id = ids[next++];
        try {
          const status = await props.onStatus(id);
          if (status.dirty) {
            found.add(id);
            // Published as each answer lands, not only at the gate and at
            // completion. This is what makes the gate a gate: with one checkout
            // git is slow to answer for, the other workers drain the queue and
            // their findings still reach the list. Publishing only on settle
            // froze the badges at the gate's snapshot while the notice below
            // promised more were coming.
            if (!cancelled) setDirty(new Set(found));
          }
        } catch {
          // An unavailable status (git error, checkout gone) must not block a
          // bin, which is non-destructive anyway. That row simply carries no
          // badge — the same degradation `TrashWorktreeDialog` takes for one.
        }
      }
    };
    const all = Promise.all(
      Array.from({ length: Math.min(STATUS_CONCURRENCY, ids.length) }, worker),
    );
    // Two publishes, not a `Promise.race` picking one. The gate expiring only
    // *unblocks the button* — it must not stop the dialog reporting what the
    // remaining checks find, which is what a race did: it published `found` once
    // and left the workers filling a set nothing would ever render again, so a
    // badge learned a second later was known and hidden.
    // The gate only ungates — the badges are published by the workers above as
    // they land, so there is nothing for it to snapshot.
    const gate = setTimeout(() => {
      if (cancelled) return;
      setChecking(false);
      setPending(true);
    }, STATUS_GATE_MS);
    void all.then(() => {
      if (cancelled) return;
      clearTimeout(gate);
      // One last publish. Redundant while every dirty answer publishes itself
      // above — kept as the backstop for the case that stops being true, since
      // the failure it guards against (a badge known and never shown) is silent.
      setDirty(new Set(found));
      setChecking(false);
      setPending(false);
    });
    return () => {
      cancelled = true;
      clearTimeout(gate);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [idKey]);

  const n = rows.length;
  return (
    <Modal
      title={
        n === 0
          ? `Nothing left in ${props.from}`
          : n === 1
            ? `Move 1 worktree from ${props.from} to the trash?`
            : `Move ${n} worktrees from ${props.from} to the trash?`
      }
      /* A no-op while the batch runs — see the same guard on the move dialog. */
      onClose={busy ? () => {} : props.onClose}
    >
      <form onSubmit={submit}>
        <Stack gap="sm">
          {n === 0 ? (
            <Text size="sm" c="dimmed">
              {props.mainExcluded
                ? "Only the main checkout is left here, and that is the repository itself — it is never trashed."
                : "There is nothing left to trash here — the last worktree left this section while the dialog was open."}
            </Text>
          ) : (
            <>
              <Text size="sm" c="dimmed">
                Nothing is deleted. Every checkout stays on disk and can be
                restored from the trash in the rail. They are deleted for good
                when the retention period runs out (Settings → General, off by
                default) or when you delete them from the trash. The branches are
                always kept.
              </Text>
              {props.mainExcluded && (
                <Text size="sm" c="dimmed">
                  The main checkout stays where it is — it is the repository
                  itself, so it is never trashed.
                </Text>
              )}
              <DetachedNote count={props.detached} verb="trash" />
              {checking && (
                <Group gap="xs">
                  <Loader size="xs" />
                  <Text size="sm" c="dimmed">
                    Checking for uncommitted changes…
                  </Text>
                </Group>
              )}
              {pending && (
                /* A yellow Alert, the same weight as the warning it stands in
                   for: a dimmed line reporting an incomplete check is quieter
                   than the finding it stands in for, which is backwards. */
                <Alert color="yellow" variant="light" p="sm">
                  <Text size="sm" fw={600}>
                    Still checking the rest for uncommitted changes
                  </Text>
                  <Text size="xs" c="dimmed" mt={2}>
                    You can go ahead — nothing is deleted either way, and more
                    badges will appear below as the checks finish.
                  </Text>
                </Alert>
              )}
              {!checking && dirty.size > 0 && (
                <Alert color="yellow" variant="light" p="sm">
                  <Text size="sm" fw={600}>
                    {dirty.size} of these {dirty.size === 1 ? "has" : "have"}{" "}
                    uncommitted changes
                  </Text>
                  <Text size="xs" c="dimmed" mt={2}>
                    Trashing keeps them — nothing is lost now. But a later
                    permanent delete refuses until those changes are reverted or
                    discarded, so the rows sit in the trash until you deal with
                    them.
                  </Text>
                </Alert>
              )}
              <BatchList>
                {rows.map((w) => (
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
            </>
          )}
          <ErrorText error={error} />
          {n > 0 && (
            <Button
              type="submit"
              color="red"
              variant="light"
              loading={busy}
              /* Waits for the statuses. Binning before the badges land is
                 binning without the warning, and the slowest fan-out is the
                 biggest group — the case where it matters most. */
              disabled={checking}
            >
              {n === 1
                ? "Move 1 worktree to trash"
                : `Move ${n} worktrees to trash`}
            </Button>
          )}
        </Stack>
      </form>
    </Modal>
  );
}

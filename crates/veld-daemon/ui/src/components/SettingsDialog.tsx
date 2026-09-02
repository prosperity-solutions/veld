/**
 * The settings surface.
 *
 * A dialog rather than a route or a pane, deliberately: it reuses the existing
 * discriminated-union dialog state and the native-view suspension that comes with
 * it, so it adds no new surface machinery. (Turning `/ide` into a real router is
 * the honest long-term shape and a refactor of a 2500-line component — it would
 * have eaten this batch and shipped no settings.)
 *
 * **Every row is rendered from a catalog the daemon serves.** `GET
 * /api/settings/catalog` says what each setting is called, what it does, which
 * group and section it belongs to, what shape its value has and what a control
 * should offer for it — see `shared/catalog.ts` and the module it mirrors,
 * `veld-core/src/db/settings_catalog.rs`. Adding a setting is therefore a
 * **Rust-only edit**: no label, no help string and no `<Row>` is written here.
 *
 * What stays in the bundle is what a catalog cannot say:
 *
 * - **Painting.** The tab icons (keyed off each group's stable id), the panel's
 *   scroll shape, and the blurbs that introduce a *section* rather than a row.
 * - **Six controls a description cannot produce** — the shell and font pickers,
 *   the two folder pickers, the origin-list editor and the search-URL field. They
 *   live in {@link OVERRIDES}, keyed by setting key, and still take their title from
 *   the catalog. Four of the six are a `Choices::Runtime`, which is exactly Rust's
 *   way of saying "the client owns this one"; the other two are validators only the
 *   client can run. See that map's own comment.
 * - **Hardware facts.** Whether this machine has a battery is not a preference and
 *   no daemon catalog describes it as one, so the battery rows are filtered
 *   client-side in {@link HARDWARE_GATES}.
 * - **Facts about a chosen value that only the client can know.** Whether the
 *   selected font can draw ligatures is a property of a font file the daemon never
 *   sees — it stores a CSS font-family string — so that row is filtered in
 *   {@link CAPABILITY_GATES}. Same shape as the hardware gates and the same rule:
 *   an unknown answer shows the row rather than hiding it.
 *
 * A shape this build has never heard of renders as a visible "cannot show this"
 * row rather than vanishing (`Unsupported` below). That is the one failure mode
 * worth engineering against: a setting that exists in Rust and is *invisible*
 * here. The compiler asks first — the picker's `default:` branch assigns the
 * discriminant to `never`, so a new `Choices` variant in Rust stops this file
 * compiling until it has been given a control.
 *
 * **There is no Save button, and that is the design.** Every control writes its own
 * field the moment it changes. Two windows can have this open at once, and with a
 * whole-document save the second one to close would silently revert the first. With
 * per-field writes there is no save event to conflict: two windows editing
 * different settings both win, and the same setting resolves last-write-wins, which
 * is the right answer for a font size.
 *
 * Text and number inputs commit on blur or Enter rather than per keystroke, so a
 * three-digit font size is one request instead of three — and each request's
 * response carries the daemon's clamped value, which is what makes an out-of-range
 * entry visibly snap back instead of appearing to have been accepted.
 *
 * **One group is shown at a time**, picked from a sidebar. Every setting used to be
 * on one scroll, six headings deep, which meant the font size and the worktree trash
 * retention were the same distance away — and finding either was a scroll rather
 * than a choice. The groups are Mantine `Tabs` in vertical orientation, so the
 * sidebar is a real tablist: arrow keys move between groups and each panel is
 * labelled by its tab, which a hand-rolled column of buttons would not give.
 *
 * The narrow layout drops the sidebar for a select, and it is chosen in CSS
 * (`visibleFrom` / `hiddenFrom`) rather than by measuring the viewport. Both
 * controls drive the same `group` state, so there is no second source of truth —
 * and no first-paint flash, which a `matchMedia` read would have on a surface that
 * mounts already open. This is the UI's first breakpoint of any kind; it is
 * viewport-keyed rather than modal-width-keyed because the modal's width *is* the
 * viewport once it stops fitting.
 */

import {
  type ComponentType,
  type ReactNode,
  useEffect,
  useMemo,
  useState,
} from "react";
import {
  Button,
  Checkbox,
  Code,
  Group,
  NativeSelect,
  NumberInput,
  Slider,
  Stack,
  Tabs,
  Text,
  TextInput,
  Textarea,
} from "@mantine/core";
import {
  IconActivity,
  IconAdjustments,
  IconAppWindow,
  IconCoffee,
  IconGitBranch,
  IconLink,
  IconShare,
  IconTerminal2,
} from "@tabler/icons-react";

import {
  api,
  type SettingsDoc,
  type ShellIntercept,
  type ShellList,
} from "../api";
import { searchTarget } from "../panes/model";
import { Modal } from "./dialogs";
import {
  availableFonts,
  matchFont,
  showsLigatureRow,
  type TerminalFont,
} from "../shared/terminalFonts";
import {
  asBool,
  asNumber,
  asString,
  asStringList,
  type CatalogEntry,
  type Choice,
  presetOptions,
  requirementMet,
  settingValue,
  useCatalog,
} from "../shared/catalog";
import { useCaffeinate } from "../shared/useCaffeinate";

/**
 * A help string's `backticked` spans, as inline code.
 *
 * One help string serves every surface — `veld settings describe` wraps it into a
 * terminal, this puts it under a label — so it marks code up the one way both can
 * read. An odd number of backticks is not markup at all: rendering it as written
 * beats turning the rest of the sentence into a code span.
 */
function helpNodes(help: string): ReactNode {
  const parts = help.split("`");
  if (parts.length % 2 === 0) return help;
  return parts.map((part, i) =>
    i % 2 === 1 ? <Code key={`${i}:${part}`}>{part}</Code> : part,
  );
}

/** A labelled row with its explanation under it, used for every control. */
function Row(props: {
  label: string;
  help?: string;
  children: React.ReactNode;
}) {
  return (
    <Stack gap={2}>
      <Group justify="space-between" wrap="nowrap" align="center">
        <Text size="sm">{props.label}</Text>
        {props.children}
      </Group>
      {props.help && (
        <Text size="xs" c="dimmed">
          {helpNodes(props.help)}
        </Text>
      )}
    </Stack>
  );
}

/**
 * A heading *inside* a group, for the groups long enough to need dividing.
 * Groups themselves are named by their tab, so most panels have none of these —
 * a heading repeating the tab you just clicked is noise.
 */
function SectionTitle(props: { children: ReactNode }) {
  return (
    <Text size="xs" fw={600} c="dimmed" tt="uppercase">
      {props.children}
    </Text>
  );
}

/**
 * A draft value, seeded from the daemon's and re-seeded whenever a new document
 * arrives.
 *
 * Keyed on the document's **identity**, not on the value. A clamp that resolves to
 * the value already stored leaves that value unchanged, so a value-keyed effect
 * would never fire and the rejected number would stay in the box — contradicting
 * the header's promise that an out-of-range entry visibly snaps back. The daemon
 * returns a fresh document on every write, so its identity is the signal that a
 * write landed.
 */
function useDraft<T>(
  seed: T,
  settings: SettingsDoc | null,
): [T, (v: T) => void] {
  const [draft, setDraft] = useState<T>(seed);
  useEffect(() => {
    setDraft(seed);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settings]);
  return [draft, setDraft];
}

/** What every control is handed: what to draw, what it holds, where to send it. */
interface ControlProps {
  entry: CatalogEntry;
  /**
   * The whole document rather than one value: it is both where the value is read
   * from and the identity that re-seeds a draft.
   */
  settings: SettingsDoc | null;
  /**
   * The global lock, or a closed boolean gate. Never a *mode* gate — a row whose
   * `requires` names a value rather than `true` is unmounted instead; see
   * `rowsFor`.
   */
  disabled: boolean;
  set: (patch: SettingsDoc) => void;
}

// One width per kind of control rather than one per setting. A catalog says what a
// control must offer, never how wide it should be, and the alternative is 43
// hand-tuned numbers — which is the thing this file exists to stop having.
const SELECT_W = 200;
const NUMBER_W = 140;
const SLIDER_W = 200;
const TEXT_W = 240;
const TEXTAREA_W = 280;

function BoolControl({ entry, settings, disabled, set }: ControlProps) {
  return (
    <Checkbox
      size="xs"
      checked={asBool(settingValue(settings, entry))}
      disabled={disabled}
      onChange={(e) => set({ [entry.key]: e.currentTarget.checked })}
    />
  );
}

function StaticControl(props: ControlProps & { options: Choice[] }) {
  const { entry, settings, disabled, set } = props;
  // A NativeSelect whose value matches no option renders blank and warns, so a
  // stored value this build does not know would show the user an empty control
  // for a setting that is set. The old hand-written rows could not hit it — each
  // read through `oneOf(..., FALLBACK)` in shared/settings.ts — and the case is
  // unreachable through the API today, since `validate`'s `one_of` refuses it.
  // It becomes reachable on a **downgrade**: a newer daemon stores a fourth
  // cursor style, an older bundle renders it. Showing the default is the same
  // answer the typed readers gave.
  const stored = asString(settingValue(settings, entry));
  const known = props.options.some((o) => o.value === stored);
  return (
    <NativeSelect
      size="xs"
      w={SELECT_W}
      value={known ? stored : asString(entry.default)}
      disabled={disabled}
      data={props.options}
      onChange={(e) => set({ [entry.key]: e.currentTarget.value })}
    />
  );
}

/**
 * A number offered as a short menu and accepted anywhere in its range, so the
 * stored value has to be spliced in when it is not one of the presets — see
 * `presetOptions`.
 */
function PresetsControl(
  props: ControlProps & { offered: Choice[]; unit: string | null },
) {
  const { entry, settings, disabled, set } = props;
  const value = asNumber(settingValue(settings, entry));
  return (
    <NativeSelect
      size="xs"
      w={SELECT_W}
      data={presetOptions(props.offered, value, props.unit)}
      value={String(value)}
      disabled={disabled}
      onChange={(e) => set({ [entry.key]: Number(e.currentTarget.value) })}
    />
  );
}

function NumberControl(
  props: ControlProps & {
    min?: number;
    max?: number;
    step?: number;
    unit?: string | null;
    /** What an empty box means, where the floor is an off switch. */
    placeholder?: string | null;
  },
) {
  const { entry, settings, disabled, set, unit } = props;
  const committed = asNumber(settingValue(settings, entry));
  const [draft, setDraft] = useDraft<number | string>(committed, settings);
  const commit = () => {
    // Mantine's NumberInput emits raw strings while typing, so an empty or
    // half-typed box ("1e") must not be sent as NaN — the daemon would reject it
    // and the user would see an error for having selected the text.
    const n = typeof draft === "number" ? draft : Number(draft);
    if (!Number.isFinite(n)) {
      setDraft(committed);
      return;
    }
    if (n === committed) return;
    set({ [entry.key]: n });
  };
  return (
    <NumberInput
      size="xs"
      w={NUMBER_W}
      min={props.min}
      max={props.max}
      step={props.step}
      value={draft}
      disabled={disabled}
      /* Conditional only where the floor is an off switch — `emptyMeans` is the
         catalog's way of saying so, and there " days" on an empty box would
         contradict the placeholder that explains what empty means. Everywhere
         else the suffix is unconditional, which is what `terminal.detachGraceMinutes`
         and both reconnect delays did before this became generic: their floor is a
         real duration, not an "off". */
      suffix={
        unit && (props.placeholder == null || (typeof draft === "number" && draft > 0))
          ? ` ${unit}`
          : ""
      }
      placeholder={props.placeholder ?? undefined}
      onChange={setDraft}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") e.currentTarget.blur();
      }}
    />
  );
}

/**
 * A percentage, which is dragged rather than typed — the catalog says so by giving
 * the range a `%` unit, and that is the whole rule.
 */
function SliderControl(
  props: ControlProps & { min: number; max: number; step?: number },
) {
  const { entry, settings, disabled, set } = props;
  const committed = asNumber(settingValue(settings, entry));
  const [draft, setDraft] = useDraft(committed, settings);
  return (
    <Slider
      size="sm"
      w={SLIDER_W}
      min={props.min}
      max={props.max}
      step={props.step}
      label={(v) => `${v}%`}
      value={draft}
      disabled={disabled}
      onChange={setDraft}
      onChangeEnd={(v) => set({ [entry.key]: v })}
    />
  );
}

function TextControl({ entry, settings, disabled, set }: ControlProps) {
  const committed = asString(settingValue(settings, entry));
  const [draft, setDraft] = useDraft(committed, settings);
  return (
    <TextInput
      size="xs"
      w={TEXT_W}
      spellCheck={false}
      value={draft}
      disabled={disabled}
      onChange={(e) => setDraft(e.currentTarget.value)}
      onBlur={() => {
        const v = draft.trim();
        if (v === committed) return;
        set({ [entry.key]: v });
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter") e.currentTarget.blur();
      }}
    />
  );
}

function TextListControl({ entry, settings, disabled, set }: ControlProps) {
  const committed = asStringList(settingValue(settings, entry));
  const [draft, setDraft] = useDraft(committed.join("\n"), settings);
  return (
    <Textarea
      size="xs"
      w={TEXTAREA_W}
      autosize
      minRows={2}
      maxRows={8}
      spellCheck={false}
      value={draft}
      disabled={disabled}
      onChange={(e) => setDraft(e.currentTarget.value)}
      onBlur={() => {
        const lines = draft
          .split("\n")
          .map((line) => line.trim())
          .filter((line) => line !== "");
        // Nothing to say if the list is unchanged — including the common case of
        // opening the dialog, clicking through the box and leaving.
        if (lines.join("\n") === committed.join("\n")) return;
        set({ [entry.key]: lines });
      }}
    />
  );
}

/**
 * A setting this build cannot draw.
 *
 * **Visible, never silent.** A key that exists in Rust and renders nothing here is
 * unreachable for anyone who does not already know the CLI, and nothing anywhere
 * would report it — which is the single failure mode the catalog design is engineered
 * against. The compiler catches the ordinary case first (the `never` assertions in
 * `SettingControl`), so what this actually covers is a *newer daemon* serving a
 * shape an older bundle predates.
 *
 * `shape` is the unhandled discriminant, carried into the element's tooltip: that
 * is what makes the exhaustiveness `never` a value that is read rather than one the
 * compiler complains about.
 */
function Unsupported(props: { entry: CatalogEntry; shape: string }) {
  return (
    <Text size="xs" c="dimmed" ta="right" title={props.shape}>
      This version of the Veld IDE cannot show this setting — use{" "}
      <Code>veld settings set {props.entry.key}</Code>
    </Text>
  );
}

/**
 * The control for one entry, chosen by its value shape and then by what it offers.
 *
 * Both switches end in a `never`, which is the machine-checked half of the promise
 * in this file's header: a `ValueShape` or a `Choices` variant added in Rust does
 * not compile here until somebody has decided what it looks like.
 */
function SettingControl(props: ControlProps) {
  const { entry } = props;
  switch (entry.type) {
    case "bool":
      return <BoolControl {...props} />;
    case "textList":
      return <TextListControl {...props} />;
    case "int":
    case "text":
      return <ChoiceControl {...props} />;
    default: {
      const unhandledShape: never = entry.type;
      return <Unsupported entry={entry} shape={String(unhandledShape)} />;
    }
  }
}

function ChoiceControl(props: ControlProps) {
  const { entry } = props;
  const choices = entry.choices;
  switch (choices.kind) {
    case "static":
      return <StaticControl {...props} options={choices.options} />;
    case "range":
      // A percentage is a slider; everything else is a spinner. The unit is what
      // decides, so `terminal.bellVolume` needs no mention by name here.
      return choices.unit === "%" ? (
        <SliderControl
          {...props}
          min={choices.min}
          max={choices.max}
          step={choices.step ?? undefined}
        />
      ) : (
        <NumberControl
          {...props}
          min={choices.min}
          max={choices.max}
          step={choices.step ?? undefined}
          unit={choices.unit}
          placeholder={choices.emptyMeans}
        />
      );
    case "presets":
      return (
        <PresetsControl {...props} offered={choices.offered} unit={choices.unit} />
      );
    case "free":
      // Shape decides: an int with no stated range is still a number box, and
      // sending it as text is what the daemon would reject.
      return entry.type === "int" ? (
        <NumberControl {...props} />
      ) : (
        <TextControl {...props} />
      );
    case "runtime":
      // Every runtime source there is has a hand-written control in `OVERRIDES`,
      // so reaching here means Rust attached one to a *key* the map does not
      // cover. Not a compile error — the union is exhausted — and not silence
      // either.
      return <Unsupported entry={entry} shape={choices.source} />;
    default: {
      const unhandledKind: never = choices;
      return <Unsupported entry={entry} shape={String(unhandledKind)} />;
    }
  }
}

// ---------------------------------------------------------------------------
// The controls a catalog cannot describe
//
// Each one is hand-written, keyed by setting key in `OVERRIDES` below, and each
// still takes its title from the catalog. Four of the six are the keys Rust marks
// `Choices::Runtime` — its way of saying the client owns the list. The other two,
// `browser.externalOrigins` and `browser.searchUrl`, are `Choices::Free` and are
// here for a different reason: each needs a validator only the client can run.
// (The count said "four of the five" before `backup.dir` joined, and was already
// wrong by one then — `externalOrigins` was never Runtime either.)
//
// A `Choices::Runtime` key with no entry here does not fail to compile: the union is
// exhausted by `source`, not by key, so it renders as a visible "cannot show this"
// row instead. `backup.dir` shipped that way for one review round.
// ---------------------------------------------------------------------------

/**
 * Sentinel for the shell picker's "Custom path…" option.
 *
 * A `\u0000` prefix like `CUSTOM_FONT` below, and for the same reason: every real
 * value in that select is something the daemon would accept, and the daemon refuses
 * a shell path containing a NUL — so this sentinel cannot collide with one.
 *
 * Written as an escape, never as a literal NUL byte: a NUL in a source file makes
 * ripgrep and grep classify it as binary and skip it silently (see the AGENTS.md
 * note about `App.tsx`).
 */
const CUSTOM_SHELL = "\u0000custom-shell";

/** Sentinel for the "Custom…" option; not a font stack. */
const CUSTOM_FONT = "\u0000custom";

/**
 * Mirrors `MAX_WORKTREE_STORAGE_DIR_LEN` and `MAX_BACKUP_DIR_LEN` in veld-core's
 * settings.rs — one number because both validators use the same one.
 */
const MAX_WORKTREE_STORAGE_DIR_LEN = 1024;

/**
 * Every rule the daemon's directory validators enforce, mirrored so a value this
 * box would reject never round-trips through a save attempt first — the daemon's
 * 400 has no body a user would ever see. `null` means empty is a real value here
 * too (the off switch, not an error).
 *
 * Shared by `worktree.storageDir` and `backup.dir`, whose Rust validators are the
 * same four rules. If one of them ever diverges, this splits into two — a single
 * mirror of two different validators is worse than none, because the box that
 * silently disagrees is the one nobody suspects.
 */
function absoluteDirError(path: string): string | null {
  const v = path.trim();
  if (v === "") return null;
  // Bytes, not JS's UTF-16 code units: the Rust validators this mirrors
  // (`WorktreeStorageDir` and `BackupDir` in settings.rs) measure `s.len()`,
  // which is a byte count — a path with any multi-byte character would
  // otherwise pass this check under the limit and still get 400'd server-side.
  if (new TextEncoder().encode(v).length > MAX_WORKTREE_STORAGE_DIR_LEN) {
    return `Must be ${MAX_WORKTREE_STORAGE_DIR_LEN} bytes or fewer`;
  }
  // Also rejects the Unicode C1 controls (U+0080–U+009F) `char::is_control`
  // catches on the Rust side, not only the ASCII C0 range + DEL.
  // biome-ignore lint/suspicious/noControlCharactersInRegex: rejecting them is the point.
  if (/[\x00-\x1f\x7f-\x9f]/.test(v)) return "Must not contain control characters";
  if (!v.startsWith("/")) return "Must be an absolute path";
  if (v.split("/").includes("..")) return 'Must not contain ".."';
  return null;
}

/**
 * What the hand-written controls need and no catalog can supply.
 *
 * Fetched by the dialog rather than by the controls themselves, because the panels
 * are unmounted when you leave them (`keepMounted={false}`): held here, the shells
 * list and the shim probe cost one request per *open* rather than one per visit to
 * a tab — and the shim probe spawns a login shell.
 */
interface CustomControls {
  /** What this machine has. Null means the fetch failed; see `api.shells`. */
  shells: ShellList | null;
  /** Whether the `open` shim actually wins there. Null while in flight. */
  shimStatus: ShellIntercept | null;
  /** The fonts the browser reports it can render. */
  fonts: TerminalFont[];
  /** This dialog's own intent — "I clicked Custom path…" — and nothing else. */
  customShell: boolean;
  setCustomShell: (v: boolean) => void;
  customFont: boolean;
  setCustomFont: (v: boolean) => void;
  /**
   * Where backups go while `backup.dir` is empty, as the daemon resolves it.
   * `null` while the catalog is still loading, or from a daemon too old to send it.
   */
  backupDir: string | null;
}

interface OverrideProps extends ControlProps {
  custom: CustomControls;
  /**
   * Whether this entry's own `requires` is satisfied. Only the rows that trail a
   * *mode*-gated setting need it — a boolean gate is already folded into
   * `disabled`.
   */
  gateOpen: boolean;
}

function ShellRow({ entry, settings, disabled, set, custom }: OverrideProps) {
  const { shells, shimStatus, customShell, setCustomShell } = custom;
  const shellValue = asString(settingValue(settings, entry));
  const [shellPath, setShellPath] = useDraft(shellValue, settings);
  // **A positive fact, never a negation.** This asks only "do we *know* the stored
  // shell is not one of the listed ones", which is answerable exactly when a list
  // actually arrived. Every previous shape of this was some flavour of "not
  // listed", which had to decide what an absent list meant and got it wrong three
  // times running: sticky-only went stale when another window changed the shell;
  // treating a missing list as listed hid the field for ever when the fetch
  // failed; treating it as unlisted flashed the field open on every dialog open
  // and, on a failed fetch, opened it for ordinary listed shells. With the
  // question posed positively there is no third state to get wrong — no list means
  // no claim, and the user can still open the field from the select.
  const knownUnlisted =
    shells !== null &&
    shellValue !== "auto" &&
    !shells.shells.some((s) => s.path === shellValue);
  const showCustomShell = customShell || knownUnlisted;
  return (
    <>
      <Row
        label={entry.title}
        /* Not the catalog's help: this one answers "which shell am I actually
           getting", and the answer differs by what is stored. */
        help={
          shellValue === "auto"
            ? "Your login shell. Pick another if your aliases and integrations live in a different shell's startup files — a terminal already open keeps the shell it started with."
            : "A terminal already open keeps the shell it started with."
        }
      >
        <NativeSelect
          size="xs"
          w={220}
          value={
            customShell
              ? CUSTOM_SHELL
              : // The stored value is always one of the options below, so a
                // shell that is not on this machine's list — uninstalled, or
                // somewhere unusual — still shows as chosen rather than
                // silently reading as "Automatic".
                shellValue
          }
          disabled={disabled}
          data={[
            {
              value: "auto",
              // Named, not just "Automatic": the whole question this setting
              // answers is "which shell am I actually getting?", and the
              // client cannot work that out itself.
              label: shells
                ? `Automatic (${shells.auto.split("/").pop()})`
                : "Automatic",
            },
            ...(shells?.shells ?? []).map((s) => ({
              value: s.path,
              label: `${s.name} (${s.path})`,
            })),
            ...(shellValue !== "auto" &&
            !(shells?.shells ?? []).some((s) => s.path === shellValue)
              ? [{ value: shellValue, label: shellValue }]
              : []),
            { value: CUSTOM_SHELL, label: "Custom path…" },
          ]}
          onChange={(e) => {
            const v = e.currentTarget.value;
            if (v === CUSTOM_SHELL) {
              // Seed the field with what is in effect, so "Custom" starts
              // from the current shell rather than from empty.
              setCustomShell(true);
              setShellPath(shellValue === "auto" ? "" : shellValue);
              return;
            }
            setCustomShell(false);
            setShellPath(v);
            set({ [entry.key]: v });
          }}
        />
      </Row>
      {shimStatus?.enabled && shimStatus.works === false && (
        <Stack gap={4}>
          <Text size="xs" c="var(--warning, #d08770)">
            Programs in {shimStatus.name} that call{" "}
            <code>open</code> directly — an agent running{" "}
            <code>open https://…</code>, for instance — will use your
            system browser rather than a Veld pane.{" "}
            {shimStatus.resolved
              ? `open resolves to ${shimStatus.resolved}.`
              : ""}{" "}
            Links you click, and anything reading <code>$BROWSER</code>,
            are unaffected.
          </Text>
          {shimStatus.hint && (
            <>
              <Text size="xs" c="dimmed">
                To catch them too, add this to {shimStatus.hint.file}:
              </Text>
              <Text
                size="xs"
                ff="monospace"
                style={{
                  background: "var(--surface-2, rgba(127,127,127,0.12))",
                  padding: "4px 8px",
                  borderRadius: 4,
                  // A shell line is wide and must not widen the panel;
                  // the file's own flexbox note explains why.
                  overflowX: "auto",
                  whiteSpace: "pre",
                }}
              >
                {shimStatus.hint.line}
              </Text>
            </>
          )}
        </Stack>
      )}
      {showCustomShell && (
        <Row
          label="Custom shell path"
          help="An absolute path — a bare name would be looked up on the daemon's own PATH, which is not your terminal's."
        >
          <TextInput
            size="xs"
            w={240}
            placeholder="/opt/homebrew/bin/fish"
            value={shellPath}
            disabled={disabled}
            onChange={(e) => setShellPath(e.currentTarget.value)}
            onBlur={() => {
              const v = shellPath.trim();
              // Clearing the box is "never mind", not a value to send: the
              // daemon would refuse an empty string and the user would see
              // an error for having deleted their own typing.
              if (!v) {
                setShellPath(shellValue === "auto" ? "" : shellValue);
                return;
              }
              if (v !== shellValue) set({ [entry.key]: v });
            }}
          />
        </Row>
      )}
    </>
  );
}

function FontRow({ entry, settings, disabled, set, custom }: OverrideProps) {
  const { fonts, customFont, setCustomFont } = custom;
  const committed = asString(settingValue(settings, entry));
  const [fontFamily, setFontFamily] = useDraft(committed, settings);
  const fontOption = matchFont(committed, fonts);
  return (
    <>
      <Row
        label={entry.title}
        /* Not the catalog's help: this says what the *chosen* font costs, which
           the catalog describes for the setting as a whole. */
        help={
          fontOption
            ? fontOption.bundled
              ? "Bundled with Veld, so it renders the same everywhere."
              : "Installed on this machine. Another machine may not have it."
            : "A CSS font-family list of your own."
        }
      >
        <NativeSelect
          size="xs"
          w={200}
          // The stored stack is the value, not the label: two stacks that
          // differ only in their fallback are genuinely different settings.
          value={fontOption?.stack ?? CUSTOM_FONT}
          disabled={disabled}
          data={[
            ...fonts.map((f) => ({
              value: f.stack,
              label: f.bundled ? f.label : `${f.label} (system)`,
            })),
            { value: CUSTOM_FONT, label: "Custom…" },
          ]}
          onChange={(e) => {
            const v = e.currentTarget.value;
            if (v === CUSTOM_FONT) {
              // Reveal the field seeded with what is in effect, so "Custom"
              // starts from the current font rather than from empty.
              setCustomFont(true);
              setFontFamily(committed);
              return;
            }
            setCustomFont(false);
            setFontFamily(v);
            set({ [entry.key]: v });
          }}
        />
      </Row>
      {(customFont || !fontOption) && (
        <Row
          label="Custom font family"
          help="A CSS font-family list. Ends up in a stylesheet, so { } ; < > are refused."
        >
          <TextInput
            size="xs"
            w={240}
            value={fontFamily}
            disabled={disabled}
            onChange={(e) => setFontFamily(e.currentTarget.value)}
            onBlur={() => {
              const v = fontFamily.trim();
              // An empty family would render as the browser default and read as
              // a bug, so treat clearing the box as "reset" rather than sending
              // a value the daemon must reject.
              if (!v) {
                setFontFamily(committed);
                return;
              }
              if (v !== committed) set({ [entry.key]: v });
            }}
          />
        </Row>
      )}
    </>
  );
}

/**
 * A path box with a native folder picker beside it, for a setting Rust marks
 * `Choices::Runtime { Directory }`.
 *
 * Parameterised rather than copied: `backup.dir` arrived with the same
 * `Choices::Runtime` marking and no entry in {@link OVERRIDES}, which meant it fell
 * through to `Unsupported` and the one backup setting whose whole point is *where
 * the copies go* rendered as "this version cannot show this setting". A second
 * hand-written copy of this row would have been the other way to fix that, and the
 * two would have drifted.
 */
function DirectoryRow({
  entry,
  settings,
  disabled,
  set,
  purpose,
  placeholder,
  emptyNote,
}: OverrideProps & {
  purpose: "worktree-storage" | "backup-dir";
  placeholder: string;
  emptyNote: string;
}) {
  const committed = asString(settingValue(settings, entry));
  const [dir, setDir] = useDraft(committed, settings);
  const [picking, setPicking] = useState(false);
  const [pickError, setPickError] = useState<string | null>(null);
  // Mirrors the daemon's own validators — `WorktreeStorageDir` and `BackupDir` in
  // veld-core's settings.rs, which enforce the same four rules — see the blur
  // handler below for why a client-side mirror exists at all. All of the daemon's rules, not only the
  // absolute-path one: a mirror that only caught the common case would still let a
  // pasted over-long path or one carrying a tab pass here and 400 with no
  // explanation, which is the exact failure this exists to prevent.
  const dirError = absoluteDirError(dir);
  const dirBroken = dirError !== null;
  return (
    <Row label={entry.title} help={entry.help}>
      <Stack gap={4} style={{ alignItems: "flex-end" }}>
        <Group gap="xs" wrap="nowrap">
          <TextInput
            size="xs"
            w={220}
            value={dir}
            disabled={disabled}
            placeholder={placeholder}
            styles={{
              input: {
                fontFamily: "var(--mantine-font-family-monospace)",
              },
            }}
            error={dirError ?? undefined}
            onChange={(e) => setDir(e.currentTarget.value)}
            onBlur={() => {
              const v = dir.trim();
              if (v === committed) return;
              // Reuses `dirBroken` (computed above from this same state)
              // rather than a second inline copy of the predicate — see the
              // note on `searchUrl` below for why the check exists here at
              // all: the daemon's 400 has no body a user would ever see, so
              // a rejected value would otherwise just snap back with no
              // explanation.
              if (dirBroken) return;
              set({ [entry.key]: v });
            }}
          />
          <Button
            size="xs"
            variant="default"
            loading={picking}
            disabled={disabled}
            onClick={async () => {
              setPicking(true);
              setPickError(null);
              try {
                const picked = await api.pickDirectory(purpose);
                if (picked) {
                  setDir(picked);
                  set({ [entry.key]: picked });
                }
              } catch (e) {
                setPickError(e instanceof Error ? e.message : String(e));
              } finally {
                setPicking(false);
              }
            }}
          >
            Browse…
          </Button>
        </Group>
        {pickError && (
          <Text size="xs" c="red">
            {pickError}
          </Text>
        )}
        {!committed && !dirBroken && (
          <Text size="xs" c="dimmed">
            {emptyNote}
          </Text>
        )}
      </Stack>
    </Row>
  );
}

function StorageDirRow(props: OverrideProps) {
  return (
    <DirectoryRow
      {...props}
      purpose="worktree-storage"
      placeholder="/Users/you/veld-worktrees"
      emptyNote="No folder chosen yet — new checkouts still land next to each repository until one is."
    />
  );
}

/**
 * `backup.dir`, whose empty state is a real value — "the folder veld derives" —
 * rather than an unset one.
 *
 * **The empty box shows the actual derived path**, greyed, because the only
 * question this row is ever asked is *where are my backups*. It used to show an
 * invented example (`/Volumes/backup/veld`) with a sentence underneath saying the
 * default was in use, which read as a contradiction: a placeholder looks like the
 * value, so the box and the note disagreed about what veld was doing. The daemon
 * sends the resolved path with the catalog; the note now adds only the thing the
 * path itself cannot say, which is that it shares a disk with the database.
 */
function BackupDirRow(props: OverrideProps) {
  const derived = props.custom.backupDir;
  return (
    <DirectoryRow
      {...props}
      purpose="backup-dir"
      placeholder={derived ?? "/Volumes/backup/veld"}
      emptyNote={
        derived
          ? "That is the folder Veld picks, and it is on the same disk as the database it is copying. Pick another to keep the copies somewhere this machine's own storage cannot take with it."
          : "Using the folder Veld picks beside its own data — which is on the same disk as the database it is copying."
      }
    />
  );
}

/**
 * The one row here that is not a setting: it *does* something rather than storing
 * anything. Anchored after `worktree.storageDir` in `TRAILING_ROWS`, and rendered
 * whether or not that row is — the "Only available with Custom location" sentence
 * is the whole point of it in the other mode.
 */
function OpenStorageFolderRow({ entry, settings, disabled, gateOpen }: OverrideProps) {
  const storageDirValue = asString(settingValue(settings, entry));
  const [opening, setOpening] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // The error belongs to a button that only fires in `custom` mode; leaving it
  // set after switching back to `sibling` would strand red text under a control
  // that cannot have produced it.
  useEffect(() => {
    if (!gateOpen) setError(null);
  }, [gateOpen]);
  return (
    <>
      <Row
        label="Open worktree storage folder"
        help={
          !gateOpen
            ? "Only available with Custom location: the default has no single folder — each repo's worktrees live beside it, so open one from its own context menu instead."
            : storageDirValue
              ? "Opens the folder above in Finder (or your file manager)."
              : "Choose a folder above first."
        }
      >
        <Button
          size="xs"
          variant="default"
          loading={opening}
          disabled={disabled || !gateOpen || !storageDirValue}
          onClick={async () => {
            setOpening(true);
            setError(null);
            try {
              await api.openWorktreeStorageDir();
            } catch (e) {
              setError(e instanceof Error ? e.message : String(e));
            } finally {
              setOpening(false);
            }
          }}
        >
          Open Folder
        </Button>
      </Row>
      {error && (
        <Text size="xs" c="red">
          {error}
        </Text>
      )}
    </>
  );
}

/**
 * A full-width field rather than a `Row`, whose label and control sit side by
 * side: a list needs the width, and its explanation is longer than a row's
 * trailing help line.
 */
function ExternalOriginsRow({ entry, settings, disabled, set }: OverrideProps) {
  const committed = asStringList(settingValue(settings, entry));
  // One origin per line, which is what an exempt list reads as. Held locally and
  // committed on blur like every other text field here — the daemon refuses the
  // whole list if one entry is not an origin, and its error lands in `props.error`.
  const [exempt, setExempt] = useDraft(committed.join("\n"), settings);
  return (
    <>
      <Stack gap={2}>
        <Text size="sm">{entry.title}</Text>
        <Text size="xs" c="dimmed">
          {helpNodes(entry.help)}
        </Text>
      </Stack>
      <Textarea
        size="xs"
        autosize
        minRows={2}
        maxRows={8}
        spellCheck={false}
        placeholder="https://accounts.google.com"
        value={exempt}
        disabled={disabled}
        onChange={(e) => setExempt(e.currentTarget.value)}
        onBlur={() => {
          const lines = exempt
            .split("\n")
            .map((line) => line.trim())
            .filter((line) => line !== "");
          // Nothing to say if the list is unchanged — including the common case
          // of opening the dialog, clicking through the box and leaving.
          if (lines.join("\n") === committed.join("\n")) return;
          set({ [entry.key]: lines });
        }}
      />
    </>
  );
}

/**
 * Full width rather than a `Row`: a URL needs the room, and the explanation
 * carries two things a trailing help line cannot — what %s means, and that empty
 * is a supported answer.
 */
function SearchUrlRow({ entry, settings, disabled, set }: OverrideProps) {
  // Committed on blur like the other text fields. Empty is a real value here — it
  // turns search off — so this one is never coerced back to the default on the way
  // out.
  const committed = asString(settingValue(settings, entry));
  const [search, setSearch] = useDraft(committed, settings);
  // Shown under the field as it is typed, and the same predicate the blur handler
  // refuses on — so the reason the value did not save is on screen rather than
  // inferred from nothing having happened. Empty is not broken; it is the off switch.
  const searchBroken =
    search.trim() !== "" && searchTarget(search.trim(), "veld") === null;
  return (
    <>
      <Stack gap={2}>
        <Text size="sm">{entry.title}</Text>
        <Text size="xs" c="dimmed">
          {helpNodes(entry.help)}
        </Text>
      </Stack>
      <TextInput
        size="xs"
        spellCheck={false}
        placeholder="https://www.google.com/search?q=%s"
        value={search}
        disabled={disabled}
        onChange={(e) => setSearch(e.currentTarget.value)}
        error={searchBroken ? "Veld cannot build a search URL from this" : undefined}
        onBlur={() => {
          const next = search.trim();
          // Nothing to say if it is unchanged — including opening the dialog,
          // clicking through the field and leaving.
          if (next === committed) return;
          // **Validated here with the parser that will actually use it.** The
          // daemon has its own rules, but they are hand-written and review found
          // a hole in them three rounds running; this runs `searchTarget` — the
          // same function a pane calls, so the same `new URL()` — against a probe
          // query, which makes "would this template ever work" a question answered
          // by the thing that has to answer it. Without it, a template that parses
          // nowhere is stored happily and then fails on the user's next query,
          // blaming the query.
          if (next !== "" && searchTarget(next, "veld") === null) return;
          set({ [entry.key]: next });
        }}
      />
    </>
  );
}

/**
 * The controls the catalog hands to the bundle, keyed by setting key.
 *
 * **This map is the exception that keeps the rule honest**, not a back door. Every
 * entry in it is a control whose *options* only the client can produce — the shells
 * this machine has, the fonts this browser can render, a native folder picker — or
 * a validator that has to be the very parser which will later consume the value.
 * Rust marks the first four `Choices::Runtime` for exactly that reason, so the two
 * halves agree by construction rather than by this map being kept in step by hand.
 *
 * An override still renders the catalog's title, and takes its help from there
 * wherever the help is not itself a function of the current value.
 */
const OVERRIDES: Record<string, ComponentType<OverrideProps>> = {
  "terminal.shell": ShellRow,
  "terminal.fontFamily": FontRow,
  "worktree.storageDir": StorageDirRow,
  "backup.dir": BackupDirRow,
  "browser.externalOrigins": ExternalOriginsRow,
  "browser.searchUrl": SearchUrlRow,
};

/**
 * Rows that are not settings, anchored *after* the setting they belong beside.
 *
 * Rendered whether or not that setting's own row is: a mode-gated row is unmounted
 * when its gate closes, and this button's whole job in the other mode is to say why
 * it is unavailable.
 */
const TRAILING_ROWS: Record<string, ComponentType<OverrideProps>> = {
  "worktree.storageDir": OpenStorageFolderRow,
};

// ---------------------------------------------------------------------------
// Painting, which is the bundle's and stays the bundle's
// ---------------------------------------------------------------------------

/**
 * Tab icons, keyed by the catalog's group id.
 *
 * A group is *what a setting is about*, which is why it belongs beside the title
 * and the help text in Rust; a tab is how that gets drawn, which is not — and
 * `SettingGroup::as_str` is documented as stable precisely because this map keys
 * off it. A group Rust adds that has no icon here still gets a tab with its label:
 * a missing icon is a far smaller failure than a missing group.
 */
const GROUP_ICONS: Record<string, ReactNode> = {
  general: <IconAdjustments size={15} />,
  git: <IconGitBranch size={15} />,
  terminal: <IconTerminal2 size={15} />,
  activity: <IconActivity size={15} />,
  keepAwake: <IconCoffee size={15} />,
  sharing: <IconShare size={15} />,
  links: <IconLink size={15} />,
  browser: <IconAppWindow size={15} />,
};

/**
 * A sentence under a *heading*, introducing the rows beneath it.
 *
 * Not in the catalog, and deliberately: a spec describes one setting, and each of
 * these describes the relationship between several — why the same question is asked
 * twice, what the table below is and is not. Keyed by the section title, which is a
 * single named constant on the Rust side for the same reason a typo in one would
 * otherwise split a section in two.
 */
const SECTION_BLURBS: Record<string, string> = {
  "Database backups":
    "Every repository, checkout, lane, layout and setting on this page lives in one file, and nothing else on this machine has a copy of it. These decide how often Veld takes one, how many it keeps, and where they go. Copies leave out logs and resource samples, so one is a few megabytes rather than the hundreds the live file can reach. `veld backup` lists them; `veld backup restore` puts one back.",
  Notifying:
    "System notifications, and only while Veld is not the focused window — nothing interrupts you about a pane you could be looking at. The rail still marks everything either way.",
  "Focus mode":
    "The top-bar toggle between search and settings. On, it silences whichever of these three channels are checked below — for the background-activity channel only, never for feedback on something you just clicked yourself.",
  "While you're sharing":
    "A share is only useful while this machine is up, so Veld holds it awake for you and lets go when the share ends. Asked twice because the answer differs: on mains nothing is being spent, and on battery it is your charge. Neither one ever keeps the machine awake with the lid shut on battery — that is the durable setting Veld only ever touches when you ask it to yourself.",
  // No "above"/"below" here either: one group renders per tab, so the keep-awake
  // rows this refers to are on a different tab and not on screen. Same rule the
  // catalog's own spec help follows, for the same reason — it is stated there as
  // "a positional reference is a lie on a terminal", and it is equally a lie
  // across a tab boundary.
  "Share links expire after":
    "How long a link keeps working, counted from when you started sharing. This is usually what ends a share — and so what ends the automatic keep-awake with it, since the caps under Keep awake are a ceiling over this rather than a second countdown. A project can override these in its veld.json, and veld share --ttl overrides everything for one share.",
};

/** What this machine is, as far as this dialog needs to know. */
interface Machine {
  hasBattery: boolean;
  platform: string | null;
}

/**
 * Settings that only apply to some hardware.
 *
 * **A runtime machine fact, not a setting dependency** — which is exactly why it is
 * here and not in the catalog's `requires`. A desktop does not have
 * `keepAwake.sharingOnBattery` switched *off*; it has nowhere for the question to
 * land. The daemon still stores and validates the key, so `veld settings set` works
 * on a desktop: this hides a control, never a capability.
 *
 * Applied before headings are emitted, so a section whose only row is filtered out
 * loses its heading with it — which is what "When you ask" needs on Linux.
 */
const HARDWARE_GATES: Record<string, (m: Machine) => boolean> = {
  "keepAwake.sharingOnBattery": (m) => m.hasBattery,
  "keepAwake.sharingOnBatteryMinutes": (m) => m.hasBattery,
  "keepAwake.manualOnBattery": (m) => m.hasBattery && m.platform === "macos",
  // The menu bar is macOS's. On Windows and Linux the shell creates no tray at
  // all (`desktop/src/main.js` gates on `process.platform === "darwin"`), so the
  // question has nowhere to land — the case above this comment's rule.
  //
  // Gated on the **machine**, not on whether this client is the app: a macOS
  // browser tab has no menu bar of its own either, but it is configuring the same
  // machine's Veld Desktop, and hiding the row there would leave the setting
  // reachable only from the app whose icon it removes. The help text already says
  // only the desktop app reads it.
  //
  // `null` — the probe has not answered yet, or failed — **shows** the row, which
  // is why this is not spelled `m.platform === "macos"`. That is the same
  // direction `hasBattery`'s `?? true` fallback takes below: unknown must not hide
  // a control, or a macOS user whose one `/api/caffeinate` request failed loses
  // the setting entirely, and every open flickers the row in as the probe lands.
  "desktop.menuBarIcon": (m) => m.platform === null || m.platform === "macos",
};

/**
 * Settings that only apply to what the *current* settings have selected.
 *
 * A third gate, beside the catalog's `requires` and {@link HARDWARE_GATES}, and it
 * exists because neither of those can ask this question. `requires` compares
 * another setting's stored value for equality — but "the chosen font can draw
 * them" is not a value of any setting, it is a property of the font behind one.
 * And the daemon cannot answer it: it stores a CSS font-family string and never
 * sees a font file, so the knowledge has to sit next to the font list, which is
 * here in the bundle.
 *
 * **Unmounts rather than disables**, which is the `worktree.storageDir` direction
 * and not the `backup.enabled` one: a ligature switch under Menlo is not a
 * disabled control, it is a question with nowhere to land — the same reading
 * {@link HARDWARE_GATES} takes for a battery setting on a desktop. The daemon
 * still stores and validates the key, so `veld settings set terminal.ligatures`
 * works whatever font is selected; this hides a control, never a capability.
 *
 * A hidden row can therefore sit over a stored `true`. That is deliberate and
 * harmless — the value is inert on a font that cannot draw them, and it comes back
 * as it was when a font that has them is selected again, which is better than
 * silently rewriting a preference the user set.
 */
type CapabilityGate = (
  settings: SettingsDoc | null,
  fonts: TerminalFont[],
) => boolean;

// The type is named rather than inlined so the declaration fits on one line, like
// the four maps above it — and it has to. The Rust tripwire that checks these keys
// (`every_setting_the_dialog_names_by_hand_exists`) finds a map by its declaration
// and then skips exactly the *first* line to reach the entries. A multi-line
// `Record<…>` type would leave a colon-bearing type line where an entry belongs,
// and that tripwire fails loudly on a shape it cannot parse rather than skipping
// it. For the same reason, do not write this map's name after the word `const`
// anywhere above — including in a comment, which is how this note first broke it.
const CAPABILITY_GATES: Record<string, CapabilityGate> = {
  // The three-way rule and its `!== false` comparison live in `showsLigatureRow`,
  // which is exported and tested; what belongs here is only the reach into the
  // settings document. `?? ""` covers the document not having arrived yet, and an
  // empty stack matches nothing — so a not-yet-loaded dialog reads as unknown and
  // shows the row, the same side every other unknown lands on.
  "terminal.ligatures": (settings, fonts) =>
    showsLigatureRow(asString(settings?.["terminal.fontFamily"] ?? ""), fonts),
};

/**
 * The panel scrolls, the sidebar and the footnote do not — the same inner-scroll
 * shape `NewWorktreeDialog` uses, and for the same reason: with the modal body as
 * the scroller, choosing a group would scroll the sidebar out of reach of the next
 * choice. `minHeight` is a floor, not a fixed height — it stops the two-row Browser
 * panes group from collapsing to a modal barely taller than its own title bar. The
 * modal does still grow between that floor and the `maxHeight` cap, which is the
 * shared `min(58vh, …)` so a laptop sees the bottom of the tallest panel; a fixed
 * height would stop the growth at the cost of a lot of dead space under the short
 * groups.
 */
const PANEL_STYLE = {
  flex: 1,
  // The repo's flexbox idiom: without it, one long help line sets the panel's
  // width and the sidebar gets squeezed instead of the text wrapping.
  minWidth: 0,
  minHeight: 260,
  maxHeight: "min(58vh, 520px)",
  overflowY: "auto" as const,
  // Keeps a focused control's ring off the scrollbar, as in NewWorktreeDialog.
  paddingRight: 8,
  marginRight: -8,
};

export function SettingsDialog(props: {
  settings: SettingsDoc | null;
  saving: boolean;
  error: string | null;
  onSave: (patch: SettingsDoc) => Promise<void>;
  onClose: () => void;
}) {
  const { settings } = props;
  const locked = !settings || props.saving;
  const { catalog, error: catalogError } = useCatalog();
  const byKey = useMemo(
    () =>
      new Map<string, CatalogEntry>(
        (catalog?.settings ?? []).map((e) => [e.key, e]),
      ),
    [catalog],
  );

  // Only to learn whether this machine has a battery, so the two battery rows can
  // be left out on a desktop rather than offering controls that can never apply.
  // The dialog is remounted on every open, so this is one request per open.
  const { state: caffeinate } = useCaffeinate();
  const machine: Machine = {
    hasBattery: caffeinate?.has_battery ?? true,
    platform: caffeinate?.platform ?? null,
  };

  // Not persisted, and deliberately so: the dialog is remounted on every open, so
  // it always opens on the first group rather than wherever a previous visit ended
  // up. Null rather than "general" because the catalog names the groups and has not
  // arrived yet on the first render.
  const [group, setGroup] = useState<string | null>(null);
  const active = group ?? catalog?.groups[0]?.id ?? "";

  // What this machine has. Fetched once per open rather than read from the
  // settings document, because it is not a setting — see `api.shells`. A failure
  // leaves it null, which still renders a working picker: the stored value is
  // always an option, so the only thing lost is the list of alternatives.
  const [shells, setShells] = useState<ShellList | null>(null);
  useEffect(() => {
    let live = true;
    api
      .shells()
      .then((list) => {
        if (live) setShells(list);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, []);
  // Does the `open` shim actually win in this shell? Re-asked whenever the stored
  // shell changes, because the answer is per shell — and the daemon does not cache
  // it, so pasting the suggested line and reopening this dialog shows the change.
  // Null while in flight: the row says nothing rather than flashing a warning it is
  // about to withdraw.
  const [shimStatus, setShimStatus] = useState<ShellIntercept | null>(null);
  const settingsLoaded = settings != null;
  const shellPref = settings?.["terminal.shell"];
  const openUrlsInApp = settings?.["terminal.openUrlsInApp"];
  const interceptSystemOpen = settings?.["terminal.interceptSystemOpen"];
  useEffect(() => {
    // Nothing to probe until the document exists, and probing anyway costs a
    // real login shell: the three reads below come raw off the document, so on a
    // cold `localStorage` mirror they go undefined -> real and fire this effect a
    // second time. The endpoint's own doc promises one spawn per dialog open.
    if (!settingsLoaded) return;
    let live = true;
    setShimStatus(null);
    api
      .shellIntercept()
      .then((r) => {
        if (live) setShimStatus(r);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
    // Not just the shell: the endpoint's answer is gated on both Links switches,
    // and both are editable in this same mounted dialog. Without them, turning
    // interception off leaves a warning on screen telling the user to edit
    // ~/.bashrc for a feature they just disabled.
    // `settings != null` is part of the dependency set, not just a guard: these
    // three are read raw from the document, so on a cold `localStorage` mirror
    // they go undefined -> real and re-run the effect. That is a second login
    // shell per dialog open, for a probe whose own comment promises one.
  }, [settingsLoaded, shellPref, openUrlsInApp, interceptSystemOpen]);
  // Availability is probed against the DOM, so compute it once per open rather
  // than on every render — the list cannot change while the dialog is up.
  const fonts = useMemo(() => availableFonts(), []);
  // Both are this dialog's own intent — "I clicked Custom…" — and neither is
  // reset from the settings document. That was the last piece of machinery the
  // shell row had and the source of its fourth consecutive review finding: `save`
  // updates the document optimistically, so resetting on every document change
  // closed the field the moment a custom path was saved, whenever the shells list
  // happened to be absent. Intent belongs to the window that expressed it and dies
  // with the dialog, which is remounted on every open. Held here rather than in the
  // rows because the panels are unmounted when you leave them.
  const [customShell, setCustomShell] = useState(false);
  const [customFont, setCustomFont] = useState(false);
  const custom: CustomControls = {
    shells,
    shimStatus,
    fonts,
    customShell,
    setCustomShell,
    customFont,
    setCustomFont,
    backupDir: catalog?.machine?.backupDir ?? null,
  };

  const set = (patch: SettingsDoc) => {
    // Fire and forget: the hook holds the error, and awaiting here would freeze
    // the control that was just clicked for a loopback round trip.
    void props.onSave(patch).catch(() => {});
  };

  /**
   * One group's rows, in the order the catalog gives them.
   *
   * **Never sorted, never regrouped.** `SettingKey::ALL` is maintained as display
   * order and a Rust test asserts sections stay contiguous, so a heading is emitted
   * exactly when the section changes between consecutive rows — and the hardware
   * filter runs first, which is what makes a heading vanish along with its only row.
   */
  const rowsFor = (groupId: string): ReactNode[] => {
    if (!catalog) return [];
    const out: ReactNode[] = [];
    let section: string | undefined;
    for (const entry of catalog.settings) {
      if (entry.group !== groupId) continue;
      const hardware = HARDWARE_GATES[entry.key];
      if (hardware && !hardware(machine)) continue;
      const capable = CAPABILITY_GATES[entry.key];
      if (capable && !capable(settings, fonts)) continue;

      // Two shapes of dependency, two behaviours — today's, decided from the
      // catalog rather than from a list of keys kept in step by hand. A boolean
      // master switch *disables* the rows it governs, so you can see what it
      // governs; a gate naming a value (`worktree.storageMode === "custom"`)
      // unmounts them, because a folder for a storage mode you are not in is not
      // a disabled control, it is a different screen.
      const gateOpen = requirementMet(settings, entry.requires, byKey);
      const modeGated = entry.requires != null && entry.requires.equals !== null;
      const Trailing = TRAILING_ROWS[entry.key];

      // Decided *before* the heading, for the same reason the hardware filter is:
      // an entry that renders nothing must not leave a section title and its
      // blurb standing over an empty section. Today's only mode-gated key has no
      // section of its own, so this is latent rather than live — which is exactly
      // when it is cheap to get right, and it would otherwise appear the first
      // time somebody gives a `requires_eq` setting a heading. A trailing row
      // still counts as content: it is deliberately rendered whether or not its
      // setting's own row is.
      if (modeGated && !gateOpen && !Trailing) continue;

      if (entry.section !== section) {
        section = entry.section;
        if (section) {
          out.push(
            <SectionTitle key={`section:${section}`}>{section}</SectionTitle>,
          );
          const blurb = SECTION_BLURBS[section];
          if (blurb) {
            out.push(
              <Text key={`blurb:${section}`} size="xs" c="dimmed">
                {blurb}
              </Text>,
            );
          }
        }
      }

      const rowProps: OverrideProps = {
        entry,
        settings,
        disabled: locked || (!modeGated && !gateOpen),
        set,
        custom,
        gateOpen,
      };

      if (!(modeGated && !gateOpen)) {
        const Override = OVERRIDES[entry.key];
        out.push(
          Override ? (
            <Override key={entry.key} {...rowProps} />
          ) : (
            <Row key={entry.key} label={entry.title} help={entry.help}>
              <SettingControl {...rowProps} />
            </Row>
          ),
        );
      }
      if (Trailing) {
        out.push(<Trailing key={`${entry.key}:after`} {...rowProps} />);
      }
    }
    return out;
  };

  return (
    <Modal title="Settings" size={820} onClose={props.onClose}>
      <Stack gap="md">
        {(!settings || !catalog) && !catalogError && (
          <Text size="sm" c="dimmed">
            Loading settings from the daemon…
          </Text>
        )}
        {props.error && (
          <Text size="sm" c="var(--danger)">
            {props.error}
          </Text>
        )}
        {/* Its own line rather than folded into the one above: without a catalog
            there is nothing at all below, so the reason has to be on screen. */}
        {catalogError && (
          <Text size="sm" c="var(--danger)">
            Could not read the settings catalog: {catalogError}
          </Text>
        )}

        {catalog && (
          <>
            {/* The narrow-layout group picker. Above the Tabs rather than inside
                them: a non-tab child of a vertical Tabs root is laid out as a third
                flex column beside the sidebar. */}
            <NativeSelect
              hiddenFrom="sm"
              size="sm"
              aria-label="Settings group"
              value={active}
              data={catalog.groups.map((g) => ({ value: g.id, label: g.label }))}
              onChange={(e) => setGroup(e.currentTarget.value)}
            />

            <Tabs
              orientation="vertical"
              value={active}
              /* `settings-tabs` scopes the selected-tab background to this sidebar so
                 the pane-strip tabs (custom classes) and any other Mantine Tabs are
                 untouched — see styles.css. */
              className="settings-tabs"
              /* Mantine's onChange is nullable (a tab can be deselected); this surface
                 always has exactly one group showing, so a null falls back to the
                 first group rather than rendering an empty panel. */
              onChange={(v) => setGroup(v ?? catalog.groups[0]?.id ?? null)}
              /* The exempt-origins Textarea is `autosize`, and autosize measures the
                 element — inside a `display: none` panel it measures zero and comes back
                 collapsed to one row. Unmounting the inactive panels avoids that
                 entirely, and costs no edit in progress: every draft here commits on
                 blur, and clicking another tab blurs the field first. */
              keepMounted={false}
              /* Mantine gives a tab label `flex: 1; text-align: center`, which is right
                 for a horizontal tab bar and wrong for a sidebar: it centred each label
                 in the space left over after its icon, so four labels of different
                 lengths started at four different x positions. `flex: 1` stays, so the
                 label still fills the row and the whole tab is the hit target. */
              styles={{ tabLabel: { textAlign: "left" } }}
            >
              <Tabs.List visibleFrom="sm" w={180} style={{ flex: "none" }}>
                {catalog.groups.map((g) => (
                  <Tabs.Tab
                    key={g.id}
                    value={g.id}
                    leftSection={GROUP_ICONS[g.id]}
                  >
                    {g.label}
                  </Tabs.Tab>
                ))}
              </Tabs.List>

              {/* `pl` only from `sm`: below it the sidebar is display:none, and a
                  fixed left padding would indent the panel against nothing. */}
              {catalog.groups.map((g) => (
                <Tabs.Panel
                  key={g.id}
                  value={g.id}
                  style={PANEL_STYLE}
                  pl={{ base: 0, sm: "lg" }}
                >
                  <Stack gap="md">{rowsFor(g.id)}</Stack>
                </Tabs.Panel>
              ))}
            </Tabs>
          </>
        )}

        <Text size="xs" c="dimmed">
          Settings are stored by the veld daemon, so they are shared between Veld
          Desktop and a browser tab — and between every window. Changes save as you
          make them; there is no Save button.
        </Text>
      </Stack>
    </Modal>
  );
}

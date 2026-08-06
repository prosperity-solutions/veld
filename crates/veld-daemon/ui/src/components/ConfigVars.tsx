import { useCallback, useEffect, useState } from "react";
import {
  Alert,
  Badge,
  Button,
  Code,
  Group,
  Loader,
  Modal,
  NativeSelect,
  Paper,
  SegmentedControl,
  Stack,
  Text,
  TextInput,
  Tooltip,
} from "@mantine/core";
import {
  IconAlertTriangle,
  IconHelpCircle,
  IconLock,
} from "@tabler/icons-react";
import { api, type ConfigVar, type ConfigVarScope } from "../api";

/**
 * Editing surface for vars the config declared machine-overridable.
 *
 * The CLI can ask a human at a terminal; a run started from here cannot — there
 * is no TTY, so `veld start` refuses rather than guessing at a value nobody
 * chose. That refusal is the right behaviour and a dead end in a GUI, which is
 * what this panel exists to resolve: the human is present, so we ask them.
 *
 * Two things it never does. It never displays a secret's value — the daemon
 * sends a description for those and there is no endpoint that would return more.
 * And it never writes on render: an answer is stored because someone pressed
 * Save, which is the whole distinction between "this machine's answer" and "what
 * some process happened to fall back to".
 *
 * **The copy is deliberately plain.** Nothing here says "var", "override",
 * "scope" or "resolution" to the reader. Someone who has never opened a
 * `veld.json` can be the person sitting at the machine that needs an answer, and
 * a dialog they cannot parse is a dialog they click away from.
 */

/** Render `backticks` in a description as inline code. */
function withInlineCode(text: string) {
  // A capture group puts the delimited parts at odd indices.
  return text.split(/`([^`]+)`/).map((part, i) =>
    i % 2 === 1 ? (
      // biome-ignore lint/suspicious/noArrayIndexKey: split output is positional
      <Code key={i}>{part}</Code>
    ) : (
      part
    ),
  );
}

/** What the badge beside a name says, in the reader's terms rather than ours. */
function scopeBadge(from: ConfigVarScope): { label: string; color: string } {
  switch (from) {
    // `green` is the theme's accent tuple (theme.ts) — not a bespoke name, which
    // Mantine would pass through as a CSS colour and render as nothing.
    case "project":
      return { label: "your answer", color: "green" };
    case "worktree":
      return { label: "your answer, this checkout", color: "blue" };
    case "default":
      return { label: "project default", color: "gray" };
    case "unset":
      return { label: "needs an answer", color: "red" };
  }
}

/** Plain-language explanation of the two scopes, shown on the control itself. */
const SCOPE_HELP =
  "“Everywhere” saves your answer for this project wherever you have it checked " +
  "out on this computer — that is almost always what you want, so you only " +
  "answer once. “Just here” saves it for this one folder, for when this branch " +
  "needs something different from the rest.";

/** The ways an answer can be supplied. A plain value is the overwhelming case. */
type SourceKind = "value" | "env" | "file" | "shell";

const SOURCE_LABEL: Record<SourceKind, string> = {
  value: "Type the value",
  env: "From an environment variable",
  file: "From a file",
  shell: "From a command",
};

const SOURCE_HELP: Record<SourceKind, string> = {
  value: "Saved as you type it.",
  env: "Veld reads this environment variable each time it starts the project.",
  file: "Veld reads this file each time it starts the project.",
  shell: "Veld runs this and uses what it prints, each time it starts the project.",
};

const SOURCE_PLACEHOLDER: Record<SourceKind, string> = {
  value: "",
  env: "MY_VARIABLE",
  file: "~/.config/thing/value",
  shell: "op read op://vault/item/field",
};

export function ConfigVarsPanel({
  project,
  onChanged,
}: {
  /** Any directory inside the checkout; the daemon walks up for the config. */
  project: string;
  /** Called after a successful write, so a blocked start can retry. */
  onChanged?: () => void;
}) {
  const [vars, setVars] = useState<ConfigVar[] | null>(null);
  const [projectId, setProjectId] = useState<string>("");
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    api
      .configVars(project)
      .then((r) => {
        setVars(r.vars);
        setProjectId(r.projectId);
        setError(null);
      })
      .catch((e: Error) => setError(e.message));
  }, [project]);

  useEffect(load, [load]);

  if (error) {
    return (
      <Alert color="red" icon={<IconAlertTriangle size={16} />}>
        {error}
      </Alert>
    );
  }
  if (!vars) return <Loader size="sm" />;
  if (vars.length === 0) {
    return (
      <Text size="sm" c="dimmed">
        This project doesn’t ask you for anything. Nothing to fill in here.
      </Text>
    );
  }

  return (
    <Stack gap="md">
      {vars.map((v) => (
        <VarRow
          key={v.name}
          v={v}
          project={project}
          onSaved={() => {
            load();
            onChanged?.();
          }}
        />
      ))}
      <Text size="xs" c="dimmed">
        Saved on this computer only — never committed, never shared with your
        team. Answers marked “your answer” apply everywhere you have{" "}
        <Code>{projectId}</Code> checked out. The same values show up in a
        terminal with <Code>veld config vars</Code>.
      </Text>
    </Stack>
  );
}

function VarRow({
  v,
  project,
  onSaved,
}: {
  v: ConfigVar;
  project: string;
  onSaved: () => void;
}) {
  const [kind, setKind] = useState<SourceKind>("value");
  // Whether the "read it from somewhere else" controls are showing. Off by
  // default for *every* var, not only the ones with a fixed set of answers:
  // typing the value is what nearly everyone does, and four source options as
  // the default face made a two-digit number look like a configuration task.
  const [advanced, setAdvanced] = useState(false);
  const [draft, setDraft] = useState("");
  const [worktree, setWorktree] = useState(v.from === "worktree");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const effectiveKind: SourceKind = advanced ? kind : "value";
  const badge = scopeBadge(v.from);

  const save = async () => {
    setBusy(true);
    setErr(null);
    try {
      await api.setConfigVar({
        project,
        name: v.name,
        worktree,
        ...(effectiveKind === "value"
          ? { value: draft }
          : effectiveKind === "env"
            ? { env: draft }
            : effectiveKind === "file"
              ? { file: draft }
              : { shell: draft }),
      });
      setDraft("");
      onSaved();
    } catch (e) {
      setErr((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const clear = async () => {
    setBusy(true);
    setErr(null);
    try {
      await api.clearConfigVar(project, v.name, v.from === "worktree");
      onSaved();
    } catch (e) {
      setErr((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  // A stored answer can be removed; a project default cannot — there is nothing
  // on this machine to remove, and offering the button would imply veld could
  // edit the project's own file, which it never does.
  const clearable = v.from === "project" || v.from === "worktree";
  // A fixed set of answers only constrains a typed value. A pointer's value is
  // not known until the project starts, so veld checks it then instead.
  const pickList = v.choices && v.choices.length > 0 && effectiveKind === "value";

  return (
    <Paper withBorder radius="md" p="sm">
      <Stack gap="xs">
        <Group gap="xs" wrap="nowrap" justify="space-between">
          <Group gap="xs" wrap="nowrap">
            <Text fw={600} size="sm" ff="monospace">
              {v.name}
            </Text>
            {v.secret && (
              <Tooltip label="Sensitive — veld never shows this value back to you">
                <IconLock size={13} />
              </Tooltip>
            )}
          </Group>
          <Badge size="xs" color={badge.color}>
            {badge.label}
          </Badge>
        </Group>

        {v.description && (
          <Text size="xs" c="dimmed">
            {withInlineCode(v.description)}
          </Text>
        )}

        {/* What is in effect right now, as a code block: these are values that
            get pasted into a terminal or a config, so the exact characters
            matter and a proportional font hides a trailing space. */}
        <Group gap="xs" align="center" wrap="nowrap">
          <Text size="xs" c="dimmed" style={{ flex: "none" }}>
            Now using
          </Text>
          <Code
            block
            style={{ flex: 1, minWidth: 0 }}
            c={v.from === "unset" ? "red" : undefined}
          >
            {v.value ?? "nothing yet — the project needs an answer"}
          </Code>
        </Group>

        <Group gap="sm" align="flex-end" wrap="wrap">
          {pickList ? (
            <NativeSelect
              size="xs"
              label="Change to"
              data={[{ label: "Choose…", value: "" }, ...(v.choices ?? [])]}
              value={draft}
              onChange={(e) => setDraft(e.currentTarget.value)}
              style={{ minWidth: 180 }}
            />
          ) : (
            <TextInput
              size="xs"
              label={advanced ? SOURCE_LABEL[effectiveKind] : "Change to"}
              description={advanced ? SOURCE_HELP[effectiveKind] : undefined}
              placeholder={SOURCE_PLACEHOLDER[effectiveKind]}
              value={draft}
              onChange={(e) => setDraft(e.currentTarget.value)}
              style={{ flex: 1, minWidth: 220 }}
            />
          )}

          <Stack gap={2}>
            <Group gap={4} wrap="nowrap">
              <Text size="xs" fw={500}>
                Use it
              </Text>
              <Tooltip label={SCOPE_HELP} multiline w={300} withArrow>
                <IconHelpCircle
                  size={13}
                  style={{ opacity: 0.6, cursor: "help" }}
                />
              </Tooltip>
            </Group>
            <SegmentedControl
              size="xs"
              value={worktree ? "worktree" : "project"}
              onChange={(val) => setWorktree(val === "worktree")}
              data={[
                { label: "Everywhere", value: "project" },
                { label: "Just here", value: "worktree" },
              ]}
            />
          </Stack>

          <Button size="xs" onClick={save} loading={busy} disabled={!draft}>
            Save
          </Button>
          {clearable && (
            <Button size="xs" variant="subtle" onClick={clear} disabled={busy}>
              Remove
            </Button>
          )}
        </Group>

        {advanced && (
          <NativeSelect
            size="xs"
            label="Where the value comes from"
            data={(Object.keys(SOURCE_LABEL) as SourceKind[]).map((k) => ({
              label: SOURCE_LABEL[k],
              value: k,
            }))}
            value={kind}
            onChange={(e) => {
              setKind(e.currentTarget.value as SourceKind);
              setDraft("");
            }}
            style={{ maxWidth: 320 }}
          />
        )}

        <Group gap="xs" justify="space-between" wrap="wrap">
          <Button
            size="compact-xs"
            variant="subtle"
            onClick={() => {
              setAdvanced((a) => !a);
              setKind("value");
              setDraft("");
            }}
          >
            {advanced
              ? "Just type a value"
              : "Or point at an environment variable, file, or command…"}
          </Button>
          {v.secret && effectiveKind === "value" && (
            <Text size="xs" c="dimmed">
              Typing it saves it in veld’s database. Pointing at a password
              manager keeps it out.
            </Text>
          )}
        </Group>

        {err && (
          <Text size="xs" c="red">
            {err}
          </Text>
        )}
      </Stack>
    </Paper>
  );
}

/**
 * The dialog a start opens when the project needs values this machine has not
 * given. Answering here and pressing Retry is the GUI's replacement for the
 * terminal prompt the CLI would have shown.
 */
export function ConfigVarsDialog({
  opened,
  onClose,
  project,
  onRetry,
}: {
  opened: boolean;
  onClose: () => void;
  project: string;
  onRetry?: () => void;
}) {
  return (
    <Modal
      opened={opened}
      onClose={onClose}
      title="Values for this machine"
      size="lg"
    >
      <Stack gap="md">
        <Text size="sm" c="dimmed">
          A few things about this project depend on your computer rather than on
          the project itself — which tools you have installed, how much memory
          you can spare, where something lives on disk. The project asks; you
          answer once, here or in a terminal. Your answers are saved on this
          computer and are never committed or shared.
        </Text>
        <ConfigVarsPanel project={project} />
        {onRetry && (
          <Group justify="flex-end">
            <Button
              onClick={() => {
                onClose();
                onRetry();
              }}
            >
              Retry start
            </Button>
          </Group>
        )}
      </Stack>
    </Modal>
  );
}

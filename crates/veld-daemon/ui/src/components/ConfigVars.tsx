import { useCallback, useEffect, useState } from "react";
import {
  Alert,
  Badge,
  Button,
  Group,
  Loader,
  Modal,
  NativeSelect,
  SegmentedControl,
  Stack,
  Text,
  TextInput,
  Tooltip,
} from "@mantine/core";
import { IconAlertTriangle, IconLock } from "@tabler/icons-react";
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
 */

/** How a value's origin reads in the list. */
function scopeLabel(from: ConfigVarScope): string {
  switch (from) {
    case "project":
      return "this machine, all worktrees";
    case "worktree":
      return "this worktree only";
    case "default":
      return "config default";
    case "unset":
      return "no value";
  }
}

function scopeColor(from: ConfigVarScope): string {
  switch (from) {
    case "project":
      return "veldGreen";
    case "worktree":
      return "blue";
    case "default":
      return "gray";
    case "unset":
      return "red";
  }
}

/** The source kinds an answer can take. A pointer keeps veld out of custody. */
type SourceKind = "value" | "env" | "file" | "shell";

const SOURCE_HELP: Record<SourceKind, string> = {
  value: "Stored as-is in veld's database.",
  env: "Read from this environment variable when a run starts.",
  file: "Read from this file (relative to the project root) when a run starts.",
  shell: "Run this command when a run starts and use its output.",
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
        This project declares no machine-overridable vars. Add a{" "}
        <code>machine</code> block to a var in veld.json to let each machine
        answer it differently.
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
        Answers marked “all worktrees” are shared by every checkout of{" "}
        <code>{projectId}</code>. The same values are visible from the CLI with{" "}
        <code>veld config vars</code>.
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
  const [draft, setDraft] = useState("");
  const [worktree, setWorktree] = useState(v.from === "worktree");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const save = async () => {
    setBusy(true);
    setErr(null);
    try {
      await api.setConfigVar({
        project,
        name: v.name,
        worktree,
        ...(kind === "value"
          ? { value: draft }
          : kind === "env"
            ? { env: draft }
            : kind === "file"
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

  // A stored answer is clearable; a config default is not — there is nothing on
  // this machine to remove, and offering the button would imply veld could edit
  // veld.json, which it never does.
  const clearable = v.from === "project" || v.from === "worktree";

  return (
    <Stack gap={4}>
      <Group gap="xs" wrap="nowrap">
        <Text fw={600} size="sm">
          {v.name}
        </Text>
        {v.secret && (
          <Tooltip label="Declared secret — its value is never shown here">
            <IconLock size={14} />
          </Tooltip>
        )}
        <Badge size="xs" color={scopeColor(v.from)}>
          {scopeLabel(v.from)}
        </Badge>
      </Group>

      {v.description && (
        <Text size="xs" c="dimmed">
          {v.description}
        </Text>
      )}

      <Text size="xs" ff="monospace" c={v.from === "unset" ? "red" : undefined}>
        {v.value ?? "needs a value on this machine"}
      </Text>

      <Group gap="xs" align="flex-end" wrap="wrap">
        {/* A var with enumerated choices gets a picker: typing a value the
            config forbids is a round-trip to a 422 for no reason. */}
        {v.choices && v.choices.length > 0 ? (
          <NativeSelect
            size="xs"
            label="Value"
            data={[{ label: "Choose…", value: "" }, ...v.choices]}
            value={draft}
            onChange={(e) => {
              setKind("value");
              setDraft(e.currentTarget.value);
            }}
          />
        ) : (
          <>
            <NativeSelect
              size="xs"
              label="Source"
              data={[
                { label: "Value", value: "value" },
                { label: "Env var", value: "env" },
                { label: "File", value: "file" },
                { label: "Command", value: "shell" },
              ]}
              value={kind}
              onChange={(e) => setKind(e.currentTarget.value as SourceKind)}
            />
            <TextInput
              size="xs"
              label={kind === "value" ? "Value" : "Pointer"}
              description={SOURCE_HELP[kind]}
              value={draft}
              onChange={(e) => setDraft(e.currentTarget.value)}
              style={{ flex: 1, minWidth: 220 }}
            />
          </>
        )}

        <SegmentedControl
          size="xs"
          value={worktree ? "worktree" : "project"}
          onChange={(val) => setWorktree(val === "worktree")}
          data={[
            { label: "All worktrees", value: "project" },
            { label: "This one", value: "worktree" },
          ]}
        />

        <Button size="xs" onClick={save} loading={busy} disabled={!draft}>
          Save
        </Button>
        {clearable && (
          <Button size="xs" variant="subtle" onClick={clear} disabled={busy}>
            Clear
          </Button>
        )}
      </Group>

      {v.secret && kind === "value" && (
        <Text size="xs" c="dimmed">
          Saving a value stores it in veld's database. To keep veld holding only
          a pointer, choose Env var, File, or Command.
        </Text>
      )}
      {err && (
        <Text size="xs" c="red">
          {err}
        </Text>
      )}
    </Stack>
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
          This project declares vars each machine answers for itself — a
          container runtime, a memory ceiling, a path to a local tool. The
          declaration is committed; your answers stay on this machine.
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

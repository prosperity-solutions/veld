import { type FormEvent, type ReactNode, useState } from "react";
import {
  Button,
  Checkbox,
  Group,
  Modal as MantineModal,
  Stack,
  Text,
  TextInput,
} from "@mantine/core";
import { api, type Repo } from "../api";

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

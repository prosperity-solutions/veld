import { type FormEvent, type ReactNode, useState } from "react";

export function Modal(props: {
  title: string;
  onClose: () => void;
  children: ReactNode;
}) {
  return (
    <>
      <div className="scrim" onClick={props.onClose} />
      <div className="modal" role="dialog" aria-label={props.title}>
        <h3>{props.title}</h3>
        {props.children}
      </div>
    </>
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

export function ImportRepoDialog(props: {
  onImport: (path: string) => Promise<void>;
  onClose: () => void;
}) {
  const [path, setPath] = useState("");
  const { busy, error, submit } = useSubmit(() => props.onImport(path.trim()));
  return (
    <Modal title="Import repository" onClose={props.onClose}>
      <form className="modal-body" onSubmit={submit}>
        <div className="field">
          <label htmlFor="repo-path">Absolute path to the repository</label>
          <input
            id="repo-path"
            className="mono"
            value={path}
            onChange={(e) => setPath(e.target.value)}
            placeholder="/Users/you/git/my-project"
            autoFocus
          />
        </div>
        <p style={{ margin: 0, fontSize: 11.5, color: "var(--faint)" }}>
          Any directory inside the repo works — the main checkout and existing
          worktrees are discovered automatically.
        </p>
        {error && <div className="error-text">{error}</div>}
        <button className="primary-btn" disabled={busy || !path.trim()}>
          {busy ? "Importing…" : "Import"}
        </button>
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
      <form className="modal-body" onSubmit={submit}>
        <div className="field">
          <label htmlFor="wt-branch">Branch</label>
          <input
            id="wt-branch"
            className="mono"
            value={branch}
            onChange={(e) => setBranch(e.target.value)}
            placeholder="feat/checkout-v2"
            autoFocus
          />
        </div>
        <label
          style={{
            display: "flex",
            gap: 7,
            alignItems: "center",
            fontSize: 12.5,
          }}
        >
          <input
            type="checkbox"
            checked={createBranch}
            onChange={(e) => setCreateBranch(e.target.checked)}
          />
          Create this branch (from the repo&apos;s current HEAD)
        </label>
        <div className="field">
          <label htmlFor="wt-alias">Alias (optional)</label>
          <input
            id="wt-alias"
            className="mono"
            value={alias}
            onChange={(e) => setAlias(e.target.value)}
            placeholder="derived from the branch name"
          />
        </div>
        {error && <div className="error-text">{error}</div>}
        <button className="primary-btn" disabled={busy || !branch.trim()}>
          {busy ? "Creating…" : "Create worktree"}
        </button>
      </form>
    </Modal>
  );
}

export function RenameWorktreeDialog(props: {
  current: string;
  onRename: (alias: string) => Promise<void>;
  onDelete: () => Promise<void>;
  isMain: boolean;
  onClose: () => void;
}) {
  const [alias, setAlias] = useState(props.current);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const rename = useSubmit(() => props.onRename(alias.trim()));
  const del = useSubmit(() => props.onDelete());
  return (
    <Modal title="Edit worktree" onClose={props.onClose}>
      <form className="modal-body" onSubmit={rename.submit}>
        <div className="field">
          <label htmlFor="wt-rename">Alias</label>
          <input
            id="wt-rename"
            className="mono"
            value={alias}
            onChange={(e) => setAlias(e.target.value)}
            autoFocus
          />
        </div>
        {rename.error && <div className="error-text">{rename.error}</div>}
        <button
          className="primary-btn"
          disabled={rename.busy || !alias.trim()}
        >
          {rename.busy ? "Saving…" : "Save"}
        </button>
      </form>
      {!props.isMain && (
        <div
          className="modal-body"
          style={{ borderTop: "1px solid var(--border)" }}
        >
          {confirmDelete ? (
            <>
              <p style={{ margin: 0, fontSize: 12, color: "var(--muted)" }}>
                Removes the checkout from disk (git refuses if it has
                uncommitted changes). The branch itself is kept.
              </p>
              {del.error && <div className="error-text">{del.error}</div>}
              <button
                className="btn"
                style={{
                  color: "var(--danger)",
                  background: "var(--danger-bg)",
                  border: "none",
                }}
                onClick={del.submit}
                disabled={del.busy}
              >
                {del.busy ? "Removing…" : "Really remove worktree"}
              </button>
            </>
          ) : (
            <button
              className="btn"
              style={{ color: "var(--danger)" }}
              onClick={(e) => {
                e.preventDefault();
                setConfirmDelete(true);
              }}
            >
              Remove worktree…
            </button>
          )}
        </div>
      )}
    </Modal>
  );
}

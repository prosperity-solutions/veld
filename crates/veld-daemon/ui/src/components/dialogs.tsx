import { type FormEvent, type ReactNode, useState } from "react";
import { api, type Repo } from "../api";

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
      <form className="modal-body" onSubmit={submit}>
        <div className="field">
          <label htmlFor="repo-path">Repository directory</label>
          <div style={{ display: "flex", gap: 6 }}>
            <input
              id="repo-path"
              className="mono"
              style={{ flex: 1 }}
              value={path}
              onChange={(e) => setPath(e.target.value)}
              placeholder="/Users/you/git/my-project"
              autoFocus
            />
            <button
              type="button"
              className="btn"
              onClick={browse}
              disabled={picking}
            >
              {picking ? "Choosing…" : "Browse…"}
            </button>
          </div>
        </div>
        <p style={{ margin: 0, fontSize: 11.5, color: "var(--faint)" }}>
          Any directory inside the repo works — the main checkout and existing
          worktrees are discovered automatically.
        </p>
        {pickError && <div className="error-text">{pickError}</div>}
        {error && <div className="error-text">{error}</div>}
        <button
          className="primary-btn"
          disabled={busy || picking || !path.trim()}
        >
          {busy ? "Importing…" : "Import"}
        </button>
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
      <form className="modal-body" onSubmit={submit}>
        <p style={{ margin: 0, fontSize: 12.5, color: "var(--muted)" }}>
          Removes the project (and its worktree list) from Veld Desktop only —
          nothing on disk is touched. You can re-import it anytime.
        </p>
        <p className="mono" style={{ margin: 0, fontSize: 11, color: "var(--faint)" }}>
          {props.repo.root}
        </p>
        {error && <div className="error-text">{error}</div>}
        <button
          className="btn"
          style={{
            color: "var(--danger)",
            background: "var(--danger-bg)",
            border: "none",
            justifyContent: "center",
          }}
          disabled={busy}
        >
          {busy ? "Removing…" : "Remove project"}
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
  onDelete: (force: boolean) => Promise<void>;
  isMain: boolean;
  onClose: () => void;
}) {
  const [alias, setAlias] = useState(props.current);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [force, setForce] = useState(false);
  const rename = useSubmit(() => props.onRename(alias.trim()));
  const del = useSubmit(() => props.onDelete(force));
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
                uncommitted changes). The branch itself is kept. Stop any
                running environment in this worktree first — removing pulls
                the directory out from under it.
              </p>
              {del.error && <div className="error-text">{del.error}</div>}
              {del.error && (
                <label
                  style={{
                    display: "flex",
                    gap: 7,
                    alignItems: "center",
                    fontSize: 12,
                    color: "var(--danger)",
                  }}
                >
                  <input
                    type="checkbox"
                    checked={force}
                    onChange={(e) => setForce(e.target.checked)}
                  />
                  Force remove — discards uncommitted changes
                </label>
              )}
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

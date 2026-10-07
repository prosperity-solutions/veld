/**
 * Changed files: what this branch changed, one click from a new pane.
 *
 * The cheap half of reviewing an agent's work — not a diff view, a *list*: every
 * file changed against the merge-base with the default branch, plus whatever is
 * uncommitted or untracked, each a click from a file pane whose gutter marks the
 * lines that moved. Fetched when the modal opens and not polled: it is a screen you
 * pick from, and one left open is not being read.
 */

import { Loader, Modal, TextInput } from "@mantine/core";
import { IconSearch } from "@tabler/icons-react";
import { useEffect, useState } from "react";

import { api, type ChangedFile, type WorktreeChanges } from "../api";
import { fileDir } from "../panes/places";
import { viewableAsText } from "./textKind";

/** One letter per status, the way `git status --short` spells them. */
const STATUS_MARK: Record<ChangedFile["status"], string> = {
  added: "A",
  modified: "M",
  deleted: "D",
  renamed: "R",
  untracked: "U",
};

const STATUS_WORD: Record<ChangedFile["status"], string> = {
  added: "added",
  modified: "modified",
  deleted: "deleted",
  renamed: "renamed",
  untracked: "untracked",
};

/** Why a row cannot be opened, or `null` when it can. */
export function unopenableReason(f: ChangedFile): string | null {
  // A deleted file has no text left to show; the row stays so the list is the
  // whole answer to "what did this branch touch".
  if (f.status === "deleted") return `${f.path} was deleted`;
  // A file the pane would refuse by name opens into a "not found" that reads like
  // a bug, so it is refused here with the real reason instead.
  if (!viewableAsText(f.path)) return "Veld's file view shows text files only";
  return null;
}

/** The rows a query keeps — the same substring-over-the-path rule as the Files modal. */
export function filterChanges(files: ChangedFile[], query: string): ChangedFile[] {
  const q = query.trim().toLowerCase();
  return q === "" ? files : files.filter((f) => f.path.toLowerCase().includes(q));
}

export function ChangedFilesModal(props: {
  worktreeId: number;
  opened: boolean;
  onClose: () => void;
  onOpen: (path: string) => void;
}) {
  const [answer, setAnswer] = useState<
    { worktreeId: number; changes: WorktreeChanges } | { worktreeId: number; error: string } | null
  >(null);
  const [query, setQuery] = useState("");
  const { opened, worktreeId } = props;

  useEffect(() => {
    if (!opened) return;
    let cancelled = false;
    setQuery("");
    api
      .worktreeChanges(worktreeId)
      .then((changes) => !cancelled && setAnswer({ worktreeId, changes }))
      .catch(
        (e) =>
          !cancelled &&
          setAnswer({ worktreeId, error: e instanceof Error ? e.message : String(e) }),
      );
    return () => {
      cancelled = true;
    };
  }, [opened, worktreeId]);

  // Compared during render, like the chooser's other per-worktree answers: a list
  // fetched for another worktree must read as "not yet", not as this one's.
  const current = answer?.worktreeId === worktreeId ? answer : null;
  const shown = current && "changes" in current ? filterChanges(current.changes.files, query) : [];

  return (
    <Modal opened={opened} onClose={props.onClose} title="Changed files" size="lg" centered>
      <TextInput
        data-autofocus
        value={query}
        onChange={(e) => setQuery(e.currentTarget.value)}
        placeholder="Search by name or folder"
        leftSection={<IconSearch size={14} />}
        mb="sm"
        aria-label="Search changed files"
      />
      {current && "changes" in current && (
        <p className="faint changed-base">
          {current.changes.base
            ? `Against ${current.changes.baseRef ?? shortBase(current.changes.base)}, plus anything uncommitted.`
            : "No default branch found — showing uncommitted and untracked files only."}
          {current.changes.truncated ? " The list is cut short; there are more." : ""}
        </p>
      )}
      <div className="place-list bookmarks-modal-list">
        {!current ? (
          <div className="file-pane-message">
            <Loader size="sm" />
          </div>
        ) : "error" in current ? (
          <p className="faint place-nomatch">Could not list the changes: {current.error}</p>
        ) : current.changes.files.length === 0 ? (
          <p className="faint place-nomatch">Nothing has changed on this branch yet.</p>
        ) : shown.length === 0 ? (
          <p className="faint place-nomatch">No changed file matches {query}.</p>
        ) : (
          shown.map((f) => {
            const refused = unopenableReason(f);
            return (
              <div className="link-row" key={`${f.status}:${f.path}`} data-status={f.status}>
                <button
                  type="button"
                  className="link-open"
                  disabled={refused !== null}
                  onClick={() => props.onOpen(f.path)}
                  title={refused ?? `Open ${f.path}`}
                >
                  <span className={`place-mark changed-mark ${f.status}`} title={STATUS_WORD[f.status]}>
                    {STATUS_MARK[f.status]}
                  </span>
                  <span className="link-text">
                    <span className="name">{f.path.split("/").pop() ?? f.path}</span>
                    <span className="url">
                      {f.oldPath
                        ? `from ${f.oldPath}`
                        : (fileDir(f.path) ?? "worktree root")}
                    </span>
                  </span>
                </button>
              </div>
            );
          })
        )}
      </div>
    </Modal>
  );
}

/** A sha shortened; a ref name as it is. */
function shortBase(base: string): string {
  return /^[0-9a-f]{40}$/i.test(base) ? base.slice(0, 8) : base;
}

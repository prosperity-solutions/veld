import { useCallback, useEffect, useMemo, useState } from "react";
import {
  api,
  type EnvironmentList,
  type Repo,
  type RepoList,
  type Worktree,
} from "./api";
import {
  activeRun,
  filterWorktrees,
  runsForWorktree,
  sortedUrls,
  worktreeStatus,
} from "./model";
import { Wordmark } from "./components/Wordmark";
import {
  ImportRepoDialog,
  Modal,
  NewWorktreeDialog,
  RenameWorktreeDialog,
} from "./components/dialogs";

const POLL_MS = 5000;

// The Electron shell loads /v2?shell=electron: the top bar then doubles as
// the frameless window's native title bar (drag region, traffic-light inset).
const isElectron =
  new URLSearchParams(window.location.search).get("shell") === "electron";

function usePersisted(key: string, initial: string): [string, (v: string) => void] {
  const [value, setValue] = useState(
    () => window.localStorage.getItem(key) ?? initial,
  );
  const set = useCallback(
    (v: string) => {
      setValue(v);
      window.localStorage.setItem(key, v);
    },
    [key],
  );
  return [value, set];
}

export function App() {
  const [theme, setTheme] = usePersisted("veld.theme", "dark");
  useEffect(() => {
    document.body.dataset.theme = theme;
  }, [theme]);

  // ---- polled server state ------------------------------------------------
  const [repoList, setRepoList] = useState<RepoList | null>(null);
  const [envs, setEnvs] = useState<EnvironmentList | null>(null);
  const [offline, setOffline] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const [repos, environments] = await Promise.all([
        api.repos(),
        api.environments(),
      ]);
      setRepoList(repos);
      setEnvs(environments);
      setOffline(false);
    } catch {
      setOffline(true);
    }
  }, []);

  useEffect(() => {
    void refresh();
    const t = window.setInterval(() => void refresh(), POLL_MS);
    return () => window.clearInterval(t);
  }, [refresh]);

  // ---- selection ----------------------------------------------------------
  const [activeRepoRoot, setActiveRepoRoot] = usePersisted("veld.repo", "");
  const [activeWtKey, setActiveWtKey] = usePersisted("veld.worktree", "");

  const repos = useMemo(() => repoList?.repos ?? [], [repoList]);
  const repo: Repo | null =
    repos.find((r) => r.root === activeRepoRoot) ?? repos[0] ?? null;
  const worktrees = useMemo(() => repo?.worktrees ?? [], [repo]);
  const worktree: Worktree | null =
    worktrees.find((w) => String(w.id) === activeWtKey) ??
    worktrees.find((w) => w.is_main) ??
    worktrees[0] ??
    null;

  const selectWorktree = (w: Worktree) => {
    setActiveRepoRoot(w.repo_root);
    setActiveWtKey(String(w.id));
  };

  // ---- derived run state --------------------------------------------------
  const runs = worktree ? runsForWorktree(envs, worktree) : [];
  const run = activeRun(runs);
  const urls = sortedUrls(run);
  const status = worktreeStatus(runs);

  // Preset choice, remembered per worktree.
  const presetKey = worktree ? `veld.preset.${worktree.path}` : "veld.preset._";
  const [presetChoice, setPresetChoice] = usePersisted(presetKey, "");
  const preset =
    worktree && worktree.presets.includes(presetChoice) ? presetChoice : "";

  // Optimistic pending marker while a 202'd start/stop/restart takes effect.
  const [pending, setPending] = useState<string | null>(null);
  useEffect(() => setPending(null), [status]);

  const [actionError, setActionError] = useState<string | null>(null);
  const act = async (label: string, fn: () => Promise<void>) => {
    setActionError(null);
    setPending(label);
    try {
      await fn();
    } catch (e) {
      setPending(null);
      setActionError(e instanceof Error ? e.message : String(e));
    }
  };

  // ---- dialogs ------------------------------------------------------------
  const [dialog, setDialog] = useState<
    | { kind: "none" }
    | { kind: "import" }
    | { kind: "new-worktree" }
    | { kind: "rename"; worktree: Worktree }
    | { kind: "search" }
  >({ kind: "none" });
  const closeDialog = () => setDialog({ kind: "none" });

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "k") {
        e.preventDefault();
        setDialog({ kind: "search" });
      }
      if (e.key === "Escape") closeDialog();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const [urlsOpen, setUrlsOpen] = useState(false);
  const [railWide, setRailWide] = useState(false);

  // ---- render -------------------------------------------------------------
  return (
    <div className="frame">
      <TopBar
        repos={repos}
        repo={repo}
        worktree={worktree}
        preset={preset}
        onPreset={setPresetChoice}
        running={status !== "stopped"}
        pending={pending}
        run={run}
        urlCount={urls.length}
        urlsOpen={urlsOpen}
        onToggleUrls={() => setUrlsOpen((v) => !v)}
        onSelectRepo={(root) => {
          setActiveRepoRoot(root);
          setActiveWtKey("");
        }}
        onImport={() => setDialog({ kind: "import" })}
        onStart={() =>
          worktree &&
          void act("start", () => api.startRun(worktree.id, preset || null))
        }
        onStop={() => run && void act("stop", () => api.stopRun(run.name))}
        onRestart={() =>
          run && void act("restart", () => api.restartRun(run.name))
        }
        onSearch={() => setDialog({ kind: "search" })}
        theme={theme}
        onToggleTheme={() => setTheme(theme === "dark" ? "light" : "dark")}
      />
      {urlsOpen && (
        <UrlsPopover
          runName={run?.name ?? null}
          urls={urls}
          onClose={() => setUrlsOpen(false)}
        />
      )}

      {offline && (
        <div
          style={{
            padding: "6px 14px",
            background: "var(--warn-bg)",
            color: "var(--warn)",
            fontSize: 12,
            flex: "none",
          }}
        >
          Can&apos;t reach the veld daemon — is it running? Retrying…
        </div>
      )}
      {actionError && (
        <div
          style={{
            padding: "6px 14px",
            background: "var(--danger-bg)",
            color: "var(--danger)",
            fontSize: 12,
            flex: "none",
            display: "flex",
            gap: 10,
          }}
        >
          <span style={{ flex: 1 }}>{actionError}</span>
          <button
            style={{ border: "none", background: "none", color: "inherit" }}
            onClick={() => setActionError(null)}
          >
            ×
          </button>
        </div>
      )}

      {repos.length === 0 ? (
        <div className="center-page">
          <Wordmark />
          <p>
            Import a git repository to manage its worktrees and drive veld runs
            from here. Terminals and embedded previews arrive in later
            increments.
          </p>
          <button
            className="primary-btn"
            onClick={() => setDialog({ kind: "import" })}
          >
            Import repository…
          </button>
        </div>
      ) : (
        <div className="workspace">
          <Rail
            worktrees={worktrees}
            active={worktree}
            envs={envs}
            wide={railWide}
            onToggle={() => setRailWide((v) => !v)}
            onSelect={selectWorktree}
            onAdd={() => setDialog({ kind: "new-worktree" })}
            onEdit={(w) => setDialog({ kind: "rename", worktree: w })}
          />
          <TerminalPlaceholder worktree={worktree} />
          <UrlLauncher worktree={worktree} urls={urls} />
        </div>
      )}

      {dialog.kind === "import" && (
        <ImportRepoDialog
          onClose={closeDialog}
          onImport={async (path) => {
            const imported = await api.importRepo(path);
            await refresh();
            setActiveRepoRoot(imported.root);
            setActiveWtKey("");
            closeDialog();
          }}
        />
      )}
      {dialog.kind === "new-worktree" && repo && (
        <NewWorktreeDialog
          onClose={closeDialog}
          onCreate={async (body) => {
            const created = await api.createWorktree({
              repo_root: repo.root,
              ...body,
            });
            await refresh();
            setActiveWtKey(String(created.id));
            closeDialog();
          }}
        />
      )}
      {dialog.kind === "rename" && (
        <RenameWorktreeDialog
          current={dialog.worktree.alias}
          isMain={dialog.worktree.is_main}
          onClose={closeDialog}
          onRename={async (alias) => {
            await api.renameWorktree(dialog.worktree.id, alias);
            await refresh();
            closeDialog();
          }}
          onDelete={async () => {
            await api.deleteWorktree(dialog.worktree.id, false);
            await refresh();
            closeDialog();
          }}
        />
      )}
      {dialog.kind === "search" && (
        <SearchOverlay
          project={repo?.name ?? ""}
          worktrees={worktrees}
          envs={envs}
          onSelect={(w) => {
            selectWorktree(w);
            closeDialog();
          }}
          onClose={closeDialog}
        />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Top bar
// ---------------------------------------------------------------------------

function TopBar(props: {
  repos: Repo[];
  repo: Repo | null;
  worktree: Worktree | null;
  preset: string;
  onPreset: (p: string) => void;
  running: boolean;
  pending: string | null;
  run: { name: string } | null;
  urlCount: number;
  urlsOpen: boolean;
  onToggleUrls: () => void;
  onSelectRepo: (root: string) => void;
  onImport: () => void;
  onStart: () => void;
  onStop: () => void;
  onRestart: () => void;
  onSearch: () => void;
  theme: string;
  onToggleTheme: () => void;
}) {
  const { worktree } = props;
  const canRun = !!worktree?.has_veld_config;
  return (
    <div className={`topbar${isElectron ? " electron" : ""}`}>
      <Wordmark />
      {props.repos.length > 0 && (
        <select
          title="Switch project"
          value={props.repo?.root ?? ""}
          onChange={(e) =>
            e.target.value === "__import__"
              ? props.onImport()
              : props.onSelectRepo(e.target.value)
          }
          style={{ fontWeight: 600 }}
        >
          {props.repos.map((r) => (
            <option key={r.root} value={r.root}>
              {r.name}
            </option>
          ))}
          <option value="__import__">Import repository…</option>
        </select>
      )}
      {worktree && (
        <>
          <span style={{ color: "var(--faint)" }}>/</span>
          <span className="mono" style={{ fontSize: 12.5, fontWeight: 600 }}>
            {worktree.branch}
          </span>
          <div className="sep" />
          {canRun && worktree.presets.length > 0 && (
            <select
              title="Preset"
              className="mono"
              value={props.preset}
              onChange={(e) => props.onPreset(e.target.value)}
            >
              <option value="">default</option>
              {worktree.presets.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </select>
          )}
          {canRun && (
            <>
              <button
                title={props.running ? "Stop run" : "Start run"}
                className={`iconbtn runbtn ${props.running ? "stop" : "start"}`}
                disabled={props.pending !== null}
                onClick={props.running ? props.onStop : props.onStart}
              >
                {props.pending ? "…" : props.running ? "■" : "▶"}
              </button>
              <button
                title="Restart"
                className="iconbtn"
                disabled={!props.running || props.pending !== null}
                onClick={props.onRestart}
              >
                ⟳
              </button>
              <button
                title="Run URLs"
                className="btn"
                onClick={props.onToggleUrls}
              >
                🌐{" "}
                <span className="mono" style={{ fontSize: 11, color: "var(--faint)" }}>
                  {props.urlCount}
                </span>{" "}
                ▾
              </button>
            </>
          )}
          {!canRun && (
            <span className="chip" title="No veld.json in this worktree">
              no veld config
            </span>
          )}
        </>
      )}
      <div style={{ flex: 1 }} />
      {isElectron && (
        <span
          className="mono"
          style={{ fontSize: 10.5, color: "var(--faint)" }}
          title="Rendered by the desktop shell into the native title bar"
        >
          ◆ native title bar
        </span>
      )}
      <button title="Search (⌘K)" className="iconbtn" onClick={props.onSearch}>
        ⌕
      </button>
      <button title="Theme" className="iconbtn" onClick={props.onToggleTheme}>
        {props.theme === "dark" ? "☀" : "☾"}
      </button>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Worktree rail
// ---------------------------------------------------------------------------

function Rail(props: {
  worktrees: Worktree[];
  active: Worktree | null;
  envs: EnvironmentList | null;
  wide: boolean;
  onToggle: () => void;
  onSelect: (w: Worktree) => void;
  onAdd: () => void;
  onEdit: (w: Worktree) => void;
}) {
  return (
    <div className={`rail${props.wide ? " wide" : ""}`}>
      <div className="rail-head">
        <button
          className="rail-toggle"
          title="Expand / collapse"
          onClick={props.onToggle}
        >
          {props.wide ? "‹" : "›"}
        </button>
        <button className="rail-add" title="New worktree" onClick={props.onAdd}>
          +
        </button>
      </div>
      <div className="rail-list">
        {props.worktrees.map((w) => {
          const status = worktreeStatus(runsForWorktree(props.envs, w));
          return (
            <button
              key={w.id}
              className={`wt-row${props.active?.id === w.id ? " active" : ""}`}
              title={w.branch}
              onClick={() => props.onSelect(w)}
            >
              <span className={`dot ${status}`} />
              <span className="wt-alias">{w.alias}</span>
              {props.wide && <span className="wt-branch">{w.branch}</span>}
              {props.wide && (
                <span
                  className="wt-edit"
                  title="Rename / remove"
                  role="button"
                  tabIndex={0}
                  onClick={(e) => {
                    e.stopPropagation();
                    props.onEdit(w);
                  }}
                >
                  ✎
                </span>
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Panes (foundation: terminal is a placeholder, browser pane is the launcher)
// ---------------------------------------------------------------------------

function TerminalPlaceholder(props: { worktree: Worktree | null }) {
  return (
    <div className="terminal-pane">
      <div className="pane-tabs">
        <span className="chip">terminal</span>
        <div style={{ flex: 1 }} />
      </div>
      <div
        className="terminal-body"
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        <span className="placeholder-chip">
          terminal panes — later increment
          {props.worktree ? ` · ${props.worktree.path}` : ""}
        </span>
      </div>
    </div>
  );
}

function UrlLauncher(props: {
  worktree: Worktree | null;
  urls: Array<[string, string]>;
}) {
  return (
    <div className="launcher">
      <div className="pane-tabs">
        <span>Run URLs</span>
        <span className="chip">opens in your browser</span>
        <div style={{ flex: 1 }} />
        {props.urls.length > 1 && (
          <button
            className="btn"
            style={{ border: "none", color: "var(--accent)" }}
            onClick={() =>
              props.urls.forEach(([, url]) => window.open(url, "_blank"))
            }
          >
            Open all ↗
          </button>
        )}
      </div>
      <div className="launcher-list">
        {props.urls.length === 0 && (
          <div className="note-card">
            {props.worktree?.has_veld_config
              ? "No live URLs — start the run to see its services here."
              : "This worktree has no veld.json, so there is nothing to run."}
          </div>
        )}
        {props.urls.map(([name, url]) => (
          <ServiceCard key={name} name={name} url={url} />
        ))}
        {props.urls.length > 0 && (
          <div className="note-card">
            Embedded preview &amp; isolated sessions arrive with the desktop
            app&apos;s webview increment.
          </div>
        )}
      </div>
    </div>
  );
}

function ServiceCard(props: { name: string; url: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="svc-card">
      <span className="dot running" style={{ animation: "none" }} />
      <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 1 }}>
        <span className="name">{props.name}</span>
        <span className="url">{props.url}</span>
      </div>
      <button
        className="iconbtn"
        style={{ border: "none", width: 26, height: 26 }}
        title="Copy URL"
        onClick={() => {
          void navigator.clipboard.writeText(props.url);
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1200);
        }}
      >
        {copied ? "✓" : "⧉"}
      </button>
      <a href={props.url} target="_blank" rel="noreferrer" title="Open" style={{ fontWeight: 600 }}>
        ↗
      </a>
    </div>
  );
}

// ---------------------------------------------------------------------------
// URLs popover + search overlay
// ---------------------------------------------------------------------------

function UrlsPopover(props: {
  runName: string | null;
  urls: Array<[string, string]>;
  onClose: () => void;
}) {
  return (
    <div className="popover">
      <div className="popover-head">
        <span>
          {props.runName ? (
            <>
              Run <span className="mono">{props.runName}</span> ·{" "}
              {props.urls.length} URLs
            </>
          ) : (
            "No active run"
          )}
        </span>
        <div style={{ flex: 1 }} />
        {props.urls.length > 0 && (
          <button
            className="btn"
            style={{ border: "none", color: "var(--accent)", padding: "2px 6px" }}
            onClick={() => {
              void navigator.clipboard.writeText(
                props.urls.map(([, u]) => u).join("\n"),
              );
              props.onClose();
            }}
          >
            Copy all
          </button>
        )}
      </div>
      <div className="popover-list">
        {props.urls.map(([name, url]) => (
          <ServiceCard key={name} name={name} url={url} />
        ))}
        {props.urls.length === 0 && (
          <div className="note-card">Start a run to see its URLs here.</div>
        )}
      </div>
    </div>
  );
}

function SearchOverlay(props: {
  project: string;
  worktrees: Worktree[];
  envs: EnvironmentList | null;
  onSelect: (w: Worktree) => void;
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const matches = filterWorktrees(props.worktrees, query);
  return (
    <Modal title={`Search ${props.project}`} onClose={props.onClose}>
      <div className="modal-body">
        <input
          className="mono"
          placeholder="Search worktrees…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          autoFocus
        />
        <div className="section-label">Worktrees</div>
        {matches.map((w) => {
          const status = worktreeStatus(runsForWorktree(props.envs, w));
          return (
            <button
              key={w.id}
              className="wt-row"
              onClick={() => props.onSelect(w)}
            >
              <span className={`dot ${status}`} />
              <span className="wt-alias">{w.alias}</span>
              <span className="wt-branch">{w.branch}</span>
              <span style={{ fontSize: 11, color: "var(--faint)" }}>
                {status}
              </span>
            </button>
          );
        })}
        {matches.length === 0 && (
          <div className="note-card">No matching worktrees.</div>
        )}
      </div>
    </Modal>
  );
}

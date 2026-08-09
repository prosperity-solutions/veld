const test = require("node:test");
const assert = require("node:assert");
const fs = require("node:fs");
const path = require("node:path");

const preload = fs.readFileSync(path.join(__dirname, "preload.js"), "utf8");

/**
 * The bridge members a shipped `/ide` bundle calls **without** checking for
 * them.
 *
 * Worktree ownership moved to the daemon, and deleting these from the bridge
 * looked free — nothing in the current bundle calls them. But this shell loads
 * whatever `/ide` the *daemon* serves, and the two update independently, so a
 * shell newer than the daemon is a real pairing rather than a hypothetical one.
 * The bundle it then serves calls `shell.onYieldWorktree(...)` and
 * `desktopWindow.holdsWorktrees(...)` unguarded — they were never optional — and
 * `window.veldDesktop.window` still exists, so those are `TypeError`s inside a
 * `useEffect`. With no error boundary anywhere in `/ide`, that is a white screen
 * rather than a missing feature.
 *
 * A source-level check because the bridge cannot be imported outside Electron
 * (`contextBridge`, `ipcRenderer`), and the property that matters is exactly
 * "the name is still exposed". Remove an entry from this list only once no
 * shipped bundle calls it.
 */
const RETIRED_BUT_STILL_CALLED = [
  "claimWorktree",
  "claimedElsewhere",
  "onClaimsChanged",
  "holdsWorktrees",
  "worktreesGone",
  "onYieldWorktree",
  "yielded",
  "yieldsReady",
];

test("the bridge keeps stubs for the ownership API an older /ide still calls", () => {
  for (const name of RETIRED_BUT_STILL_CALLED) {
    assert.match(
      preload,
      new RegExp(`\\b${name}\\s*:`),
      `preload.js must still expose ${name}: an /ide bundle older than this shell calls it ` +
        "unguarded, and a missing member white-screens the whole app",
    );
  }
});

/**
 * …and `claimWorktree` must still *arbitrate*, not answer a blanket yes.
 *
 * The bundle that calls it keeps its main-window layouts in one `localStorage`
 * key shared between windows, so the claim is the only thing between two windows
 * and one set of terminal ids — and a second PTY attach takes the session over.
 * A stub that always grants would make `⌘N` open a second copy of the
 * last-selected worktree and have the two trade every shell.
 */
test("the legacy claim stub asks the main process rather than granting", () => {
  assert.match(
    preload,
    /claimWorktree:[^\n]*\n?[^\n]*ipcRenderer\.invoke\("veld:window:legacy-claim"/,
    "claimWorktree must route to the shell's own arbitration",
  );
  assert.match(
    preload,
    /claimedElsewhere:[^\n]*ipcRenderer\.invoke\("veld:window:legacy-elsewhere"/,
    "claimedElsewhere must report what other windows show",
  );
});

test("the bridge exposes what the current /ide needs", () => {
  for (const name of ["showsWorktree", "focusSelf"]) {
    assert.match(preload, new RegExp(`\\b${name}\\s*:`), `preload.js must expose ${name}`);
  }
});

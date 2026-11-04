// `veld://session/<id>` — a session link handed to Veld Desktop.
//
// The link a coding agent puts in a pull request is an https URL on the user's own
// Veld (`/session/<id>`), because GitHub renders no other scheme. That page asks
// the daemon to send a window to the session; when no window is there to take it
// and this app is installed, it hands the link here instead, as `veld://`, which
// is what starts the app when it is not running.
//
// From here it is the same request the page makes — `POST
// /api/session-links/<id>/open` — and the daemon still decides which window goes
// where (`push_reveal` in `crates/veld-daemon/src/ide.rs`). The one thing this
// adds is patience: on a cold start no window of ours is connected yet, so the
// first answers say nobody took it, and the request is repeated until one does.
//
// Pure apart from the `deps` it is handed, so it is tested without Electron.

/** How long a cold start may take before the link is given up on. */
const CONNECT_BUDGET_MS = 30_000;
const RETRY_MS = 500;

/**
 * The link id in `url`, or `null` for anything that is not a session link.
 *
 * Strict on purpose: this is the one string the operating system hands the app
 * from anywhere on the machine, so only the exact shape the daemon mints passes.
 *
 * @param {string} url
 * @returns {string | null}
 */
function parseSessionLink(url) {
  let parsed;
  try {
    parsed = new URL(url);
  } catch {
    return null;
  }
  if (parsed.protocol !== "veld:" || parsed.hostname !== "session") return null;
  const id = parsed.pathname.replace(/^\/+|\/+$/g, "");
  return /^[0-9a-f]{32}$/.test(id) ? id : null;
}

/**
 * Take the user to the session link `id`.
 *
 * @param {string} id
 * @param {{
 *   open: (id: string, body: {resume?: boolean, restore?: boolean}) => Promise<any | null>,
 *   ensureWindow: () => void,
 *   ask: (message: string, detail: string, buttons: string[]) => Promise<number>,
 *   tell: (message: string, detail: string) => Promise<void>,
 *   sleep: (ms: number) => Promise<void>,
 *   now: () => number,
 * }} deps `open` answers the daemon's JSON, `{found: false}` for a 404, or `null`
 *   while the daemon is unreachable.
 */
async function openSessionLink(id, deps) {
  deps.ensureWindow();
  // Nobody took it: our window is still starting, or the daemon is. Keep asking
  // until a window of ours is connected — every unanswered ask does nothing, so
  // repeating it is safe. A request that got no answer at all is sent again as it
  // was (a restore of a worktree already out of the Trash does nothing); one that
  // was answered is followed by plain asks.
  const request = async (first) => {
    const deadline = deps.now() + CONNECT_BUDGET_MS;
    let answer = await deps.open(id, first);
    while ((answer === null || (answer.found && !answer.trashed && !answer.delivered)) && deps.now() < deadline) {
      await deps.sleep(RETRY_MS);
      answer = await deps.open(id, answer === null ? first : {});
    }
    return answer;
  };
  let answer = await request({});
  // `deleting: false` is also how a daemon that can restore from a link says so;
  // one from before that sends no `deleting` and is sent to the sidebar below.
  const restorable = answer?.found && answer.trashed && answer.deleting === false;
  if (restorable) {
    // Restoring changes what the sidebar shows, so it waits for a click — the
    // same rule the page's Restore button follows.
    const choice = await deps.ask(
      `\u201c${answer.worktree || "the worktree"}\u201d is in the Trash`,
      "Veld can take it out of the Trash and open it, the same as Restore in the sidebar.",
      ["Restore and open", "Not now"],
    );
    if (choice !== 0) return "trashed";
    answer = await request({ restore: true });
  }
  if (answer === null) {
    await deps.tell(
      "Veld is not answering",
      "The session link could not be opened because Veld's daemon did not answer. Try the link again in a moment.",
    );
    return "unreachable";
  }
  if (!answer.found) {
    await deps.tell(
      "This session could not be found",
      "The link points at a terminal in Veld on the machine that made it, and this machine has no session by that name. Either it was made somewhere else, or the worktree it lived in has since been deleted.",
    );
    return "missing";
  }
  const name = answer.worktree || "the worktree";
  if (answer.trashed) {
    await deps.tell(
      `“${name}” is in the Trash`,
      answer.deleting
        ? "Veld is deleting it right now, so it cannot be restored. If the deletion fails, it returns to Veld's sidebar."
        : "Restore it from the Trash in Veld's sidebar, then open the link again.",
    );
    return "trashed";
  }
  if (answer.state === "resumable" && answer.delivered) {
    // Starting an agent is a process, so it waits for a click — the same rule
    // the page's Resume button follows.
    const choice = await deps.ask(
      `The conversation in “${name}” has ended`,
      "No terminal is running it any more. Veld can start it again where it left off, in a new tab.",
      ["Resume the conversation", "Not now"],
    );
    if (choice === 0) await deps.open(id, { resume: true });
    return choice === 0 ? "resumed" : "shown";
  }
  if (!answer.delivered) {
    await deps.tell(
      "Veld could not open the session",
      `\u201c${name}\u201d is on this machine, but no Veld window connected to show it. Open Veld and use the link again.`,
    );
    return "undelivered";
  }
  return "shown";
}

module.exports = { parseSessionLink, openSessionLink, CONNECT_BUDGET_MS };

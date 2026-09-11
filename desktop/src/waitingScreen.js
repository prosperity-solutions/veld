// The page Veld Desktop shows while the daemon is unreachable.
//
// Electron-free on purpose, like `updatePolicy.js` and `validate.js`: the part
// worth testing here is which of three things a person is told, and that is a
// decision over an elapsed time and a filesystem probe, neither of which needs a
// Chromium to evaluate.
//
// **Why there are three pages and not one.** The single page this replaces told
// everybody the same thing — "install veld, then run setup" — from the first
// second the daemon did not answer. That is right exactly once, on a fresh
// machine, and wrong in the case that actually happens most: veld updates,
// restarts the daemon, the app reopens a second later, and a working install is
// told to go and install itself. The instruction is not just noise there, it is
// alarming, and a user who follows it re-runs an installer over a machine that
// was mid-update.
//
// So the screen asks a question before it gives advice: is the veld CLI on this
// machine at all?
//
//  - **No binary.** Nothing is coming. Say so at once, with the commands — this
//    is the first-impression case the original screen was written for, and
//    making it wait would be a regression. It keeps the old screen's `veld
//    doctor` line too, and that is not decoration: the probe only knows the
//    three directories `install.sh` prefers, and `install.sh` will happily
//    install elsewhere — `VELD_INSTALL_DIR`, or an existing veld found anywhere
//    and updated in place. Somebody in that position reads "no binary" wrongly,
//    so the page must still hand them the command that tells them the truth.
//  - **A binary, and it has been seconds.** Something is starting, or something
//    just restarted. Say that, name no commands, and wait.
//  - **A binary, and it has been a minute.** Now it is a fault worth debugging,
//    and the advice is different from the fresh-machine advice: the daemon agent
//    may never have been installed (`veld setup unprivileged`), or it is
//    installed and unhealthy (`veld doctor`). Neither of those is "install
//    veld", which is the line that made the old screen wrong.

const { cliCandidatePaths } = require("./updatePolicy");

/**
 * How long a machine that *has* veld is given before the screen starts
 * diagnosing.
 *
 * Sized for the case it exists for rather than for impatience: a `veld update`
 * restarts the daemon and the helper and then relaunches the app, and the app
 * can win that race by a wide margin. A minute is comfortably longer than that
 * sequence takes and still short enough that somebody staring at a genuinely
 * broken install is not left with a spinner and no next step.
 */
const STALL_AFTER_MS = 60_000;

/**
 * The veld CLI on this machine, or `null`.
 *
 * Existence and the execute bit only — deliberately *not* the
 * `--version`-and-check-the-output probe the updater runs. The two are asking
 * different questions: the updater is about to *run* the thing it found and has
 * to know it is veld, whereas this only wants to know whether the user has an
 * install that could be starting up. Spawning three processes to render a
 * waiting screen would be the wrong trade, and the worst case here is a page
 * that says "you have veld" to somebody who has some other program called veld.
 *
 * **This can say `null` about a machine that has veld.** `install.sh` honours
 * `VELD_INSTALL_DIR` and updates an existing binary wherever it finds one, so a
 * veld outside these three directories is legitimate and invisible here. That is
 * why the `not-installed` page keeps a `veld doctor` line rather than only
 * offering the installer: the failure mode of guessing wrong has to be a
 * redundant instruction, never a dead end.
 *
 * `isExecutable` is injected so this stays testable without a filesystem.
 *
 * @param {{home: string, isExecutable: (p: string) => boolean}} ctx
 * @returns {string | null}
 */
function installedCliPath({ home, isExecutable }) {
  for (const candidate of cliCandidatePaths({ home })) {
    try {
      if (isExecutable(candidate)) return candidate;
    } catch {
      // An unreadable directory is not an install. Keep looking.
    }
  }
  return null;
}

/**
 * Which of the three pages to show.
 *
 * `cliPath` decides first and elapsed time only breaks the tie, which is the
 * ordering that matters: a machine with no veld gets its instructions
 * immediately, and the wait applies only to the machine that has something to
 * wait for.
 *
 * @param {{cliPath: string | null, elapsedMs: number, stallAfterMs?: number}} ctx
 * @returns {"not-installed" | "starting" | "stalled"}
 */
function waitingStage({ cliPath, elapsedMs, stallAfterMs = STALL_AFTER_MS }) {
  if (!cliPath) return "not-installed";
  return elapsedMs >= stallAfterMs ? "stalled" : "starting";
}

/**
 * Text into HTML.
 *
 * Two interpolated values do not come from this file — `cliPath`, a filesystem
 * path from `cliCandidatePaths`, and `baseUrl`, which is `VELD_DESKTOP_URL` from
 * the environment when that is set. Today both are effectively fixed strings, and
 * both are escaped anyway: `cliCandidatePaths` is a function whose contents have
 * changed once already, `VELD_DESKTOP_URL` is whatever a developer exports, and
 * the day either learns something new is not the day anybody will remember this
 * page renders it into markup.
 *
 * @param {string} s
 */
function esc(s) {
  return String(s)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/** The commands, spelled out rather than linked — see the module header. */
const INSTALL_COMMAND = "curl -fsSL https://veld.oss.life.li/get | bash";
const SETUP_COMMAND = "veld setup unprivileged";

/**
 * Self-contained and branded (dark tokens + wordmark dot styling from the design
 * handoff). Served as a `data:` URL, so it can reference nothing.
 *
 * `-webkit-user-select` is re-enabled on the commands alone — the rest of the
 * page is a drag region, which otherwise swallows the selection.
 *
 * @param {{stage: "not-installed" | "starting" | "stalled", cliPath?: string | null, baseUrl?: string}} ctx
 * @returns {string}
 */
function waitingHtml({ stage, cliPath = null, baseUrl = "" }) {
  let body;
  if (stage === "starting") {
    // No commands, and no "waiting for the daemon" framing either: the user does
    // not have a daemon problem yet, they have an app that opened early. Naming
    // the update is what makes the sentence land, because that is what they
    // just did.
    body = `
  <p class="lead">Starting Veld…</p>
  <p>If you just updated, the daemon is restarting. This takes a moment.</p>
  <p class="quiet">Hang tight — the window opens by itself.</p>`;
  } else if (stage === "stalled") {
    // The fault page. Both commands are here because the two failures look
    // identical from the app's side: an install that never ran setup has no
    // daemon agent, and one that did has an agent that is not answering.
    const where = cliPath
      ? `<p>veld is installed at <code>${esc(cliPath)}</code>, but nothing is answering${
          baseUrl ? ` at <code>${esc(baseUrl)}</code>` : ""
        }.</p>`
      : "";
    body = `
  <p class="lead">The veld daemon still isn't answering.</p>
  ${where}
  <p>If you've never run setup, this is the step that installs the daemon agent:</p>
  <p><code class="cmd">${SETUP_COMMAND}</code></p>
  <p>Otherwise, this says what's wrong:</p>
  <p><code class="cmd">veld doctor</code></p>
  <p class="quiet">Still retrying automatically.</p>`;
  } else {
    body = `
  <p class="lead">Waiting for the veld daemon…</p>
  <p>On a fresh machine, install veld and set it up — no sudo needed:</p>
  <p><code class="cmd">${INSTALL_COMMAND}</code></p>
  <p><code class="cmd">${SETUP_COMMAND}</code></p>
  <p>Already have veld somewhere else? <code>veld doctor</code> says what's wrong.</p>
  <p class="quiet">Retrying automatically.</p>`;
  }

  return `<!doctype html><html><head><meta charset="utf-8"><title>Veld</title>
<link rel="icon" href="data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 48 48'><path d='M40 0H8C3.58 0 0 3.58 0 8V40C0 44.42 3.58 48 8 48H40C44.42 48 48 44.42 48 40V8C48 3.58 44.42 0 40 0Z' fill='%230A0A0B'/><path d='M21.2 36L12 12H16.4L23.7 31.8H23.8L31.1 12H35.5L26.3 36H21.2Z' fill='white'/><path d='M32.5 37C33.8807 37 35 35.8807 35 34.5C35 33.1193 33.8807 32 32.5 32C31.1193 32 30 33.1193 30 34.5C30 35.8807 31.1193 37 32.5 37Z' fill='%23C4F56A'/></svg>">
<style>
  body{margin:0;height:100vh;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:14px;
       background:#0d0e10;color:#98a0a9;font:13px/1.6 system-ui,sans-serif;-webkit-app-region:drag}
  .wm{font-weight:700;font-size:22px;color:#e7e9ec}.wm i{color:oklch(0.74 0.14 158);font-style:normal}
  code{font-family:ui-monospace,monospace;background:#1a1d21;border:1px solid #2a2e35;border-radius:6px;padding:2px 7px}
  code.cmd{-webkit-user-select:text;user-select:text;-webkit-app-region:no-drag;color:#e7e9ec}
  p{max-width:420px;text-align:center;margin:0}
  p.lead{color:#e7e9ec}
  p.quiet{color:#6b737c}
</style></head><body>
  <div class="wm">veld<i>.</i></div>${body}
</body></html>`;
}

module.exports = {
  INSTALL_COMMAND,
  SETUP_COMMAND,
  STALL_AFTER_MS,
  installedCliPath,
  waitingHtml,
  waitingStage,
};

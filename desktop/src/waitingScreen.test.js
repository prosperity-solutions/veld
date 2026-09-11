const test = require("node:test");
const assert = require("node:assert/strict");

const {
  INSTALL_COMMAND,
  SETUP_COMMAND,
  STALL_AFTER_MS,
  installedCliPath,
  waitingHtml,
  waitingStage,
} = require("./waitingScreen");

const HOME = "/Users/nobody";

/** A probe that says yes to exactly the listed paths. */
const has = (...paths) => (p) => paths.includes(p);

test("a machine with no veld is told how to get it, immediately", () => {
  // The first-impression case the original screen was written for. Making it
  // wait would be a regression: nothing is starting, so there is nothing to
  // stay tuned for.
  assert.equal(installedCliPath({ home: HOME, isExecutable: () => false }), null);
  assert.equal(waitingStage({ cliPath: null, elapsedMs: 0 }), "not-installed");
  assert.equal(waitingStage({ cliPath: null, elapsedMs: 10 * 60_000 }), "not-installed");

  const html = waitingHtml({ stage: "not-installed" });
  assert.ok(html.includes(INSTALL_COMMAND));
  assert.ok(html.includes(SETUP_COMMAND));
});

test("a machine that has veld is not told to install veld", () => {
  // The bug this file exists for: the app reopens a second after `veld update`
  // restarted the daemon, and a working install is told to install itself.
  const cliPath = "/opt/homebrew/bin/veld";
  assert.equal(waitingStage({ cliPath, elapsedMs: 0 }), "starting");
  assert.equal(waitingStage({ cliPath, elapsedMs: STALL_AFTER_MS - 1 }), "starting");

  const html = waitingHtml({ stage: "starting", cliPath });
  assert.ok(html.includes("Starting Veld"));
  assert.ok(html.includes("just updated"));
  assert.equal(html.includes(INSTALL_COMMAND), false);
  assert.equal(html.includes(SETUP_COMMAND), false);
  assert.equal(html.includes("veld doctor"), false);
});

test("a minute later the same machine gets something to debug with", () => {
  const cliPath = "/usr/local/bin/veld";
  assert.equal(waitingStage({ cliPath, elapsedMs: STALL_AFTER_MS }), "stalled");

  const html = waitingHtml({
    stage: "stalled",
    cliPath,
    baseUrl: "http://127.0.0.1:19899",
  });
  // Both commands, because the two failures are indistinguishable from here: an
  // install that never ran setup has no daemon agent, one that did has an
  // unhealthy one.
  assert.ok(html.includes(SETUP_COMMAND));
  assert.ok(html.includes("veld doctor"));
  assert.ok(html.includes(cliPath));
  assert.ok(html.includes("127.0.0.1:19899"));
  // Still never the installer: this machine demonstrably has veld.
  assert.equal(html.includes(INSTALL_COMMAND), false);
});

test("the stall page works without a base URL to name", () => {
  const html = waitingHtml({ stage: "stalled", cliPath: "/usr/local/bin/veld" });
  assert.ok(html.includes("veld doctor"));
  assert.equal(html.includes("at <code></code>"), false);
});

test("the CLI probe follows the installer's own order", () => {
  // Same order as `cliCandidatePaths`, which is the order `install.sh` prefers —
  // so the screen names the binary the installer last wrote.
  assert.equal(
    installedCliPath({
      home: HOME,
      isExecutable: has("/opt/homebrew/bin/veld", `${HOME}/.local/bin/veld`),
    }),
    "/opt/homebrew/bin/veld",
  );
  assert.equal(
    installedCliPath({ home: HOME, isExecutable: has(`${HOME}/.local/bin/veld`) }),
    `${HOME}/.local/bin/veld`,
  );
});

test("a probe that throws is not an install", () => {
  // An unreadable directory must keep the loop going rather than take down the
  // one screen a user sees when nothing else works.
  assert.equal(
    installedCliPath({
      home: HOME,
      isExecutable: (p) => {
        if (p === "/usr/local/bin/veld") throw new Error("EACCES");
        return p === "/opt/homebrew/bin/veld";
      },
    }),
    "/opt/homebrew/bin/veld",
  );
});

test("a path is escaped into the page rather than interpolated raw", () => {
  const html = waitingHtml({
    stage: "stalled",
    cliPath: '/tmp/<script>alert("x")</script>/veld',
  });
  assert.equal(html.includes("<script>"), false);
  assert.ok(html.includes("&lt;script&gt;"));
});

test("every page carries the wordmark and says it is still trying", () => {
  for (const stage of ["not-installed", "starting", "stalled"]) {
    const html = waitingHtml({ stage, cliPath: "/usr/local/bin/veld" });
    assert.ok(html.startsWith("<!doctype html>"), stage);
    assert.ok(html.includes('<div class="wm">veld<i>.</i></div>'), stage);
    assert.ok(/retrying|opens by itself/i.test(html), stage);
  }
});

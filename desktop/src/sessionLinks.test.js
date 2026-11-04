// Tests for `veld://session/<id>` handling: no Electron, a scripted daemon.
const test = require("node:test");
const assert = require("node:assert/strict");
const { parseSessionLink, openSessionLink, CONNECT_BUDGET_MS } = require("./sessionLinks");

const ID = "0123456789abcdef0123456789abcdef";

test("only the exact link shape parses", () => {
  assert.equal(parseSessionLink(`veld://session/${ID}`), ID);
  assert.equal(parseSessionLink(`veld://session/${ID}/`), ID);
  for (const bad of [
    `https://veld.localhost/session/${ID}`,
    `veld://other/${ID}`,
    `veld://session/${ID.toUpperCase()}`,
    `veld://session/${ID}x`,
    "veld://session/../../etc",
    "veld://session/",
    "not a url",
  ]) {
    assert.equal(parseSessionLink(bad), null, bad);
  }
});

/** A daemon that answers from a script, one entry per call; the last repeats. */
function harness(answers, choice = 0) {
  let t = 0;
  const calls = [];
  const said = [];
  return {
    calls,
    said,
    deps: {
      open: async (id, body) => {
        calls.push({ id, resume: !!body.resume, restore: !!body.restore });
        return answers[Math.min(calls.length - 1, answers.length - 1)];
      },
      ensureWindow: () => said.push("window"),
      ask: async (message) => {
        said.push(`ask:${message}`);
        return choice;
      },
      tell: async (message) => {
        said.push(`tell:${message}`);
      },
      sleep: async (ms) => {
        t += ms;
      },
      now: () => t,
    },
  };
}

const shown = { found: true, worktree: "W", state: "open", delivered: "electron" };

test("a cold start keeps asking until a window of ours takes it", async () => {
  const h = harness([null, { found: true, state: "open", delivered: null }, shown]);
  assert.equal(await openSessionLink(ID, h.deps), "shown");
  assert.equal(h.calls.length, 3);
  assert.deepEqual(h.said, ["window"]);
});

test("an unknown link says so, at once", async () => {
  const h = harness([{ found: false }]);
  assert.equal(await openSessionLink(ID, h.deps), "missing");
  assert.equal(h.calls.length, 1);
  assert.deepEqual(h.said, ["window", "tell:This session could not be found"]);
});

test("a daemon that never answers gives up after the budget", async () => {
  const h = harness([null]);
  assert.equal(await openSessionLink(ID, h.deps), "unreachable");
  assert.ok(h.calls.length > 1 && h.calls.length <= CONNECT_BUDGET_MS / 500 + 1);
});

test("an ended conversation is resumed only on a click", async () => {
  const ended = { found: true, worktree: "W", state: "resumable", delivered: "electron" };
  const yes = harness([ended], 0);
  assert.equal(await openSessionLink(ID, yes.deps), "resumed");
  assert.deepEqual(yes.calls.map((c) => c.resume), [false, true]);

  const no = harness([ended], 1);
  assert.equal(await openSessionLink(ID, no.deps), "shown");
  assert.deepEqual(no.calls.map((c) => c.resume), [false]);
});

const trashed = { found: true, worktree: "W", trashed: true, deleting: false };

test("a trashed worktree is restored only on a click, then opened", async () => {
  const yes = harness([trashed, shown], 0);
  assert.equal(await openSessionLink(ID, yes.deps), "shown");
  assert.deepEqual(yes.calls.map((c) => c.restore), [false, true]);
  assert.deepEqual(yes.said, ["window", "ask:\u201cW\u201d is in the Trash"]);

  const no = harness([trashed], 1);
  assert.equal(await openSessionLink(ID, no.deps), "trashed");
  assert.deepEqual(no.calls.map((c) => c.restore), [false]);
});

test("a restore keeps asking until a window takes it, without restoring again", async () => {
  const h = harness([trashed, { found: true, state: "closed", delivered: null }, shown], 0);
  assert.equal(await openSessionLink(ID, h.deps), "shown");
  assert.deepEqual(h.calls.map((c) => c.restore), [false, true, false]);
});

test("a restored worktree whose conversation ended still offers the resume", async () => {
  const ended = { found: true, worktree: "W", state: "resumable", delivered: "electron" };
  const h = harness([trashed, ended], 0);
  assert.equal(await openSessionLink(ID, h.deps), "resumed");
  assert.deepEqual(
    h.calls.map((c) => [c.restore, c.resume]),
    [[false, false], [true, false], [false, true]],
  );
});

test("a restore that got no answer is sent again, not reported as a deletion", async () => {
  const h = harness([trashed, null, shown], 0);
  assert.equal(await openSessionLink(ID, h.deps), "shown");
  assert.deepEqual(h.calls.map((c) => c.restore), [false, true, true]);
});

test("a daemon that cannot restore from a link points at the sidebar", async () => {
  const h = harness([{ found: true, worktree: "W", trashed: true }]);
  assert.equal(await openSessionLink(ID, h.deps), "trashed");
  assert.equal(h.calls.length, 1);
  assert.deepEqual(h.said, ["window", "tell:\u201cW\u201d is in the Trash"]);
});

test("a worktree already being deleted is explained, not offered", async () => {
  const h = harness([{ ...trashed, deleting: true }]);
  assert.equal(await openSessionLink(ID, h.deps), "trashed");
  assert.equal(h.calls.length, 1);
  assert.deepEqual(h.said, ["window", "tell:\u201cW\u201d is in the Trash"]);
});

test("a window that never connects is explained, not silent", async () => {
  const h = harness([{ found: true, worktree: "W", state: "open", delivered: null }]);
  assert.equal(await openSessionLink(ID, h.deps), "undelivered");
  assert.equal(h.said.at(-1), "tell:Veld could not open the session");
});

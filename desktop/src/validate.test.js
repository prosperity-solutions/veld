// Tests for the shell's trust boundary. `node --test src/validate.test.js` —
// no Electron binary needed, which is why the validation lives in its own module.
const test = require("node:test");
const assert = require("node:assert/strict");
const { safeUrl, isViewId, isProfileName, partitionFor } = require("./validate");

test("safeUrl accepts only http(s)", () => {
  assert.equal(safeUrl("http://localhost:3000/"), "http://localhost:3000/");
  assert.equal(
    safeUrl("https://web.dev.veld.localhost/a?b=1#c"),
    "https://web.dev.veld.localhost/a?b=1#c",
  );

  // Each of these turns a preview pane into something else.
  for (const hostile of [
    "javascript:alert(1)",
    "file:///etc/passwd",
    "data:text/html,<b>x",
    "blob:https://x.test/1234",
    "chrome://settings",
    "devtools://devtools/bundled/inspector.html",
    "about:blank",
    "ws://localhost:19899/api/pty/attach",
    "mailto:someone@example.com",
    "tel:+15550100",
  ]) {
    assert.equal(safeUrl(hostile), null, hostile);
  }

  // Non-strings and junk must return null, never throw — this runs on every
  // navigate from the page.
  for (const junk of ["", "   ", "http://", "not a url", null, undefined, 42, {}, []]) {
    assert.equal(safeUrl(junk), null, JSON.stringify(junk));
  }
});

test("safeUrl does not lose the port, path, query or credentials-free host", () => {
  // The renderer sends an already-normalised URL; the shell must not mangle it.
  assert.equal(safeUrl("http://127.0.0.1:5199/ide?repo=%2Ftmp%2Fx&wt=1"),
    "http://127.0.0.1:5199/ide?repo=%2Ftmp%2Fx&wt=1");
  assert.equal(safeUrl("http://[::1]:3000/"), "http://[::1]:3000/");
});

test("isViewId matches the daemon's session-id charset", () => {
  assert.ok(isViewId("probe-a"));
  assert.ok(isViewId("0f9c1e42-6b3a-4d5f-9a1b-2c3d4e5f6a7b"));
  assert.ok(isViewId("a".repeat(64)));

  assert.ok(!isViewId("a".repeat(65)), "bounded");
  assert.ok(!isViewId(""), "non-empty");
  for (const bad of ["../etc", "a/b", "a b", "a.b", "a:b", "üñ", null, undefined, 7, {}]) {
    assert.ok(!isViewId(bad), JSON.stringify(bad));
  }
});

test("isProfileName cannot escape its partition namespace", () => {
  for (const ok of ["default", "otter", "session-2", "a", "0", "a".repeat(32)]) {
    assert.ok(isProfileName(ok), ok);
  }
  for (const bad of [
    "../etc",
    "a/b",
    "persist:other",
    "Otter", // uppercase would make two names for one jar
    "-leading",
    ".dotfile",
    "a".repeat(33),
    "",
    null,
    undefined,
    {},
  ]) {
    assert.ok(!isProfileName(bad), JSON.stringify(bad));
  }
});

test("partitionFor is namespaced and persistent", () => {
  assert.equal(partitionFor("otter"), "persist:veld-browser-otter");
  // `persist:` is what makes a named session mean anything across restarts, and
  // the prefix is what keeps it out of the app's own session.
  assert.ok(partitionFor("default").startsWith("persist:veld-browser-"));
});

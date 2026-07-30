// Tests for the shell's trust boundary. `node --test src/validate.test.js` —
// no Electron binary needed, which is why the validation lives in its own module.
const test = require("node:test");
const assert = require("node:assert/strict");
const {
  fitScale,
  isProfileName,
  isViewId,
  partitionFor,
  safeEmulation,
  safeUrl,
  safeUserAgent,
  safeZoom,
} = require("./validate");

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

test("safeUserAgent refuses anything that could not be a header value", () => {
  const real =
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";
  assert.equal(safeUserAgent(real), real);
  assert.equal(safeUserAgent("  trimmed/1.0  "), "trimmed/1.0");

  // The reason this function exists: `setUserAgent` takes a header value, so a
  // CR or LF in it is header injection against every origin the pane visits.
  for (const hostile of [
    "UA/1.0\r\nX-Injected: 1",
    "UA/1.0\nX-Injected: 1",
    "UA/1.0\r\n\r\nGET /admin HTTP/1.1",
    "UA/1.0\u0000embedded",
    "UA/1.0\tTabbed",
    "UA/1.0 üñïçø∂é",
  ]) {
    assert.equal(safeUserAgent(hostile), null, JSON.stringify(hostile));
  }

  // Bounded, and non-strings are a null rather than a throw — this is on the
  // path of every emulation the page sets.
  assert.equal(safeUserAgent("a".repeat(512)), "a".repeat(512));
  assert.equal(safeUserAgent("a".repeat(513)), null);
  for (const junk of ["", "   ", null, undefined, 42, {}, []]) {
    assert.equal(safeUserAgent(junk), null, JSON.stringify(junk));
  }
});

test("safeEmulation clamps every number and keeps only what Electron consumes", () => {
  assert.deepEqual(
    safeEmulation({
      device: "iphone-pro",
      width: 393,
      height: 852,
      deviceScaleFactor: 3,
      mobile: true,
      touch: true,
      ua: "UA/1.0",
      fit: true,
    }),
    {
      width: 393,
      height: 852,
      deviceScaleFactor: 3,
      mobile: true,
      touch: true,
      userAgent: "UA/1.0",
      fit: true,
    },
  );

  // Out of range in both directions, and fractional sizes rounded.
  assert.equal(safeEmulation({ width: 1, height: 99999 }).width, 120);
  assert.equal(safeEmulation({ width: 1, height: 99999 }).height, 4096);
  assert.equal(safeEmulation({ width: 390.6, height: 800.2 }).width, 391);

  // `deviceScaleFactor` is typed Integer by Electron, and 0 means "the display's
  // own" — which is also the honest answer for a value that makes no sense.
  assert.equal(safeEmulation({ width: 400, height: 800, deviceScaleFactor: 2.625 }).deviceScaleFactor, 3);
  assert.equal(safeEmulation({ width: 400, height: 800, deviceScaleFactor: -1 }).deviceScaleFactor, 0);
  assert.equal(safeEmulation({ width: 400, height: 800, deviceScaleFactor: 99 }).deviceScaleFactor, 4);
  assert.equal(safeEmulation({ width: 400, height: 800, deviceScaleFactor: "x" }).deviceScaleFactor, 0);

  // Flags default to off, `fit` defaults to on: an emulation from an older build
  // that predates the toggle must still fit, or a 1920-wide device is unusable in
  // a pane.
  const bare = safeEmulation({ width: 400, height: 800 });
  assert.equal(bare.mobile, false);
  assert.equal(bare.touch, false);
  assert.equal(bare.userAgent, null);
  assert.equal(bare.fit, true);
  assert.equal(safeEmulation({ width: 400, height: 800, mobile: "yes" }).mobile, false);

  // A hostile user agent drops the UA, not the emulation: the size is still a
  // legitimate thing to apply.
  assert.equal(safeEmulation({ width: 400, height: 800, ua: "UA\r\nX: 1" }).userAgent, null);

  // Unusable payloads degrade to "no emulation", which is a correct state.
  for (const junk of [null, undefined, 42, "iphone", [], {}, { width: 400 }, { width: "x", height: 8 }]) {
    assert.equal(safeEmulation(junk), null, JSON.stringify(junk));
  }
});

test("safeZoom stays inside Chromium's own range", () => {
  assert.equal(safeZoom(1), 1);
  assert.equal(safeZoom(0.67), 0.67);
  assert.equal(safeZoom(0.01), 0.25);
  assert.equal(safeZoom(99), 3);
  // `setZoomFactor` throws on a non-positive factor, and the page is not a
  // trusted caller.
  for (const junk of [0, -1, NaN, Infinity, "big", null, undefined, {}]) {
    assert.equal(safeZoom(junk), null, JSON.stringify(junk));
  }
});

test("fitScale shrinks to the smaller dimension and never magnifies", () => {
  const desktop = { width: 1440, height: 900, fit: true };
  // The case the whole feature exists for: a desktop layout in a narrow pane.
  assert.equal(fitScale(desktop, { width: 720, height: 900 }), 0.5);
  // Height binds too — the emulated screen *is* the view, so a viewport scaled to
  // the width but taller than the box is clipped with nothing to scroll it.
  assert.equal(fitScale(desktop, { width: 1440, height: 450 }), 0.5);
  // Fits already: never scaled up, or a phone would be blown up to pane size.
  assert.equal(fitScale({ width: 390, height: 800, fit: true }, { width: 900, height: 900 }), 1);
  // Off, or nothing to measure against yet (bounds arrive after `create`).
  assert.equal(fitScale({ width: 1440, height: 900, fit: false }, { width: 100, height: 100 }), 1);
  assert.equal(fitScale(desktop, null), 1);
  assert.equal(fitScale(desktop, { width: 0, height: 0 }), 1);
  assert.equal(fitScale(null, { width: 100, height: 100 }), 1);
});

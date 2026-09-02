// Tests for the shell's trust boundary. `node --test src/validate.test.js` —
// no Electron binary needed, which is why the validation lives in its own module.
const test = require("node:test");
const assert = require("node:assert/strict");
const { readFileSync } = require("node:fs");
const path = require("node:path");
const {
  MAX_SEED_BYTES,
  MAX_TAB_BYTES,
  MAX_STRIP_TABS,
  MAX_TRANSFER_TABS,
  PANE_KINDS,
  buildSeedLayout,
  isProfileName,
  isViewId,
  partitionFor,
  safeColor,
  safeEmulation,
  safeRadius,
  safeRepoRoot,
  safeScale,
  safeTabIds,
  safeTitle,
  safeTransferTab,
  safeTransferTabs,
  safeUrl,
  safeUserAgent,
  safeWorktreeId,
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
      safeArea: { top: 59, right: 0, bottom: 34, left: 0 },
    }),
    {
      width: 393,
      height: 852,
      deviceScaleFactor: 3,
      mobile: true,
      touch: true,
      userAgent: "UA/1.0",
      safeArea: { top: 59, right: 0, bottom: 34, left: 0 },
      // No `fit`: it arrives on the wire and is dropped here, because fitting is a
      // question about the pane that `deviceLayout` answers in the renderer.
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

  // Flags default to off. `fit` is not part of this shape at all — see above.
  const bare = safeEmulation({ width: 400, height: 800 });
  assert.equal(bare.mobile, false);
  assert.equal(bare.touch, false);
  assert.equal(bare.userAgent, null);
  assert.equal(bare.fit, undefined);
  assert.equal(safeEmulation({ width: 400, height: 800, mobile: "yes" }).mobile, false);

  // A hostile user agent drops the UA, not the emulation: the size is still a
  // legitimate thing to apply.
  assert.equal(safeEmulation({ width: 400, height: 800, ua: "UA\r\nX: 1" }).userAgent, null);

  // Unusable payloads degrade to "no emulation", which is a correct state.
  for (const junk of [null, undefined, 42, "iphone", [], {}, { width: 400 }, { width: "x", height: 8 }]) {
    assert.equal(safeEmulation(junk), null, JSON.stringify(junk));
  }
});

test("safeEmulation forwards exactly the fields Electron applies", () => {
  // The other half of the drift gate. Its twin lives in
  // `crates/veld-daemon/ui/src/panes/devices.test.ts` ("the emulation's field set") and
  // catches a field *appearing* on the renderer's `PaneEmulation`; this one catches a
  // field quietly *vanishing* from what this process forwards — a rename here, or a
  // dropped `touch`, is otherwise silent in both packages: the pane keeps offering the
  // control and the shell stops applying it.
  //
  // `device` and `fit` are absent on purpose and documented in `safeEmulation`: a preset
  // id means nothing here, and fitting is a question about the pane that the renderer
  // answers, sending on only the resulting scale (with the bounds).
  assert.deepEqual(
    Object.keys(safeEmulation({ width: 400, height: 800, ua: "UA/1.0" })).sort(),
    ["deviceScaleFactor", "height", "mobile", "safeArea", "touch", "userAgent", "width"],
  );

  // `safeArea` is the one nested field, so its members cross this boundary without
  // the key-set check above seeing them — and it is the field where that matters
  // most: `Emulation.setSafeAreaInsetsOverride` replaces the whole set per call
  // rather than merging, so a side this validator stops reading is sent as `0` and
  // flattens that edge rather than keeping its previous number.
  assert.deepEqual(
    Object.keys(
      safeEmulation({ width: 400, height: 800, safeArea: { top: 59, bottom: 34 } }).safeArea,
    ).sort(),
    ["bottom", "left", "right", "top"],
  );
});

test("safeEmulation repairs safe-area insets per side and collapses an empty set", () => {
  // These become `Emulation.setSafeAreaInsetsOverride`, which is strict in both
  // directions: it refuses a fractional inset outright — which would reject the
  // whole applier's CDP round, taking touch and the media overrides with it — and
  // accepts an absurd one *literally*, laying the page out inside a 100000px
  // gutter. Neither may reach it.
  const insets = (safeArea) => safeEmulation({ width: 400, height: 800, safeArea }).safeArea;
  assert.deepEqual(insets({ top: 59, bottom: 34 }), { top: 59, right: 0, bottom: 34, left: 0 });
  assert.deepEqual(insets({ top: 100000, right: -5, bottom: 34.6, left: "x" }), {
    top: 200,
    right: 0,
    bottom: 35,
    left: 0,
  });
  // One representation of "no gutters" on this side of the wire too: the applier
  // tests this to decide whether the shared debugger session is wanted at all, and
  // a second spelling of off would hold it for an override that does nothing.
  assert.equal(insets({ top: 0, right: 0, bottom: 0, left: 0 }), null);
  assert.equal(insets(undefined), null);
  assert.equal(insets("59"), null);
  assert.equal(insets([]), null);
});

test("safeColor takes hex and nothing else", () => {
  // The page sends its theme's surface so a view does not flash white in a dark app.
  assert.equal(safeColor("#0d0e10"), "#0d0e10");
  assert.equal(safeColor("  #FFF  "), "#FFF");
  assert.equal(safeColor("#0d0e10ff"), "#0d0e10ff");
  // Chromium accepts a broad colour syntax; there is no reason for a page-supplied string
  // to be parsed liberally here, and a partial match is how a validator becomes a hole.
  for (const bad of [
    "red",
    "rgb(0,0,0)",
    "#0d0e1",
    "#0d0e10; background: url(x)",
    "javascript:alert(1)",
    "",
    null,
    undefined,
    0x0d0e10,
    {},
  ]) {
    assert.equal(safeColor(bad), null, JSON.stringify(bad));
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

test("safeScale bounds what the renderer asks for", () => {
  // The fit calculation itself lives in the renderer (`deviceLayout`), which is
  // the side that knows the pane's padding and where the screen is centred. This
  // only has to keep the number applicable.
  assert.equal(safeScale(0.5), 0.5);
  assert.equal(safeScale(1), 1);
  // Never magnified — emulation shrinks a screen to fit a pane, and enlarging one
  // is what page zoom is for.
  assert.equal(safeScale(4), 1);
  // Never zero: a page rendered into nothing looks exactly like a broken view.
  assert.equal(safeScale(0), 1);
  assert.equal(safeScale(0.0001), 0.02);
  for (const junk of [-1, NaN, Infinity, "half", null, undefined, {}]) {
    assert.equal(safeScale(junk), 1, JSON.stringify(junk));
  }
});

test("safeRadius bounds the screen's corners", () => {
  assert.equal(safeRadius(48), 48);
  assert.equal(safeRadius(12.4), 12);
  assert.equal(safeRadius(9999), 64);
  // A square screen is the honest answer for anything unusable.
  for (const none of [0, -8, NaN, "round", null, undefined, {}]) {
    assert.equal(safeRadius(none), 0, JSON.stringify(none));
  }
});

// ---------------------------------------------------------------------------
// Window transfers
// ---------------------------------------------------------------------------

test("safeTitle strips what a title bar cannot render", () => {
  assert.equal(safeTitle("npm run dev"), "npm run dev");
  // Control characters become spaces rather than rejecting the whole title: a
  // page title carrying a stray newline is common and still worth showing.
  assert.equal(safeTitle("a\nb\tc"), "a b c");
  assert.equal(safeTitle("  spaced  "), "spaced");
  assert.equal(safeTitle("x".repeat(500)).length, 200);
  for (const none of ["", "   ", "\n", 42, null, undefined, {}]) {
    assert.equal(safeTitle(none), null, JSON.stringify(none));
  }
});

test("safeWorktreeId takes a rowid and nothing else", () => {
  assert.equal(safeWorktreeId(7), 7);
  assert.equal(safeWorktreeId("7"), 7);
  for (const none of [0, -1, 1.5, Number.NaN, Number.POSITIVE_INFINITY, "", "abc", null, {}]) {
    assert.equal(safeWorktreeId(none), null, JSON.stringify(none));
  }
});

test("safeRepoRoot rejects anything that could not be a path in a URL", () => {
  assert.equal(safeRepoRoot("/Users/x/code/veld"), "/Users/x/code/veld");
  // A newline in a value the shell puts in a query parameter is the shape worth
  // refusing outright rather than escaping.
  assert.equal(safeRepoRoot("/Users/x\n/evil"), null);
  // A space is legal in a path and must survive — URLSearchParams encodes it.
  assert.equal(safeRepoRoot("/Users/x/My Code"), "/Users/x/My Code");
  assert.equal(safeRepoRoot(`/${"x".repeat(5000)}`), null);
  for (const none of ["", 42, null, undefined, {}]) {
    assert.equal(safeRepoRoot(none), null, JSON.stringify(none));
  }
});

test("PANE_KINDS agrees with the renderer's", () => {
  // A tab crosses into this process when a pane is detached into its own
  // window, so `safeTransferTab` needs the kind list — and this is plain JS, so
  // nothing type-checks the two copies against each other. A kind added on the
  // renderer side and forgotten here works everywhere *except* detach, which
  // refuses with "the desktop shell refused the request" and points nowhere
  // near the cause. That is this codebase's worst failure shape, and the same
  // one `safeEmulation`'s field-set gate exists for.
  //
  // The gate lives on this side rather than the renderer's — unlike the
  // emulation one — for a boring reason: `crates/veld-daemon/ui` has no
  // `@types/node`, so reading a file from a vitest test costs a dependency,
  // while `node --test` has `fs` already.
  const source = readFileSync(
    path.join(__dirname, "..", "..", "crates", "veld-daemon", "ui", "src", "panes", "model.ts"),
    "utf8",
  );
  const match = source.match(/export const PANE_KINDS = \[([^\]]*)\]/);
  assert.ok(match, "PANE_KINDS not found in panes/model.ts");
  const theirs = match[1]
    .split(",")
    .map((s) => s.trim().replace(/^["']|["']$/g, ""))
    .filter(Boolean);
  assert.deepEqual(PANE_KINDS, theirs);
});

test("safeTransferTab keeps a tab's own fields and refuses a non-tab", () => {
  const tab = {
    id: "abc_123",
    kind: "browser",
    title: "Preview",
    url: "http://localhost:3000/",
    profile: "otter",
    emulation: { width: 390, height: 844, ua: "Mozilla/5.0" },
    zoom: 1.25,
  };
  // Carried through whole: the receiving renderer's `parseTab` is what decides
  // which fields mean anything, and a field dropped here is invisible until the
  // pane arrives in the new window without it.
  assert.deepEqual(safeTransferTab(tab), tab);

  assert.equal(safeTransferTab({ id: "a", kind: "wormhole" }), null);
  assert.equal(safeTransferTab({ id: "not a valid id!", kind: "terminal" }), null);
  assert.equal(safeTransferTab({ kind: "terminal" }), null);
  for (const none of [null, undefined, 42, "tab", []]) {
    assert.equal(safeTransferTab(none), null, JSON.stringify(none));
  }
});

test("safeTabIds keeps strip order, drops junk, deduplicates and bounds", () => {
  assert.deepEqual(safeTabIds(["a", "b", "c"]), ["a", "b", "c"], "order is the strip's");

  // A repeated id would appear twice in the cycle, so the same press would land
  // on it twice running.
  assert.deepEqual(safeTabIds(["a", "b", "a"]), ["a", "b"]);

  // The charset is the daemon's PTY session charset — a tab id is one.
  assert.deepEqual(safeTabIds(["ok-1", "", "has space", "../../etc", 7, null, {}]), ["ok-1"]);

  // Bounded by the *strip* ceiling, not the transfer one: a transfer is at most
  // two docks of a window, a strip just accumulates. At 64 the cut was silent
  // AND undetectable — the shell still answered, so the renderer's own fallback
  // never ran, and the tabs past it were unreachable by keyboard for good.
  assert.ok(MAX_STRIP_TABS > MAX_TRANSFER_TABS, "a strip is not bounded like a transfer");
  const many = Array.from({ length: MAX_STRIP_TABS + 20 }, (_, i) => `t${i}`);
  assert.equal(safeTabIds(many).length, MAX_STRIP_TABS);

  for (const notAList of [null, undefined, "a", 7, { 0: "a" }]) {
    assert.deepEqual(safeTabIds(notAList), []);
  }
});

test("safeTransferTabs deduplicates by id and bounds the list", () => {
  const tabs = safeTransferTabs([
    { id: "a", kind: "terminal", title: "one" },
    // Two tabs on one id would fight over one shell in the receiving window.
    { id: "a", kind: "terminal", title: "again" },
    { id: "b", kind: "new", title: "two" },
    { id: "!!", kind: "new" },
    "not a tab",
  ]);
  assert.deepEqual(
    tabs.map((t) => t.id),
    ["a", "b"],
  );
  assert.equal(tabs[0].title, "one");

  const many = Array.from({ length: MAX_TRANSFER_TABS + 20 }, (_, i) => ({
    id: `t${i}`,
    kind: "new",
  }));
  assert.equal(safeTransferTabs(many).length, MAX_TRANSFER_TABS);
  assert.deepEqual(safeTransferTabs("nope"), []);
});

test("buildSeedLayout produces a layout the renderer can parse", () => {
  const tabs = safeTransferTabs([{ id: "sh1", kind: "terminal", title: "Terminal" }]);
  const seed = JSON.parse(buildSeedLayout(9, tabs, 0.3));
  assert.deepEqual(Object.keys(seed), ["9"]);
  assert.deepEqual(seed[9].docks[0].tabs, tabs);
  assert.equal(seed[9].docks[0].activeId, "sh1");
  // The second dock exists and is empty: `parseLayout` requires exactly two and
  // returns null for a layout that has any other number.
  assert.deepEqual(seed[9].docks[1], { tabs: [], activeId: null });
  assert.equal(seed[9].ratio, 0.3);

  // A ratio that is not a number must not reach the JSON as NaN, which
  // serializes to `null` and would clamp to the default on the far side anyway —
  // but only after `Number(null)` had quietly made it 0.
  assert.equal(JSON.parse(buildSeedLayout(9, tabs, "wide"))[9].ratio, 0.5);
  assert.equal(buildSeedLayout(9, [], 0.5), null);
});

test("a page's title cannot set the size of a transfer", () => {
  // `document.title` is pushed onto the tab record from a *previewed page* —
  // arbitrary web content. It is the one page-controlled string in a tab, so it
  // is truncated at the boundary rather than allowed to size everything
  // downstream: the seed, and the snapshot the main process retains and
  // re-copies on every layout change.
  const tab = safeTransferTab({ id: "v1", kind: "browser", title: "x".repeat(50_000) });
  assert.equal(tab.title.length, 200);
});

test("a transfer is bounded in UTF-8 bytes, not string length", () => {
  // The bug this replaced: the ceiling counted JavaScript string length while
  // what had to fit was the UTF-8 encoding — up to 4× larger. A 50 000-character
  // CJK title measured 50 134 and encoded to 150 402 bytes, so it passed a
  // 64 KB check and produced a payload well past Linux's 128 KB per-argument
  // limit back when the seed rode argv. The window never started, *after* the
  // origin had already released its tabs to it.
  const cjk = "中".repeat(50_000);
  assert.ok(cjk.length < MAX_SEED_BYTES);
  assert.ok(Buffer.byteLength(cjk, "utf8") > MAX_SEED_BYTES);
  // Truncation now stops it at the tab, and the seed is measured in bytes below.
  assert.equal(safeTransferTab({ id: "v1", kind: "browser", title: cjk }).title.length, 200);

  // A tab too big for reasons other than its title is dropped whole.
  assert.equal(
    safeTransferTab({ id: "v1", kind: "new", padding: "x".repeat(MAX_TAB_BYTES) }),
    null,
  );
});

test("buildSeedLayout refuses a seed too large to carry", () => {
  const fat = Array.from({ length: MAX_TRANSFER_TABS }, (_, i) => ({
    id: `t${i}`,
    kind: "browser",
    title: "x".repeat(200),
    // Under the per-tab cap individually; over the seed's ceiling together.
    url: `http://localhost/${"p".repeat(6000)}`,
  }));
  const tabs = safeTransferTabs(fat);
  assert.equal(tabs.length, MAX_TRANSFER_TABS);
  assert.ok(Buffer.byteLength(JSON.stringify(tabs), "utf8") > MAX_SEED_BYTES);
  assert.equal(buildSeedLayout(1, tabs, 0.5), null);
});

test("safeMedia keeps only features and values Chromium accepts", () => {
  const { safeMedia } = require("./validate");
  assert.deepStrictEqual(safeMedia({ "prefers-color-scheme": "dark" }), {
    "prefers-color-scheme": "dark",
  });
  // One bad value must not cost a good override beside it.
  assert.deepStrictEqual(
    safeMedia({ "prefers-color-scheme": "dark", "forced-colors": "sideways" }),
    { "prefers-color-scheme": "dark" },
  );
  // Nothing left means null, which is what releases the debugger.
  assert.strictEqual(safeMedia({ "prefers-color-scheme": "sepia" }), null);
  assert.strictEqual(safeMedia({ "prefers-contrast": "more" }), null);
  assert.strictEqual(safeMedia(null), null);
  assert.strictEqual(safeMedia("dark"), null);
});

test("isPermissionRule re-applies the wildcard confinement the config parser enforces", () => {
  const { isPermissionRule } = require("./validate");
  const IDS = ["camera", "display-capture"];
  const rule = (origin, over = {}) => ({ origin, allow: ["camera"], ...over });
  const origin = (over = {}) => ({ scheme: "https", host: "veld.localhost", port: 443, ...over });

  assert.ok(isPermissionRule(rule(origin()), IDS));
  assert.ok(isPermissionRule(rule(origin({ wildcard: true })), IDS));
  assert.ok(isPermissionRule(rule(origin({ port: null })), IDS), "null port = any port");

  // The confinement `veld_core::ide` spends its longest comment on. A copy of the
  // check that omitted it would let one crafted rule grant every host under a TLD.
  assert.ok(!isPermissionRule(rule(origin({ host: "com", wildcard: true })), IDS));
  assert.ok(!isPermissionRule(rule(origin({ host: "dev", wildcard: true })), IDS));
  // …but the loopback TLD stays legal, as it is in the parser.
  assert.ok(isPermissionRule(rule(origin({ host: "localhost", wildcard: true })), IDS));
  // An IP literal has no subdomains.
  assert.ok(!isPermissionRule(rule(origin({ host: "[::1]", wildcard: true })), IDS));
  // A `*` left in the host would be compared literally, or read as a label.
  assert.ok(!isPermissionRule(rule(origin({ host: "*.veld.localhost" })), IDS));

  assert.ok(!isPermissionRule(rule(origin({ scheme: "file" })), IDS));
  assert.ok(!isPermissionRule(rule(origin({ host: "" })), IDS));
  assert.ok(!isPermissionRule(rule(origin({ port: "443" })), IDS));
  assert.ok(!isPermissionRule(rule(origin({ wildcard: "yes" })), IDS));
  assert.ok(!isPermissionRule(rule(origin(), { allow: ["root-access"] }), IDS));
  assert.ok(!isPermissionRule(rule(origin(), { deny: "camera" }), IDS));
  assert.ok(!isPermissionRule(null, IDS));
  assert.ok(!isPermissionRule({ origin: "https://x" }, IDS));
});

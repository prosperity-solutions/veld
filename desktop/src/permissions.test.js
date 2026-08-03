const test = require("node:test");
const assert = require("node:assert");
const fs = require("node:fs");
const path = require("node:path");

const {
  VELD_PERMISSIONS,
  ELECTRON_TO_VELD,
  FIXED_VERDICTS,
  parseOrigin,
  originKey,
  matchesPattern,
  configVerdict,
  permissionIds,
  resolve,
  siteSettings,
  setAnswer,
  forgetPartition,
  sanitizeStore,
  mergeForWrite,
  revocationKey,
} = require("./permissions");

const localhost = parseOrigin("http://localhost:3000");
const anyPort = { raw: "http://localhost:*", scheme: "http", host: "localhost", wildcard: false, port: null };

// -- the drift gate --------------------------------------------------------

test("the id list matches the JSON schema", () => {
  const schemaPath = path.join(__dirname, "..", "..", "schema", "v3", "veld.schema.json");
  const schema = JSON.parse(fs.readFileSync(schemaPath, "utf8"));
  assert.deepStrictEqual(schema.$defs.permissionId.enum, VELD_PERMISSIONS);
});

// The mapping is the half a schema cannot check: an Electron upgrade that adds a
// permission name leaves it unmapped, and unmapped means denied — which is safe,
// but silently so. This at least pins what today's Electron says.
test("every Electron 43 permission name is answered", () => {
  const request = [
    "clipboard-read", "clipboard-sanitized-write", "display-capture", "fullscreen",
    "geolocation", "idle-detection", "media", "mediaKeySystem", "midi", "midiSysex",
    "notifications", "pointerLock", "keyboardLock", "openExternal", "speaker-selection",
    "storage-access", "top-level-storage-access", "window-management", "unknown", "fileSystem",
  ];
  const check = [
    "clipboard-read", "clipboard-sanitized-write", "geolocation", "fullscreen", "hid",
    "idle-detection", "media", "mediaKeySystem", "midi", "midiSysex", "notifications",
    "openExternal", "pointerLock", "serial", "storage-access", "top-level-storage-access",
    "usb", "deprecated-sync-clipboard-read", "fileSystem",
  ];
  for (const name of new Set([...request, ...check])) {
    const mapped = permissionIds(name, { mediaTypes: ["video"] });
    assert.ok(
      !mapped.unmapped,
      `${name} is not in ELECTRON_TO_VELD or FIXED_VERDICTS — it would fail closed and silently`,
    );
  }
});

test("every mapped id is a real veld permission", () => {
  for (const id of Object.values(ELECTRON_TO_VELD)) {
    assert.ok(VELD_PERMISSIONS.includes(id), `${id} is not a veld permission`);
  }
  for (const verdict of Object.values(FIXED_VERDICTS)) {
    assert.ok(["allow", "deny"].includes(verdict));
  }
});

// -- origins ---------------------------------------------------------------

test("an origin normalises its port and case", () => {
  assert.deepStrictEqual(parseOrigin("http://LOCALHOST:3000/path?q=1#x"), {
    scheme: "http",
    host: "localhost",
    port: 3000,
  });
  assert.deepStrictEqual(parseOrigin("https://example.com"), {
    scheme: "https",
    host: "example.com",
    port: 443,
  });
});

test("a URL with no usable origin is null, not a guess", () => {
  for (const url of ["about:blank", "data:text/html,x", "file:///tmp/x", "", null, "nonsense"]) {
    assert.strictEqual(parseOrigin(url), null, String(url));
  }
});

test("the origin key hides a default port and keeps a real one", () => {
  assert.strictEqual(originKey(parseOrigin("https://example.com:443")), "https://example.com");
  assert.strictEqual(originKey(parseOrigin("http://example.com:8080")), "http://example.com:8080");
});

test("a leading `*.` matches any depth of subdomain, label-wise", () => {
  // The case that needs it: veld's own URLs carry the run name in the hostname,
  // and `website.<run>.veld.localhost` is two labels under the suffix — so
  // single-label wildcard semantics would not reach it.
  const wild = { raw: "https://*.veld.localhost", scheme: "https", host: "veld.localhost", wildcard: true, port: 443 };
  assert.ok(matchesPattern(parseOrigin("https://website.my-run.veld.localhost"), wild));
  assert.ok(matchesPattern(parseOrigin("https://api.veld.localhost"), wild));
  // Not a bare string suffix — this is the version of the check that leaks.
  assert.ok(!matchesPattern(parseOrigin("https://evilveld.localhost"), wild));
  // And not the suffix itself: `*.x` is subdomains of x. Write `x` out if wanted.
  assert.ok(!matchesPattern(parseOrigin("https://veld.localhost"), wild));
  // The scheme and port still apply.
  assert.ok(!matchesPattern(parseOrigin("http://website.my-run.veld.localhost"), wild));
});

test("a `*` port matches any port and a fixed port matches only itself", () => {
  assert.ok(matchesPattern(localhost, anyPort));
  assert.ok(matchesPattern(parseOrigin("http://localhost:9999"), anyPort));
  assert.ok(!matchesPattern(parseOrigin("https://localhost:3000"), anyPort), "scheme must match");
  assert.ok(
    !matchesPattern(parseOrigin("http://localhost.evil.com:3000"), anyPort),
    "the host is exact — a suffix must not match",
  );
  const fixed = { scheme: "http", host: "localhost", port: 3000 };
  assert.ok(matchesPattern(localhost, fixed));
  assert.ok(!matchesPattern(parseOrigin("http://localhost:3001"), fixed));
});

// -- precedence ------------------------------------------------------------

test("deny wins over allow across separate matching rules", () => {
  const rules = [
    { origin: anyPort, allow: ["camera"], deny: [] },
    { origin: { scheme: "http", host: "localhost", port: 3000 }, allow: [], deny: ["camera"] },
  ];
  assert.strictEqual(configVerdict(rules, localhost, "camera"), "deny");
  assert.strictEqual(configVerdict(rules, parseOrigin("http://localhost:4000"), "camera"), "allow");
});

test("a user answer outranks the project config in both directions", () => {
  const rules = [{ origin: anyPort, allow: ["camera"], deny: ["geolocation"] }];
  const stored = { "http://localhost:3000": { camera: "deny", geolocation: "allow" } };
  const denied = resolve({
    electronName: "media",
    details: { mediaTypes: ["video"] },
    origin: localhost,
    stored,
    rules,
  });
  assert.strictEqual(denied.verdict, "deny");
  assert.strictEqual(denied.source, "user");
  const allowed = resolve({ electronName: "geolocation", origin: localhost, stored, rules });
  assert.strictEqual(allowed.verdict, "allow");
  assert.strictEqual(allowed.source, "user");
});

test("with nothing set, most permissions ask and the display-only ones do not", () => {
  assert.strictEqual(resolve({ electronName: "geolocation", origin: localhost }).verdict, "ask");
  assert.strictEqual(resolve({ electronName: "notifications", origin: localhost }).verdict, "ask");
  assert.strictEqual(resolve({ electronName: "fullscreen", origin: localhost }).verdict, "allow");
  assert.strictEqual(resolve({ electronName: "pointerLock", origin: localhost }).verdict, "allow");
  // `keyboard-lock` is deliberately NOT in the default-allow set: the set is
  // justified as "reversible with Escape", and capturing Escape is exactly what
  // keyboard lock does. The shell also has no fullscreen handling of its own for
  // a pane, so there is no exit affordance to fall back on.
  assert.strictEqual(resolve({ electronName: "keyboardLock", origin: localhost }).verdict, "ask");
});

// An inferred id is one veld could not identify — an empty `media` request, which
// is *probably* getDisplayMedia and cannot be shown to be, since `mediaTypes` is
// optional in Electron's typings. Letting that ride the trusted-origin
// default-allow would have granted an unidentified media request at a veld URL
// with one `callback(true)` covering whatever it really was, camera included.
test("an inferred permission honours an explicit grant but never a default", () => {
  const veldOrigin = parseOrigin("https://website.my-run.veld.localhost");
  const trustedOrigins = ["https://website.my-run.veld.localhost"];
  const inferred = { electronName: "media", kind: "request", details: {}, origin: veldOrigin };

  // Default alone: asks, even though display-capture is default-allowed here.
  assert.strictEqual(resolve({ ...inferred, trustedOrigins }).verdict, "ask");
  // A *stated* display-capture request at the same origin still gets the default.
  assert.strictEqual(
    resolve({ electronName: "display-capture", kind: "request", origin: veldOrigin, trustedOrigins })
      .verdict,
    "allow",
  );
  // And the project's own grant still applies to the inferred one.
  const rules = [
    {
      origin: { scheme: "https", host: "veld.localhost", wildcard: true, port: 443 },
      allow: ["display-capture"],
      deny: [],
    },
  ];
  assert.strictEqual(resolve({ ...inferred, trustedOrigins, rules }).verdict, "allow");
});

test("screen capture is granted at an origin veld serves and asked for anywhere else", () => {
  const trustedOrigins = ["http://web.run.proj.localhost:19899"];
  const veldOrigin = parseOrigin("http://web.run.proj.localhost:19899");
  assert.strictEqual(
    resolve({ electronName: "display-capture", origin: veldOrigin, trustedOrigins }).verdict,
    "allow",
  );
  assert.strictEqual(
    resolve({ electronName: "display-capture", origin: localhost, trustedOrigins }).verdict,
    "ask",
  );
});

test("a project can withdraw a veld default", () => {
  const rules = [{ origin: anyPort, allow: [], deny: ["display-capture", "fullscreen"] }];
  const trustedOrigins = ["http://localhost:3000"];
  assert.strictEqual(
    resolve({ electronName: "display-capture", origin: localhost, rules, trustedOrigins }).verdict,
    "deny",
  );
  assert.strictEqual(
    resolve({ electronName: "fullscreen", origin: localhost, rules }).verdict,
    "deny",
  );
});

// -- media splitting -------------------------------------------------------

test("camera and microphone are separate switches behind one Electron request", () => {
  const rules = [{ origin: anyPort, allow: ["camera"], deny: [] }];
  const video = resolve({
    electronName: "media",
    details: { mediaTypes: ["video"] },
    origin: localhost,
    rules,
  });
  assert.strictEqual(video.verdict, "allow");
  const audio = resolve({
    electronName: "media",
    details: { mediaTypes: ["audio"] },
    origin: localhost,
    rules,
  });
  assert.strictEqual(audio.verdict, "ask", "the microphone was never granted");
});

test("a request for both is only allowed when both are", () => {
  const rules = [{ origin: anyPort, allow: ["camera", "microphone"], deny: [] }];
  const both = { electronName: "media", details: { mediaTypes: ["video", "audio"] }, origin: localhost };
  assert.strictEqual(resolve({ ...both, rules }).verdict, "allow");
  // Granting only the camera must not hand over a microphone stream that never
  // got a prompt — the pair is one boolean to Electron.
  const half = [{ origin: anyPort, allow: ["camera"], deny: [] }];
  assert.strictEqual(resolve({ ...both, rules: half }).verdict, "ask");
});

test("a media check naming no type is a device enumeration and is refused", () => {
  const enumeration = resolve({
    electronName: "media",
    kind: "check",
    details: { mediaType: "unknown" },
    origin: localhost,
    rules: [{ origin: anyPort, allow: ["camera", "microphone"], deny: [] }],
  });
  assert.strictEqual(enumeration.verdict, "deny");
  assert.strictEqual(enumeration.source, "no-permission");
});

// The regression this exists for, and it cost three test rounds to find: Electron
// raises `getDisplayMedia` as a `media` REQUEST with an empty `mediaTypes` — it
// populates that array only for device capture — so it never arrives under the
// `display-capture` name that also exists in the request union. Reading the empty
// case as an enumeration refused every screenshot taken inside a pane, before the
// display-media handler was ever consulted, and with nothing to name it could not
// even prompt.
test("a media request naming no type is getDisplayMedia, not an enumeration", () => {
  assert.deepStrictEqual(permissionIds("media", {}, "request").ids, ["display-capture"]);
  assert.deepStrictEqual(permissionIds("media", { mediaTypes: [] }, "request").ids, [
    "display-capture",
  ]);
  // …and the config grant on veld's own URLs then applies to it.
  const rules = [
    {
      origin: { scheme: "https", host: "veld.localhost", wildcard: true, port: 443 },
      allow: ["display-capture"],
      deny: [],
    },
  ];
  const outcome = resolve({
    electronName: "media",
    kind: "request",
    details: {},
    origin: parseOrigin("https://website.my-run.veld.localhost"),
    rules,
  });
  assert.strictEqual(outcome.verdict, "allow");
  assert.deepStrictEqual(outcome.ids, ["display-capture"]);
});

// With nothing pre-allowed the same request must ASK, never silently refuse:
// a permission the user could grant that is denied without a prompt has no
// recourse in the UI at all.
test("an unconfigured screen-capture request asks instead of denying", () => {
  const outcome = resolve({
    electronName: "media",
    kind: "request",
    details: {},
    origin: parseOrigin("https://example.com"),
  });
  assert.strictEqual(outcome.verdict, "ask");
});

// -- unattributable and unknown -------------------------------------------

test("a request with no origin is denied whatever the config says", () => {
  const rules = [{ origin: anyPort, allow: ["camera"], deny: [] }];
  const verdict = resolve({
    electronName: "media",
    details: { mediaTypes: ["video"] },
    origin: null,
    rules,
  });
  assert.strictEqual(verdict.verdict, "deny");
  assert.strictEqual(verdict.source, "no-origin");
});

test("an unknown permission is denied and never prompts", () => {
  assert.strictEqual(resolve({ electronName: "unknown", origin: localhost }).verdict, "deny");
  assert.strictEqual(resolve({ electronName: "not-a-permission", origin: localhost }).verdict, "deny");
});

// These two were allowed before the policy ran and had no row in the per-site
// panel — an allow nobody could see and nobody could revoke, which is the exact
// thing a permission UI exists to abolish. They are ordinary permissions now.
test("clipboard write and protected media go through the policy like everything else", () => {
  for (const [electronName, id] of [
    ["clipboard-sanitized-write", "clipboard-write"],
    ["mediaKeySystem", "protected-media"],
  ]) {
    assert.deepStrictEqual(permissionIds(electronName, {}, "request").ids, [id]);
    // Nothing set anywhere: it asks, rather than being silently granted.
    assert.strictEqual(
      resolve({ electronName, kind: "request", origin: localhost }).verdict,
      "ask",
      electronName,
    );
    // And a project can still make them silent on purpose.
    const rules = [{ origin: anyPort, allow: [id], deny: [] }];
    assert.strictEqual(
      resolve({ electronName, kind: "request", origin: localhost, rules }).verdict,
      "allow",
      electronName,
    );
  }
});

test("only an unmodellable permission is answered without the policy", () => {
  assert.deepStrictEqual(Object.keys(FIXED_VERDICTS), ["unknown"]);
  assert.strictEqual(FIXED_VERDICTS.unknown, "deny");
});

// -- the store -------------------------------------------------------------

test("the panel lists every permission with where its answer came from", () => {
  const rules = [{ origin: anyPort, allow: ["geolocation"], deny: [] }];
  const stored = { "http://localhost:3000": { camera: "deny" } };
  const rows = siteSettings({ origin: localhost, stored, rules });
  assert.strictEqual(rows.length, VELD_PERMISSIONS.length);
  const byId = Object.fromEntries(rows.map((r) => [r.id, r]));
  assert.deepStrictEqual(byId.camera, { id: "camera", verdict: "deny", source: "user" });
  assert.deepStrictEqual(byId.geolocation, { id: "geolocation", verdict: "allow", source: "config" });
  assert.deepStrictEqual(byId.notifications, { id: "notifications", verdict: "ask", source: "default" });
});

test("setting an answer back to default leaves no trace", () => {
  let store = {};
  store = setAnswer(store, "persist:veld-otter", localhost, "camera", "allow");
  assert.deepStrictEqual(store, {
    "persist:veld-otter": { "http://localhost:3000": { camera: "allow" } },
  });
  store = setAnswer(store, "persist:veld-otter", localhost, "camera", "default");
  assert.deepStrictEqual(store, {}, "empty branches must be pruned, not left behind");
});

test("an unknown permission id cannot be written into the store", () => {
  const store = setAnswer({}, "persist:veld-otter", localhost, "root-access", "allow");
  assert.deepStrictEqual(store, {});
});

test("clearing a session forgets what that session was allowed", () => {
  let store = setAnswer({}, "persist:veld-otter", localhost, "camera", "allow");
  store = setAnswer(store, "persist:veld-fox", localhost, "camera", "allow");
  const after = forgetPartition(store, "persist:veld-otter");
  assert.deepStrictEqual(Object.keys(after), ["persist:veld-fox"]);
});

test("a corrupted store loads as empty rather than as permissive", () => {
  assert.deepStrictEqual(sanitizeStore(null), {});
  assert.deepStrictEqual(sanitizeStore("nope"), {});
  assert.deepStrictEqual(sanitizeStore([1, 2]), {});
  assert.deepStrictEqual(
    sanitizeStore({
      "persist:veld-otter": {
        "http://localhost:3000": { camera: "allow", microphone: "maybe", "root-access": "allow" },
        "not an origin": { camera: "allow" },
      },
      bad: "shape",
    }),
    { "persist:veld-otter": { "http://localhost:3000": { camera: "allow" } } },
  );
});

// This logic was wrong twice in review before anything could run it, because it
// lived in the Electron-importing half no test can load. Both failures were
// fail-OPEN — first clobbering another instance's answers, then resurrecting
// revoked ones — which is why it is here now.
test("mergeForWrite keeps another instance's answers", () => {
  const onDisk = { "persist:veld-otter": { "https://a.example": { camera: "allow" } } };
  const mine = { "persist:veld-otter": { "https://b.example": { geolocation: "deny" } } };
  assert.deepStrictEqual(mergeForWrite(onDisk, mine), {
    "persist:veld-otter": {
      "https://a.example": { camera: "allow" },
      "https://b.example": { geolocation: "deny" },
    },
  });
});

test("this process wins for an answer both hold", () => {
  const onDisk = { p: { "https://a.example": { camera: "allow" } } };
  const mine = { p: { "https://a.example": { camera: "deny" } } };
  assert.strictEqual(mergeForWrite(onDisk, mine).p["https://a.example"].camera, "deny");
});

test("a revoked answer stays revoked instead of coming back off disk", () => {
  // The panel's Default button. Without an explicit record the merge cannot tell
  // "deleted here" from "never seen here", and resolves it in favour of the file.
  const onDisk = { p: { "https://a.example": { camera: "allow", microphone: "allow" } } };
  const mine = { p: { "https://a.example": { microphone: "allow" } } };
  const merged = mergeForWrite(onDisk, mine, {
    revoked: [revocationKey("p", "https://a.example", "camera")],
  });
  assert.deepStrictEqual(merged.p["https://a.example"], { microphone: "allow" });
});

test("revoking the last answer prunes the origin and the partition", () => {
  const onDisk = { p: { "https://a.example": { camera: "allow" } } };
  const merged = mergeForWrite(onDisk, {}, {
    revoked: [revocationKey("p", "https://a.example", "camera")],
  });
  assert.deepStrictEqual(merged, {});
});

test("a cleared session is not resurrected by the merge", () => {
  const onDisk = { keep: { "https://a.example": { camera: "allow" } }, gone: { "https://b.example": { camera: "allow" } } };
  const merged = mergeForWrite(onDisk, {}, { cleared: ["gone"] });
  assert.deepStrictEqual(Object.keys(merged), ["keep"]);
});

test("a revocation naming something absent is harmless", () => {
  // Two instances can revoke the same answer; the second finds nothing to delete.
  const merged = mergeForWrite({}, {}, { revoked: [revocationKey("p", "https://a.example", "camera")] });
  assert.deepStrictEqual(merged, {});
});

const test = require("node:test");
const assert = require("node:assert/strict");
const { EventEmitter } = require("node:events");
const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");
const vm = require("node:vm");
const { pathToFileURL } = require("node:url");
const { createRemoteWindows, assertChooserSender, remoteRequestHeaders, isRemoteOrigin, remotePartition } = require("./remoteWindows");
const { runnerRendererURL, previewOrigin } = require("./remotePreview");

const ENDPOINT = "https://builder.example.ts.net:19896";
const CONNECTION = {
  id: "9d4e46dc-9eac-4ede-95be-b79588931e27", label: "Build machine", client_id: "test-client",
  descriptor: { protocol: 1, instance_id: "test-machine", endpoint: ENDPOINT, public_key: "test-key", version: "1" },
};

function setup(overrides = {}) {
  const created = [];
  class Contents extends EventEmitter {
    constructor() { super(); this.id = created.length + 1; this.mainFrame = { url: "" }; }
    getURL() { return this.mainFrame.url; }
    setWindowOpenHandler(handler) { this.popup = handler; }
    setWebRTCIPHandlingPolicy(policy) { this.webRTCPolicy = policy; }
  }
  class Window extends EventEmitter {
    constructor(options) {
      super(); this.options = options; this.webContents = new Contents();
      this.destroyed = false; this.visible = false; this.minimized = false; this.focused = 0;
      created.push(this);
    }
    async loadFile(filename) { this.webContents.mainFrame.url = pathToFileURL(filename).href; }
    async loadURL(url) { this.webContents.mainFrame.url = url; }
    isDestroyed() { return this.destroyed; }
    isVisible() { return this.visible; }
    isMinimized() { return this.minimized; }
    show() { this.visible = true; }
    focus() { this.focused++; }
    restore() { this.minimized = false; }
    destroy() { this.destroyed = true; this.emit("closed"); }
  }
  const partitions = [];
  const previews = [];
  const session = { fromPartition(name, options) {
    const partition = new EventEmitter();
    partition.name = name;
    partition.options = options;
    partition.fetch = async () => { throw new Error("control fetch is not used by the fake preview"); };
    partition.setPermissionRequestHandler = (handler) => { partition.permissionRequest = handler; };
    partition.setPermissionCheckHandler = (handler) => { partition.permissionCheck = handler; };
    partition.setDevicePermissionHandler = (handler) => { partition.devicePermission = handler; };
    partition.setDisplayMediaRequestHandler = (handler) => { partition.displayMedia = handler; };
    partition.webRequest = {
      onBeforeSendHeaders: (handler) => { partition.headers = handler; },
      onHeadersReceived: (handler) => { partition.response = handler; },
    };
    partitions.push(partition);
    return partition;
  } };
  const handlers = new Map();
  const ipcMain = { handle: (channel, handler) => handlers.set(channel, handler), removeHandler: (channel) => handlers.delete(channel) };
  let connectionsCalls = 0;
  let pairs = 0;
  let forgotten = 0;
  let dialogs = 0;
  let dialogResponse = 1;
  const connections = {
    list: () => [CONNECTION],
    connect: async () => { connectionsCalls++; return { ...CONNECTION, credential: "synthetic-credential" }; },
    pair: async (_invitation, _label, confirm) => {
      pairs++;
      if (!(await confirm(CONNECTION))) throw new Error("cancelled");
      return CONNECTION;
    },
    discover: async () => ({ status: "ready", candidates: [] }),
    forget: () => { forgotten++; },
    ...overrides,
  };
  const dialog = { showMessageBox: async (owner, options) => {
    dialogs++;
    assert.equal(owner, created[0]);
    assert.equal(options.defaultId, 0);
    assert.equal(options.cancelId, 0);
    assert.match(options.detail, /full control/);
    assert.match(options.detail, /test-machine/);
    assert.match(options.detail, /test-key/);
    assert.ok(!JSON.stringify(options).includes("synthetic-credential"));
    return { response: dialogResponse };
  } };
  const manager = createRemoteWindows({ app: {}, BrowserWindow: Window, session, ipcMain, dialog, connections,
    previewFactory: (options) => {
      const preview = {
        options, started: false, disposed: false,
        start: async () => { preview.started = true; },
        dispose: () => { preview.disposed = true; },
        isPreviewURL: (url) => previewOrigin(url) !== null,
        isRegisteredURL: (url) => typeof url === "string" && /^https:\/\/preview\.project\.localhost(?:\/|$)/.test(url),
        proxyCredentials: (info) => info?.isProxy ? ["test-proxy-user", "test-proxy-password"] : null,
        contents: new Set(), requests: [],
        registerPreviewContents: (id) => preview.contents.add(id),
        unregisterPreviewContents: (id) => preview.contents.delete(id),
        allowRequest: async (details) => {
          preview.requests.push(details);
          return preview.contents.has(details.webContentsId) && preview.isRegisteredURL(details.url);
        },
      };
      previews.push(preview);
      return preview;
    },
  });
  manager.register();
  function event(win = created[0]) { return { sender: win.webContents, senderFrame: win.webContents.mainFrame }; }
  const invoke = (action, ...args) => handlers.get(`veld:remote:${action}`)(event(), ...args);
  return { manager, created, partitions, previews, handlers, invoke, event, connections, dialog,
    counters: () => ({ connectionsCalls, pairs, forgotten, dialogs }),
    cancelDialog: () => { dialogResponse = 0; },
  };
}

test("Authorization is injected only for the assigned main frame and exact HTTPS/WSS origin", () => {
  const webContents = { id: 7, mainFrame: {} };
  const auth = { endpoint: ENDPOINT, credential: "synthetic-credential", webContents, active: true };
  const request = { webContentsId: 7, frame: webContents.mainFrame, url: `${ENDPOINT}/api/repos`,
    requestHeaders: { authorization: "old", AUTHORIZATION: "another", Accept: "application/json" } };
  assert.deepEqual(remoteRequestHeaders(request, auth), { Accept: "application/json", Authorization: "Bearer synthetic-credential" });
  assert.equal(remoteRequestHeaders({ ...request, url: ENDPOINT.replace("https:", "wss:") + "/socket" }, auth).Authorization, "Bearer synthetic-credential");
  for (const url of [
    "http://builder.example.ts.net:19896/api", "ws://builder.example.ts.net:19896/socket",
    "https://builder.example.ts.net/api", "https://builder.example.ts.net:19897/api",
    "https://builder.example.ts.net.evil.test:19896/api", "https://other.example.ts.net:19896/api",
    "https://user:password@builder.example.ts.net:19896/api", "file:///tmp/file", "veld:local", "not a URL",
  ]) assert.deepEqual(remoteRequestHeaders({ ...request, url }, auth), { Accept: "application/json" }, url);
  for (const change of [{ frame: {} }, { frame: null }, { webContentsId: 8 }, { webContentsId: undefined }]) {
    assert.deepEqual(remoteRequestHeaders({ ...request, ...change }, auth), { Accept: "application/json" });
  }
  assert.deepEqual(remoteRequestHeaders(request, { ...auth, active: false }), { Accept: "application/json" });
  assert.equal(isRemoteOrigin(`${ENDPOINT}/ide`, ENDPOINT), true);
  assert.equal(isRemoteOrigin(ENDPOINT.replace("https:", "wss:"), ENDPOINT), false);
});

test("sender validation requires exact chooser contents, current main frame and exact local file URL", async () => {
  const fixture = setup();
  const chooser = await fixture.manager.openChooser();
  const event = fixture.event();
  const url = chooser.webContents.getURL();
  assert.doesNotThrow(() => assertChooserSender(event, chooser, url));
  for (const invalid of [
    { sender: {}, senderFrame: event.senderFrame },
    { sender: event.sender, senderFrame: { url } },
    { sender: event.sender, senderFrame: null },
  ]) assert.throws(() => assertChooserSender(invalid, chooser, url), { code: "sender" });
  chooser.webContents.mainFrame.url = `${url}?forged=1`;
  assert.throws(() => assertChooserSender(event, chooser, url), { code: "sender" });
  chooser.webContents.mainFrame.url = url;
  chooser.destroy();
  assert.throws(() => assertChooserSender(event, chooser, url), { code: "sender" });
});

test("every privileged IPC handler rejects another window and subframes before touching service", async () => {
  const fixture = setup();
  await fixture.manager.openChooser();
  for (const handler of fixture.handlers.values()) {
    await assert.rejects(handler({ sender: {}, senderFrame: {} }), { code: "sender" });
    await assert.rejects(handler({ sender: fixture.created[0].webContents, senderFrame: {} }), { code: "sender" });
  }
  assert.deepEqual(fixture.counters(), { connectionsCalls: 0, pairs: 0, forgotten: 0, dialogs: 0 });
});

test("remote window has no preload, isolated session, fixed machine title and deny policies", async () => {
  const fixture = setup();
  await fixture.manager.openChooser();
  const result = await fixture.invoke("open", CONNECTION.id);
  assert.deepEqual(result, { ok: true, value: { id: CONNECTION.id } });
  const remote = fixture.created[1];
  const preferences = remote.options.webPreferences;
  assert.equal(preferences.nodeIntegration, false);
  assert.equal(preferences.contextIsolation, true);
  assert.equal(preferences.sandbox, true);
  assert.equal(remote.webContents.webRTCPolicy, "disable_non_proxied_udp");
  assert.ok(!("preload" in preferences));
  assert.ok(!("additionalArguments" in preferences));
  assert.match(remote.options.title, /test-machine/);
  assert.match(remote.options.title, /builder\.example\.ts\.net/);
  assert.equal(remote.webContents.getURL(), `${ENDPOINT}/ide?remote=1`);
  assert.equal(preferences.session.name, remotePartition(CONNECTION));
  assert.notEqual(preferences.session, fixture.created[0].options.webPreferences.session);
  assert.equal(fixture.partitions[0].headers, undefined, "control requests never inherit a credential hook");
  assert.equal(fixture.partitions[1].headers, undefined, "chooser session has no credential hook");
  assert.deepEqual(remote.webContents.popup({ url: `${ENDPOINT}/ide` }), { action: "deny" });
  assert.equal(preferences.session.permissionCheck(), false);
  assert.equal(preferences.session.devicePermission(), false);
  let permission;
  preferences.session.permissionRequest({}, "media", (value) => { permission = value; });
  assert.equal(permission, false);
  for (const eventName of ["will-navigate", "will-frame-navigate", "will-redirect", "will-attach-webview", "page-title-updated"]) {
    let prevented = false;
    const event = { url: "https://other.example.ts.net", preventDefault: () => { prevented = true; } };
    remote.webContents.emit(eventName, event, event.url);
    assert.equal(prevented, true, eventName);
  }
  let cancelled;
  preferences.session.response({ statusCode: 302 }, (result) => { cancelled = result.cancel; });
  assert.equal(cancelled, true);
  preferences.session.response({ statusCode: 200 }, (result) => { cancelled = result.cancel; });
  assert.equal(cancelled, false);
  assert.equal(fixture.manager.owns(remote.webContents), true);
  assert.equal(fixture.manager.owns({}), false);
});

test("opening an existing remote focuses it; reconnect verifies again and preserves its registration partition", async () => {
  const fixture = setup();
  await fixture.manager.openChooser();
  await fixture.manager.open(CONNECTION.id);
  const first = fixture.created[1];
  await fixture.manager.open(CONNECTION.id);
  assert.equal(fixture.created.length, 2);
  assert.equal(first.focused, 2);
  assert.equal(fixture.counters().connectionsCalls, 1);
  first.destroy();
  await fixture.manager.open(CONNECTION.id);
  assert.equal(fixture.counters().connectionsCalls, 2);
  assert.equal(fixture.created[2].options.webPreferences.session.name, first.options.webPreferences.session.name);
  assert.equal(fixture.previews[0].disposed, true);
  assert.equal(fixture.previews[1].started, true);
  let headers;
  first.options.webPreferences.session.headers({
    webContentsId: first.webContents.id, frame: first.webContents.mainFrame, url: ENDPOINT,
    requestHeaders: { Authorization: "Bearer synthetic-credential" },
  }, (value) => { headers = value.requestHeaders; });
  assert.deepEqual(headers, {});
});

test("a failed proof creates no remote window and sanitized IPC errors contain no secrets", async () => {
  const fixture = setup({ connect: async () => { throw new Error("Authorization: Bearer synthetic-credential"); } });
  await fixture.manager.openChooser();
  const result = await fixture.invoke("open", CONNECTION.id);
  assert.equal(result.ok, false);
  assert.ok(!JSON.stringify(result).includes("synthetic-credential"));
  assert.equal(fixture.created.length, 1);
});

test("concurrent opens share one proof, and disposal during proof cannot create a window", async () => {
  let resolveProof;
  let calls = 0;
  const fixture = setup({ connect: () => {
    calls++;
    return new Promise((resolve) => { resolveProof = resolve; });
  } });
  await fixture.manager.openChooser();
  const first = fixture.manager.open(CONNECTION.id);
  const second = fixture.manager.open(CONNECTION.id);
  assert.equal(calls, 1);
  assert.equal(fixture.created.length, 1);
  resolveProof({ ...CONNECTION, credential: "synthetic-credential" });
  await Promise.all([first, second]);
  assert.equal(fixture.created.length, 2);
  fixture.created[1].destroy();
  const reopening = fixture.manager.open(CONNECTION.id);
  fixture.manager.dispose();
  resolveProof({ ...CONNECTION, credential: "synthetic-credential" });
  await assert.rejects(reopening, { code: "closed" });
  assert.equal(fixture.created.length, 2);
});

test("discovery does not block manual pairing", async () => {
  let resolveDiscovery;
  const fixture = setup({ discover: () => new Promise((resolve) => { resolveDiscovery = resolve; }) });
  await fixture.manager.openChooser();
  const discovery = fixture.invoke("discover");
  assert.equal((await fixture.invoke("pair", "synthetic-invitation", "Build machine")).ok, true);
  resolveDiscovery({ status: "unavailable", candidates: [], truncated: false });
  assert.equal((await discovery).ok, true);
});

test("pair confirmation is main-owned, visible, cancel-default and sender-revalidated after dialog", async () => {
  const fixture = setup();
  const chooser = await fixture.manager.openChooser();
  assert.equal((await fixture.invoke("pair", "synthetic-invitation", "Build machine")).ok, true);
  assert.equal(fixture.counters().dialogs, 1);
  chooser.visible = false;
  assert.equal((await fixture.invoke("pair", "synthetic-invitation", "Build machine")).ok, false);
  assert.equal(fixture.counters().dialogs, 1);
  chooser.visible = true;
  fixture.cancelDialog();
  assert.equal((await fixture.invoke("pair", "synthetic-invitation", "Build machine")).ok, false);
  fixture.dialog.showMessageBox = async () => {
    chooser.webContents.mainFrame.url = "https://other.example.ts.net";
    return { response: 1 };
  };
  const result = await fixture.invoke("pair", "synthetic-invitation", "Build machine");
  assert.equal(result.ok, false);
  assert.equal(result.error.code, "sender");
});

test("forget closes its window and dispose removes handlers without touching local windows", async () => {
  const fixture = setup();
  const chooser = await fixture.manager.openChooser();
  await fixture.manager.open(CONNECTION.id);
  const remote = fixture.created[1];
  assert.equal((await fixture.invoke("forget", CONNECTION.id)).ok, true);
  assert.equal(remote.isDestroyed(), true);
  assert.equal(fixture.counters().forgotten, 1);
  fixture.manager.register();
  assert.equal(fixture.handlers.size, 5);
  fixture.manager.dispose();
  assert.equal(fixture.handlers.size, 0);
  assert.equal(chooser.isDestroyed(), true);
  await assert.rejects(fixture.manager.openChooser(), { code: "closed" });
});

test("preload exports only the five dedicated methods, without arbitrary IPC channels", async () => {
  let exposed;
  const calls = [];
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, "remotePreload.js"), "utf8"), {
    require(name) {
      assert.equal(name, "electron");
      return {
        contextBridge: { exposeInMainWorld: (name, api) => { exposed = { name, api }; } },
        ipcRenderer: { invoke: async (...args) => { calls.push(args); } },
      };
    },
  });
  assert.equal(exposed.name, "veldRemote");
  assert.deepEqual(Object.keys(exposed.api).sort(), ["discover", "forget", "list", "open", "pair"]);
  await exposed.api.pair("synthetic-invitation", "Build machine");
  assert.deepEqual(calls, [["veld:remote:pair", "synthetic-invitation", "Build machine"]]);
});

test("chooser permits only its exact script hash and has no remote requests or HTML interpolation", () => {
  const html = fs.readFileSync(path.join(__dirname, "remoteChooser.html"), "utf8");
  const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
  const hash = crypto.createHash("sha256").update(script).digest("base64");
  assert.ok(html.includes(`script-src 'sha256-${hash}'`));
  assert.ok(html.includes("connect-src 'none'"));
  assert.ok(html.includes("frame-src 'none'"));
  assert.ok(!/innerHTML|localStorage|sessionStorage|document\.write/.test(script));
  assert.ok(!/src=["']https?:/.test(html));
});

test("preview frames and redirects are allowed while the main window remains on the runner", async () => {
  const fixture = setup();
  await fixture.manager.openChooser();
  await fixture.manager.open(CONNECTION.id);
  const remote = fixture.created[1];
  assert.equal(fixture.previews[0].started, true);
  const previewURL = "https://preview.project.localhost/page";
  for (const [eventName, isMainFrame, url, expected] of [
    ["will-frame-navigate", false, previewURL, false],
    ["will-frame-navigate", true, previewURL, true],
    ["will-frame-navigate", false, "https://example.test/api", true],
    ["will-redirect", false, previewURL, false],
    ["will-redirect", true, previewURL, true],
    ["will-redirect", false, "https://example.test", true],
  ]) {
    let prevented = false;
    remote.webContents.emit(eventName, { url, isMainFrame, preventDefault: () => { prevented = true; } }, url);
    assert.equal(prevented, expected, `${eventName} ${isMainFrame} ${url}`);
  }
  let cancelled;
  const respond = remote.options.webPreferences.session.response;
  respond({ url: previewURL, statusCode: 302, responseHeaders: { Location: ["/next"] } }, (result) => { cancelled = result.cancel; });
  assert.equal(cancelled, false);
  respond({ url: previewURL, statusCode: 302, responseHeaders: { Location: [`${ENDPOINT}/api`] } }, (result) => { cancelled = result.cancel; });
  assert.equal(cancelled, true);
  fixture.manager.dispose();
});

test("preview application Authorization survives but the runner credential never enters a preview frame", () => {
  const webContents = { id: 7, mainFrame: {} };
  const auth = { endpoint: ENDPOINT, credential: "runner-secret", webContents, active: true,
    preview: { isRegisteredURL: (url) => url.startsWith("https://preview.project.localhost/") } };
  const request = { webContentsId: 7, frame: {}, url: "https://preview.project.localhost/api",
    requestHeaders: { Authorization: "Bearer app-token", Cookie: "app=1" } };
  assert.deepEqual(remoteRequestHeaders(request, auth), request.requestHeaders);
  assert.deepEqual(remoteRequestHeaders({ ...request, requestHeaders: { Authorization: "Bearer runner-secret" } }, auth), {});
  assert.deepEqual(remoteRequestHeaders({ ...request, url: `${ENDPOINT}/api` }, auth), { Cookie: "app=1" });
  assert.deepEqual(remoteRequestHeaders(request, { ...auth, active: false }), { Cookie: "app=1" });
});

test("proxy login is handled in main and closing a window disposes its preview controller", async () => {
  const fixture = setup(); await fixture.manager.openChooser(); await fixture.manager.open(CONNECTION.id);
  const remote = fixture.created[1];
  let credentials;
  remote.webContents.emit("login", { preventDefault() {} }, {}, { isProxy: true }, (...values) => { credentials = values; });
  assert.deepEqual(credentials, ["test-proxy-user", "test-proxy-password"]);
  remote.webContents.emit("login", { preventDefault() {} }, {}, { isProxy: false }, (...values) => { credentials = values; });
  assert.deepEqual(credentials, []);
  remote.destroy();
  assert.equal(fixture.previews[0].disposed, true);
});

test("persistent partitions bind runner and paired-client identity without storing credentials in the partition name", () => {
  const partition = remotePartition(CONNECTION);
  assert.match(partition, /^persist:veld-remote-ide-[a-f0-9]{64}$/);
  assert.equal(remotePartition({ ...CONNECTION, label: "Renamed", credential: "never-in-name",
    descriptor: { ...CONNECTION.descriptor, version: "new-version" } }), partition);
  for (const change of [{ id: "replacement-record" }, { client_id: "replacement-client" },
    { descriptor: { ...CONNECTION.descriptor, instance_id: "replacement-runner" } },
    { descriptor: { ...CONNECTION.descriptor, public_key: "replacement-key" } },
    { descriptor: { ...CONNECTION.descriptor, endpoint: "https://other.example.ts.net:19896" } },
    { descriptor: { ...CONNECTION.descriptor, protocol: 2 } }]) {
    assert.notEqual(remotePartition({ ...CONNECTION, ...change }), partition);
  }
});

test("transport paths never receive a runner token even if beforeRequest is bypassed", () => {
  const webContents = { id: 7, mainFrame: {} };
  const auth = { endpoint: ENDPOINT, credential: "runner-secret", webContents, active: true };
  for (const path of ["/remote/preview?origin=app&path=payload.html", "/%72emote/preview", "/remote/%70review",
    "/remote%2fpreview", "/remote%252fpreview", "/remote\\preview", "/ide/%2e%2e/remote/preview",
    "//remote/preview", "/remote;parameter/preview"]) {
    for (const resourceType of ["mainFrame", "subFrame", "script", "xhr", "webSocket"]) {
      const request = { url: ENDPOINT + path, webContentsId: 7, frame: webContents.mainFrame,
        resourceType, requestHeaders: { Authorization: "Bearer runner-secret" } };
      assert.deepEqual(remoteRequestHeaders(request, auth), {}, path);
    }
  }
  assert.deepEqual(remoteRequestHeaders({ url: ENDPOINT + "/api/health", resourceType: "mainFrame",
    webContentsId: 7, frame: webContents.mainFrame, requestHeaders: {} }, auth), {});
  assert.equal(runnerRendererURL(ENDPOINT + "/ide?remote=1", ENDPOINT, true), true);
});

test("IDE agent and encoded lane requests receive authentication only in the owned IDE frame", () => {
  const webContents = { id: 7, mainFrame: {} };
  const auth = { endpoint: ENDPOINT, credential: "runner-secret", webContents, active: true };
  for (const path of ["/remote/agents/17", "/remote/agents/17/launch", "/api/lanes/In%20Review",
    "/api/lanes/%C3%9Cberpr%C3%BCfung", "/api/lanes/100%25"]) {
    const request = { url: ENDPOINT + path, webContentsId: 7, frame: webContents.mainFrame,
      resourceType: "xhr", requestHeaders: {} };
    assert.deepEqual(remoteRequestHeaders(request, auth), { Authorization: "Bearer runner-secret" }, path);
    assert.deepEqual(remoteRequestHeaders({ ...request, webContentsId: 8 }, auth), {}, path);
    assert.deepEqual(remoteRequestHeaders({ ...request, frame: {} }, auth), {}, path);
    assert.deepEqual(remoteRequestHeaders({ ...request, resourceType: "mainFrame" }, auth), {}, path);
  }
});

test("existing window.open button creates a separate top-level preview using the same cookie partition", async () => {
  const fixture = setup(); await fixture.manager.openChooser(); await fixture.manager.open(CONNECTION.id);
  const ide = fixture.created[1];
  const target = "https://preview.project.localhost/login";
  assert.deepEqual(ide.webContents.popup({ url: target, disposition: "new-window", features: "noreferrer" }), { action: "deny" });
  await new Promise((resolve) => setImmediate(resolve));
  const child = fixture.created[2];
  const preview = fixture.previews[0];
  assert.equal(child.webContents.getURL(), target, "preview loads in its own main frame, not an IDE iframe");
  assert.equal(child.options.webPreferences.session, ide.options.webPreferences.session, "application cookies use the same isolated partition");
  assert.deepEqual(preview.requests, [{ url: target, webContentsId: child.webContents.id, resourceType: "mainFrame" }]);
  assert.ok(preview.contents.has(child.webContents.id));
  assert.equal(child.options.webPreferences.nodeIntegration, false);
  assert.equal(child.options.webPreferences.contextIsolation, true);
  assert.equal(child.options.webPreferences.sandbox, true);
  assert.equal(child.options.webPreferences.preload, undefined);
  assert.equal(child.options.parent, undefined);
  assert.deepEqual(child.webContents.popup({ url: target }), { action: "deny" });
  let prevented = false;
  child.webContents.emit("will-navigate", { preventDefault: () => { prevented = true; } }, `${ENDPOINT}/ide`);
  assert.equal(prevented, true);
  const headers = child.options.webPreferences.session.headers;
  let result;
  headers({ url: ENDPOINT + "/api/repos", webContentsId: child.webContents.id,
    frame: child.webContents.mainFrame, requestHeaders: {} }, (value) => { result = value.requestHeaders; });
  assert.deepEqual(result, {});
  assert.equal(fixture.manager.owns(child.webContents), true);
  ide.destroy();
  assert.equal(child.isDestroyed(), true);
  assert.equal(preview.contents.size, 0);
});

test("preview windows are capped and unregistered targets are never loaded", async () => {
  const fixture = setup(); await fixture.manager.openChooser(); await fixture.manager.open(CONNECTION.id);
  const ide = fixture.created[1];
  ide.webContents.popup({ url: "https://veld.localhost/api" });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(fixture.created[2].isDestroyed(), true);
  assert.equal(fixture.created[2].webContents.getURL(), "");
  for (let index = 0; index < 8; index++) ide.webContents.popup({ url: `https://preview.project.localhost/${index}` });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(fixture.created.filter((win) => !win.isDestroyed()).length, 6, "chooser, IDE, and at most four previews");
  fixture.manager.dispose();
});

test("revocation and forget destroy top-level previews and remove their request authority", async () => {
  for (const revoke of [true, false]) {
    const fixture = setup(); await fixture.manager.openChooser(); await fixture.manager.open(CONNECTION.id);
    fixture.created[1].webContents.popup({ url: "https://preview.project.localhost/" });
    await new Promise((resolve) => setImmediate(resolve));
    const child = fixture.created[2];
    if (revoke) fixture.previews[0].options.onRevoked();
    else await fixture.invoke("forget", CONNECTION.id);
    assert.equal(child.isDestroyed(), true);
    assert.equal(fixture.previews[0].contents.size, 0);
    assert.equal(fixture.previews[0].disposed, true);
  }
});

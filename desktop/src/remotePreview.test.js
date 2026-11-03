const test = require("node:test");
const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const { EventEmitter } = require("node:events");
const { Duplex, Readable } = require("node:stream");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { createRemotePreview, createLocalCertificates, canonicalPreviewOrigin, previewOrigin, connectOrigin,
  certificatePin, runnerRendererURL, applicationURL, applicationHeaders, matchesApplication, bridgeWebSockets } = require("./remotePreview");

// Public, synthetic leaf certificates. No private keys are kept. Test time is
// derived from their validity interval so these fixtures do not expire in CI.
const CERTIFICATE = `-----BEGIN CERTIFICATE-----
MIIBgDCCATKgAwIBAgIUctF5HQ9cg9PcuR0FQkAM9VIdFpcwBQYDK2VwMCQxIjAg
BgNVBAMMGXByZXZpZXcucHJvamVjdC5sb2NhbGhvc3QwHhcNMjYxMDI1MTkyODA3
WhcNMjYxMTI0MTkyODA3WjAkMSIwIAYDVQQDDBlwcmV2aWV3LnByb2plY3QubG9j
YWxob3N0MCowBQYDK2VwAyEAfeaf2lMXPN0Ib7Ppp+nNHhoUdjeC2KWrngI2Vd4D
GgKjdjB0MB0GA1UdDgQWBBQMGbLVAUZ/l8szjqtFRT/hqeJKYDAfBgNVHSMEGDAW
gBQMGbLVAUZ/l8szjqtFRT/hqeJKYDAkBgNVHREEHTAbghlwcmV2aWV3LnByb2pl
Y3QubG9jYWxob3N0MAwGA1UdEwEB/wQCMAAwBQYDK2VwA0EAl3Qd5wMUwWLr4ev+
TB3xtGAw2FrvGL4Ppkjck03kF83ZJaJ06KpPZyvq8t/N+mawcTuej2u7q8J/o42Z
S+OjCg==
-----END CERTIFICATE-----
`;
const OTHER_CERTIFICATE = `-----BEGIN CERTIFICATE-----
MIIBgDCCATKgAwIBAgIUYrLbBEwmyc5x+ypxiI+6VcSihYkwBQYDK2VwMCQxIjAg
BgNVBAMMGXByZXZpZXcucHJvamVjdC5sb2NhbGhvc3QwHhcNMjYxMDI1MTkyODA3
WhcNMjYxMTI0MTkyODA3WjAkMSIwIAYDVQQDDBlwcmV2aWV3LnByb2plY3QubG9j
YWxob3N0MCowBQYDK2VwAyEAAmby8x3FzElibRcmA1XdYQi91gGvf4AmTzCWUHtQ
VMSjdjB0MB0GA1UdDgQWBBTNNtbR72r/7vjQ/CO66ec5x1236DAfBgNVHSMEGDAW
gBTNNtbR72r/7vjQ/CO66ec5x1236DAkBgNVHREEHTAbghlwcmV2aWV3LnByb2pl
Y3QubG9jYWxob3N0MAwGA1UdEwEB/wQCMAAwBQYDK2VwA0EA0RIK6/Yu+OocdimJ
7BmhvK9mHGyYsDpSuRFnZxsIhTzsS/8H2Zl01Ihv1IpOzjkHWoLmB8I3OYHwqrDr
T1msBg==
-----END CERTIFICATE-----
`;

const ENDPOINT = "https://runner.example.ts.net:19896";
const ORIGIN = "https://preview.project.localhost";
const HOSTNAME = "preview.project.localhost";
const NOW = Date.parse(new crypto.X509Certificate(CERTIFICATE).validFrom) + 1000;
const tick = () => new Promise((resolve) => setImmediate(resolve));
const code = (value) => (error) => error.code === value;

class Socket extends Duplex {
  constructor() { super(); this.writes = []; this.headersSent = false; }
  _read() {}
  _write(chunk, _encoding, callback) { this.writes.push(Buffer.from(chunk)); callback(); }
  setTimeout(milliseconds, callback) { this.timeout = milliseconds; this.onTimeout = callback; return this; }
  writeHead(status, headers) { this.status = status; this.headers = headers; this.headersSent = true; }
}
class FakeWebSocket extends EventEmitter {
  constructor() { super(); this.sent = []; this.paused = false; this.closed = false; }
  pause() { this.paused = true; }
  resume() { this.paused = false; }
  send(data, options, callback) { this.sent.push({ data, options }); callback(); }
  close(code = 1000, reason = Buffer.alloc(0)) { if (!this.closed) { this.closed = true; this.emit("close", code, reason); } }
  terminate() { this.close(1006); }
}

function setup(overrides = {}) {
  const errors = [];
  const sessions = {};
  const secureServers = [];
  const outgoing = [];
  const wsOutgoing = [];
  const wsClients = [];
  const server = new EventEmitter();
  server.listening = false;
  server.listen = (port, host, callback) => {
    assert.equal(port, 0); assert.equal(host, "127.0.0.1");
    server.listening = true; queueMicrotask(callback);
  };
  server.address = () => ({ address: "127.0.0.1", port: 45678 });
  server.close = () => { server.listening = false; };
  const session = {
    webRequest: { onBeforeRequest: (handler) => { sessions.request = handler; } },
    setCertificateVerifyProc: (handler) => { sessions.verify = handler; },
    setProxy: async (config) => { sessions.config = config; },
    closeAllConnections: async () => { sessions.closedConnections = true; },
    resolveProxy: async (url) => new URL(url).origin === ENDPOINT ? "DIRECT" : "PROXY 127.0.0.1:45678",
  };
  const certificates = {
    get: async (hostname) => { assert.equal(hostname, HOSTNAME); return { key: Buffer.from("synthetic-key"), cert: Buffer.from(CERTIFICATE) }; },
    dispose: () => { certificates.disposed = true; },
  };
  const wsModule = {
    WebSocket: class extends FakeWebSocket {
      constructor(url, protocols, options) {
        super(); this.protocol = protocols[0] || "";
        wsOutgoing.push({ url, protocols, options, socket: this });
        queueMicrotask(() => { this.emit("upgrade", { headers: { "set-cookie": ["ws=1; Secure; SameSite=Lax; Path=/"] } }); this.emit("open"); });
      }
    },
    WebSocketServer: class extends EventEmitter {
      constructor(options) { super(); this.options = options; }
      handleUpgrade(_request, _socket, _head, callback) {
        const client = new FakeWebSocket();
        const headers = [];
        this.emit("headers", headers);
        wsClients.push({ socket: client, headers, protocol: this.options.handleProtocols() });
        callback(client);
      }
      close() { this.closed = true; }
    },
  };
  let registry = [ORIGIN];
  let status = 200;
  const fetched = [];
  const controller = createRemotePreview({
    session, endpoint: ENDPOINT, credential: "synthetic-runner-credential", now: () => NOW, ideWebContentsId: 7,
    certificates, wsModule,
    createServer: (_options, handler) => { server.request = handler; return server; },
    createSecureContext: (options) => ({ options }),
    createSecureServer: (options, handler) => {
      const secure = new EventEmitter();
      secureServers.push({ options, handler, server: secure });
      return secure;
    },
    request: (url, options, callback) => {
      const socket = new Socket();
      outgoing.push({ url, options, socket });
      socket.on("finish", () => {
        const response = Readable.from(["application response"]);
        response.statusCode = 302;
        response.headers = { "set-cookie": ["one=1; Secure; SameSite=Lax; Path=/", "two=2; Secure; SameSite=Strict; Path=/"], location: "/next?keep=1" };
        response.once("end", () => socket.destroy());
        callback(response);
      });
      return socket;
    },
    onError: (error) => errors.push(error),
    fetch: async (url, options) => { fetched.push({ url, options }); return Response.json({ origins: registry }, { status }); },
    ...overrides,
  });
  function connect(authority = `${HOSTNAME}:443`, authenticated = true) {
    const socket = new Socket();
    server.emit("connection", socket);
    const credentials = controller.proxyCredentials({ isProxy: true, host: "127.0.0.1", port: 45678,
      scheme: "basic", realm: "Veld remote preview" });
    const headers = authenticated ? { "proxy-authorization": `Basic ${Buffer.from(credentials.join(":")).toString("base64")}` } : {};
    server.emit("connect", { url: authority, headers }, socket, Buffer.alloc(0));
    return socket;
  }
  function application({ requestPath = "/path?q=original", hostname = HOSTNAME, host = HOSTNAME, headers = {}, method = "POST" } = {}) {
    const incoming = Readable.from(["application body"]);
    incoming.method = method; incoming.url = requestPath;
    incoming.headers = { host, ...headers };
    incoming.socket = new Socket(); incoming.socket.servername = hostname;
    const response = new Socket();
    return { incoming, response };
  }
  return { controller, sessions, session, server, certificates, secureServers, outgoing, wsOutgoing, wsClients,
    fetched, errors, connect, application,
    setRegistry: (value) => { registry = value; }, setStatus: (value) => { status = value; } };
}

test("origins and CONNECT authorities reject local ports and noncanonical spellings", () => {
  assert.equal(canonicalPreviewOrigin(ORIGIN), ORIGIN);
  assert.equal(canonicalPreviewOrigin(`${ORIGIN}:8443`), `${ORIGIN}:8443`);
  assert.equal(previewOrigin(`wss://${HOSTNAME}/hmr`), ORIGIN);
  assert.equal(connectOrigin(`${HOSTNAME}:443`), ORIGIN);
  assert.equal(connectOrigin(`${HOSTNAME}:8443`), `${ORIGIN}:8443`);
  for (const origin of ["https://localhost", "https://127.0.0.1:19899", "https://[::1]", "http://preview.project.localhost",
    `${ORIGIN}/`, `${ORIGIN}/path`, `${ORIGIN}?`, `${ORIGIN}#`, `${ORIGIN}:0`, `${ORIGIN}:443`,
    "https://user:pass@preview.project.localhost", "https://preview.project.localhost.evil.test", "https://PREVIEW.project.localhost"]) {
    assert.throws(() => canonicalPreviewOrigin(origin), code("origin"), origin);
  }
  for (const authority of [HOSTNAME, `${HOSTNAME}:0443`, "127.0.0.1:443", "localhost:19899", `${HOSTNAME}:443/path`,
    `${HOSTNAME}:443?`, `${HOSTNAME}:443#`, `user:pass@${HOSTNAME}:443`]) assert.throws(() => connectOrigin(authority), code("origin"));
});

test("application URL binds the approved origin and preserves the original escaped path and query", () => {
  const originalPath = "/a%2Fb?x=1&next=https%3A%2F%2Fveld.localhost%2Fapi";
  const url = applicationURL(ENDPOINT, ORIGIN, originalPath);
  assert.equal(url.origin, ENDPOINT);
  assert.equal(url.pathname, "/remote/preview");
  assert.equal(url.searchParams.get("origin"), ORIGIN);
  assert.equal(url.searchParams.get("path"), originalPath);
  for (const value of ["https://veld.localhost/api", "//veld.localhost/api", "/\\veld.localhost", "/x\r\nHost: veld.localhost", "/x#fragment"]) {
    assert.throws(() => applicationURL(ENDPOINT, ORIGIN, value), code("target"));
  }
});

test("spoofed X-Veld headers never select credentials, origin or target", () => {
  const headers = applicationHeaders({ host: "veld.localhost", authorization: "Bearer app-token", origin: ORIGIN,
    cookie: "session=app-cookie", "x-veld-preview-authorization": "Bearer stolen", "X-Veld-Preview-Origin": "https://veld.localhost",
    "x-veld-target": "http://127.0.0.1:19899", "proxy-authorization": "Basic local-secret",
    connection: "keep-alive, x-hop", "x-hop": "remove", "sec-websocket-key": "old-key" }, "runner-token");
  assert.deepEqual(headers, { cookie: "session=app-cookie", Authorization: "Bearer runner-token",
    "X-Veld-Preview-Authorization": "Bearer app-token", "X-Veld-Preview-Origin": ORIGIN });
});

test("local leaf pins require an exact hostname, validity interval and fingerprint", () => {
  const pin = certificatePin(CERTIFICATE, HOSTNAME, NOW);
  assert.match(pin.fingerprint, /^[a-f0-9]{64}$/);
  assert.notEqual(pin.fingerprint, certificatePin(OTHER_CERTIFICATE, HOSTNAME, NOW).fingerprint);
  assert.throws(() => certificatePin(CERTIFICATE, "veld.localhost", NOW), code("certificate"));
  assert.throws(() => certificatePin(CERTIFICATE, HOSTNAME, pin.validFrom - 1), code("certificate"));
  assert.throws(() => certificatePin(CERTIFICATE, HOSTNAME, pin.validTo), code("certificate"));
});

test("certificate generation uses a bounded command and owner-only ephemeral files", async (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "veld-preview-test-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  let work;
  let commands = 0;
  const certificates = createLocalCertificates({ temporaryRoot: root, now: () => NOW, execute: async (command, args, options) => {
    commands++;
    assert.equal(command, "openssl");
    assert.equal(options.timeout, 5000);
    assert.ok(options.signal);
    assert.equal(options.shell, undefined);
    const config = args[args.indexOf("-config") + 1];
    const key = args[args.indexOf("-keyout") + 1];
    const cert = args[args.indexOf("-out") + 1];
    work = path.dirname(key);
    assert.equal(fs.statSync(work).mode & 0o777, 0o700);
    for (const filename of [config, key, cert]) assert.equal(fs.statSync(filename).mode & 0o777, 0o600);
    assert.match(fs.readFileSync(config, "utf8"), /subjectAltName=DNS:preview\.project\.localhost/);
    fs.writeFileSync(key, "synthetic-private-key");
    fs.writeFileSync(cert, CERTIFICATE);
  } });
  t.after(() => certificates.dispose());
  const [first, second] = await Promise.all([certificates.get(HOSTNAME), certificates.get(HOSTNAME)]);
  assert.equal(commands, 1);
  assert.equal(first, second);
  assert.equal(fs.existsSync(work), false);
  certificates.dispose();
  await tick();
  assert.ok(first.key.every((byte) => byte === 0));
  assert.deepEqual(fs.readdirSync(root), []);
});

test("fixed proxy disables implicit localhost bypass without DIRECT fallback", async (t) => {
  const fixture = setup(); t.after(() => fixture.controller.dispose());
  await fixture.controller.start();
  assert.deepEqual(fixture.sessions.config, { mode: "fixed_servers", proxyRules: "http://127.0.0.1:45678",
    proxyBypassRules: "<-loopback>,runner.example.ts.net:19896" });
  assert.equal(fixture.sessions.closedConnections, true);
  assert.equal(fixture.server.maxConnections, 32);
  assert.equal(fixture.fetched[0].options.redirect, "error");
  assert.equal(fixture.fetched[0].options.headers.Authorization, "Bearer synthetic-runner-credential");
  assert.ok(!fixture.fetched[0].url.includes("credential"));
});

test("effective localhost bypass fails startup closed", async () => {
  const fixture = setup(); fixture.session.resolveProxy = async () => "DIRECT";
  await assert.rejects(fixture.controller.start(), code("proxy"));
  assert.equal(fixture.server.listening, false);
  assert.equal(await fixture.controller.allowRequest({ webContentsId: 7, url: ORIGIN, resourceType: "subFrame" }), false);
});

test("requests refresh newly started services and never permit Mac loopback or unknown origins", async (t) => {
  const fixture = setup(); t.after(() => fixture.controller.dispose()); await fixture.controller.start();
  for (const url of [`${ORIGIN}/`, `wss://${HOSTNAME}/hmr`]) assert.equal(await fixture.controller.allowRequest({ webContentsId: 7, url, resourceType: "subFrame" }), true);
  for (const url of ["http://127.0.0.1:19899", "https://localhost", "http://preview.project.localhost", "https://veld.localhost", "https://example.test", "file:///tmp/file"]) {
    assert.equal(await fixture.controller.allowRequest({ webContentsId: 7, url, resourceType: "xhr" }), false, url);
  }
  assert.equal(await fixture.controller.allowRequest({ webContentsId: 7, url: ORIGIN, resourceType: "mainFrame" }), false);
  const newOrigin = "https://new.project.localhost:8443";
  fixture.setRegistry([ORIGIN, newOrigin]);
  assert.equal(await fixture.controller.allowRequest({ webContentsId: 7, url: `${newOrigin}/page`, resourceType: "subFrame" }), true);
  fixture.setRegistry([]);
  assert.equal(await fixture.controller.allowRequest({ webContentsId: 7, url: ORIGIN, resourceType: "xhr" }), false);
});

test("CONNECT requires a per-window proxy password before authorization or certificate generation", async (t) => {
  const fixture = setup(); t.after(() => fixture.controller.dispose()); await fixture.controller.start();
  const before = fixture.fetched.length;
  const socket = fixture.connect(undefined, false); await tick();
  assert.match(Buffer.concat(socket.writes).toString(), /^HTTP\/1.1 407/);
  assert.equal(fixture.fetched.length, before);
  assert.equal(fixture.secureServers.length, 0);
  assert.equal(fixture.controller.proxyCredentials({ isProxy: false, host: "127.0.0.1", port: 45678, scheme: "basic", realm: "Veld remote preview" }), null);
});

test("approved CONNECT cannot reach veld.localhost through different SNI, Host or absolute request target", async (t) => {
  const fixture = setup(); t.after(() => fixture.controller.dispose()); await fixture.controller.start();
  const socket = fixture.connect(); await tick();
  assert.match(Buffer.concat(socket.writes).toString(), /^HTTP\/1.1 200/);
  assert.equal(fixture.outgoing.length, 0, "CONNECT does not open a raw remote tunnel");
  const secure = fixture.secureServers[0];
  secure.options.SNICallback("veld.localhost", (error) => assert.equal(error.code, "target"));
  for (const options of [{ hostname: "veld.localhost" }, { host: "veld.localhost" },
    { requestPath: "https://veld.localhost/api" }, { requestPath: "//veld.localhost/api" }]) {
    const { incoming, response } = fixture.application(options);
    assert.equal(matchesApplication(incoming, ORIGIN), !options.hostname && !options.host);
    secure.handler(incoming, response); await tick();
    assert.equal(response.status, 502);
  }
  assert.equal(fixture.outgoing.length, 0);
  const missingSNI = new Socket();
  secure.server.emit("secureConnection", missingSNI);
  assert.equal(missingSNI.destroyed, true);
});

test("HTTP proxy preserves method, path, body, application credentials, cookies and redirects", async (t) => {
  const fixture = setup(); t.after(() => fixture.controller.dispose()); await fixture.controller.start();
  fixture.connect(); await tick();
  const { incoming, response } = fixture.application({ headers: { authorization: "Bearer app-token", origin: ORIGIN,
    cookie: "session=app-cookie", "x-veld-preview-origin": "https://veld.localhost" }, method: "PATCH" });
  fixture.secureServers[0].handler(incoming, response); await tick();
  const upstream = fixture.outgoing[0];
  assert.equal(upstream.url.origin, ENDPOINT);
  assert.equal(upstream.url.searchParams.get("origin"), ORIGIN);
  assert.equal(upstream.url.searchParams.get("path"), "/path?q=original");
  assert.equal(upstream.options.method, "PATCH");
  assert.equal(upstream.options.rejectUnauthorized, true);
  assert.equal(upstream.options.headers.Authorization, "Bearer synthetic-runner-credential");
  assert.equal(upstream.options.headers["X-Veld-Preview-Authorization"], "Bearer app-token");
  assert.equal(upstream.options.headers["X-Veld-Preview-Origin"], ORIGIN);
  assert.equal(Buffer.concat(upstream.socket.writes).toString(), "application body");
  assert.equal(response.status, 302);
  assert.deepEqual(response.headers["set-cookie"], ["one=1; Secure; SameSite=Lax; Path=/", "two=2; Secure; SameSite=Strict; Path=/"]);
  assert.equal(response.headers.location, "/next?keep=1");
  assert.equal(fixture.outgoing.length, 1, "redirects are returned to Chromium, never followed in main");
});

test("only the main-generated leaf is pinned; the runner retains strict normal verification", async (t) => {
  const fixture = setup(); t.after(() => fixture.controller.dispose()); await fixture.controller.start();
  let verdict;
  const verify = (hostname, data) => fixture.sessions.verify({ hostname, certificate: { data } }, (value) => { verdict = value; });
  verify(HOSTNAME, CERTIFICATE); assert.equal(verdict, -2);
  fixture.connect(); await tick();
  verify(HOSTNAME, CERTIFICATE); assert.equal(verdict, 0);
  verify(HOSTNAME, OTHER_CERTIFICATE); assert.equal(verdict, -2);
  verify("veld.localhost", CERTIFICATE); assert.equal(verdict, -2);
  verify("runner.example.ts.net", CERTIFICATE); assert.equal(verdict, -3);
});

test("WebSocket proxy preserves application protocol, cookies and frame types without raw TCP", async (t) => {
  const fixture = setup(); t.after(() => fixture.controller.dispose()); await fixture.controller.start();
  fixture.connect(); await tick();
  const { incoming } = fixture.application({ method: "GET", requestPath: "/hmr?token=app-token", headers: {
    "sec-websocket-protocol": "vite-hmr", origin: ORIGIN, authorization: "Bearer app-ws-token", "x-veld-preview-origin": "spoof",
  } });
  fixture.secureServers[0].server.emit("upgrade", incoming, incoming.socket, Buffer.alloc(0)); await tick();
  const outer = fixture.wsOutgoing[0];
  assert.equal(new URL(outer.url).origin, ENDPOINT.replace("https:", "wss:"));
  assert.equal(new URL(outer.url).searchParams.get("origin"), ORIGIN);
  assert.equal(new URL(outer.url).searchParams.get("path"), "/hmr?token=app-token");
  assert.deepEqual(outer.protocols, ["vite-hmr"]);
  assert.equal(outer.options.rejectUnauthorized, true);
  assert.equal(outer.options.followRedirects, false);
  assert.equal(outer.options.headers["X-Veld-Preview-Authorization"], "Bearer app-ws-token");
  assert.equal(fixture.wsClients[0].protocol, "vite-hmr");
  assert.ok(fixture.wsClients[0].headers.includes("Set-Cookie: ws=1; Secure; SameSite=Lax; Path=/"));
  outer.socket.emit("message", Buffer.from("hmr text"), false);
  const message = fixture.wsClients[0].socket.sent[0];
  assert.equal(message.data.toString(), "hmr text"); assert.equal(message.options.binary, false);
  fixture.wsClients[0].socket.emit("message", Buffer.from([1, 2]), true);
  assert.equal(outer.socket.sent[0].options.binary, true);
});

test("revocation rejects every subsequent application request without opening an upstream", async (t) => {
  const fixture = setup(); t.after(() => fixture.controller.dispose()); await fixture.controller.start();
  fixture.connect(); await tick(); fixture.setStatus(401);
  const { incoming, response } = fixture.application();
  fixture.secureServers[0].handler(incoming, response); await tick();
  assert.equal(response.status, 502); assert.equal(fixture.outgoing.length, 0);
  assert.equal(await fixture.controller.allowRequest({ webContentsId: 7, url: ORIGIN, resourceType: "xhr" }), false);
});

test("WebSocket forwarding pauses reads until send completes and propagates close", () => {
  const left = new FakeWebSocket(); const right = new FakeWebSocket();
  let complete;
  right.send = (_data, _options, callback) => { complete = callback; };
  bridgeWebSockets(left, right, (error) => assert.fail(error.code));
  left.emit("message", Buffer.from("message"), false); assert.equal(left.paused, true);
  complete(); assert.equal(left.paused, false);
  right.close(1000); assert.equal(left.closed, true);
});

test("dispose closes proxy, removes ephemeral keys and keeps late requests blocked", async () => {
  const fixture = setup(); await fixture.controller.start(); const socket = fixture.connect(); await tick();
  fixture.controller.dispose(); await tick();
  assert.equal(socket.destroyed, true); assert.equal(fixture.server.listening, false);
  assert.equal(fixture.certificates.disposed, true);
  let cancelled;
  fixture.sessions.request({ url: ORIGIN, resourceType: "xhr" }, (value) => { cancelled = value.cancel; }); await tick();
  assert.equal(cancelled, true);
  assert.equal(fixture.sessions.config.mode, "fixed_servers", "never reset a closed session to DIRECT");
});

test("renderer transport URLs are blocked before network for encoded, escaped and normalized paths", async (t) => {
  const fixture = setup(); t.after(() => fixture.controller.dispose()); await fixture.controller.start();
  fixture.controller.registerPreviewContents(8);
  const before = fixture.fetched.length;
  const paths = [
    "/remote/preview?origin=https%3A%2F%2Fpreview.project.localhost&path=payload.html",
    "/remote/previews", "/remote/pair", "/remote/preview/", "/REMOTE/preview", "/remote;parameter/preview",
    "/%72emote/preview", "/remote/%70review", "/remote%2fpreview", "/remote%252fpreview",
    "/remote%5cpreview", "/remote\\preview", "//remote/preview", "/ide/../remote/preview",
    "/ide/%2e%2e/remote/preview", "/a/..%2fremote%2fpreview", "/remote/preview%00", "/%2572emote/preview",
  ];
  for (const protocol of ["https:", "wss:"]) for (const path of paths) {
    const url = ENDPOINT.replace("https:", protocol) + path;
    assert.equal(runnerRendererURL(url, ENDPOINT), false, url);
    for (const webContentsId of [7, 8, 99, undefined]) for (const resourceType of ["mainFrame", "subFrame", "xhr", "script", "webSocket"]) {
      assert.equal(await fixture.controller.allowRequest({ url, webContentsId, resourceType }), false, `${url} ${resourceType}`);
    }
  }
  assert.equal(fixture.fetched.length, before, "renderer transport requests never reach the network");
  assert.equal(fixture.outgoing.length, 0);
  // The main-only application proxy still constructs the required transport URL.
  assert.equal(applicationURL(ENDPOINT, ORIGIN, "/payload.html").pathname, "/remote/preview");
});

test("IDE agent endpoints and encoded API data pass the request guard without granting preview access", async (t) => {
  const fixture = setup(); t.after(() => fixture.controller.dispose()); await fixture.controller.start();
  fixture.controller.registerPreviewContents(8);
  for (const path of ["/remote/agents/17", "/remote/agents/17/launch", "/api/lanes/In%20Review",
    "/api/lanes/%C3%9Cberpr%C3%BCfung", "/api/lanes/100%25", "/api/lanes/Issue%23one"]) {
    const details = { url: ENDPOINT + path, resourceType: "xhr", webContentsId: 7 };
    assert.equal(await fixture.controller.allowRequest(details), true, path);
    for (const webContentsId of [8, 99, undefined]) {
      assert.equal(await fixture.controller.allowRequest({ ...details, webContentsId }), false, path);
    }
    assert.equal(await fixture.controller.allowRequest({ ...details, resourceType: "mainFrame" }), false, path);
  }
  for (const path of ["/remote/agents/17/preview", "/remote/agents/17/launch/extra", "/remote/agents/%31%37",
    "/api/lanes/%2f..%2fremote%2fpreview", "/api/lanes/%5cremote", "/api/lanes/%00", "/api/lanes/%ZZ",
    "/api/../remote/preview", "/api/%2e%2e/remote/preview"]) {
    assert.equal(await fixture.controller.allowRequest({ url: ENDPOINT + path, resourceType: "xhr", webContentsId: 7 }), false, path);
  }
});

test("only the owned IDE main frame can navigate to /ide; preview windows cannot reach any runner path", async (t) => {
  const fixture = setup(); t.after(() => fixture.controller.dispose()); await fixture.controller.start();
  fixture.controller.registerPreviewContents(8);
  for (const path of ["/ide", "/ide?remote=1", "/ide?query=%2Fremote%2Fpreview#tab"]) {
    assert.equal(await fixture.controller.allowRequest({ url: ENDPOINT + path, webContentsId: 7, resourceType: "mainFrame" }), true);
    assert.equal(await fixture.controller.allowRequest({ url: ENDPOINT + path, webContentsId: 8, resourceType: "mainFrame" }), false);
  }
  for (const path of ["/api/health", "/", "/ide/", "/ide.html", "/ide%2f", "/ide%3fremote=1"]) {
    assert.equal(await fixture.controller.allowRequest({ url: ENDPOINT + path, webContentsId: 7, resourceType: "mainFrame" }), false);
  }
  assert.equal(await fixture.controller.allowRequest({ url: ENDPOINT + "/api/repos", webContentsId: 7, resourceType: "xhr" }), true);
  for (const webContentsId of [8, 99, undefined]) {
    assert.equal(await fixture.controller.allowRequest({ url: ENDPOINT + "/api/repos", webContentsId, resourceType: "xhr" }), false);
  }
});

test("top-level application requests require a main-owned preview webContents registration", async (t) => {
  const fixture = setup(); t.after(() => fixture.controller.dispose()); await fixture.controller.start();
  const details = { url: `${ORIGIN}/login`, webContentsId: 8, resourceType: "mainFrame" };
  assert.equal(await fixture.controller.allowRequest(details), false);
  fixture.controller.registerPreviewContents(8);
  assert.equal(await fixture.controller.allowRequest(details), true);
  assert.equal(await fixture.controller.allowRequest({ ...details, webContentsId: 7 }), false);
  assert.equal(await fixture.controller.allowRequest({ ...details, url: "https://veld.localhost/api" }), false);
  fixture.controller.unregisterPreviewContents(8);
  assert.equal(await fixture.controller.allowRequest(details), false);
});

test("registry revocation clears authority and notifies the owner to close all windows", async () => {
  let revoked = 0;
  const fixture = setup({ onRevoked: () => { revoked++; } });
  await fixture.controller.start(); fixture.controller.registerPreviewContents(8);
  fixture.setStatus(401);
  assert.equal(await fixture.controller.allowRequest({ url: ORIGIN, webContentsId: 8, resourceType: "mainFrame" }), false);
  assert.equal(revoked, 1);
  assert.equal(fixture.server.listening, false);
  assert.equal(fixture.controller.proxyCredentials({ isProxy: true, host: "127.0.0.1", port: 45678, scheme: "basic", realm: "Veld remote preview" }), null);
  fixture.controller.dispose();
});

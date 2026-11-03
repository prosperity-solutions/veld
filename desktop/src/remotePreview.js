// Main-process application proxy. Never tunnel opaque client TLS to the runner:
// an approved CONNECT must not let a client select another Caddy SNI or Host.
const http = require("node:http");
const https = require("node:https");
const tls = require("node:tls");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { execFile } = require("node:child_process");
const { promisify } = require("node:util");
const { canonicalEndpoint } = require("./remoteConnections");

const MAX_REGISTRY_BYTES = 64 * 1024;
const MAX_ORIGINS = 512;
const MAX_CERTIFICATES = 64;
const MAX_CONNECTIONS = 32;
const MAX_REQUESTS = 128;
const MAX_WS_MESSAGE_BYTES = 8 * 1024 * 1024;
const CONNECT_TIMEOUT_MS = 15_000;
const NETWORK_TIMEOUT_MS = 5_000;
const IDLE_TIMEOUT_MS = 120_000;
const REVOCATION_POLL_MS = 15_000;
const PROXY_REALM = "Veld remote preview";
const PROXY_USER = "veld-preview";
const execFileAsync = promisify(execFile);
const HOP_HEADERS = new Set(["connection", "keep-alive", "proxy-authenticate", "proxy-authorization",
  "proxy-connection", "te", "trailer", "transfer-encoding", "upgrade", "alt-svc"]);

class RemotePreviewError extends Error {
  constructor(code) {
    super("The remote preview connection was refused.");
    this.name = "RemotePreviewError";
    this.code = code;
  }
}
const fail = (code) => { throw new RemotePreviewError(code); };

/** Renderer URLs exclude preview transport, while permitting the IDE agent API. */
function runnerRendererURL(value, endpoint, ideOnly = false) {
  try {
    const url = new URL(value);
    if (url.username || url.password || !["https:", "wss:"].includes(url.protocol)) return false;
    if (ideOnly && url.protocol !== "https:") return false;
    url.protocol = "https:";
    if (url.origin !== endpoint || url.pathname.includes("//")) return false;
    if (ideOnly) return url.pathname === "/ide";
    // API parameters can contain spaces and Unicode. Decode each segment so
    // encoded data cannot introduce path separators or traversal segments.
    if (url.pathname.startsWith("/api/")) {
      return url.pathname.split("/").every((segment) => {
        const decoded = decodeURIComponent(segment);
        return decoded !== "." && decoded !== ".." && !/[\/\\\u0000-\u001f\u007f]/u.test(decoded);
      });
    }
    if (/^\/remote\/agents\/[0-9]+(?:\/launch)?$/.test(url.pathname)) return true;
    return !/[%\\]/.test(url.pathname) && !/^\/remote(?:\/|;|$)/i.test(url.pathname);
  } catch { return false; }
}

/** Only canonical HTTPS application origins can become CONNECT destinations. */
function canonicalPreviewOrigin(value) {
  if (typeof value !== "string" || value.length > 512) fail("origin");
  let url;
  try { url = new URL(value); } catch { fail("origin"); }
  if (url.protocol !== "https:" || url.origin !== value || url.username || url.password ||
      url.pathname !== "/" || url.search || url.hash || url.port === "0" ||
      !url.hostname.endsWith(".localhost") || url.hostname.length > 253 ||
      !url.hostname.split(".").every((label) => /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(label))) fail("origin");
  return url.origin;
}

function previewOrigin(value) {
  try {
    const url = new URL(value);
    if (url.username || url.password) return null;
    if (url.protocol === "wss:") url.protocol = "https:";
    if (url.protocol !== "https:") return null;
    return canonicalPreviewOrigin(url.origin);
  } catch { return null; }
}

function connectOrigin(authority) {
  if (typeof authority !== "string") fail("origin");
  const origin = canonicalPreviewOrigin(`https://${authority.replace(/:443$/, "")}`);
  const url = new URL(origin);
  if (authority !== `${url.hostname}:${url.port || "443"}`) fail("origin");
  return origin;
}

function applicationPath(value) {
  if (typeof value !== "string" || value.length > 16_384 || !value.startsWith("/") ||
      value.startsWith("//") || /[\\#\u0000-\u0020\u007f]/u.test(value)) fail("target");
  return value;
}

function matchesApplication(request, origin) {
  const target = new URL(origin);
  return request.socket.servername === target.hostname &&
    (request.headers.host === target.host || (!target.port && request.headers.host === `${target.hostname}:443`));
}

/** The only upstream URL builder: destination and origin cannot come from Host/SNI/path. */
function applicationURL(endpoint, origin, requestPath, websocket = false) {
  const url = new URL(`${canonicalEndpoint(endpoint)}/remote/preview`);
  url.searchParams.set("origin", canonicalPreviewOrigin(origin));
  url.searchParams.set("path", applicationPath(requestPath));
  if (websocket) url.protocol = "wss:";
  return url;
}

function endToEndHeaders(headers) {
  const connectionTokens = new Set(String(headers.connection || "").toLowerCase().split(",").map((name) => name.trim()));
  return Object.fromEntries(Object.entries(headers).filter(([name]) => {
    const lower = name.toLowerCase();
    return !HOP_HEADERS.has(lower) && !connectionTokens.has(lower) && !lower.startsWith("x-veld-");
  }));
}

/** Application credentials are distinct from the outer runner credential. */
function applicationHeaders(headers, credential) {
  const result = endToEndHeaders(headers);
  for (const name of Object.keys(result)) {
    const lower = name.toLowerCase();
    if (["host", "authorization", "origin", "expect"].includes(lower) || lower.startsWith("sec-websocket-")) delete result[name];
  }
  result.Authorization = `Bearer ${credential}`;
  if (headers.authorization !== undefined) result["X-Veld-Preview-Authorization"] = headers.authorization;
  if (headers.origin !== undefined) result["X-Veld-Preview-Origin"] = headers.origin;
  return result;
}

/** Validate and fingerprint only leaves generated by this main process. */
function certificatePin(raw, hostname, now = Date.now()) {
  try {
    const certificate = new crypto.X509Certificate(raw);
    const validFrom = Date.parse(certificate.validFrom);
    const validTo = Date.parse(certificate.validTo);
    if (!certificate.checkHost(hostname, { subject: "never" }) || !Number.isFinite(validFrom) ||
        !Number.isFinite(validTo) || now < validFrom || now >= validTo) fail("certificate");
    return { hostname, fingerprint: crypto.createHash("sha256").update(certificate.raw).digest("hex"), validFrom, validTo };
  } catch { fail("certificate"); }
}

/** Ephemeral owner-only files; no shell, installed CA, or persistent private key. */
function createLocalCertificates({ execute = execFileAsync, temporaryRoot = os.tmpdir(), now = Date.now } = {}) {
  const entries = new Map();
  const active = new Set();
  const queue = [];
  let directory;
  let disposed = false;
  let running = 0;

  async function generate(hostname) {
    if (running >= 2) await new Promise((resolve) => queue.push(resolve));
    else running++;
    const controller = new AbortController();
    active.add(controller);
    let work;
    try {
      if (disposed) fail("closed");
      if (!directory) {
        directory = fs.mkdtempSync(path.join(temporaryRoot, "veld-preview-"));
        fs.chmodSync(directory, 0o700);
      }
      work = fs.mkdtempSync(path.join(directory, "leaf-"));
      fs.chmodSync(work, 0o700);
      const keyFile = path.join(work, "key.pem");
      const certFile = path.join(work, "cert.pem");
      const configFile = path.join(work, "openssl.cnf");
      const config = `prompt=no\ndistinguished_name=dn\nx509_extensions=ext\n[dn]\nCN=Veld remote preview\n[ext]\nsubjectAltName=DNS:${hostname}\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\n`;
      for (const [filename, data] of [[keyFile, ""], [certFile, ""], [configFile, config]]) {
        fs.writeFileSync(filename, data, { mode: 0o600, flag: "wx" });
      }
      await execute("openssl", ["req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1",
        "-nodes", "-days", "1", "-config", configFile, "-keyout", keyFile, "-out", certFile], {
        timeout: NETWORK_TIMEOUT_MS, maxBuffer: 16_384, signal: controller.signal, windowsHide: true,
      });
      if (disposed) fail("closed");
      const cert = fs.readFileSync(certFile);
      const pin = certificatePin(cert, hostname, now());
      return { key: fs.readFileSync(keyFile), cert, pin };
    } catch { fail(disposed ? "closed" : "certificate"); }
    finally {
      active.delete(controller);
      if (work) fs.rmSync(work, { recursive: true, force: true });
      const next = queue.shift();
      if (next) next();
      else running--;
    }
  }

  async function get(hostname) {
    canonicalPreviewOrigin(`https://${hostname}`);
    if (disposed) fail("closed");
    const old = entries.get(hostname);
    if (old) {
      const leaf = await old;
      if (leaf.pin.validTo > now() + 60_000) return leaf;
      leaf.key.fill(0);
      entries.delete(hostname);
    }
    if (entries.size >= MAX_CERTIFICATES) fail("capacity");
    const promise = generate(hostname);
    entries.set(hostname, promise);
    try { return await promise; }
    catch (error) { entries.delete(hostname); throw error; }
  }

  function dispose() {
    disposed = true;
    for (const controller of active) controller.abort();
    for (const promise of entries.values()) void promise.then((leaf) => leaf.key.fill(0), () => {
      // The request awaiting this same promise reports the generation failure.
    });
    entries.clear();
    if (directory) fs.rmSync(directory, { recursive: true, force: true });
  }
  return { get, dispose };
}

/** Preserve application WebSocket message boundaries, type and backpressure. */
function bridgeWebSockets(left, right, report) {
  const stop = () => { left.terminate(); right.terminate(); };
  for (const [source, target] of [[left, right], [right, left]]) {
    source.on("message", (data, binary) => {
      source.pause();
      target.send(data, { binary }, (error) => {
        if (error) { report(new RemotePreviewError("websocket")); stop(); }
        else source.resume();
      });
    });
    source.on("error", () => { report(new RemotePreviewError("websocket")); stop(); });
    source.once("close", (code, reason) => {
      // 1005/1006 are local observations, not legal close-frame codes.
      if (code === 1005 || code === 1006) target.terminate();
      else target.close(code, reason);
    });
  }
}

/**
 * One controller per registration/session. start() precedes loadURL(). fetch
 * belongs to the separate control session, never the proxied renderer session.
 */
function createRemotePreview({ session, endpoint, credential, fetch, ideWebContentsId, onRevoked = () => {},
  certificates = createLocalCertificates(), now = Date.now,
  createServer = http.createServer, createSecureServer = https.createServer,
  createSecureContext = tls.createSecureContext, request = https.request,
  wsModule, onError = (code) => console.warn("[veld] remote preview refused:", code) }) {
  canonicalEndpoint(endpoint);
  const runner = new URL(endpoint);
  const sockets = new Set();
  const requests = new Set();
  const websockets = new Set();
  const pins = new Map();
  const reported = new Set();
  const previewContents = new Set();
  const lifetime = new AbortController();
  const proxyPassword = crypto.randomBytes(32).toString("base64url");
  const proxyAuthorization = Buffer.from(`Basic ${Buffer.from(`${PROXY_USER}:${proxyPassword}`).toString("base64")}`);
  let origins = new Set();
  let registryRequest = null;
  let disposed = false;
  let ready = false;
  let proxyPort = 0;
  let pendingRequests = 0;
  let revocationTimer = null;

  function report(error) {
    const code = error instanceof RemotePreviewError ? error.code : "transport";
    if (!disposed && !reported.has(code)) { reported.add(code); onError(code); }
  }
  function ensureActive() { if (disposed) fail("closed"); }
  function revoke() {
    if (disposed) return;
    dispose();
    onRevoked();
  }

  async function readRegistry() {
    const controller = new AbortController();
    const abort = () => controller.abort();
    lifetime.signal.addEventListener("abort", abort, { once: true });
    const timeout = setTimeout(abort, NETWORK_TIMEOUT_MS);
    try {
      const response = await fetch(`${endpoint}/remote/previews`, {
        headers: { Authorization: `Bearer ${credential}` }, redirect: "error", credentials: "omit",
        cache: "no-store", signal: controller.signal,
      });
      if (response.status === 401) revoke();
      if (!response.ok || response.redirected) fail("registry");
      const reader = response.body?.getReader();
      if (!reader) fail("registry");
      const chunks = [];
      let size = 0;
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        size += value.byteLength;
        if (size > MAX_REGISTRY_BYTES) fail("registry");
        chunks.push(Buffer.from(value));
      }
      const data = JSON.parse(Buffer.concat(chunks).toString("utf8"));
      if (!Array.isArray(data.origins) || data.origins.length > MAX_ORIGINS) fail("registry");
      const next = new Set(data.origins.map(canonicalPreviewOrigin));
      ensureActive();
      origins = next;
      return origins;
    } catch { origins.clear(); fail("registry"); }
    finally {
      clearTimeout(timeout);
      controller.abort();
      lifetime.signal.removeEventListener("abort", abort);
    }
  }

  async function refresh() {
    ensureActive();
    if (registryRequest) return registryRequest;
    registryRequest = readRegistry();
    try { return await registryRequest; } finally { registryRequest = null; }
  }

  async function authorize(origin) {
    if (!(await refresh()).has(origin)) fail("origin");
    if (await session.resolveProxy(origin) !== `PROXY 127.0.0.1:${proxyPort}`) fail("proxy");
    ensureActive();
  }

  async function allowRequest(details) {
    if (!ready || disposed) return false;
    const fromIDE = details.webContentsId === ideWebContentsId && Number.isInteger(ideWebContentsId);
    const fromPreview = previewContents.has(details.webContentsId);
    if (!fromIDE && !fromPreview) return false;
    if (runnerRendererURL(details.url, endpoint)) {
      return fromIDE && (details.resourceType !== "mainFrame" || runnerRendererURL(details.url, endpoint, true));
    }
    const origin = previewOrigin(details.url);
    if (!origin || (details.resourceType === "mainFrame" && !fromPreview) || pendingRequests >= MAX_REQUESTS) return false;
    pendingRequests++;
    try {
      await authorize(origin);
      if (await session.resolveProxy(details.url) !== `PROXY 127.0.0.1:${proxyPort}`) fail("proxy");
      return !disposed;
    }
    catch (error) { report(error); return false; }
    finally { pendingRequests--; }
  }

  function verifyCertificate(details, callback) {
    if (disposed) { callback(-2); return; }
    if (details.hostname === runner.hostname) { callback(-3); return; }
    try {
      const candidate = certificatePin(details.certificate.data, details.hostname, now());
      const pin = pins.get(details.hostname);
      const registered = [...origins].some((origin) => new URL(origin).hostname === details.hostname);
      callback(registered && pin?.fingerprint === candidate.fingerprint && now() < pin.validTo ? 0 : -2);
    } catch { callback(-2); }
  }

  function rejectResponse(response, error) {
    report(error);
    if (!response.headersSent) { response.writeHead(502, { "Content-Type": "text/plain", "Cache-Control": "no-store" }); response.end("Remote preview unavailable."); }
    else response.destroy();
  }

  async function proxyHTTP(origin, incoming, response) {
    let outgoing;
    try {
      if (!matchesApplication(incoming, origin)) fail("target");
      const url = applicationURL(endpoint, origin, incoming.url);
      if (incoming.method === "CONNECT" || incoming.method === "TRACE") fail("target");
      await authorize(origin);
      if (requests.size >= MAX_REQUESTS || incoming.destroyed) fail("capacity");
      outgoing = request(url, { method: incoming.method, headers: applicationHeaders(incoming.headers, credential),
        rejectUnauthorized: true, agent: false }, (upstream) => {
        clearTimeout(timeout);
        response.writeHead(upstream.statusCode, endToEndHeaders(upstream.headers));
        upstream.on("error", () => rejectResponse(response, new RemotePreviewError("http")));
        upstream.pipe(response);
      });
      requests.add(outgoing);
      const timeout = setTimeout(() => outgoing.destroy(new RemotePreviewError("http")), CONNECT_TIMEOUT_MS);
      outgoing.setTimeout(IDLE_TIMEOUT_MS, () => outgoing.destroy(new RemotePreviewError("http")));
      outgoing.once("close", () => { clearTimeout(timeout); requests.delete(outgoing); });
      outgoing.on("error", () => rejectResponse(response, new RemotePreviewError("http")));
      incoming.once("aborted", () => outgoing.destroy());
      incoming.on("error", () => outgoing.destroy(new RemotePreviewError("http")));
      response.once("close", () => { if (!response.writableFinished) outgoing.destroy(); });
      incoming.pipe(outgoing);
    } catch (error) { outgoing?.destroy(); rejectResponse(response, error); }
  }

  async function proxyWebSocket(origin, incoming, socket, head) {
    let upstream;
    let clientSocket;
    try {
      if (!matchesApplication(incoming, origin)) fail("target");
      const url = applicationURL(endpoint, origin, incoming.url, true);
      await authorize(origin);
      if (websockets.size >= MAX_CONNECTIONS * 2 || socket.destroyed) fail("capacity");
      const ws = wsModule || require("ws");
      const protocols = incoming.headers["sec-websocket-protocol"]?.split(",").map((value) => value.trim()) || [];
      upstream = new ws.WebSocket(url.href, protocols, {
        headers: applicationHeaders(incoming.headers, credential), rejectUnauthorized: true,
        followRedirects: false, perMessageDeflate: false, maxPayload: MAX_WS_MESSAGE_BYTES,
        handshakeTimeout: NETWORK_TIMEOUT_MS,
      });
      websockets.add(upstream);
      const cookies = [];
      upstream.on("upgrade", (response) => cookies.push(...(response.headers["set-cookie"] || [])));
      upstream.on("unexpected-response", (_request, response) => {
        response.destroy(); upstream.terminate(); socket.destroy(); report(new RemotePreviewError("websocket"));
      });
      upstream.on("error", () => { socket.destroy(); report(new RemotePreviewError("websocket")); });
      upstream.once("close", () => { websockets.delete(upstream); if (!clientSocket) socket.destroy(); });
      socket.once("close", () => upstream.terminate());
      upstream.once("open", () => {
        if (disposed || socket.destroyed) { upstream.terminate(); return; }
        upstream.pause();
        // One noServer upgrader per connection avoids cross-connection protocol
        // selection or Set-Cookie state. It never listens on another local port.
        try {
          const upgrader = new ws.WebSocketServer({ noServer: true, perMessageDeflate: false,
            maxPayload: MAX_WS_MESSAGE_BYTES, handleProtocols: () => upstream.protocol || false });
          socket.once("close", () => upgrader.close());
          upgrader.on("headers", (headers) => { for (const cookie of cookies) headers.push(`Set-Cookie: ${cookie}`); });
          upgrader.handleUpgrade(incoming, socket, head, (client) => {
            clientSocket = client;
            websockets.add(client);
            client.once("close", () => websockets.delete(client));
            bridgeWebSockets(client, upstream, report);
            upstream.resume();
          });
        } catch { upstream.terminate(); socket.destroy(); report(new RemotePreviewError("websocket")); }
      });
    } catch (error) { upstream?.terminate(); socket.destroy(); report(error); }
  }

  function proxyAuthenticated(request) {
    const supplied = Buffer.from(request.headers["proxy-authorization"] || "");
    return supplied.length === proxyAuthorization.length && crypto.timingSafeEqual(supplied, proxyAuthorization);
  }

  async function acceptConnect(request, socket, head) {
    if (!proxyAuthenticated(request)) {
      socket.end(`HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm="${PROXY_REALM}"\r\nConnection: close\r\nContent-Length: 0\r\n\r\n`);
      return;
    }
    try {
      ensureActive();
      const origin = connectOrigin(request.url);
      await authorize(origin);
      const hostname = new URL(origin).hostname;
      const leaf = await certificates.get(hostname);
      ensureActive();
      if (socket.destroyed || head.length) fail("target");
      const pin = certificatePin(leaf.cert, hostname, now());
      pins.set(hostname, pin);
      const context = createSecureContext({ key: leaf.key, cert: leaf.cert });
      const local = createSecureServer({ key: leaf.key, cert: leaf.cert, ALPNProtocols: ["http/1.1"],
        maxHeaderSize: 16 * 1024, SNICallback: (name, callback) => {
          if (name !== hostname) callback(new RemotePreviewError("target"));
          else callback(null, context);
        } }, (incoming, response) => { void proxyHTTP(origin, incoming, response); });
      local.headersTimeout = NETWORK_TIMEOUT_MS;
      local.requestTimeout = IDLE_TIMEOUT_MS;
      local.on("secureConnection", (secure) => { if (secure.servername !== hostname) secure.destroy(); });
      local.on("upgrade", (incoming, secure, pending) => { void proxyWebSocket(origin, incoming, secure, pending); });
      local.on("tlsClientError", (_error, secure) => { secure.destroy(); report(new RemotePreviewError("certificate")); });
      local.on("clientError", (_error, secure) => { secure.destroy(); report(new RemotePreviewError("target")); });
      local.on("error", () => { socket.destroy(); report(new RemotePreviewError("tls")); });
      socket.write("HTTP/1.1 200 Connection Established\r\n\r\n");
      socket.setTimeout(IDLE_TIMEOUT_MS, () => socket.destroy());
      local.emit("connection", socket);
    } catch (error) { socket.destroy(); report(error); }
  }

  const server = createServer({ maxHeaderSize: 16 * 1024 }, (_request, response) => {
    response.writeHead(405, { Connection: "close" }); response.end();
  });
  server.maxConnections = MAX_CONNECTIONS;
  server.headersTimeout = NETWORK_TIMEOUT_MS;
  server.requestTimeout = NETWORK_TIMEOUT_MS;
  server.on("connection", (socket) => {
    if (disposed || sockets.size >= MAX_CONNECTIONS) { socket.destroy(); return; }
    sockets.add(socket);
    socket.setTimeout(CONNECT_TIMEOUT_MS, () => socket.destroy());
    socket.on("error", report);
    socket.once("close", () => sockets.delete(socket));
  });
  server.on("connect", (request, socket, head) => { void acceptConnect(request, socket, head).catch((error) => { socket.destroy(); report(error); }); });
  server.on("clientError", (_error, socket) => { socket.destroy(); report(new RemotePreviewError("proxy")); });
  server.on("error", (error) => { report(error); if (ready) dispose(); });

  async function start() {
    ensureActive();
    if (ready) return;
    try {
      await new Promise((resolve, reject) => {
        server.once("error", reject);
        server.listen(0, "127.0.0.1", () => { server.removeListener("error", reject); resolve(); });
      });
      ensureActive();
      proxyPort = server.address().port;
      session.setCertificateVerifyProc(verifyCertificate);
      session.webRequest.onBeforeRequest((details, callback) => {
        void allowRequest(details).then((allowed) => callback({ cancel: !allowed }), (error) => {
          report(error); callback({ cancel: true });
        });
      });
      // PAC cannot disable Chromium's implicit localhost bypass. Manual rules
      // can: https://chromium.googlesource.com/chromium/src/+/HEAD/net/docs/proxy.md#Overriding-the-implicit-bypass-rules
      await session.setProxy({ mode: "fixed_servers", proxyRules: `http://127.0.0.1:${proxyPort}`,
        proxyBypassRules: `<-loopback>,${runner.hostname}:${runner.port || "443"}` });
      await session.closeAllConnections();
      if (await session.resolveProxy("https://veld-proxy-check.localhost") !== `PROXY 127.0.0.1:${proxyPort}` ||
          await session.resolveProxy("wss://veld-proxy-check.localhost/socket") !== `PROXY 127.0.0.1:${proxyPort}` ||
          await session.resolveProxy(endpoint) !== "DIRECT") fail("proxy");
      await refresh();
      ensureActive();
      ready = true;
    } catch (error) { dispose(); throw error instanceof RemotePreviewError ? error : new RemotePreviewError("proxy"); }
  }

  function proxyCredentials(authInfo) {
    if (!disposed && ready && authInfo?.isProxy && authInfo.host === "127.0.0.1" &&
        authInfo.port === proxyPort && authInfo.scheme === "basic" && authInfo.realm === PROXY_REALM) return [PROXY_USER, proxyPassword];
    return null;
  }

  /** Called only by the window manager, never by renderer IPC. */
  function registerPreviewContents(id) {
    ensureActive();
    if (!Number.isInteger(id) || id <= 0 || id === ideWebContentsId) fail("target");
    previewContents.add(id);
    if (!revocationTimer) {
      revocationTimer = setInterval(() => { void refresh().catch(report); }, REVOCATION_POLL_MS);
      revocationTimer.unref?.();
    }
  }

  function unregisterPreviewContents(id) {
    previewContents.delete(id);
    if (!previewContents.size) { clearInterval(revocationTimer); revocationTimer = null; }
  }

  function dispose() {
    if (disposed) { if (server.listening) server.close(); return; }
    disposed = true;
    ready = false;
    credential = "";
    clearInterval(revocationTimer);
    previewContents.clear();
    lifetime.abort();
    for (const request of requests) request.destroy();
    for (const websocket of websockets) websocket.terminate();
    for (const socket of sockets) socket.destroy();
    certificates.dispose();
    origins.clear();
    pins.clear();
    if (server.listening) server.close();
    // Retain refusing handlers and the dead proxy; DIRECT would expose the Mac.
  }

  return { start, dispose, allowRequest, verifyCertificate, proxyCredentials, registerPreviewContents, unregisterPreviewContents,
    isPreviewURL: (url) => previewOrigin(url) !== null,
    isRegisteredURL: (url) => !disposed && origins.has(previewOrigin(url)) };
}

module.exports = { createRemotePreview, createLocalCertificates, canonicalPreviewOrigin, previewOrigin, runnerRendererURL,
  connectOrigin, applicationURL, applicationHeaders, matchesApplication, certificatePin,
  bridgeWebSockets, RemotePreviewError };

#!/usr/bin/env node
"use strict";

// Run only after the parent confirms the remote-trial service uses its rebuilt
// daemon: node tests/remote/preview_trial.cjs --binary target/debug/veld
// The CLI invitation and paired credential stay in this process, never on disk.
const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const http = require("node:http");
const https = require("node:https");
const os = require("node:os");
const path = require("node:path");
const tls = require("node:tls");
const { execFile } = require("node:child_process");
const { createRequire } = require("node:module");
const { promisify } = require("node:util");
const { setTimeout: delay } = require("node:timers/promises");
const { parseInvitation, validateDescriptor, verifyDescriptorProof } = require("../../desktop/src/remoteConnections");
const { createRemotePreview, createLocalCertificates } = require("../../desktop/src/remotePreview");

const execute = promisify(execFile);
const desktopRequire = createRequire(path.resolve(__dirname, "../../desktop/package.json"));
const wsModule = desktopRequire("ws");
const TIMEOUT_MS = 15_000;
const ACTION_TIMEOUT_MS = 60_000;
const MAX_BODY_BYTES = 2 * 1024 * 1024;
const APP_AUTHORIZATION = "Bearer synthetic-preview-application";
const HTTP_COOKIES = ["trial=one; Secure; HttpOnly; Path=/", "trial-second=two; Secure; Path=/"];
const WS_COOKIE = "trial-ws=one; Secure; HttpOnly; Path=/";
const PROTOCOL = "veld-trial-v1";

function options() {
  const result = { binary: path.resolve(__dirname, "../../target/debug/veld"), name: "remote-trial" };
  for (let index = 2; index < process.argv.length; index += 2) {
    const key = process.argv[index];
    assert.ok(["--binary", "--name"].includes(key) && process.argv[index + 1], "Use --binary PATH and --name NAME");
    result[key.slice(2)] = process.argv[index + 1];
  }
  assert.match(result.name, /^[a-z0-9-]*trial$/, "Use an isolated runner name ending in trial");
  result.binary = path.resolve(result.binary);
  return result;
}

function failure(message, code) {
  return Object.assign(new Error(message), { code });
}

function check(value, message) {
  // Assertions must not format response headers: a regression could expose the
  // runner credential there, and the test must still keep its output private.
  assert.ok(value, message);
}

class Trial {
  constructor(args) {
    this.args = args;
    this.sockets = new Set();
    this.secrets = [];
    this.results = [];
    this.stage = "runner status";
    this.run = `preview-${crypto.randomBytes(6).toString("hex")}`;
    this.project = `trial-${this.run}`;
  }

  async admin(...arguments_) {
    try {
      const result = await execute(this.args.binary, ["runner", "--name", this.args.name, ...arguments_], {
        timeout: TIMEOUT_MS, maxBuffer: MAX_BODY_BYTES,
      });
      return result.stdout.trim();
    } catch {
      // Child-process errors include stdout, which can contain an invitation.
      throw failure(`Runner CLI ${arguments_[0]} failed`);
    }
  }

  passed(name) {
    this.results.push(name);
    console.log(`PASS ${name}`);
  }

  async api(method, route, body, { key, expected = 200, authenticated = true } = {}) {
    const headers = { "X-Veld-Request": "1" };
    if (authenticated) headers.Authorization = `Bearer ${this.client.credential}`;
    if (key) headers["Idempotency-Key"] = key;
    if (body !== undefined) headers["Content-Type"] = "application/json";
    const response = await fetch(`${this.endpoint}${route}`, {
      method, headers, redirect: "error", signal: AbortSignal.timeout(TIMEOUT_MS),
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    check(response.status === expected, `${method} ${route.split("?")[0]}: expected ${expected}, got ${response.status}`);
    const text = await response.text();
    check(Buffer.byteLength(text) <= MAX_BODY_BYTES, "API response exceeds trial limit");
    return text ? JSON.parse(text) : null;
  }

  async pair() {
    const status = JSON.parse(await this.admin("status"));
    const descriptor = validateDescriptor(status.descriptor);
    this.endpoint = descriptor.endpoint;
    const invitationText = await this.admin("pair");
    this.secrets.push(invitationText);
    const invitation = parseInvitation(invitationText);
    this.secrets.push(invitation.secret);
    check(JSON.stringify(invitation.descriptor) === JSON.stringify(descriptor), "CLI invitation identity differs from runner status");
    const nonce = crypto.randomBytes(32).toString("base64url");
    const proof = await this.api("GET", `/remote/descriptor?nonce=${nonce}`, undefined, { authenticated: false });
    verifyDescriptorProof(proof, invitation.descriptor, nonce);
    this.client = await this.api("POST", "/remote/pair", {
      secret: invitation.secret, label: this.project,
    }, { authenticated: false });
    this.secrets.push(this.client.credential);
    check(typeof this.client.client_id === "string" && typeof this.client.credential === "string", "Pair response is incomplete");
    this.passed("Tailscale HTTPS endpoint, CLI invitation, fresh signed identity, and pairing");
  }

  async fixture() {
    this.root = fs.mkdtempSync(path.join(os.tmpdir(), `${this.project}-`));
    fs.chmodSync(this.root, 0o700);
    const fixture = `"use strict";
const http = require("node:http");
const fs = require("node:fs");
const { WebSocketServer } = require(${JSON.stringify(desktopRequire.resolve("ws"))});
fs.appendFileSync("launches", "once\\n");
const server = http.createServer((request, response) => {
  const chunks = [];
  request.on("data", chunk => chunks.push(chunk));
  request.on("end", () => {
    response.writeHead(200, { "Content-Type": "application/json", "Set-Cookie": ${JSON.stringify(HTTP_COOKIES)} });
    response.end(JSON.stringify({ fixture: ${JSON.stringify(this.project)}, method: request.method,
      url: request.url, headers: request.headers, body: Buffer.concat(chunks).toString("utf8") }));
  });
});
const sockets = new WebSocketServer({ server, handleProtocols: protocols => protocols.has(${JSON.stringify(PROTOCOL)}) ? ${JSON.stringify(PROTOCOL)} : false });
sockets.on("headers", headers => headers.push("Set-Cookie: ${WS_COOKIE}"));
sockets.on("connection", (socket, request) => socket.on("message", (data, binary) => {
  if (binary) socket.send(data, { binary: true });
  else socket.send(JSON.stringify({ message: data.toString("utf8"), headers: request.headers }));
}));
server.listen(Number(process.argv[2]), "127.0.0.1");
`;
    fs.writeFileSync(path.join(this.root, "app.cjs"), fixture);
    const config = {
      schemaVersion: "3", name: this.project,
      url_template: `{service}.{run}.${this.project}.localhost`,
      nodes: { probe: { variants: { local: {
        type: "long_running", argv: [process.execPath, "app.cjs", "${veld.port}"],
        probes: { readiness: { type: "http", path: "/health" } },
      } } } },
    };
    fs.writeFileSync(path.join(this.root, "veld.json"), JSON.stringify(config));
    for (const arguments_ of [["init", "-b", "main"], ["add", "app.cjs", "veld.json"],
      ["-c", "commit.gpgsign=false", "-c", "user.name=Veld trial", "-c", "user.email=trial@example.invalid", "commit", "-m", "trial fixture"]]) {
      await execute("git", ["-C", this.root, ...arguments_], { timeout: TIMEOUT_MS });
    }
    this.importAttempted = true;
    await this.api("POST", "/api/repos/import", { path: this.root });
    this.startAttempted = true;
    await this.action({ kind: "start", run: this.run, selections: ["probe:local"], all_logs: false }, true);
    const query = new URLSearchParams({ project_root: this.root, name: this.run });
    const status = await this.api("GET", `/api/engine/status?${query}`);
    check(status.run.status === "running" && status.effective["probe:local"] === "healthy", "Fixture run is not running and healthy");
    const node = status.run.nodes["probe:local"];
    const endpoints = Object.values(node.endpoints || {});
    const origin = endpoints.find(endpoint => endpoint.url)?.url || node.url;
    check(typeof origin === "string", "Fixture has no allocated preview URL");
    this.origin = new URL(origin).origin;
    this.hostname = new URL(this.origin).hostname;
    this.authority = new URL(this.origin).host;
    this.connectAuthority = `${this.hostname}:${new URL(this.origin).port || "443"}`;
    const registry = await this.api("GET", "/remote/previews");
    check(registry.origins.includes(this.origin), "Preview registry omits the owned fixture");
    check(fs.readFileSync(path.join(this.root, "launches"), "utf8") === "once\n", "Idempotent start launched the fixture more than once");
    this.passed("owned long_running fixture, allocated argv port, idempotent start, running state, and preview registry");
  }

  async action(kind, duplicate = false) {
    const body = { request: { project_root: this.root, kind } };
    const key = crypto.randomUUID();
    const receipt = await this.api("POST", "/api/engine/actions", body, { key, expected: 202 });
    if (duplicate) {
      const second = await this.api("POST", "/api/engine/actions", body, { key, expected: 202 });
      check(receipt.action_id === second.action_id, "Duplicate action does not return the same receipt");
    }
    const deadline = Date.now() + ACTION_TIMEOUT_MS;
    while (Date.now() < deadline) {
      const state = await this.api("GET", `/api/engine/actions/${receipt.action_id}`);
      if (!["pending", "running"].includes(state.status)) {
        check(state.status === "succeeded", `Fixture ${kind.kind} action status: ${state.status}`);
        return;
      }
      await delay(150);
    }
    throw failure(`Fixture ${kind.kind} action timed out`, "ETIMEDOUT");
  }

  async startProxy() {
    const handlers = {};
    let proxyConfig;
    const session = {
      webRequest: { onBeforeRequest: handler => { handlers.request = handler; } },
      setCertificateVerifyProc: handler => { handlers.certificate = handler; },
      setProxy: async config => { proxyConfig = config; },
      closeAllConnections: async () => {},
      resolveProxy: async value => new URL(value).origin === this.endpoint ? "DIRECT" :
        `PROXY ${new URL(proxyConfig.proxyRules).host}`,
    };
    this.certificates = createLocalCertificates({ temporaryRoot: this.root });
    this.proxyErrors = [];
    this.preview = createRemotePreview({ session, endpoint: this.endpoint, credential: this.client.credential,
      fetch: globalThis.fetch, certificates: this.certificates, wsModule,
      ideWebContentsId: 91001, onError: code => this.proxyErrors.push(code),
    });
    await this.preview.start();
    this.proxyPort = Number(new URL(proxyConfig.proxyRules).port);
    const credentials = this.preview.proxyCredentials({ isProxy: true, host: "127.0.0.1", port: this.proxyPort,
      scheme: "basic", realm: "Veld remote preview" });
    check(Array.isArray(credentials), "Proxy did not issue Basic credentials");
    this.proxyAuthorization = `Basic ${Buffer.from(credentials.join(":")).toString("base64")}`;
    this.secrets.push(...credentials, this.proxyAuthorization);
    this.leaf = await this.certificates.get(this.hostname);
    this.preview.registerPreviewContents(91002);
    check(typeof handlers.certificate === "function" && typeof handlers.request === "function", "Electron session handlers are absent");
    const allowed = await new Promise(resolve => handlers.request({ webContentsId: 91002,
      resourceType: "mainFrame", url: `${this.origin}/` }, resolve));
    check(allowed.cancel === false, "Registered preview window is blocked");
  }

  track(socket) {
    this.sockets.add(socket);
    socket.once("close", () => this.sockets.delete(socket));
    return socket;
  }

  async connect(authority = this.connectAuthority, authorization = this.proxyAuthorization) {
    return new Promise((resolve, reject) => {
      const request = http.request({ hostname: "127.0.0.1", port: this.proxyPort, method: "CONNECT",
        path: authority, agent: false, headers: authorization ? { "Proxy-Authorization": authorization } : {} });
      const timer = setTimeout(() => request.destroy(failure("CONNECT timed out", "ETIMEDOUT")), TIMEOUT_MS);
      request.once("error", error => { clearTimeout(timer); reject(error); });
      request.once("connect", (response, socket, head) => {
        clearTimeout(timer);
        this.track(socket);
        if (head.length) socket.unshift(head);
        resolve({ status: response.statusCode, socket });
      });
      request.end();
    });
  }

  async secure(servername = this.hostname) {
    const tunnel = await this.connect();
    check(tunnel.status === 200, "Authenticated CONNECT was rejected");
    return new Promise((resolve, reject) => {
      const socket = this.track(tls.connect({ socket: tunnel.socket, servername,
        ca: this.leaf.cert, rejectUnauthorized: true, ALPNProtocols: ["http/1.1"],
        // Keep verifying the approved certificate during the wrong-SNI trial.
        // Otherwise a client-side name error could conceal a missing SNI gate.
        checkServerIdentity: (_name, certificate) => tls.checkServerIdentity(this.hostname, certificate),
      }));
      const timer = setTimeout(() => socket.destroy(failure("Local TLS timed out", "ETIMEDOUT")), TIMEOUT_MS);
      socket.once("error", error => { clearTimeout(timer); reject(error); });
      socket.once("close", () => { clearTimeout(timer); reject(failure("Local TLS closed", "ECONNRESET")); });
      socket.once("secureConnect", () => {
        clearTimeout(timer);
        check(socket.authorized, "Local TLS did not validate its fixture CA");
        resolve(socket);
      });
    });
  }

  async application({ host = this.authority } = {}) {
    const socket = await this.secure();
    const agent = new https.Agent({ keepAlive: false });
    agent.createConnection = () => socket;
    return new Promise((resolve, reject) => {
      const request = https.request({ hostname: this.hostname, port: new URL(this.origin).port || 443,
        method: "POST", path: "/echo%2Fpath?keep=one%2Ftwo&second=2", agent,
        headers: { Host: host, Authorization: APP_AUTHORIZATION, Origin: this.origin,
          Cookie: "application-cookie=synthetic", "Content-Type": "text/plain", Connection: "close",
          "X-Veld-Preview-Authorization": "Bearer forged-metadata" },
      }, response => {
        const chunks = [];
        response.on("data", chunk => chunks.push(chunk));
        response.once("error", reject);
        response.once("end", () => resolve({ status: response.statusCode, headers: response.headers,
          body: Buffer.concat(chunks).toString("utf8") }));
      });
      const timer = setTimeout(() => request.destroy(failure("Application request timed out", "ETIMEDOUT")), TIMEOUT_MS);
      request.once("close", () => { clearTimeout(timer); agent.destroy(); });
      request.once("error", reject);
      request.end("owned application body");
    });
  }

  checkHeaders(headers) {
    check(headers.authorization === APP_AUTHORIZATION, "Application Authorization was not preserved");
    check(headers.host === this.authority, "Application Host differs from approved origin");
    check(headers.origin === this.origin, "Application Origin was not preserved");
    check(headers.cookie === "application-cookie=synthetic", "Application Cookie was not preserved");
    check(!JSON.stringify(headers).includes(this.client.credential), "Runner credential reached the application");
    check(!Object.keys(headers).some(name => name.startsWith("x-veld-") || name === "proxy-authorization"), "Private proxy headers reached the application");
  }

  async websocket() {
    const socket = await this.secure();
    const agent = new https.Agent({ keepAlive: false });
    agent.createConnection = () => socket;
    const url = new URL("/socket?trial=1", this.origin);
    url.protocol = "wss:";
    let upgraded;
    const client = new wsModule.WebSocket(url, [PROTOCOL], { agent, ca: this.leaf.cert,
      rejectUnauthorized: true, perMessageDeflate: false, handshakeTimeout: TIMEOUT_MS,
      headers: { Authorization: APP_AUTHORIZATION, Origin: this.origin, Cookie: "application-cookie=synthetic" },
    });
    try {
      await new Promise((resolve, reject) => {
        let messageIndex = 0;
        const binary = Buffer.from([0, 255, 7, 128]);
        const timer = setTimeout(() => reject(failure("WebSocket echo timed out", "ETIMEDOUT")), TIMEOUT_MS);
        const finish = error => { clearTimeout(timer); error ? reject(error) : resolve(); };
        client.on("error", finish);
        client.on("upgrade", response => { upgraded = response.headers; });
        client.on("open", () => client.send("owned text echo"));
        client.on("message", (data, isBinary) => {
          try {
            if (messageIndex++ === 0) {
              const echoed = JSON.parse(data.toString("utf8"));
              check(!isBinary && echoed.message === "owned text echo", "WebSocket text echo differs");
              this.checkHeaders(echoed.headers);
              client.send(binary, { binary: true });
            } else {
              check(isBinary && data.equals(binary), "WebSocket binary message differs");
              finish();
            }
          } catch (error) { finish(error); }
        });
        client.once("close", () => finish(failure("WebSocket closed before echo completed")));
      });
      check(client.protocol === PROTOCOL, "WebSocket subprotocol was not preserved");
      check(upgraded?.["set-cookie"]?.includes(WS_COOKIE), "WebSocket Set-Cookie was not preserved");
    } finally {
      client.terminate();
      agent.destroy();
    }
  }

  async runTrial() {
    this.stage = "pairing";
    await this.pair();
    this.stage = "fixture start";
    await this.fixture();
    this.stage = "local proxy start";
    await this.startProxy();
    this.stage = "proxy authentication";
    const unauthenticated = await this.connect(this.connectAuthority, null);
    check(unauthenticated.status === 407, "Unauthenticated CONNECT was not challenged");
    unauthenticated.socket.destroy();
    this.stage = "HTTPS application forwarding";
    const response = await this.application();
    check(response.status === 200, "Application HTTPS request failed");
    const payload = JSON.parse(response.body);
    check(payload.fixture === this.project && payload.method === "POST", "Response does not come from the owned fixture");
    check(payload.body === "owned application body", "Application request body changed");
    check(payload.url === "/echo%2Fpath?keep=one%2Ftwo&second=2", "Escaped request path or query changed");
    this.checkHeaders(payload.headers);
    check(JSON.stringify(response.headers["set-cookie"]) === JSON.stringify(HTTP_COOKIES), "HTTP Set-Cookie headers changed");
    check(this.proxyErrors.length === 0, "Preview reported an error during valid forwarding");
    this.passed("real CONNECT Basic auth, validated local TLS, outer HTTPS, body, cookies, and application credential isolation");
    this.stage = "WebSocket forwarding";
    await this.websocket();
    check(this.proxyErrors.length === 0, "Preview reported an error during valid WebSocket forwarding");
    this.passed("real Node WebSocket text/binary echo, subprotocol, Set-Cookie, and application credential isolation");
    this.stage = "wrong Host rejection";
    const wrongHost = await this.application({ host: `wrong-${this.hostname}` });
    check(wrongHost.status === 502 && !wrongHost.body.includes(this.project), "Wrong Host reached the fixture");
    this.stage = "wrong SNI rejection";
    await assert.rejects(this.secure(`wrong-${this.hostname}`), error =>
      ["ECONNRESET", "ERR_SSL_SSLV3_ALERT_HANDSHAKE_FAILURE", "ERR_SSL_TLSV1_ALERT_INTERNAL_ERROR", "ERR_SSL_SSLV3_ALERT_INTERNAL_ERROR"].includes(error.code),
    "Wrong SNI must be rejected by the proxy, not by a timeout or a client name check");
    this.stage = "unregistered CONNECT rejection";
    await assert.rejects(this.connect(`unregistered-${this.hostname}:443`), error => error.code === "ECONNRESET",
      "Unregistered CONNECT must be closed");
    this.passed("wrong Host, wrong SNI, and unregistered CONNECT are rejected");
  }

  async cleanup() {
    const errors = [];
    const attempt = async (name, operation) => {
      try { await operation(); } catch { errors.push(name); }
    };
    await attempt("close proxy", async () => {
      this.preview?.dispose();
      this.certificates?.dispose();
      for (const socket of this.sockets) socket.destroy();
    });
    let stopped = !this.startAttempted;
    if (this.startAttempted) await attempt("stop owned run", async () => {
      await this.action({ kind: "stop", run: this.run });
      stopped = true;
    });
    if (this.importAttempted) await attempt("remove owned repository registration", () =>
      this.api("DELETE", "/api/repos", { root: this.root }, { expected: 204 }));
    if (this.client) await attempt("revoke owned paired client", async () => {
      await this.admin("revoke", this.client.client_id);
      await this.api("GET", "/api/health", undefined, { expected: 401 });
    });
    if (this.root && stopped) await attempt("remove owned fixture directory", async () => fs.rmSync(this.root, { recursive: true, force: true }));
    if (errors.length) throw failure(`Cleanup failed: ${errors.join(", ")}`);
    this.passed("cleanup: proxy closed, owned run stopped, repository removed, and paired client revoked");
  }

  safeMessage(error) {
    let message = error.message || "Trial failed";
    for (const secret of this.secrets.filter(Boolean)) message = message.split(secret).join("[redacted]");
    return message;
  }
}

async function main() {
  const trial = new Trial(options());
  let failed = false;
  try { await trial.runTrial(); }
  catch (error) {
    failed = true;
    console.error(`FAIL ${trial.stage}: ${trial.safeMessage(error)}`);
  } finally {
    try { await trial.cleanup(); }
    catch (error) { failed = true; console.error(`FAIL ${trial.safeMessage(error)}`); }
  }
  console.log(JSON.stringify({ transport: "tailscale-https", passed: trial.results, status: failed ? "failed" : "passed" }, null, 2));
  process.exitCode = failed ? 1 : 0;
}

main().catch(() => { console.error("FAIL trial setup; use --binary PATH and --name NAME"); process.exitCode = 1; });

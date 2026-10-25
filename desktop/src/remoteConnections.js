// Main-process only. This module deliberately has no Electron import so its
// trust decisions can be tested without opening a desktop window.
const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const { execFile } = require("node:child_process");
const { promisify } = require("node:util");

const INVITATION_PREFIX = "veld-pair-v1:";
const MAX_INVITATION_BYTES = 16_384;
const MAX_RESPONSE_BYTES = 65_536;
const MAX_STORE_BYTES = 1_048_576;
const MAX_CONNECTIONS = 100;
const REQUEST_TIMEOUT_MS = 5_000;
const DISCOVERY_LIMIT = 24;
const DISCOVERY_CONCURRENCY = 4;
const DISCOVERY_PORT = 19896;
const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");
const execFileAsync = promisify(execFile);

const MESSAGES = Object.freeze({
  invitation: "This invitation is invalid or expired. Create a new invitation on the remote machine.",
  descriptor: "This machine does not provide a compatible Veld remote identity.",
  identity: "The remote identity could not be verified. The connection was refused.",
  network: "The remote machine is unreachable. Check its address and Tailscale connection.",
  response: "The remote machine returned an invalid response.",
  encryption: "Secure credential storage is unavailable. Unlock or configure your system keychain and try again.",
  storage: "The saved connections could not be read or written securely.",
  missing: "This connection is no longer registered.",
  capacity: "The saved connection limit has been reached. Forget a connection before pairing another.",
  windows: "The remote window limit has been reached. Close a remote window and try again.",
  label: "Enter a shorter connection name, using 1 to 80 characters.",
  cancelled: "Pairing was cancelled. No connection was saved.",
  busy: "Another remote connection operation is in progress. Try again when it finishes.",
  sender: "This connection action is available only in the local connection chooser.",
  closed: "The connection chooser has closed. Open it and try again.",
});

class RemoteConnectionError extends Error {
  constructor(code) {
    super(MESSAGES[code] || MESSAGES.response);
    this.name = "RemoteConnectionError";
    this.code = code;
  }
}

function fail(code) {
  throw new RemoteConnectionError(code);
}

function boundedText(value, max) {
  return typeof value === "string" && value.length > 0 && value.length <= max &&
    !/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/u.test(value);
}

function decodeBase64url(value, bytes, code) {
  if (typeof value !== "string" || !/^[A-Za-z0-9_-]+$/.test(value)) fail(code);
  const decoded = Buffer.from(value, "base64url");
  if (decoded.toString("base64url") !== value || (bytes !== null && decoded.length !== bytes)) fail(code);
  return decoded;
}

/** Accept only the canonical HTTPS origin, without even a trailing slash. */
function canonicalEndpoint(value) {
  if (typeof value !== "string" || value.length > 512) fail("descriptor");
  let url;
  try {
    url = new URL(value);
  } catch {
    fail("descriptor");
  }
  if (url.protocol !== "https:" || url.username || url.password || url.search || url.hash ||
      url.pathname !== "/" || url.port === "0" || url.origin !== value ||
      url.hostname.length > 253 || !url.hostname.endsWith(".ts.net") ||
      !url.hostname.split(".").every((label) => /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(label))) {
    fail("descriptor");
  }
  return url.origin;
}

/** Return a fresh object containing only the protocol's public descriptor fields. */
function validateDescriptor(value) {
  if (!value || value.protocol !== 1 || !boundedText(value.instance_id, 128) ||
      !boundedText(value.version, 128)) fail("descriptor");
  decodeBase64url(value.public_key, 32, "descriptor");
  return {
    protocol: 1,
    instance_id: value.instance_id,
    public_key: value.public_key,
    version: value.version,
    endpoint: canonicalEndpoint(value.endpoint),
  };
}

function sameIdentity(left, right) {
  return left.protocol === right.protocol && left.instance_id === right.instance_id &&
    left.public_key === right.public_key && left.endpoint === right.endpoint;
}

/** Parse untrusted clipboard text; invitation contents never enter an error. */
function parseInvitation(value, now = Date.now()) {
  try {
    if (typeof value !== "string" || value.length > MAX_INVITATION_BYTES) fail("invitation");
    const text = value.trim();
    if (!text.startsWith(INVITATION_PREFIX)) fail("invitation");
    const data = JSON.parse(decodeBase64url(text.slice(INVITATION_PREFIX.length), null, "invitation").toString("utf8"));
    const descriptor = validateDescriptor(data.descriptor);
    if (!boundedText(data.secret, 4096) || !Number.isSafeInteger(data.expires_at) ||
        data.expires_at <= Math.floor(now / 1000)) fail("invitation");
    return { descriptor, secret: data.secret, expires_at: data.expires_at };
  } catch {
    fail("invitation");
  }
}

/** Verify a fresh challenge against the invitation/stored key, never a response-selected key. */
function verifyDescriptorProof(proof, pinned, nonce) {
  try {
    const expected = validateDescriptor(pinned);
    const actual = validateDescriptor(proof?.descriptor);
    decodeBase64url(nonce, 32, "identity");
    if (proof.nonce !== nonce || !sameIdentity(expected, actual)) fail("identity");
    const publicKey = crypto.createPublicKey({
      key: Buffer.concat([ED25519_SPKI_PREFIX, decodeBase64url(expected.public_key, 32, "identity")]),
      format: "der",
      type: "spki",
    });
    const message = Buffer.from(`veld-remote-v1\n${expected.instance_id}\n${expected.endpoint}\n${nonce}`, "utf8");
    if (!crypto.verify(null, message, publicKey, decodeBase64url(proof.signature, 64, "identity"))) fail("identity");
    return actual;
  } catch {
    fail("identity");
  }
}

function validateLabel(value) {
  if (!boundedText(value, 80) || value.trim() !== value || Buffer.byteLength(value) > 128) fail("label");
  return value;
}

function validateCredential(value) {
  if (typeof value !== "string" || !/^[\x21-\x7e]{1,4096}$/.test(value)) fail("response");
  return value;
}

function publicConnection(record) {
  return { id: record.id, label: record.label, client_id: record.client_id, descriptor: { ...record.descriptor } };
}

/**
 * One store per app process. Atomic synchronous writes serialize mutations;
 * only registered connections persist, never invitations or renderer state.
 */
function createConnectionStore({ userData, safeStorage, platform = process.platform }) {
  const directory = path.join(userData, "remote-connections");
  const filename = path.join(directory, "connections.json");

  function assertSecure() {
    try {
      if (!safeStorage?.isEncryptionAvailable()) fail("encryption");
      if (platform === "linux" && !["gnome_libsecret", "kwallet", "kwallet5", "kwallet6"].includes(
        safeStorage.getSelectedStorageBackend?.(),
      )) fail("encryption");
    } catch {
      fail("encryption");
    }
    // POSIX ownership/mode checks do not establish a Windows ACL guarantee.
    if (typeof process.getuid !== "function") fail("storage");
  }

  function ownerOnly(stat, mode, directoryEntry = false) {
    if (stat.uid !== process.getuid() || (stat.mode & 0o777) !== mode ||
        (directoryEntry ? !stat.isDirectory() : !stat.isFile() || stat.nlink !== 1)) fail("storage");
  }

  function prepareDirectory() {
    fs.mkdirSync(directory, { mode: 0o700, recursive: true });
    ownerOnly(fs.lstatSync(directory), 0o700, true);
  }

  function read() {
    assertSecure();
    try {
      prepareDirectory();
      let fd;
      try {
        fd = fs.openSync(filename, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
      } catch (error) {
        if (error.code === "ENOENT") return [];
        throw error;
      }
      let raw;
      try {
        const stat = fs.fstatSync(fd);
        ownerOnly(stat, 0o600);
        if (stat.size > MAX_STORE_BYTES) fail("storage");
        raw = JSON.parse(fs.readFileSync(fd, "utf8"));
      } finally {
        fs.closeSync(fd);
      }
      if (raw.version !== 1 || !Array.isArray(raw.connections) || raw.connections.length > MAX_CONNECTIONS) fail("storage");
      const ids = new Set();
      return raw.connections.map((record) => {
        if (!/^[a-f0-9-]{36}$/.test(record.id) || ids.has(record.id) ||
            !boundedText(record.client_id, 128) || !boundedText(record.encryptedCredential, 32_768)) fail("storage");
        ids.add(record.id);
        return { ...publicConnection(record), label: validateLabel(record.label),
          descriptor: validateDescriptor(record.descriptor), encryptedCredential: record.encryptedCredential };
      });
    } catch {
      fail("storage");
    }
  }

  function write(records) {
    assertSecure();
    const temporary = path.join(directory, `${crypto.randomUUID()}.tmp`);
    let fd;
    try {
      prepareDirectory();
      const data = JSON.stringify({ version: 1, connections: records });
      if (Buffer.byteLength(data) > MAX_STORE_BYTES) fail("storage");
      fd = fs.openSync(temporary, fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_NOFOLLOW, 0o600);
      fs.fchmodSync(fd, 0o600);
      fs.writeFileSync(fd, data);
      fs.fsyncSync(fd);
      fs.closeSync(fd);
      fd = undefined;
      fs.renameSync(temporary, filename);
      const dirFd = fs.openSync(directory, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
      try { fs.fsyncSync(dirFd); } finally { fs.closeSync(dirFd); }
    } catch {
      fail("storage");
    } finally {
      if (fd !== undefined) fs.closeSync(fd);
      if (fs.existsSync(temporary)) fs.unlinkSync(temporary);
    }
  }

  return {
    assertSecure,
    list: () => read().map(publicConnection),
    get(id) {
      const record = read().find((entry) => entry.id === id);
      if (!record) fail("missing");
      return publicConnection(record);
    },
    credential(id) {
      const record = read().find((entry) => entry.id === id);
      if (!record) fail("missing");
      try {
        const sealed = JSON.parse(safeStorage.decryptString(Buffer.from(record.encryptedCredential, "base64")));
        // Bind ciphertext to its destination so edited plaintext metadata cannot
        // redirect a saved credential to a different server or registration.
        if (JSON.stringify(sealed.connection) !== JSON.stringify(publicConnection(record))) fail("storage");
        return validateCredential(sealed.credential);
      } catch {
        fail("storage");
      }
    },
    save({ label, descriptor, client_id, credential }) {
      const records = read();
      if (records.length >= MAX_CONNECTIONS) fail("capacity");
      if (!boundedText(client_id, 128)) fail("response");
      const connection = { id: crypto.randomUUID(), label: validateLabel(label), client_id,
        descriptor: validateDescriptor(descriptor) };
      validateCredential(credential);
      let encryptedCredential;
      try {
        encryptedCredential = safeStorage.encryptString(JSON.stringify({ connection, credential })).toString("base64");
      } catch {
        fail("encryption");
      }
      write([...records, { ...connection, encryptedCredential }]);
      return connection;
    },
    forget(id) {
      const records = read();
      if (!records.some((entry) => entry.id === id)) fail("missing");
      write(records.filter((entry) => entry.id !== id));
    },
  };
}

async function readResponse(response) {
  if (!response.ok || response.redirected) fail("response");
  if (Number(response.headers.get("content-length")) > MAX_RESPONSE_BYTES) fail("response");
  const reader = response.body?.getReader();
  if (!reader) fail("response");
  const chunks = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > MAX_RESPONSE_BYTES) {
      await reader.cancel();
      fail("response");
    }
    chunks.push(Buffer.from(value));
  }
  try { return JSON.parse(Buffer.concat(chunks).toString("utf8")); } catch { fail("response"); }
}

/** All main-process network calls use an Electron session.fetch supplied by the manager. */
function createRemoteConnections({ store, fetch, now = Date.now, randomBytes = crypto.randomBytes,
  tailscaleStatus = () => execFileAsync("tailscale", ["status", "--json"], {
    timeout: REQUEST_TIMEOUT_MS, maxBuffer: MAX_STORE_BYTES, windowsHide: true,
  }), timeoutMs = REQUEST_TIMEOUT_MS }) {
  async function request(endpoint, pathname, options = {}) {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), timeoutMs);
    try {
      const response = await fetch(`${canonicalEndpoint(endpoint)}${pathname}`, {
        ...options, redirect: "error", signal: controller.signal, credentials: "omit", cache: "no-store",
      });
      return await readResponse(response);
    } catch (error) {
      if (error instanceof RemoteConnectionError) throw error;
      fail("network");
    } finally {
      clearTimeout(timeout);
      controller.abort();
    }
  }

  async function proof(descriptor) {
    const nonce = randomBytes(32).toString("base64url");
    const response = await request(descriptor.endpoint, `/remote/descriptor?nonce=${nonce}`);
    return verifyDescriptorProof(response, descriptor, nonce);
  }

  return {
    list: () => store.list(),
    forget: (id) => store.forget(id),
    async pair(invitation, label, confirm) {
      const parsed = parseInvitation(invitation, now());
      validateLabel(label);
      store.assertSecure();
      if (store.list().length >= MAX_CONNECTIONS) fail("capacity");
      if (!(await confirm({ label, descriptor: { ...parsed.descriptor } }))) fail("cancelled");
      await proof(parsed.descriptor);
      if (parsed.expires_at <= Math.floor(now() / 1000)) fail("invitation");
      const result = await request(parsed.descriptor.endpoint, "/remote/pair", {
        method: "POST",
        headers: { "Content-Type": "application/json", "X-Veld-Request": "1" },
        body: JSON.stringify({ secret: parsed.secret, label }),
      });
      const descriptor = validateDescriptor(result.descriptor);
      if (!sameIdentity(parsed.descriptor, descriptor)) fail("identity");
      return store.save({ label, descriptor, client_id: result.client_id, credential: result.credential });
    },
    async connect(id) {
      const connection = store.get(id);
      await proof(connection.descriptor);
      return { ...connection, credential: store.credential(id) };
    },
    async discover() {
      let status;
      try {
        const output = await tailscaleStatus();
        status = JSON.parse(output.stdout);
      } catch {
        return { status: "unavailable", candidates: [], truncated: false };
      }
      if (status?.BackendState !== "Running" || !status.Peer || typeof status.Peer !== "object") {
        return { status: "unavailable", candidates: [], truncated: false };
      }
      const endpoints = new Set();
      for (const peer of Object.values(status.Peer)) {
        if (typeof peer?.DNSName !== "string") continue;
        const endpoint = `https://${peer.DNSName.replace(/\.$/, "")}:${DISCOVERY_PORT}`;
        try { endpoints.add(canonicalEndpoint(endpoint)); } catch { /* Non-tailnet peers are not probe targets. */ }
      }
      const candidates = [...endpoints].slice(0, DISCOVERY_LIMIT);
      const results = new Array(candidates.length);
      let next = 0;
      await Promise.all(Array.from({ length: DISCOVERY_CONCURRENCY }, async () => {
        while (next < candidates.length) {
          const index = next++;
          const endpoint = candidates[index];
          try {
            const nonce = randomBytes(32).toString("base64url");
            const response = await request(endpoint, `/remote/descriptor?nonce=${nonce}`);
            const descriptor = validateDescriptor(response.descriptor);
            if (descriptor.endpoint !== endpoint) fail("identity");
            verifyDescriptorProof(response, descriptor, nonce);
            results[index] = { endpoint, status: "available", descriptor };
          } catch (error) {
            results[index] = { endpoint, status: error.code === "network" ? "unreachable" : "incompatible" };
          }
        }
      }));
      return { status: "ready", candidates: results, truncated: endpoints.size > DISCOVERY_LIMIT };
    },
  };
}

module.exports = {
  RemoteConnectionError, canonicalEndpoint, validateDescriptor, parseInvitation,
  verifyDescriptorProof, createConnectionStore, createRemoteConnections,
};

const test = require("node:test");
const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const {
  parseInvitation, canonicalEndpoint, verifyDescriptorProof,
  createConnectionStore, createRemoteConnections,
} = require("./remoteConnections");

const ENDPOINT = "https://builder.example.ts.net:19896";
const NOW = 1_800_000_000_000;
const keys = crypto.generateKeyPairSync("ed25519");
const descriptor = {
  protocol: 1, instance_id: "test-machine", version: "1.0.0",
  public_key: keys.publicKey.export({ format: "der", type: "spki" }).subarray(-32).toString("base64url"),
  endpoint: ENDPOINT,
};
const invitationData = { descriptor, secret: "synthetic-invitation-secret", expires_at: NOW / 1000 + 60 };
const invitation = (data = invitationData) => `veld-pair-v1:${Buffer.from(JSON.stringify(data)).toString("base64url")}`;
const hasCode = (code) => (error) => error.code === code;
function signed(nonce, machine = descriptor, privateKey = keys.privateKey) {
  const bytes = Buffer.from(`veld-remote-v1\n${machine.instance_id}\n${machine.endpoint}\n${nonce}`);
  return { descriptor: machine, nonce, signature: crypto.sign(null, bytes, privateKey).toString("base64url") };
}
function response(value, options) { return Response.json(value, options); }

function fixture(t) {
  const userData = fs.mkdtempSync(path.join(os.tmpdir(), "veld-remote-test-"));
  t.after(() => fs.rmSync(userData, { recursive: true, force: true }));
  // An authenticated cipher test double proves that persistence passes only
  // ciphertext to disk. OS keychain behavior needs a separate desktop test.
  const key = crypto.randomBytes(32);
  const safeStorage = {
    isEncryptionAvailable: () => true,
    getSelectedStorageBackend: () => "gnome_libsecret",
    encryptString(value) {
      const nonce = crypto.randomBytes(12);
      const cipher = crypto.createCipheriv("aes-256-gcm", key, nonce);
      const encrypted = Buffer.concat([cipher.update(value, "utf8"), cipher.final()]);
      return Buffer.concat([nonce, cipher.getAuthTag(), encrypted]);
    },
    decryptString(value) {
      const decipher = crypto.createDecipheriv("aes-256-gcm", key, value.subarray(0, 12));
      decipher.setAuthTag(value.subarray(12, 28));
      return Buffer.concat([decipher.update(value.subarray(28)), decipher.final()]).toString("utf8");
    },
  };
  const store = createConnectionStore({ userData, safeStorage, platform: "linux" });
  return { userData, safeStorage, store, filename: path.join(userData, "remote-connections/connections.json") };
}
function registration(overrides = {}) {
  return { label: "Build machine", descriptor, client_id: "test-client", credential: "synthetic-saved-credential", ...overrides };
}

test("invitation accepts protocol v1 and requires an unexpired UNIX-seconds deadline", () => {
  assert.deepEqual(parseInvitation(invitation(), NOW), invitationData);
  for (const expires_at of [NOW / 1000, NOW / 1000 - 1, "1800000060", 1.5, null]) {
    assert.throws(() => parseInvitation(invitation({ ...invitationData, expires_at }), NOW), hasCode("invitation"));
  }
  for (const value of ["", "https://example.test", invitation() + "=", "veld-pair-v1:!!!", "x".repeat(16_385)]) {
    assert.throws(() => parseInvitation(value, NOW), hasCode("invitation"));
  }
});

test("endpoint rejects noncanonical origins, credentials, paths, queries and fragments", () => {
  assert.equal(canonicalEndpoint(ENDPOINT), ENDPOINT);
  assert.equal(canonicalEndpoint("https://machine.ts.net"), "https://machine.ts.net");
  for (const endpoint of [
    "http://builder.example.ts.net:19896", "https://builder.example.ts.net/", `${ENDPOINT}/ide`,
    `${ENDPOINT}?secret=x`, `${ENDPOINT}#x`, "https://user:pass@builder.example.ts.net",
    "https://builder.example.ts.net:443", "https://builder.example.ts.net:0", "https://BUILDER.example.ts.net", "https://builder.example.ts.net.",
    "https://builder.example.ts.net.evil.test", "https://127.0.0.1", "https://ts.net", "https://-bad.ts.net",
    "https://builder.example.ts.net/../", "https://builder.example.ts.net?", "https://builder.example.ts.net#",
  ]) assert.throws(() => canonicalEndpoint(endpoint), hasCode("descriptor"), endpoint);
});

test("proof binds the exact fresh nonce, pinned key, instance, protocol and origin", () => {
  const nonce = crypto.randomBytes(32).toString("base64url");
  assert.deepEqual(verifyDescriptorProof(signed(nonce), descriptor, nonce), descriptor);
  const anotherNonce = crypto.randomBytes(32).toString("base64url");
  assert.throws(() => verifyDescriptorProof(signed(nonce), descriptor, anotherNonce), hasCode("identity"));
  const wrongKeys = crypto.generateKeyPairSync("ed25519");
  assert.throws(() => verifyDescriptorProof(signed(nonce, descriptor, wrongKeys.privateKey), descriptor, nonce), hasCode("identity"));
  for (const change of [
    { instance_id: "other-machine" }, { endpoint: "https://other.example.ts.net:19896" },
    { public_key: crypto.randomBytes(32).toString("base64url") }, { protocol: 2 },
  ]) {
    assert.throws(() => verifyDescriptorProof(signed(nonce, { ...descriptor, ...change }), descriptor, nonce), hasCode("identity"));
  }
  assert.throws(() => verifyDescriptorProof({ ...signed(nonce), signature: "broken" }, descriptor, nonce), hasCode("identity"));
  const upgraded = { ...descriptor, version: "2.0.0" };
  assert.deepEqual(verifyDescriptorProof(signed(nonce, upgraded), descriptor, nonce), upgraded);
});

test("store persists ciphertext atomically with owner-only directory and file modes", (t) => {
  const { userData, safeStorage, store, filename } = fixture(t);
  const entry = store.save(registration());
  const text = fs.readFileSync(filename, "utf8");
  assert.ok(!text.includes("synthetic-saved-credential"));
  assert.ok(!text.includes("synthetic-invitation-secret"));
  assert.equal(fs.statSync(filename).mode & 0o777, 0o600);
  assert.equal(fs.statSync(path.dirname(filename)).mode & 0o777, 0o700);
  assert.deepEqual(fs.readdirSync(path.dirname(filename)), ["connections.json"]);
  const reloaded = createConnectionStore({ userData, safeStorage, platform: "linux" });
  assert.deepEqual(reloaded.list(), [entry]);
  assert.equal(reloaded.credential(entry.id), "synthetic-saved-credential");
  assert.ok(!("credential" in reloaded.list()[0]));
  assert.ok(!("encryptedCredential" in reloaded.list()[0]));
  reloaded.forget(entry.id);
  assert.deepEqual(reloaded.list(), []);
});

test("store rejects unavailable encryption and Linux basic_text before saving", (t) => {
  const { store, safeStorage } = fixture(t);
  safeStorage.getSelectedStorageBackend = () => "basic_text";
  assert.throws(() => store.save(registration()), hasCode("encryption"));
  safeStorage.getSelectedStorageBackend = () => "unknown";
  assert.throws(() => store.list(), hasCode("encryption"));
  safeStorage.getSelectedStorageBackend = () => "gnome_libsecret";
  safeStorage.isEncryptionAvailable = () => false;
  assert.throws(() => store.list(), hasCode("encryption"));
});

test("store refuses insecure file permissions, symlinks, corruption and altered destination metadata", (t) => {
  const { store, filename } = fixture(t);
  const entry = store.save(registration());
  const original = fs.readFileSync(filename, "utf8");
  fs.chmodSync(filename, 0o644);
  assert.throws(() => store.list(), hasCode("storage"));
  fs.chmodSync(filename, 0o600);
  const raw = JSON.parse(original);
  raw.connections[0].descriptor.endpoint = "https://other.example.ts.net:19896";
  fs.writeFileSync(filename, JSON.stringify(raw));
  assert.throws(() => store.credential(entry.id), hasCode("storage"));
  fs.writeFileSync(filename, "broken-json");
  assert.throws(() => store.list(), hasCode("storage"));
  fs.renameSync(filename, `${filename}.real`);
  fs.symlinkSync(`${filename}.real`, filename);
  assert.throws(() => store.list(), hasCode("storage"));
});

test("failed encryption leaves the previous atomic registration intact", (t) => {
  const { store, safeStorage, filename } = fixture(t);
  const entry = store.save(registration());
  const before = fs.readFileSync(filename, "utf8");
  safeStorage.encryptString = () => { throw new Error("synthetic secret must not escape"); };
  assert.throws(() => store.save(registration()), hasCode("encryption"));
  assert.equal(fs.readFileSync(filename, "utf8"), before);
  assert.deepEqual(store.list(), [entry]);
});

test("pairing confirms before network and verifies proof before disclosing invitation secret", async (t) => {
  const { store } = fixture(t);
  const calls = [];
  let confirmed = false;
  const service = createRemoteConnections({ store, now: () => NOW, fetch: async (url, options) => {
    assert.ok(confirmed);
    assert.equal(options.redirect, "error");
    assert.equal(options.credentials, "omit");
    assert.ok(options.signal);
    assert.ok(!url.includes("synthetic"));
    calls.push({ url, options });
    if (url.includes("/remote/descriptor?")) {
      assert.equal(options.body, undefined);
      return response(signed(new URL(url).searchParams.get("nonce")));
    }
    assert.equal(calls.length, 2);
    assert.equal(options.headers["X-Veld-Request"], "1");
    assert.deepEqual(JSON.parse(options.body), { secret: invitationData.secret, label: "Build machine" });
    return response(registration());
  } });
  const connection = await service.pair(invitation(), "Build machine", async (info) => {
    assert.ok(!("secret" in info));
    assert.deepEqual(info.descriptor, descriptor);
    confirmed = true;
    return true;
  });
  assert.equal(calls.length, 2);
  assert.deepEqual(store.list(), [connection]);
});

test("pairing cancellation, invalid proof and mismatched pair response never save", async (t) => {
  const { store } = fixture(t);
  let calls = 0;
  let mode = "invalid-proof";
  const service = createRemoteConnections({ store, now: () => NOW, fetch: async (url) => {
    calls++;
    if (url.includes("/remote/descriptor?")) {
      const nonce = new URL(url).searchParams.get("nonce");
      return response(mode === "invalid-proof" ? signed(crypto.randomBytes(32).toString("base64url")) : signed(nonce));
    }
    return response(registration({ descriptor: { ...descriptor, instance_id: "wrong-machine" } }));
  } });
  await assert.rejects(service.pair(invitation(), "Build machine", async () => false), hasCode("cancelled"));
  assert.equal(calls, 0);
  await assert.rejects(service.pair(invitation(), "Build machine", async () => true), hasCode("identity"));
  assert.equal(calls, 1);
  mode = "mismatched-response";
  await assert.rejects(service.pair(invitation(), "Build machine", async () => true), hasCode("identity"));
  assert.deepEqual(store.list(), []);
});

test("pairing rechecks expiry after the confirmation and proof round trip", async (t) => {
  const { store } = fixture(t);
  let current = NOW;
  let calls = 0;
  const service = createRemoteConnections({ store, now: () => current, fetch: async (url) => {
    calls++;
    current += 61_000;
    return response(signed(new URL(url).searchParams.get("nonce")));
  } });
  await assert.rejects(service.pair(invitation(), "Build machine", async () => true), hasCode("invitation"));
  assert.equal(calls, 1);
});

test("reconnect verifies a fresh proof before decrypting a saved credential", async (t) => {
  const { store } = fixture(t);
  const entry = store.save(registration());
  const nonces = [];
  let decryptions = 0;
  const original = store.credential;
  store.credential = (id) => { decryptions++; return original(id); };
  let replay;
  const service = createRemoteConnections({ store, fetch: async (url) => {
    const nonce = new URL(url).searchParams.get("nonce");
    nonces.push(nonce);
    if (!replay) replay = signed(nonce);
    return response(replay);
  } });
  assert.equal((await service.connect(entry.id)).credential, "synthetic-saved-credential");
  await assert.rejects(service.connect(entry.id), hasCode("identity"));
  assert.equal(decryptions, 1);
  assert.notEqual(nonces[0], nonces[1]);
});

test("redirects, oversized responses and timeouts fail without credential decryption", async (t) => {
  const { store } = fixture(t);
  const entry = store.save(registration());
  store.credential = () => assert.fail("must not decrypt");
  for (const fetch of [
    async () => response({}, { status: 302, headers: { Location: "https://other.example.ts.net" } }),
    async () => response({ padding: "x".repeat(65_536) }),
    async (_url, { signal }) => new Promise((_resolve, reject) => signal.addEventListener("abort", () => reject(new Error("timeout")))),
  ]) {
    const service = createRemoteConnections({ store, fetch, timeoutMs: 10 });
    await assert.rejects(service.connect(entry.id));
  }
});

test("discovery is bounded, unauthenticated and distinguishes unreachable and incompatible candidates", async () => {
  const peers = Object.fromEntries(Array.from({ length: 30 }, (_, index) => [index, { DNSName: `peer${index}.example.ts.net.` }]));
  peers.bad = { DNSName: "example.com" };
  let calls = 0;
  let active = 0;
  let maxActive = 0;
  const service = createRemoteConnections({
    store: {}, tailscaleStatus: async () => ({ stdout: JSON.stringify({ BackendState: "Running", Peer: peers }) }),
    fetch: async (url, options) => {
      calls++;
      assert.equal(options.headers, undefined);
      assert.equal(new URL(url).port, "19896");
      active++;
      maxActive = Math.max(maxActive, active);
      await new Promise((resolve) => setImmediate(resolve));
      active--;
      if (url.includes("peer0.")) throw new Error("unreachable");
      if (url.includes("peer1.")) return response({ descriptor: { protocol: 2 } });
      return response(signed(new URL(url).searchParams.get("nonce"), { ...descriptor, endpoint: new URL(url).origin }));
    },
  });
  const result = await service.discover();
  assert.equal(calls, 24);
  assert.equal(result.truncated, true);
  assert.equal(maxActive, 4);
  assert.equal(result.candidates[0].status, "unreachable");
  assert.equal(result.candidates[1].status, "incompatible");
  assert.equal(result.candidates[2].status, "available");
});

test("unavailable Tailscale leaves manual pairing independent", async (t) => {
  const { store } = fixture(t);
  const service = createRemoteConnections({ store, now: () => NOW,
    tailscaleStatus: async () => { throw new Error("not installed"); },
    fetch: async (url) => response(url.includes("/remote/descriptor?")
      ? signed(new URL(url).searchParams.get("nonce")) : registration()),
  });
  assert.deepEqual(await service.discover(), { status: "unavailable", candidates: [], truncated: false });
  const entry = await service.pair(invitation(), "Build machine", async () => true);
  assert.deepEqual(service.list(), [entry]);
});

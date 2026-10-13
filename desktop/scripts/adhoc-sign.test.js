const test = require("node:test");
const assert = require("node:assert/strict");
const { packagerWillSign } = require("./adhoc-sign.js");

// The ad-hoc pass must run for every build that keeps electron-builder.yml's
// `identity: null` — local and pull-request builds — and stand aside only when
// release CI overrides it with `-c.mac.identity=<team id>`.
test("identity: null (the config file) means ad-hoc", () => {
  assert.equal(packagerWillSign({ identity: null }), false);
});

test("an identity from the command line means electron-builder signs", () => {
  assert.equal(packagerWillSign({ identity: "ABCDE12345" }), true);
});

// Absent is auto-discovery, which signs with whatever the keychain holds — the
// ad-hoc pass would be discarded, so it stands aside rather than guessing.
test("no identity key at all is auto-discovery, not ad-hoc", () => {
  assert.equal(packagerWillSign({}), true);
});

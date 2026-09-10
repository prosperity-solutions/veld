const assert = require("node:assert/strict");
const test = require("node:test");

const { baseBackground } = require("./baseBackground.js");

const DARK = "#0d0d0f";

test("baseBackground paints the app's surface until a page has committed", () => {
  // The no-flash rule this file has always had: an empty view is an empty
  // rectangle, and Electron's default white is a flash in a dark app.
  assert.equal(baseBackground({ surface: DARK, frameReady: false }), DARK);
});

test("baseBackground paints white once a page has committed", () => {
  // Not the surface, and this is the whole point of the two branches: a document
  // that declares no background is transparent, so this colour is what shows
  // behind the text — and the UA stylesheet makes that text black. The surface
  // here gave a plain no-CSS page black-on-near-black.
  assert.equal(baseBackground({ surface: DARK, frameReady: true }), "#ffffff");
});

test("baseBackground answers on the flag alone, for one unchanging surface", () => {
  // The two branches above with everything else held still, which is what makes
  // them branches and not two readings of `surface`. A regression that dropped
  // `frameReady` and returned `surface` unconditionally passes each test above in
  // isolation; it cannot pass this one.
  const light = baseBackground({ surface: DARK, frameReady: true });
  const dark = baseBackground({ surface: DARK, frameReady: false });
  assert.notEqual(light, dark);
});

test("baseBackground is why a light app never showed the bug", () => {
  // Recorded because it is the first thing a reader wonders. In a light app the
  // surface *is* white, so both branches agree and the unreadable-text bug was
  // invisible — it only ever reproduced on a dark theme. Nothing here changes
  // for a light app, which is the blast radius of this whole change.
  assert.equal(baseBackground({ surface: "#ffffff", frameReady: true }), "#ffffff");
  assert.equal(baseBackground({ surface: "#ffffff", frameReady: false }), "#ffffff");
});

const assert = require("node:assert/strict");
const test = require("node:test");

const { safeAreaPayload } = require("./safeArea.js");

test("safeAreaPayload sends all eight fields with each Max mirroring its inset", () => {
  // The whole set, every time: CDP replaces rather than merges, so a subset is a
  // silent reset of whatever it omits. And the `Max` variables must be set, or a
  // page reads a maximum of 0 under an inset of 59 — a state no device can be in.
  assert.deepEqual(safeAreaPayload({ top: 59, right: 0, bottom: 34, left: 0 }), {
    top: 59,
    topMax: 59,
    right: 0,
    rightMax: 0,
    bottom: 34,
    bottomMax: 34,
    left: 0,
    leftMax: 0,
  });
  // Landscape, where the sides carry the sensor housing.
  assert.deepEqual(safeAreaPayload({ top: 0, right: 59, bottom: 21, left: 59 }), {
    top: 0,
    topMax: 0,
    right: 59,
    rightMax: 59,
    bottom: 21,
    bottomMax: 21,
    left: 59,
    leftMax: 59,
  });
  // Eight keys, not four, and not nine.
  assert.equal(Object.keys(safeAreaPayload({ top: 1, right: 2, bottom: 3, left: 4 })).length, 8);
  for (const side of ["top", "right", "bottom", "left"]) {
    const p = safeAreaPayload({ top: 1, right: 2, bottom: 3, left: 4 });
    assert.equal(p[`${side}Max`], p[side], `${side}Max must mirror ${side}`);
  }
});

test("safeAreaPayload's off value is an empty set, which is the protocol's own reset", () => {
  // Not an omitted `insets` — CDP rejects the command outright without it
  // (measured: "Invalid parameters"), which would throw instead of clearing.
  assert.deepEqual(safeAreaPayload(null), {});
});

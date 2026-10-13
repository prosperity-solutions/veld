import { describe, expect, it } from "vitest";
import { deepEqual, keepIfEqual } from "./keepIfEqual";

describe("keepIfEqual", () => {
  it("returns prev when next is a structurally equal copy", () => {
    const prev = { a: 1, list: [{ id: 1, tags: ["x", "y"] }], nested: { ok: true, n: null } };
    const next = JSON.parse(JSON.stringify(prev));
    expect(next).not.toBe(prev);
    expect(keepIfEqual(prev, next)).toBe(prev);
  });

  it("returns next when anything differs, however deep", () => {
    const prev = { list: [{ id: 1, tags: ["x", "y"] }] };
    const next = { list: [{ id: 1, tags: ["x", "z"] }] };
    expect(keepIfEqual(prev, next)).toBe(next);
  });

  it("handles null on either side", () => {
    const next = { a: 1 };
    expect(keepIfEqual<typeof next | null>(null, next)).toBe(next);
    expect(keepIfEqual<typeof next | null>(next, null)).toBeNull();
    expect(keepIfEqual<null>(null, null)).toBeNull();
  });

  it("returns prev for equal primitives", () => {
    expect(keepIfEqual(3, 3)).toBe(3);
    expect(keepIfEqual("a", "b")).toBe("b");
  });
});

describe("deepEqual", () => {
  it("is order-sensitive for arrays and order-insensitive for keys", () => {
    expect(deepEqual([1, 2], [2, 1])).toBe(false);
    expect(deepEqual({ a: 1, b: 2 }, { b: 2, a: 1 })).toBe(true);
  });

  it("tells length and key-count differences apart", () => {
    expect(deepEqual([1], [1, 2])).toBe(false);
    expect(deepEqual({ a: 1 }, { a: 1, b: 2 })).toBe(false);
    expect(deepEqual({ a: 1, b: 2 }, { a: 1 })).toBe(false);
  });

  it("does not confuse a missing key with an undefined one", () => {
    expect(deepEqual({ a: 1, b: undefined }, { a: 1, c: undefined })).toBe(false);
    expect(deepEqual({ a: undefined }, {})).toBe(false);
  });

  it("does not confuse arrays with objects, or null with an object", () => {
    expect(deepEqual([], {})).toBe(false);
    expect(deepEqual({}, null)).toBe(false);
    expect(deepEqual(null, {})).toBe(false);
    expect(deepEqual({ 0: "a" }, ["a"])).toBe(false);
  });

  it("compares non-plain objects by identity only", () => {
    const d = new Date(5);
    expect(deepEqual(d, d)).toBe(true);
    expect(deepEqual(new Date(5), new Date(5))).toBe(false);
    expect(deepEqual(new Map(), new Map())).toBe(false);
  });

  it("treats NaN as equal to itself (Object.is)", () => {
    expect(deepEqual({ n: Number.NaN }, { n: Number.NaN })).toBe(true);
  });

  it("is fast enough for a poll-sized payload", () => {
    const make = () => ({
      envs: Array.from({ length: 200 }, (_, i) => ({
        id: i,
        name: `env-${i}`,
        runs: Array.from({ length: 20 }, (_, j) => ({ id: j, at: `2026-01-01T00:00:${j}`, ok: j % 2 === 0 })),
      })),
    });
    const a = make();
    const b = make();
    const start = performance.now();
    for (let i = 0; i < 20; i++) expect(deepEqual(a, b)).toBe(true);
    // 20 compares of ~4k-node tree: generous bound, only catches an accidental quadratic.
    expect(performance.now() - start).toBeLessThan(500);
  });
});

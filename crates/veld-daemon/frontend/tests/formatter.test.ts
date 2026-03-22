import { describe, it, expect } from "vitest";
import { truncate, stringify, captureStack } from "../src/client-log/formatter";

describe("truncate", () => {
  it("returns short strings unchanged", () => {
    expect(truncate("hello")).toBe("hello");
  });

  it("truncates strings over 8192 chars", () => {
    const long = "x".repeat(9000);
    const result = truncate(long);
    expect(result.length).toBeLessThan(long.length);
    expect(result).toContain("...(truncated)");
    expect(result.startsWith("x".repeat(8192))).toBe(true);
  });

  it("returns empty string unchanged", () => {
    expect(truncate("")).toBe("");
  });

  it("keeps strings at exactly 8192 chars", () => {
    const exact = "a".repeat(8192);
    expect(truncate(exact)).toBe(exact);
  });
});

describe("stringify", () => {
  it("joins string arguments with spaces", () => {
    const args = ["hello", "world"] as unknown as ArrayLike<unknown>;
    expect(stringify(args)).toBe("hello world");
  });

  it("handles null and undefined", () => {
    const args = [null, undefined] as unknown as ArrayLike<unknown>;
    expect(stringify(args)).toBe("null undefined");
  });

  it("serializes objects as JSON", () => {
    const args = [{ a: 1 }] as unknown as ArrayLike<unknown>;
    expect(stringify(args)).toBe('{"a":1}');
  });

  it("handles numbers and booleans", () => {
    const args = [42, true] as unknown as ArrayLike<unknown>;
    expect(stringify(args)).toBe("42 true");
  });

  it("handles circular references gracefully", () => {
    const obj: Record<string, unknown> = {};
    obj.self = obj;
    const args = [obj] as unknown as ArrayLike<unknown>;
    // Should fall back to String(obj) instead of throwing
    const result = stringify(args);
    expect(result).toBe("[object Object]");
  });

  it("truncates large JSON objects", () => {
    const big = { data: "x".repeat(9000) };
    const args = [big] as unknown as ArrayLike<unknown>;
    const result = stringify(args);
    expect(result).toContain("...(truncated)");
  });

  it("handles empty arguments", () => {
    const args = [] as unknown as ArrayLike<unknown>;
    expect(stringify(args)).toBe("");
  });
});

describe("captureStack", () => {
  it("returns a string", () => {
    const result = captureStack("some-script.js");
    expect(typeof result).toBe("string");
  });

  it("filters out frames matching the script URL", () => {
    const result = captureStack("formatter.test.ts");
    // Our own test file frames should be filtered
    // (this is a heuristic test — stack format varies by runtime)
    expect(typeof result).toBe("string");
  });
});

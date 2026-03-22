import { describe, it, expect, vi, beforeEach } from "vitest";
import { api } from "../src/feedback-overlay/api";

describe("api", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("makes GET request to correct URL", async () => {
    const mockFetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: () => Promise.resolve([{ id: "1" }]),
    });
    vi.stubGlobal("fetch", mockFetch);

    const result = await api("GET", "/threads");
    expect(mockFetch).toHaveBeenCalledWith(
      "/__veld__/feedback/api/threads",
      expect.objectContaining({ method: "GET" }),
    );
    expect(result).toEqual([{ id: "1" }]);
  });

  it("sends JSON body for POST", async () => {
    const mockFetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: () => Promise.resolve({ id: "new" }),
    });
    vi.stubGlobal("fetch", mockFetch);

    await api("POST", "/threads", { message: "hello" });
    expect(mockFetch).toHaveBeenCalledWith(
      "/__veld__/feedback/api/threads",
      expect.objectContaining({
        method: "POST",
        body: '{"message":"hello"}',
      }),
    );
  });

  it("returns null for 204 responses", async () => {
    const mockFetch = vi.fn().mockResolvedValue({
      ok: true,
      status: 204,
    });
    vi.stubGlobal("fetch", mockFetch);

    const result = await api("PUT", "/threads/1/seen");
    expect(result).toBeNull();
  });

  it("throws on non-ok response", async () => {
    const mockFetch = vi.fn().mockResolvedValue({
      ok: false,
      status: 404,
    });
    vi.stubGlobal("fetch", mockFetch);

    await expect(api("GET", "/threads/missing")).rejects.toThrow("failed: 404");
  });
});

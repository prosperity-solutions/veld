import { describe, expect, it, vi } from "vitest";
import {
  api,
  errorMessage,
  normalizeShare,
  normalizeShares,
  runPath,
  runRef,
  runScope,
  type ShareInfo,
  type SharesList,
} from "./api";

describe("run mutation receipts", () => {
  it("exposes an ambiguous start before selection changes can create a new run", async () => {
    const saved = new Map<string, string>();
    const fetch = vi.fn<typeof globalThis.fetch>()
      .mockRejectedValueOnce(new TypeError("connection lost"))
      .mockResolvedValueOnce(new Response('{"action_id":"original-start"}', { status: 202 }));
    vi.stubGlobal("fetch", fetch);
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => saved.get(key) ?? null,
      setItem: (key: string, value: string) => { saved.set(key, value); },
      removeItem: (key: string) => { saved.delete(key); },
    });
    try {
      const original = { preset: "dev", run_name: "dev" };
      await expect(api.startRun(42, original)).rejects.toThrow("outcome is unknown");
      const pinned = api.pendingRunStart(42);
      expect(pinned).toEqual(original);
      expect(fetch).toHaveBeenCalledTimes(1);
      expect(api.pendingRunStart(43)).toBeNull();
      // Even a stale caller proposing the next name must recover the original.
      await api.startRun(42, { preset: "changed", run_name: "dev-2" });
      expect(JSON.parse(fetch.mock.calls[1][1]?.body as string)).toEqual(original);
      expect(new Headers(fetch.mock.calls[1][1]?.headers).get("Idempotency-Key"))
        .toBe(new Headers(fetch.mock.calls[0][1]?.headers).get("Idempotency-Key"));
      expect(api.pendingRunStart(42)).toBeNull();
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("keys all UI run mutations while preserving void callbacks", async () => {
    const pending = new Map<string, string>();
    const fetch = vi.fn<typeof globalThis.fetch>().mockImplementation(async () =>
      new Response('{"action_id":"operation-one"}', { status: 202 }),
    );
    vi.stubGlobal("fetch", fetch);
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => pending.get(key) ?? null,
      setItem: (key: string, value: string) => { pending.set(key, value); },
      removeItem: (key: string) => { pending.delete(key); },
    });
    try {
      const run = runRef("/repo", { name: "dev" });
      expect(await api.startRun(42, { preset: "dev", run_name: "dev" })).toBeUndefined();
      expect(await api.stopRun(run)).toBeUndefined();
      expect(await api.restartRun(run)).toBeUndefined();
      expect(await api.runAction(run, "build", "web")).toBeUndefined();
      const keys = fetch.mock.calls.map(([, init]) => {
        const headers = new Headers(init?.headers);
        expect(init?.method).toBe("POST");
        expect(headers.get("X-Veld-Request")).toBe("1");
        expect(headers.has("X-Veld-Remote-Client")).toBe(false);
        expect(headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
        return headers.get("Idempotency-Key");
      });
      expect(new Set(keys).size).toBe(4);
      expect(fetch.mock.calls.map(([path]) => path)).toEqual([
        "/api/worktrees/42/start",
        "/api/environments/dev/stop?project_root=%2Frepo",
        "/api/environments/dev/restart?project_root=%2Frepo",
        "/api/environments/dev/action?project_root=%2Frepo",
      ]);
      expect(pending.size).toBe(0);
    } finally {
      vi.unstubAllGlobals();
    }
  });
});

describe("run addressing", () => {
  // The client half of the fix: a run name alone is not an address, so every
  // run-addressed call carries the project it was read from. Nothing else
  // asserts the wire format — the callers build template strings.
  it("carries the project root as project_root", () => {
    const ref = runRef("/Users/me/repo", { name: "main" });
    expect(runScope(ref)).toBe("project_root=%2FUsers%2Fme%2Frepo");
    expect(runPath(ref)).toBe("main");
  });

  it("encodes paths that would otherwise break the query string", () => {
    // `+` must not survive as a literal (the server would read it as a space),
    // and spaces/#/& must not terminate or split the parameter.
    const params = new URLSearchParams(
      runScope(runRef("/tmp/a b+c&d#e", { name: "main" })),
    );
    expect(params.get("project_root")).toBe("/tmp/a b+c&d#e");
  });

  it("keeps the name out of the query and the root out of the path", () => {
    // A run named with a slash would otherwise escape its path segment.
    const ref = runRef("/repo", { name: "feat/x" });
    expect(runPath(ref)).toBe("feat%2Fx");
    expect(runScope(ref)).not.toContain("feat");
  });

  it("round-trips a project root through a full request URL", () => {
    const ref = runRef("/repos/alpha", { name: "main" });
    const url = new URL(
      `http://d/api/environments/${runPath(ref)}/stop?${runScope(ref)}`,
    );
    expect(url.pathname).toBe("/api/environments/main/stop");
    expect(url.searchParams.get("project_root")).toBe("/repos/alpha");
  });
});

describe("errorMessage", () => {
  const fail = (body: string, init?: ResponseInit) =>
    new Response(body, { status: 400, statusText: "Bad Request", ...init });

  it("reads the daemon's JSON error shape", () => {
    // management.rs / desktop.rs answer with {"error": "…"}.
    return expect(errorMessage(fail('{"error":"no veld.json in that worktree"}'))).resolves.toBe(
      "no veld.json in that worktree",
    );
  });

  it("reads a plain-text error body", async () => {
    // share/api.rs returns (StatusCode, String), so the actionable half of a
    // refused share used to be dropped and surface as "400 Bad Request" —
    // reading as a bug in Veld rather than as a config that has not opted in.
    const share =
      "run 'dev' has no services opted into peer sharing. Add `\"share\": { \"expose\": [\"peer\"] }` to the variant(s) you want to share (candidates: website:local).";
    await expect(errorMessage(fail(share))).resolves.toBe(share);
  });

  it("falls back to the status for an empty body or an HTML page", async () => {
    await expect(errorMessage(fail(""))).resolves.toBe("400 Bad Request");
    await expect(errorMessage(fail("<html><body>502</body></html>"))).resolves.toBe(
      "400 Bad Request",
    );
    // JSON without an `error` string is not a message either.
    await expect(errorMessage(fail('{"ok":false}'))).resolves.toBe('{"ok":false}');
  });

  it("caps a runaway body", async () => {
    const msg = await errorMessage(fail("x".repeat(2000)));
    expect(msg.length).toBe(601);
    expect(msg.endsWith("…")).toBe(true);
  });
});

describe("normalizeShare", () => {
  // The exact payload a live peer share with no joiners produces: `public_urls`
  // and `connections` are `skip_serializing_if = "Vec::is_empty"` in
  // `veld_core::share::ShareInfo`, so those keys are simply absent. Every consumer
  // reads `.length` off them, which is a TypeError that takes the view down — the
  // reason normalisation happens once in the client and not at each call site.
  const wire = {
    id: "shr_68bec4cf",
    run: "probe",
    run_id: "ad5a47bc-5b60-4172-a917-bab5dee741a4",
    approve: "manual",
    nodes: ["app"],
    urls: ["https://app.probe.shareprobe.localhost"],
    ticket: "veldshare_…",
    join_url: "https://veld.localhost/join#veldshare_…",
    joiners: 0,
  } as unknown as ShareInfo;

  it("fills the arrays the daemon omits when they are empty", () => {
    const s = normalizeShare(wire);
    expect(s.public_urls).toEqual([]);
    expect(s.connections).toEqual([]);
    // A peer share is identified by having no public URLs — the check that used
    // to throw.
    expect(s.public_urls.length === 0).toBe(true);
  });

  it("leaves present values alone", () => {
    const web = {
      ...wire,
      public_urls: [{ node: "app", hostname: "h", public_url: "https://x" }],
      connections: [{ node_id: "n", transport: "direct", rtt_ms: 12 }],
      joiners: 2,
    } as unknown as ShareInfo;
    const s = normalizeShare(web);
    expect(s.public_urls).toHaveLength(1);
    expect(s.connections[0].transport).toBe("direct");
    expect(s.joiners).toBe(2);
  });

  it("normalises every entry of a list, and a list missing its own keys", () => {
    const list = normalizeShares({ shares: [wire], joins: [wire] } as unknown as SharesList);
    expect(list.shares[0].connections).toEqual([]);
    expect(list.joins[0].public_urls).toEqual([]);
    expect(list.pending).toEqual([]);
  });
});

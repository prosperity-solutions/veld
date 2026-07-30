import { describe, expect, it } from "vitest";
import type { HistoryEntry, RunInfo, ShareInfo } from "../api";
import { runOfShare, sharesForRun } from "./Sharing";
import { nodeRows } from "./NodeList";

const share = (over: Partial<ShareInfo>): ShareInfo => ({
  id: "shr_1",
  run: "dev",
  run_id: "run-a",
  nodes: ["web"],
  urls: ["https://web.dev.p.localhost"],
  joiners: 0,
  public_urls: [],
  connections: [],
  ...over,
});

describe("sharesForRun", () => {
  it("splits this run's shares into the peer one and the web ones", () => {
    const mine = share({ id: "peer", ticket: "veldshare_x", join_url: "https://v/join#x" });
    const web = share({
      id: "web",
      public_urls: [{ node: "web", hostname: "h", public_url: "https://x.example" }],
    });
    const other = share({ id: "elsewhere", run_id: "run-b" });
    const got = sharesForRun([mine, web, other], "run-a");
    expect(got.peer?.id).toBe("peer");
    expect(got.web.map((s) => s.id)).toEqual(["web"]);
  });

  it("attaches by run_id, never by name — two repos share a run name", () => {
    // The bug this rule exists for: two checkouts both on `main` each have an
    // environment called `main`, so a name-keyed filter hangs one project's share
    // on the other's run.
    const sameName = share({ id: "other-project", run: "dev", run_id: "run-b" });
    expect(sharesForRun([sameName], "run-a")).toEqual({ peer: null, web: [] });
  });

  it("ignores joins, which carry no run_id", () => {
    const join = share({ id: "join", run_id: null });
    expect(sharesForRun([join], "run-a").peer).toBeNull();
  });

  it("known limitation: a web share still registering looks like a peer share", () => {
    // `POST /api/shares?web` inserts the share before the gateway round-trip
    // (crates/veld-daemon/src/share/api.rs), and every field that would give it
    // away — public_urls, web_password, and even ticket/join_url — is derived from
    // the registration. So for that window the wire is genuinely ambiguous and this
    // classifies it as the peer share. Pinned as a canary: when the daemon grows a
    // discriminator, this expectation is what should flip.
    const pending = share({ id: "pending-web", ticket: "veldshare_gw", public_urls: [] });
    expect(sharesForRun([pending], "run-a").peer?.id).toBe("pending-web");
  });
});

describe("runOfShare", () => {
  it("names the run a pending request is asking about", () => {
    expect(runOfShare([share({ id: "s1", run: "checkout" })], "s1")).toBe("checkout");
    expect(runOfShare([share({ id: "s1" })], "nope")).toBeNull();
  });
});

const run = (over: Partial<RunInfo> = {}): RunInfo => ({
  name: "dev",
  status: "running",
  live: true,
  run_id: "run-a",
  short_id: "run-a",
  urls: {},
  nodes: [
    {
      name: "web",
      variant: "local",
      status: "healthy",
      url: "https://web.dev.p.localhost",
      pid: 4321,
      actions: [{ name: "seed", label: "Seed" }],
      recovery_count: 2,
      consecutive_failures: 1,
      last_liveness_error: "connection refused",
    },
  ],
  ...over,
});

describe("nodeRows", () => {
  it("carries the live URL, pid, actions and health counters", () => {
    const [row] = nodeRows(run(), null);
    expect(row).toMatchObject({
      name: "web",
      variant: "local",
      url: "https://web.dev.p.localhost",
      pid: 4321,
      recovery_count: 2,
      consecutive_failures: 1,
      last_liveness_error: "connection refused",
    });
    expect(row.actions).toHaveLength(1);
  });

  it("nulls URL, pid and actions for a history entry", () => {
    // An ended run's URLs are stripped server-side, and an action fired from one
    // would run against whatever is current — so the row must not offer either.
    const entry: HistoryEntry = {
      run_id: "run-old",
      short_id: "run-old",
      status: "failed",
      created_at: "2026-07-30T10:00:00Z",
      nodes: [{ name: "web", variant: "local", status: "crashed" }],
    };
    expect(nodeRows(run(), entry)).toEqual([
      {
        name: "web",
        variant: "local",
        status: "crashed",
        url: null,
        pid: null,
        actions: [],
        recovery_count: 0,
        consecutive_failures: 0,
        last_liveness_error: null,
      },
    ]);
  });

  it("defaults every optional counter the daemon may omit", () => {
    const bare = run({ nodes: [{ name: "db", variant: "docker", status: "starting" }] });
    expect(nodeRows(bare, null)[0]).toMatchObject({
      url: null,
      pid: null,
      actions: [],
      recovery_count: 0,
      consecutive_failures: 0,
      last_liveness_error: null,
    });
  });
});

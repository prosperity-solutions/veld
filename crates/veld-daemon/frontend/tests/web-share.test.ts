// @vitest-environment jsdom
import { describe, it, expect } from "vitest";
import {
  findShare,
  connectionRow,
  type ShareConnectionInfo,
} from "../src/feedback-overlay/web-share";

const list = {
  shares: [
    { id: "s-peer", public_urls: [] },
    {
      id: "s-web",
      web_password: "k7dm-q2xp-9fzt",
      public_urls: [
        {
          node: "app",
          hostname: "app.demo.p.localhost",
          public_url: "https://abc123.share.example",
          access: "password",
        },
      ],
      connections: [
        {
          node_id: "aaaabbbbccccdddd",
          label: "gateway share.example",
          transport: "relayed",
          via: "https://euw1-1.relay.example./",
          rtt_ms: 45,
        } satisfies ShareConnectionInfo,
      ],
    },
  ],
};

describe("findShare", () => {
  it("returns the share whose public URL covers the hostname", () => {
    const share = findShare(list, "app.demo.p.localhost");
    expect(share?.id).toBe("s-web");
  });

  it("returns null for a hostname no web share covers", () => {
    expect(findShare(list, "other.demo.p.localhost")).toBeNull();
    expect(findShare({}, "app.demo.p.localhost")).toBeNull();
  });
});

describe("connectionRow", () => {
  it("labels a relayed tunnel with the relay and the throughput cost", () => {
    const row = connectionRow(list.shares[1].connections![0]);
    expect(row.textContent).toContain("gateway share.example");
    expect(row.textContent).toContain("relayed via https://euw1-1.relay.example./");
    expect(row.textContent).toContain("rtt 45ms");
    expect(row.textContent).toContain("throughput limited");
    expect(row.querySelector(".veld-feedback-web-share-dot-relayed")).not.toBeNull();
  });

  it("labels a direct tunnel without the relay warning", () => {
    const row = connectionRow({
      node_id: "aaaabbbbccccdddd",
      label: "",
      transport: "direct",
      via: "203.0.113.7:4711",
      rtt_ms: 12,
    });
    // No label → the node id (shortened) identifies the peer.
    expect(row.textContent).toContain("aaaabbbbcc: direct");
    expect(row.textContent).not.toContain("throughput limited");
    expect(row.querySelector(".veld-feedback-web-share-dot-direct")).not.toBeNull();
  });

  it("reports a pathless snapshot honestly", () => {
    const row = connectionRow({
      node_id: "x",
      transport: "none",
    } as ShareConnectionInfo);
    expect(row.textContent).toContain("no open path");
  });
});

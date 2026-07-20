// @vitest-environment jsdom
import { describe, it, expect, beforeEach } from "vitest";
import {
  findShare,
  buildSharingMenuItem,
  updateSharingIndicator,
} from "../src/feedback-overlay/sharing";
import { dispatch } from "../src/feedback-overlay/store";
import { setupOverlayEnv } from "./test-helpers";
import type { ArcItem } from "../src/feedback-overlay/arc-menu";

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
    },
  ],
};

describe("findShare", () => {
  it("returns the share whose public URL covers the hostname", () => {
    expect(findShare(list, "app.demo.p.localhost")?.id).toBe("s-web");
  });

  it("returns null for a hostname no web share covers", () => {
    expect(findShare(list, "other.demo.p.localhost")).toBeNull();
    expect(findShare({}, "app.demo.p.localhost")).toBeNull();
  });
});

describe("buildSharingMenuItem", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    setupOverlayEnv();
  });

  const sub = (item: ArcItem, id: string): ArcItem =>
    (item.sub ?? []).find((s) => s.id === id)!;
  const shown = (item: ArcItem, id: string): boolean => {
    const s = sub(item, id);
    return s.isVisible ? s.isVisible() : true;
  };

  it("shows Start (not Stop/Copy) when this page is not shared", () => {
    dispatch({ type: "SET_SHARE_STATUS", active: false, id: null });
    const item = buildSharingMenuItem();
    expect(shown(item, "share-start")).toBe(true);
    expect(shown(item, "share-stop")).toBe(false);
    expect(shown(item, "share-copy")).toBe(false);
    expect(shown(item, "share-status")).toBe(true); // always available
  });

  it("shows Stop + Copy (not Start) when this page is shared", () => {
    dispatch({ type: "SET_SHARE_STATUS", active: true, id: "s-web" });
    const item = buildSharingMenuItem();
    expect(shown(item, "share-start")).toBe(false);
    expect(shown(item, "share-stop")).toBe(true);
    expect(shown(item, "share-copy")).toBe(true);
  });

  it("lights the status dot only while sharing is active", () => {
    dispatch({ type: "SET_SHARE_STATUS", active: false, id: null });
    const item = buildSharingMenuItem();
    const dot = item.el.querySelector(".veld-feedback-status-dot")!;
    expect(dot.classList.contains("veld-feedback-status-dot-on")).toBe(false);

    dispatch({ type: "SET_SHARE_STATUS", active: true, id: "s-web" });
    updateSharingIndicator();
    expect(dot.classList.contains("veld-feedback-status-dot-on")).toBe(true);
  });
});

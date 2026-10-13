// @vitest-environment jsdom
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  findShare,
  buildSharingMenuItem,
  updateSharingIndicator,
  pollShareStatus,
  startSharePolling,
} from "../src/feedback-overlay/sharing";
import { dispatch, getState } from "../src/feedback-overlay/store";
import { refs } from "../src/feedback-overlay/refs";
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

// jsdom's default hostname is "localhost".
const HOST = "localhost";
const sharesFor = (host: string | null) => ({
  shares: host
    ? [{ id: "s-web", public_urls: [{ node: "app", hostname: host, public_url: "https://abc.share.example", access: "password" }], web_password: "pw" }]
    : [],
});

/** Collect the text of every toast currently in the shadow root. */
const toastTexts = (): string[] =>
  Array.from(refs.shadow.querySelectorAll(".veld-feedback-toast")).map((t) => t.textContent || "");

describe("sharing fetch orchestration", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    setupOverlayEnv();
    dispatch({ type: "SET_SHARE_STATUS", active: false, id: null });
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("pollShareStatus lights the dot + records the id when this page is shared", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => sharesFor(HOST) })));
    const item = buildSharingMenuItem();
    pollShareStatus();
    await vi.waitFor(() => expect(getState().shareActive).toBe(true));
    expect(getState().shareId).toBe("s-web");
    expect(item.el.querySelector(".veld-feedback-status-dot")!.classList.contains("veld-feedback-status-dot-on")).toBe(true);
  });

  it("pollShareStatus does nothing while the overlay is hidden", async () => {
    const fetchMock = vi.fn(async () => ({ ok: true, json: async () => sharesFor(HOST) }));
    vi.stubGlobal("fetch", fetchMock);
    dispatch({ type: "SET_HIDDEN", hidden: true });
    pollShareStatus();
    await Promise.resolve();
    expect(fetchMock).not.toHaveBeenCalled();
    expect(getState().shareActive).toBe(false);
  });

  it("Start sharing toasts success when this page is covered", async () => {
    vi.stubGlobal("fetch", vi.fn(async (url: string, opts?: { method?: string }) => {
      if (opts?.method === "POST") return { ok: true, json: async () => ({ public_urls: [{ hostname: HOST }] }) };
      return { ok: true, json: async () => sharesFor(HOST) }; // finally-poll GET
    }));
    const item = buildSharingMenuItem();
    const start = (item.sub ?? []).find((s) => s.id === "share-start")!;
    start.onSelect!();
    await vi.waitFor(() => expect(toastTexts().some((t) => t === "Shared to the web")).toBe(true));
  });

  it("Start sharing warns when the share covers only other services", async () => {
    vi.stubGlobal("fetch", vi.fn(async (url: string, opts?: { method?: string }) => {
      if (opts?.method === "POST") return { ok: true, json: async () => ({ public_urls: [{ hostname: "other.localhost" }] }) };
      return { ok: true, json: async () => sharesFor(null) };
    }));
    const item = buildSharingMenuItem();
    const start = (item.sub ?? []).find((s) => s.id === "share-start")!;
    start.onSelect!();
    await vi.waitFor(() => expect(toastTexts().some((t) => t.includes("other services"))).toBe(true));
  });

  it("Start sharing surfaces the daemon's error text on failure", async () => {
    vi.stubGlobal("fetch", vi.fn(async (url: string, opts?: { method?: string }) => {
      if (opts?.method === "POST") return { ok: false, status: 409, text: async () => "no gateway configured" };
      return { ok: true, json: async () => sharesFor(null) };
    }));
    const item = buildSharingMenuItem();
    const start = (item.sub ?? []).find((s) => s.id === "share-start")!;
    start.onSelect!();
    await vi.waitFor(() => expect(toastTexts().some((t) => t === "no gateway configured")).toBe(true));
  });
});

describe("startSharePolling", () => {
  const stops: Array<() => void> = [];
  let hidden = false;
  const setHidden = (next: boolean) => {
    hidden = next;
    document.dispatchEvent(new Event("visibilitychange"));
  };

  beforeEach(() => {
    document.body.innerHTML = "";
    setupOverlayEnv();
    hidden = false;
    vi.useFakeTimers();
    Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
  });
  afterEach(() => {
    for (const stop of stops.splice(0)) stop();
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  const stubFetch = () => {
    const fetchMock = vi.fn(async () => ({ ok: true, json: async () => ({ shares: [] }) }));
    vi.stubGlobal("fetch", fetchMock);
    return fetchMock;
  };

  it("polls on every tick while the tab is visible", () => {
    const fetchMock = stubFetch();
    stops.push(startSharePolling(5000));
    vi.advanceTimersByTime(15_000);
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });

  it("does not poll on ticks while the tab is hidden", () => {
    const fetchMock = stubFetch();
    stops.push(startSharePolling(5000));
    hidden = true;
    vi.advanceTimersByTime(60_000);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("catches up as soon as the tab is shown again", () => {
    const fetchMock = stubFetch();
    stops.push(startSharePolling(5000));
    hidden = true;
    vi.advanceTimersByTime(60_000);
    setHidden(false);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("does not poll when the tab becomes hidden", () => {
    const fetchMock = stubFetch();
    stops.push(startSharePolling(5000));
    setHidden(true);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("leaves a direct pollShareStatus() alone — it follows a click, not a timer", () => {
    const fetchMock = stubFetch();
    hidden = true;
    pollShareStatus();
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});

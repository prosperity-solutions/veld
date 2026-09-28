import { fireEvent, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { render } from "../shared/testRender";
import type { HighlightSlug } from "./content";
import { FeatureHighlight, HighlightProvider } from "./FeatureHighlight";

// Two highlights, so the one-per-page-load budget has something to refuse. The
// real table ships one; the budget is what keeps a second from ever doubling up.
vi.mock("./content", () => ({
  HIGHLIGHTS: {
    "pixel-markers": {
      slug: "pixel-markers",
      since: "2026-09-28",
      title: "New: Pixels",
      body: "One sentence.",
    },
    "second-hint": {
      slug: "second-hint",
      since: "2026-09-28",
      title: "Second",
      body: "Another sentence.",
    },
  },
}));

let stored: Record<string, "read" | "dismissed"> = {};
const marks: { ids: string[]; state: string }[] = [];

vi.mock("../api", () => ({
  api: {
    promotionState: () =>
      Promise.resolve({ states: stored, first_use: "2026-01-01T00:00:00Z" }),
    markPromotions: (ids: string[], state: "read" | "dismissed") => {
      marks.push({ ids, state });
      return Promise.resolve({ states: { ...stored, [ids[0]]: state } });
    },
  },
}));

beforeEach(() => {
  stored = {};
  marks.length = 0;
});

function anchored() {
  return (
    <HighlightProvider>
      <FeatureHighlight slug="pixel-markers">
        <button type="button">Face switch</button>
      </FeatureHighlight>
    </HighlightProvider>
  );
}

describe("FeatureHighlight", () => {
  it("shows once the store answers, and Got it records it as read", async () => {
    render(anchored());
    const got = await screen.findByRole("button", { name: "Got it" });
    fireEvent.click(got);
    expect(marks).toEqual([{ ids: ["hint:pixel-markers"], state: "read" }]);
    await waitFor(() => expect(screen.queryByRole("button", { name: "Got it" })).toBeNull());
  });

  it("stays closed for somebody who already answered it", async () => {
    stored = { "hint:pixel-markers": "dismissed" };
    render(anchored());
    // The anchor still renders; only the bubble is withheld.
    expect(await screen.findByRole("button", { name: "Face switch" })).toBeTruthy();
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryByRole("button", { name: "Got it" })).toBeNull();
  });

  it("spends the page load's turn on one highlight only", async () => {
    render(
      <HighlightProvider>
        <FeatureHighlight slug="pixel-markers">
          <button type="button">First</button>
        </FeatureHighlight>
        <FeatureHighlight slug={"second-hint" as HighlightSlug}>
          <button type="button">Second</button>
        </FeatureHighlight>
      </HighlightProvider>,
    );
    // Read the open state off the anchors (`aria-expanded`), which is exact, rather
    // than off the dropdowns, which Mantine mounts and fades in on its own schedule.
    const first = screen.getByRole("button", { name: "First" });
    const second = screen.getByRole("button", { name: "Second" });
    await waitFor(() => expect(first.getAttribute("aria-expanded")).toBe("true"));
    expect(second.getAttribute("aria-expanded")).toBe("false");
    // Answering the first does not hand the turn to the second.
    const gotIt = (await screen.findByText("Got it")).closest("button");
    if (gotIt === null) throw new Error("no Got it button");
    fireEvent.click(gotIt);
    await waitFor(() => expect(first.getAttribute("aria-expanded")).toBe("false"));
    expect(second.getAttribute("aria-expanded")).toBe("false");
    expect(marks).toEqual([{ ids: ["hint:pixel-markers"], state: "read" }]);
  });

  it("renders just the control outside a provider", () => {
    render(
      <FeatureHighlight slug="pixel-markers">
        <button type="button">Face switch</button>
      </FeatureHighlight>,
    );
    expect(screen.getByRole("button", { name: "Face switch" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Got it" })).toBeNull();
  });
});

import { fireEvent, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { render } from "../shared/testRender";
import { FeatureHighlight, HighlightProvider } from "./FeatureHighlight";

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

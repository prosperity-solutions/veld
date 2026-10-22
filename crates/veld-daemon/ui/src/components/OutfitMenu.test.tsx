import { fireEvent, screen } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";
import type { OutfitState } from "../api";
import { render } from "../shared/testRender";
import { OutfitMenu } from "./OutfitMenu";

/**
 * The top bar's outfit menu, rendered. A real repo's profile swaps dozens of
 * files, and the menu once listed every one under each name — this pins that it
 * names profiles and nothing else.
 */

const many = Array.from({ length: 40 }, (_, i) => `.agents/skills/s${i}/SKILL.md`);
const state: OutfitState = {
  status: "ready",
  required: false,
  current: { name: "base", pinned: false },
  profiles: [
    { name: "base", tier: "builtin", files: [] },
    { name: "pitch-roast-ship", tier: "local", files: many },
  ],
};

vi.mock("../api", async (importOriginal) => {
  const real = await importOriginal<typeof import("../api")>();
  return { ...real, api: { outfitState: () => Promise.resolve(state) } };
});

describe("OutfitMenu", () => {
  test("names each profile, never the files it swaps", async () => {
    render(<OutfitMenu path="/repo" />);
    fireEvent.click(await screen.findByRole("button", { name: /^Outfit: base/ }));
    const item = await screen.findByRole("menuitem", { name: "pitch-roast-ship", hidden: true });
    expect(item.querySelector("[title]")?.getAttribute("title")).toBe("pitch-roast-ship");
    expect(screen.queryByText(/SKILL\.md/)).toBeNull();
  });
});

import { screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { api, type WorktreeLink } from "../api";
import { render } from "../shared/testRender";
import { openLink, RailPrLinks, WorktreeLinks } from "./WorktreeLinks";

let n = 0;
const link = (over: Partial<WorktreeLink> = {}): WorktreeLink => ({
  id: `l${++n}`,
  kind: "pr",
  url: "https://github.com/o/r/pull/1",
  label: `PR ${n}`,
  icon: null,
  tone: "neutral",
  tooltip: "",
  branch: "feat/x",
  updated_at: "2026-01-01T00:00:00Z",
  ...over,
});

describe("WorktreeLinks", () => {
  it("renders nothing without links", () => {
    const { container } = render(<WorktreeLinks links={[]} branch="feat/x" worktreeId={1} />);
    expect(container.querySelector("button")).toBeNull();
  });

  it("shows one link as a pill carrying its label", () => {
    render(<WorktreeLinks links={[link({ label: "Add links" })]} branch="feat/x" worktreeId={1} />);
    expect(screen.getByRole("button", { name: /Add links/u })).toBeTruthy();
  });

  it("folds several of a kind into one pill with a count", () => {
    render(<WorktreeLinks links={[link(), link(), link()]} branch="feat/x" worktreeId={1} />);
    expect(screen.getAllByRole("button")).toHaveLength(1);
    expect(screen.getByRole("button", { name: /3 PRs/u })).toBeTruthy();
  });

  it("dims a link set on another branch", () => {
    const { container } = render(
      <WorktreeLinks links={[link({ branch: "old" })]} branch="feat/x" worktreeId={1} />,
    );
    expect(container.querySelector(".wt-link-stale")).not.toBeNull();
  });

  it("does not dim a link set on this branch", () => {
    const { container } = render(<WorktreeLinks links={[link()]} branch="feat/x" worktreeId={1} />);
    expect(container.querySelector(".wt-link-stale")).toBeNull();
  });
});

describe("RailPrLinks", () => {
  it("renders nothing when every pull request is stale", () => {
    const { container } = render(
      <RailPrLinks links={[link({ branch: "old" })]} branch="feat/x" worktreeId={1} name="chk" />,
    );
    expect(container.querySelector("button")).toBeNull();
  });

  it("shows the count beside the glyph for several, tinted by the worst tone", () => {
    const { container } = render(
      <RailPrLinks
        links={[link({ tone: "success" }), link({ tone: "danger" })]}
        branch="feat/x" worktreeId={1}
        name="chk"
      />,
    );
    const b = container.querySelector("button.wt-links");
    expect(b?.textContent).toBe("2");
    expect(b?.classList.contains("tone-danger")).toBe(true);
  });

  it("stays muted for tones other than danger and success", () => {
    const { container } = render(
      <RailPrLinks links={[link({ tone: "warning" })]} branch="feat/x" worktreeId={1} name="chk" />,
    );
    expect(container.querySelector("button.wt-links.tone-neutral")).not.toBeNull();
  });
});

describe("openLink", () => {
  const plain = { metaKey: false, ctrlKey: false, shiftKey: false };
  afterEach(() => vi.restoreAllMocks());

  it("opens a modifier-click in the system browser without asking", async () => {
    const ask = vi.spyOn(api, "linkOpenTarget");
    const external = vi.spyOn(window, "open").mockReturnValue(null);
    const pane = vi.fn();
    await openLink("https://x.dev", 1, { ...plain, metaKey: true }, pane);
    expect(ask).not.toHaveBeenCalled();
    expect(pane).not.toHaveBeenCalled();
    expect(external).toHaveBeenCalledWith("https://x.dev", "_blank", "noopener,noreferrer");
  });

  it("follows the daemon's answer otherwise", async () => {
    vi.spyOn(api, "linkOpenTarget").mockResolvedValue({ target: "pane" });
    const external = vi.spyOn(window, "open").mockReturnValue(null);
    const pane = vi.fn();
    await openLink("https://x.dev", 1, plain, pane);
    expect(pane).toHaveBeenCalledWith("https://x.dev");
    expect(external).not.toHaveBeenCalled();

    vi.spyOn(api, "linkOpenTarget").mockResolvedValue({ target: "system" });
    await openLink("https://x.dev", 1, plain, pane);
    expect(external).toHaveBeenCalledTimes(1);
  });

  it("falls back to the system browser with no pane or no answer", async () => {
    vi.spyOn(api, "linkOpenTarget").mockRejectedValue(new Error("down"));
    const external = vi.spyOn(window, "open").mockReturnValue(null);
    await openLink("https://x.dev", 1, plain, vi.fn());
    await openLink("https://x.dev", 1, plain, undefined);
    expect(external).toHaveBeenCalledTimes(2);
  });
});

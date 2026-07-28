import { describe, expect, it } from "vitest";
import { runPath, runRef, runScope } from "./api";

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

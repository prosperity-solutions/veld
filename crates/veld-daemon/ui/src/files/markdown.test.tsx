import { describe, expect, it } from "vitest";

import { renderMarkdown, resolveLink } from "./markdown";

/**
 * In jsdom (a `.tsx` test) because DOMPurify needs a DOM to sanitize against —
 * in the node project it would be a no-op and these would prove nothing.
 */
describe("rendered Markdown", () => {
  it("never passes raw HTML through", () => {
    const html = renderMarkdown('<script>alert(1)</script>\n\n<img src=x onerror="alert(1)">');
    expect(html).not.toContain("<script");
    expect(html).not.toContain("<img");
    expect(html).toContain("&lt;script&gt;");
  });

  it("refuses javascript: links", () => {
    // Refused as a link, so the source stays as text — what must not exist is an href.
    const html = renderMarkdown("[x](javascript:alert(1))");
    expect(html).not.toMatch(/href="javascript:/i);
    expect(html).not.toContain("<a");
  });

  it("renders an image as its alt text, loading nothing", () => {
    const html = renderMarkdown("![the chart](https://example.com/c.png)");
    expect(html).not.toContain("<img");
    expect(html).not.toContain("example.com");
    expect(html).toContain("[the chart]");
  });

  it("reads past a byte-order mark, keeping the line numbers", () => {
    const html = renderMarkdown("\uFEFF# Title\n\nbody\n");
    const doc = new DOMParser().parseFromString(html, "text/html");
    const h1 = doc.querySelector("h1");
    expect(h1?.textContent).toBe("Title");
    expect(h1?.getAttribute("data-src-start")).toBe("1");
    expect(doc.querySelector("p")?.getAttribute("data-src-start")).toBe("3");
  });

  it("stamps each block with the source lines it came from", () => {
    const html = renderMarkdown("# Title\n\nfirst para\nstill first\n\n- a\n- b\n");
    const doc = new DOMParser().parseFromString(html, "text/html");
    const h1 = doc.querySelector("h1");
    expect([h1?.getAttribute("data-src-start"), h1?.getAttribute("data-src-end")]).toEqual([
      "1",
      "1",
    ]);
    const p = doc.querySelector("p");
    expect([p?.getAttribute("data-src-start"), p?.getAttribute("data-src-end")]).toEqual([
      "3",
      "4",
    ]);
    const items = [...doc.querySelectorAll("li")].map((li) => li.getAttribute("data-src-start"));
    expect(items).toEqual(["6", "7"]);
  });
});

describe("links in rendered Markdown", () => {
  it("sends the web out of Veld", () => {
    expect(resolveLink("docs/a.md", "https://x.test/p")).toEqual({
      kind: "external",
      url: "https://x.test/p",
    });
    expect(resolveLink("docs/a.md", "mailto:a@b.c").kind).toBe("external");
  });

  it("resolves a relative path against the linking file's folder", () => {
    expect(resolveLink("docs/plan/a.md", "../b.md")).toEqual({ kind: "file", path: "docs/b.md" });
    expect(resolveLink("a.md", "./notes/c.md#L12")).toEqual({
      kind: "file",
      path: "notes/c.md",
      line: 12,
    });
    expect(resolveLink("/Users/me/x/a.md", "b%20c.md")).toEqual({
      kind: "file",
      path: "/Users/me/x/b c.md",
    });
  });

  it("goes nowhere for an anchor, another scheme, or a climb out of the worktree", () => {
    expect(resolveLink("a.md", "#section").kind).toBe("none");
    expect(resolveLink("a.md", "ftp://x/y").kind).toBe("none");
    expect(resolveLink("a.md", "//host/x").kind).toBe("none");
    expect(resolveLink("docs/a.md", "../../x.md").kind).toBe("none");
  });
});

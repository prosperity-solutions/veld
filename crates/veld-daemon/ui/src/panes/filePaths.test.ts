import { describe, expect, it } from "vitest";
import { findFilePaths } from "./filePaths";

/** The paths, in order, for a line — the shape most assertions want. */
const paths = (line: string) => findFilePaths(line).map((m) => m.path);

/** What the underline would cover, so a span bug is visible as text. */
const spans = (line: string) =>
  findFilePaths(line).map((m) => line.slice(m.start, m.end));

describe("findFilePaths — what must link", () => {
  it("finds a repo-relative path, the shape an agent prints", () => {
    expect(paths("see crates/veld-daemon/src/pty.rs for the handler")).toEqual([
      "crates/veld-daemon/src/pty.rs",
    ]);
  });

  it("carries a line and a column off the tail", () => {
    const [m] = findFilePaths("crates/veld-daemon/src/pty.rs:2529:17");
    expect(m).toMatchObject({ path: "crates/veld-daemon/src/pty.rs", line: 2529, column: 17 });
  });

  it("takes a line with no column", () => {
    const [m] = findFilePaths("src/api.ts:604");
    expect(m).toMatchObject({ path: "src/api.ts", line: 604 });
    expect(m.column).toBeUndefined();
  });

  it("links a bare filename when its extension is one a project's files have", () => {
    expect(paths("edit Cargo.toml and README.md")).toEqual(["Cargo.toml", "README.md"]);
  });

  it("links a dotfile on its name, since it has no extension to read", () => {
    expect(paths("copy .env first")).toEqual([".env"]);
  });

  it("links an explicit path with no extension at all, because ./ said so", () => {
    expect(paths("run ./scripts/build now")).toEqual(["./scripts/build"]);
  });

  it("links an absolute path", () => {
    expect(paths("wrote /tmp/out/report.json")).toEqual(["/tmp/out/report.json"]);
  });

  it("finds several on one line", () => {
    expect(paths("src/a.ts src/b.ts src/c.ts")).toEqual([
      "src/a.ts",
      "src/b.ts",
      "src/c.ts",
    ]);
  });
});

describe("findFilePaths — what must NOT link", () => {
  // Each of these is a real shape from terminal output, and each one underlining
  // would be a link that cannot open. This block is the reason the rule is narrow;
  // loosening `LINKABLE_EXTENSIONS` or rule (a) shows up here first.
  it.each([
    ["a hostname", "deployed to example.com today"],
    ["a hostname with a subdomain", "see docs.example.org for more"],
    ["a version number", "bumped to v1.2.3 this morning"],
    ["a bare version", "now 16.74.0 after the release"],
    ["a method call in a stack trace", "at Object.foo.bar() line 3"],
    ["prose using a slash", "either and/or works here"],
    ["a ratio", "scaled 3.14/2 for the chart"],
    ["a plain word", "the handler returns early"],
    ["an ellipsis", "loading..."],
  ])("ignores %s", (_what, line) => {
    expect(findFilePaths(line)).toEqual([]);
  });

  it("leaves URLs to the link addon that already owns them", () => {
    expect(findFilePaths("open https://example.com/src/a.ts now")).toEqual([]);
  });

  it("leaves a file:// URL alone too — it is already a link there", () => {
    expect(findFilePaths("file:///tmp/a.ts")).toEqual([]);
  });
});

describe("findFilePaths — span boundaries", () => {
  it("stops the underline before a sentence's full stop", () => {
    expect(spans("the bug is in src/model.ts.")).toEqual(["src/model.ts"]);
  });

  it("excludes wrapping parentheses but keeps the line tail", () => {
    expect(spans("the call (src/api.ts:604) returns")).toEqual(["src/api.ts:604"]);
  });

  it("excludes a trailing comma in a list", () => {
    expect(spans("src/a.ts, src/b.ts")).toEqual(["src/a.ts", "src/b.ts"]);
  });

  it("drops a separator colon that carried no line number", () => {
    const [m] = findFilePaths("src/api.ts: expected 3 arguments");
    expect(m.path).toBe("src/api.ts");
    expect(m.line).toBeUndefined();
    expect(spans("src/api.ts: expected 3 arguments")).toEqual(["src/api.ts"]);
  });

  it("excludes surrounding quotes", () => {
    expect(spans(`cannot find "src/missing.ts" anywhere`)).toEqual(["src/missing.ts"]);
  });

  it("reports spans that index back into the line correctly", () => {
    const line = "    --> crates/veld-core/src/ide.rs:340:1";
    const [m] = findFilePaths(line);
    expect(line.slice(m.start, m.end)).toBe("crates/veld-core/src/ide.rs:340:1");
    expect(m).toMatchObject({ path: "crates/veld-core/src/ide.rs", line: 340, column: 1 });
  });
});

describe("findFilePaths — real output", () => {
  it("handles a rustc diagnostic block", () => {
    const out = [
      "error[E0061]: this function takes 7 arguments but 6 arguments were supplied",
      "   --> crates/veld-daemon/src/extensions.rs:445:19",
    ].join("\n");
    expect(paths(out)).toEqual(["crates/veld-daemon/src/extensions.rs"]);
  });

  it("handles a grep hit, whose numbers sit mid-token", () => {
    const line = "crates/veld-core/src/ide.rs:340:const SHELL_REFUSED_BUILTINS";
    const [m] = findFilePaths(line);
    expect(m).toMatchObject({ path: "crates/veld-core/src/ide.rs", line: 340 });
    expect(m.column).toBeUndefined();
    // The underline must stop before the matched source text.
    expect(line.slice(m.start, m.end)).toBe("crates/veld-core/src/ide.rs:340");
  });

  it("does not link a line number that is really part of a time", () => {
    expect(findFilePaths("finished at 12:04:33 today")).toEqual([]);
  });
});

/**
 * Reported from a real session, on `grep -rn` over this repo. Three separate bugs
 * in one screenful, and every one of them is a line somebody actually read.
 */
describe("findFilePaths — regressions from driving it", () => {
  // The token ends `:377:///`, which *contains* `://`. The scheme guard was a
  // substring test, so every hit whose matched line began at column 1 with a Rust
  // doc comment stopped being a link — while the indented ones beside it worked,
  // which is what made it look arbitrary.
  it.each([
    "crates/veld-core/src/ide.rs:377:/// class of value as SHELL_REFUSED_BUILTINS",
    "crates/veld-core/src/ide.rs:2378:/// A second, narrower check rides along",
    "src/foo.ts:12://a C or JS comment at column one",
  ])("links a hit whose content starts with a comment marker: %s", (line) => {
    const [m] = findFilePaths(line);
    expect(m?.path).toBe(line.split(":")[0]);
  });

  // `///` and `//` start with a slash, so they satisfied "explicitly a path" and
  // underlined on their own.
  it.each(["///", "//", "/", "./", "../"])("does not link the bare token %s", (t) => {
    expect(findFilePaths(`some text ${t} more text`)).toEqual([]);
  });

  // The worst of the three: a bare word whose whole spelling is an extension in the
  // table. These are ordinary English, and they underlined in prose.
  it.each([
    "we can go ahead with it",
    "check the log for details",
    "read env from the shell",
    "the conf is wrong",
    "compile c and h together",
    "written in go and rs",
  ])("does not link an extension name used as a word: %s", (line) => {
    expect(findFilePaths(line)).toEqual([]);
  });

  // The counterpart, so the fix above did not simply delete rule (b).
  it("still links the same words when they are real filenames", () => {
    expect(findFilePaths("see main.go and build.conf and app.log").map((m) => m.path)).toEqual([
      "main.go",
      "build.conf",
      "app.log",
    ]);
  });

  it("still links a dotfile, whose leading dot is not an extension separator", () => {
    expect(findFilePaths("copy .env and .gitignore").map((m) => m.path)).toEqual([".env"]);
  });

  // Reported from the same session: the daemon happily opens a directory, but the
  // matcher never underlined one, so the capability was unreachable from the UI.
  describe("directories", () => {
    it.each([
      "crates/veld-daemon/ui/src/panes",
      "crates/veld-daemon/ui/src/panes/",
      "docs/adr/0001-pick-a-database",
      ".github/workflows/nightly",
    ])("links the extensionless directory path %s", (dir) => {
      expect(findFilePaths(`see ${dir} for the rest`).map((m) => m.path)).toEqual([dir]);
    });

    it("links a two-segment directory when it is written as one", () => {
      // Two segments are below the threshold, so the trailing slash is what says
      // "this is a directory" — which is exactly what it means.
      expect(findFilePaths("open src/panes/ next").map((m) => m.path)).toEqual(["src/panes/"]);
      expect(findFilePaths("open src/panes next")).toEqual([]);
    });

    // The guards on rule (c), each closing a shape that has three letter-ish
    // segments and is not a path. Losing any of these puts underlines in prose.
    it.each([
      ["prose with one slash", "either and/or works here"],
      ["a date", "released on 2026/09/21 at noon"],
      ["a bare hostname path", "docs at example.com/docs/intro today"],
      ["a scheme-less two-parter", "the and/or question"],
    ])("still ignores %s", (_what, line) => {
      expect(findFilePaths(line)).toEqual([]);
    });

    // **English slash-lists.** Every one of these underlined when rule (c) only
    // required three segments and a letter — found by review, not by imagination,
    // and the reason the rule also demands a non-letter character.
    it.each([
      "read/write/execute permissions",
      "pick yes/no/maybe here",
      "wire input/output/error up",
      "he/she/they prefer it",
      "a client/server/proxy split",
      "and/or/but works too",
      "the on/off/auto switch",
    ])("does not link the prose slash-list in: %s", (line) => {
      expect(findFilePaths(line)).toEqual([]);
    });

    // The counterpart: a real path is kept because something in it is not a letter.
    it.each([
      "crates/veld-daemon/ui/src/panes",
      "docs/adr/0001-pick-a-database",
      ".github/workflows/nightly",
      "packages/ui-kit/src",
    ])("still links the real path %s", (dir) => {
      expect(findFilePaths(`see ${dir} here`).map((m) => m.path)).toEqual([dir]);
    });

    // The documented loss, pinned so it is a decision rather than a surprise: an
    // all-letter directory path needs the trailing slash to link.
    it("needs a trailing slash for an all-letter directory path", () => {
      expect(findFilePaths("open app/models/user now")).toEqual([]);
      expect(findFilePaths("open app/models/user/ now").map((m) => m.path)).toEqual([
        "app/models/user/",
      ]);
    });
  });

  it("reaches an extensionless file through rule (a) instead", () => {
    expect(findFilePaths("run ./Makefile and Makefile").map((m) => m.path)).toEqual([
      "./Makefile",
    ]);
  });

  // Cost, not correctness. Asserted behaviourally rather than by timing, which is
  // flaky in CI: the bound is what the guarantee rests on, so pin the bound.
  it("ignores a token no filesystem could hold, however path-shaped", () => {
    const huge = `${"a/".repeat(6000)}b.ts`;
    expect(huge.length).toBeGreaterThan(8 * 1024);
    expect(findFilePaths(huge)).toEqual([]);
    // And the line around it still works, so the guard is per-token.
    expect(findFilePaths(`${huge} src/api.ts`).map((m) => m.path)).toEqual(["src/api.ts"]);
  });

  // The daemon's `line` is a `u32`, and serde rejects the **whole body** on an
  // out-of-range integer — so an oversized tail does not degrade to "line 1", it
  // fails the click on a file that resolves perfectly well.
  it.each([
    ["epoch milliseconds", "trace.json:1758499200000", "trace.json"],
    ["a timestamp", "build.log:20260922120000", "build.log"],
  ])("treats %s as not a line, keeping the path clickable", (_what, line, path) => {
    const [m] = findFilePaths(line);
    expect(m.path).toBe(path);
    expect(m.line).toBeUndefined();
    // And the underline stops before the digits, because they are not a line.
    expect(line.slice(m.start, m.end)).toBe(path);
  });

  it("still takes a line at the top of the range", () => {
    const [m] = findFilePaths("a.ts:4294967295");
    expect(m.line).toBe(4_294_967_295);
    const [over] = findFilePaths("a.ts:4294967296");
    expect(over.line).toBeUndefined();
  });

  it("stays linear on a long run of punctuation", () => {
    // The shape that backtracked quadratically through an anchored character class
    // (measured 1.6s at 64k before `punctuationSpan` walked from the ends instead).
    // Correctness assertion only; the point is that it returns at all.
    expect(findFilePaths(`${".".repeat(64_000)}x`)).toEqual([]);
  });

  // The full screenful from the report, as one assertion: every line yields exactly
  // the path, and nothing else on the line.
  it("links every line of the reported grep output, and only the path", () => {
    const report = [
      'crates/veld-core/src/ide.rs:340:const SHELL_REFUSED_BUILTINS: &[&str] = &["branch_raw"];',
      "crates/veld-core/src/ide.rs:377:/// class of value as [`SHELL_REFUSED_BUILTINS`]'s `branch_raw`",
      "crates/veld-core/src/ide.rs:2378:/// A second, narrower check rides along: [`SHELL_REFUSED_BUILTINS`]",
      "crates/veld-core/src/ide.rs:2396:        .filter(|n| !is_shell || !SHELL_REFUSED_BUILTINS.contains(n))",
      "crates/veld-core/src/ide.rs:2413:            if is_shell && SHELL_REFUSED_BUILTINS.contains(&name) {",
      "crates/veld-core/src/ide.rs:4860:    /// `SHELL_REFUSED_BUILTINS` only says anything if every name in it is one",
      "crates/veld-core/src/ide.rs:4867:        for name in SHELL_REFUSED_BUILTINS {",
    ];
    for (const line of report) {
      const found = findFilePaths(line);
      expect(found.map((m) => m.path), line).toEqual(["crates/veld-core/src/ide.rs"]);
    }
  });
});

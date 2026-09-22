/**
 * Finding file paths in a line of terminal output.
 *
 * Pure and DOM-free so it can be tested against real agent and compiler output
 * rather than through a rendered terminal — the same reason `terminalKeys.ts` and
 * `dropModel.ts` are split out of their hosts.
 *
 * # What this is allowed to get wrong
 *
 * The known failure mode of clickable paths is the **false positive**: `example.com`,
 * `v1.2.3` and `foo.bar()` are all path-shaped, and a link that underlines and then
 * cannot be opened is worse than no link. So the rule below is deliberately narrow
 * and the daemon checks the filesystem before anything happens — a match here is a
 * candidate, never a promise. Being *silently narrow* is the cheaper mistake: a real
 * path that does not underline costs a copy-paste, where a wrong underline costs
 * trust in every other one.
 *
 * It follows that this file may not grow "helpful" loose cases. A pattern that
 * matches a bare word, or any extension at all, turns ordinary prose into a field of
 * underlines.
 */

/**
 * Extensions a bare, directory-less token may be linked on.
 *
 * The list's job is to separate `README.md` from `example.com`, so it holds the
 * extensions a project's own files have and nothing else. Notably absent: `com`,
 * `net`, `org`, `io`, `sh` — the first four because they are hostnames far more often
 * than files, and the fifth because `install.sh` reads as a path only when written
 * with a directory (`scripts/install.sh`), which rule (a) below already covers.
 */
const LINKABLE_EXTENSIONS = new Set([
  // Rust, and what sits beside it.
  "rs",
  "toml",
  "lock",
  // Web.
  "ts",
  "tsx",
  "js",
  "jsx",
  "mjs",
  "cjs",
  "css",
  "scss",
  "html",
  "vue",
  "svelte",
  // Other languages that turn up in a monorepo.
  "py",
  "go",
  "rb",
  "java",
  "kt",
  "swift",
  "c",
  "h",
  "cc",
  "cpp",
  "hpp",
  "cs",
  "php",
  "sql",
  // Config and docs.
  "json",
  "jsonc",
  "yaml",
  "yml",
  "md",
  "mdx",
  "txt",
  "log",
  "xml",
  "csv",
  "env",
  "ini",
  "conf",
  "gradle",
  "dockerfile",
]);

/** One candidate path found in a line, with the span it occupies. */
export interface PathMatch {
  /** Index of the first character of the path, within the line handed in. */
  start: number;
  /** Index one past the last character — including any `:line:col` tail, so the
   *  underline covers what the reader sees as one thing. */
  end: number;
  /** The path itself, without the tail. Relative paths stay relative: resolving
   *  them is the daemon's job, against the worktree root. */
  path: string;
  /** 1-based line from a `:N` tail, if it carried one. */
  line?: number;
  /** 1-based column from a `:N:M` tail. Parsed so it can be dropped rather than
   *  left dangling in `end`, and currently handed to nothing. */
  column?: number;
}

/**
 * Characters that end a token but are not part of it.
 *
 * `:` is absent on purpose — it separates the line and column, so it belongs to
 * {@link splitLineTail}. `(` is here because `foo.bar()` in a
 * stack trace must reduce to `foo.bar` and be rejected on its extension, and `<>`
 * because a path inside `<...>` is a common way to write a placeholder.
 */
const TRAILING_PUNCTUATION = new Set([
  ".", ",", ";", "!", "?", "(", ")", "]", "}", "'", '"', "`", "<", ">",
]);

/** Characters that open a token but are not part of it. */
const LEADING_PUNCTUATION = new Set(["(", "'", '"', "`", "<", "[", "{"]);

/**
 * How many characters at the start and end of `token` are punctuation.
 *
 * **Walked from the ends rather than matched with an anchored class**, and that is
 * a cost fix rather than a style one. `/[.,;…]+$/` backtracks quadratically on a
 * long run of its own characters that does not reach the end — a token of 64k dots
 * followed by one letter measured at 1.6 seconds, on the main thread, re-run for
 * every row of a wrapped block the mouse crosses. The text comes from whatever
 * wrote the terminal, which this file already treats as untrusted. A walk is linear
 * in the punctuation actually present.
 */
function punctuationSpan(token: string): { lead: number; trail: number } {
  let lead = 0;
  while (lead < token.length && LEADING_PUNCTUATION.has(token[lead])) {
    lead += 1;
  }
  let end = token.length;
  while (end > lead && TRAILING_PUNCTUATION.has(token[end - 1])) {
    end -= 1;
  }
  return { lead, trail: token.length - end };
}

/**
 * A URL scheme at the start of a token.
 *
 * **Not `token.includes("://")`**, which was the first spelling and was wrong in a
 * way only real output shows: `grep -n` on Rust prints
 * `src/ide.rs:377:/// a doc comment`, whose token ends `:377:///` and so *contains*
 * `://`. Every such line silently stopped being a link. A scheme has to look like
 * one — letters then scheme characters then `://` — and a digit-then-colon cannot.
 */
const URL_SCHEME = /^[a-z][a-z0-9+.-]*:\/\//iu;

/** At least one letter or digit, anywhere. */
const HAS_ALNUM = /[A-Za-z0-9]/u;

/** At least one letter. */
const HAS_LETTER = /[A-Za-z]/u;

/**
 * At least one character that is neither a letter nor a slash.
 *
 * This is what separates `crates/veld-daemon/ui/src/panes` from
 * `read/write/execute` — and the second shape is why it exists. Prose writes
 * slash-separated alternatives all the time (`yes/no/maybe`, `input/output/error`,
 * `client/server/proxy`, `he/she/they`), and every one of them satisfied "three
 * segments with a letter in them" when rule (c) was first written. Real path
 * segments overwhelmingly carry a hyphen, underscore, digit or dot somewhere —
 * `veld-daemon`, `0001-pick-a-database`, `.github` — and prose words do not.
 *
 * **The trade, stated because it is a real loss:** an all-letter directory path
 * like `app/models/user` no longer underlines. That is the cheaper mistake by this
 * file's own rule — a missed path costs a copy-paste, a wrong underline costs trust
 * in every other one — and writing it with a trailing slash (`app/models/user/`)
 * links it through rule (a).
 */
const HAS_NON_LETTER_SEGMENT_CHAR = /[^A-Za-z/]/u;

/**
 * A first segment shaped like a hostname — something before a dot and something
 * after it.
 *
 * `example.com/docs/intro` is three segments of letters and would otherwise satisfy
 * rule (c) below. A leading dot does not count, so `.github/workflows/nightly` is
 * still a path: the pattern needs a non-dot character *before* the dot.
 */
const HOSTNAME_FIRST_SEGMENT = /^[^./]+\.[^./]/u;

/**
 * How many segments an **extensionless** token needs before it reads as a path.
 *
 * Two is not enough, and `and/or` is why — prose uses a slash between words, and at
 * two segments there is nothing to tell the two apart. Three is where a token stops
 * looking like a sentence and starts looking like a directory, and it is the shape
 * the case that prompted this rule has: `crates/veld-daemon/ui/src/panes`.
 */
const MIN_EXTENSIONLESS_SEGMENTS = 3;

/**
 * A path the writer marked as one: absolute, or `./`- or `../`-relative.
 *
 * Spelled as one pattern rather than three `startsWith` calls — none of which is
 * redundant (`../x` does not start with `./`), but the intent reads better here.
 */
const EXPLICIT_PATH = /^(?:\.\.?\/|\/)/u;

/** A run of digits, and nothing else. */
const ALL_DIGITS = /^\d+$/u;

/**
 * The largest `:N` this will read as a line number.
 *
 * `u32::MAX`, because that is what the daemon's `ActivateBody.line` is — and serde
 * rejects the **whole request body** on an out-of-range integer, so a number past
 * this does not degrade to "open at line 1", it fails the click outright with a
 * deserializer message. The shape that produces one is ordinary: epoch
 * milliseconds after a linkable extension, as in `trace.json:1758499200000` or
 * `build.log:20260922120000`. Treating it as *not a line* keeps the path clickable
 * and stops the underline before the digits, which is what it is.
 */
const MAX_LINE = 4_294_967_295;

/**
 * The longest whitespace-free run this will even look at.
 *
 * Past every real path (`PATH_MAX` is 4096), and comfortably inside the range where
 * the per-token work left here — a `split(":")` and its allocations — stays cheap.
 * A token longer than this is not a path somebody wants to click; it is a base64
 * blob, a minified line, or output built to be expensive.
 */
const MAX_TOKEN_CHARS = 8 * 1024;

/**
 * The `path`, `line` and `column` in a colon-separated token, and how much of it
 * they account for.
 *
 * Deliberately *not* a regex anchored at the end of the token, which is the obvious
 * spelling and gets `grep -n` wrong: its output is `path:line:content`, so the
 * numbers sit in the middle and an anchored tail never matches. Here the colon parts
 * are consumed left to right for as long as they are numeric — at most two — and
 * whatever follows is somebody's matched line, left out of both the path and the
 * underline.
 */
function splitLineTail(body: string): {
  path: string;
  line?: number;
  column?: number;
  /** Characters of `body` the path and its numbers occupy. */
  width: number;
} {
  const isLine = (part: string | undefined) =>
    part !== undefined && ALL_DIGITS.test(part) && Number(part) <= MAX_LINE;

  const parts = body.split(":");
  const path = parts[0];
  if (!isLine(parts[1])) {
    return { path, width: path.length };
  }
  const line = Number(parts[1]);
  if (!isLine(parts[2])) {
    return { path, line, width: `${path}:${parts[1]}`.length };
  }
  return {
    path,
    line,
    column: Number(parts[2]),
    width: `${path}:${parts[1]}:${parts[2]}`.length,
  };
}

/**
 * Whether `token` is shaped enough like a path to underline.
 *
 * Three independent ways in, and a token needs one:
 *
 * (a) it is **explicitly** a path — it starts with `/`, `./` or `../`, or it ends
 *     with `/`, so whoever wrote it said so;
 * (b) its final segment carries an extension from {@link LINKABLE_EXTENSIONS}; or
 * (c) it has no extension but is unmistakably a **directory path** — see below.
 *
 * A bare `and/or` qualifies under none of them, which is the case that made rule (a)
 * narrower than "contains a slash": prose uses that character too.
 */
function isPathShaped(token: string): boolean {
  if (EXPLICIT_PATH.test(token) || token.endsWith("/")) {
    // **And it must name something.** `///` and `//` start with a slash and are a
    // Rust doc comment and a C/JS comment, which appear on their own in grep output
    // constantly — as bare tokens they were passing rule (a) and underlining.
    return HAS_ALNUM.test(token);
  }
  const segments = token.split("/");
  const lastSegment = segments[segments.length - 1];
  const dot = lastSegment.lastIndexOf(".");
  if (dot === 0) {
    // A dotfile: the leading dot is not an extension separator, so `.env` is judged
    // on `env`.
    return LINKABLE_EXTENSIONS.has(lastSegment.slice(1).toLowerCase());
  }
  if (dot > 0) {
    return LINKABLE_EXTENSIONS.has(lastSegment.slice(dot + 1).toLowerCase());
  }
  // **(c) No extension.** A single bare word is never a path here, even when the
  // whole word is in the table: `go`, `log`, `env`, `conf`, `c` and `h` are
  // ordinary English and they underlined in prose when this branch was looser.
  //
  // But a *directory* has no extension either, and a directory is a legitimate
  // target — the daemon opens one and `ls some/dir` prints them. So a token with
  // enough segments to stop reading as a sentence qualifies, with three guards that
  // each close a real shape: something must contain a letter, or a date like
  // `2026/09/21` becomes a link; something must *not* be a letter, or English
  // slash-lists like `read/write/execute` do; and the first segment must not be
  // hostname-shaped, or `example.com/docs/intro` does. An extensionless *file* is
  // still reachable only through rule (a) — `./Makefile`.
  return (
    segments.length >= MIN_EXTENSIONLESS_SEGMENTS &&
    segments.some((segment) => HAS_LETTER.test(segment)) &&
    HAS_NON_LETTER_SEGMENT_CHAR.test(token) &&
    !HOSTNAME_FIRST_SEGMENT.test(segments[0])
  );
}

/**
 * The candidate paths in one **logical** line of terminal output.
 *
 * Callers hand in a line already un-wrapped, because a path broken across two
 * visual rows is still one path and splitting it is how a link comes out truncated.
 *
 * URLs are skipped rather than competed with: `WebLinksAddon` already owns them, and
 * `https://example.com/a.ts` would otherwise match rule (b) on its own tail.
 */
export function findFilePaths(text: string): PathMatch[] {
  const out: PathMatch[] = [];
  // **A token is bounded before anything else looks at it.** The quadratic cost
  // this started as defence against is gone — `punctuationSpan` walks from the ends
  // instead of backtracking through an anchored class — so this is now
  // defence-in-depth against the linear-but-real work that remains: a `split(":")`
  // and its allocations, run per token, re-run for every row of a wrapped block the
  // mouse crosses. The text comes from whatever wrote the terminal, which this file
  // already treats as untrusted, so the length of one token is not veld's to assume.
  // Nothing real is lost: the longest path any filesystem accepts is 4096 bytes.
  const oversized = (token: string) => token.length > MAX_TOKEN_CHARS;
  // Whitespace is the only separator. A path containing a space cannot be found in
  // rendered output at all — nothing distinguishes it from two tokens — and guessing
  // is how a link swallows the next word.
  for (const m of text.matchAll(/\S+/gu)) {
    const raw = m[0];
    if (oversized(raw)) {
      continue;
    }
    const { lead, trail } = punctuationSpan(raw);
    const body = raw.slice(lead, raw.length - trail);
    if (!body) {
      continue;
    }
    // Anything carrying a scheme belongs to the URL addon, including the `file://`
    // case: it is already absolute and already a link there. Tested after the
    // leading punctuation is off, so `(https://…)` is still recognised.
    if (URL_SCHEME.test(body)) {
      continue;
    }
    const { path, line, column, width } = splitLineTail(body);
    if (!path || !isPathShaped(path)) {
      continue;
    }
    const start = m.index + lead;
    out.push({
      start,
      // `width`, not the token's length: it stops before a sentence's full stop, and
      // before a grep hit's matched text.
      end: start + width,
      path,
      ...(line === undefined ? {} : { line }),
      ...(column === undefined ? {} : { column }),
    });
  }
  return out;
}

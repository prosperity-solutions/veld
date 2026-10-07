/**
 * Markdown, rendered for the file pane.
 *
 * # What keeps this safe on `/ide`'s origin
 *
 * The rendered HTML goes into this page with `innerHTML`, on the origin that holds
 * the management API — so the one thing a file must never be able to do is put
 * markup of its own here. Two layers, each sufficient on its own for what it
 * covers:
 *
 * 1. **markdown-it with `html: false`.** Raw HTML in the source is escaped as text,
 *    not passed through, and its default `validateLink` refuses `javascript:`,
 *    `vbscript:`, `file:` and non-image `data:` URLs before a link is rendered.
 * 2. **DOMPurify over the result**, so a renderer bug or a future plugin cannot
 *    turn into script on this origin.
 *
 * There is no Content-Security-Policy on `/ide` behind these, deliberately for now:
 * the page is one inlined file whose scripts and styles a strict policy would have
 * to hash, and these two layers are what stands in for it. Say so here rather than
 * let somebody assume a CSP is catching what these miss.
 *
 * # What is deliberately not rendered
 *
 * **Images.** A remote image is a request this page makes on a file's say-so — a
 * read receipt for whoever wrote the file, from inside the management origin — and
 * a local one would need a route this pane does not have. So an image renders as
 * its alt text, bracketed, which is also what tells you something was there.
 *
 * # Source lines
 *
 * Every block carries the source lines it came from (`data-src-start`/`-end`, from
 * markdown-it's `token.map`), which is what lets you select a paragraph, a list item
 * or a fenced block in the *rendered* view and copy a reference to the lines of the
 * file it is.
 */

import DOMPurify from "dompurify";
import MarkdownIt from "markdown-it";

const md = new MarkdownIt({ html: false, linkify: true, typographer: false });

/**
 * Stamp every block token with the source lines it covers.
 *
 * `map` is `[firstLine, lineAfterLast)`, 0-based — so `+ 1` on the start and the
 * end as-is gives a 1-based inclusive range. Inline tokens have no map and are
 * skipped, which is right: a reference names lines, and an emphasis span does not
 * have any of its own.
 */
md.core.ruler.push("veld_source_lines", (state) => {
  for (const token of state.tokens) {
    if (!token.map || token.nesting === -1) continue;
    token.attrSet("data-src-start", String(token.map[0] + 1));
    token.attrSet("data-src-end", String(token.map[1]));
  }
});

md.renderer.rules.image = (tokens, idx) => {
  const alt = tokens[idx].content.trim();
  return `<span class="md-image-alt">[${md.utils.escapeHtml(alt || "image")}]</span>`;
};

/** The sanitized HTML for a Markdown document. */
export function renderMarkdown(text: string): string {
  // A UTF-8 byte-order mark (Windows editors write one) is a character to
  // markdown-it, so `\uFEFF# Title` is a paragraph, not a heading. Dropping it
  // removes no line, so every `data-src-*` below still counts the file's lines.
  const source = text.startsWith("\uFEFF") ? text.slice(1) : text;
  return DOMPurify.sanitize(md.render(source), {
    // Belt and braces with the image rule above: no element that can make this
    // page fetch something survives, whatever produced it.
    FORBID_TAGS: ["img", "picture", "source", "video", "audio", "iframe", "style", "form"],
    FORBID_ATTR: ["style", "srcset"],
  });
}

/** Where a link in a rendered Markdown file goes. */
export type LinkTarget =
  | { kind: "external"; url: string }
  /** A path, resolved against the linking file's folder. */
  | { kind: "file"; path: string; line?: number }
  /** In-page, or something with nowhere to go. */
  | { kind: "none" };

/**
 * Resolve a link's `href` from the file at `from` (a display path: relative to the
 * worktree, or absolute).
 *
 * `http(s)` and `mailto` leave Veld — this pane is not a browser, and a page opened
 * from a document belongs in a real one. Anything without a scheme is a path next to
 * the linking file; the caller decides whether that path is something a file pane
 * can show. A `#L12` fragment (GitHub's line anchor) becomes the line to open at;
 * any other fragment is dropped, since headings carry no ids here.
 */
export function resolveLink(from: string, href: string): LinkTarget {
  const raw = href.trim();
  if (raw === "" || raw.startsWith("#")) return { kind: "none" };
  if (/^(https?:|mailto:)/i.test(raw)) return { kind: "external", url: raw };
  // Any other scheme — or a protocol-relative `//host` — is not a file next to this one.
  if (/^[a-z][a-z0-9+.-]*:/i.test(raw) || raw.startsWith("//")) return { kind: "none" };
  const [pathPart, fragment] = raw.split("#", 2);
  const withoutQuery = pathPart.split("?")[0];
  let decoded: string;
  try {
    decoded = decodeURIComponent(withoutQuery);
  } catch {
    return { kind: "none" };
  }
  if (decoded === "") return { kind: "none" };
  const line = fragment?.match(/^L(\d+)/)?.[1];
  const path = decoded.startsWith("/") ? normalize(decoded) : joinFrom(from, decoded);
  if (path === null) return { kind: "none" };
  return line ? { kind: "file", path, line: Number(line) } : { kind: "file", path };
}

function joinFrom(from: string, rel: string): string | null {
  const dir = from.includes("/") ? from.slice(0, from.lastIndexOf("/")) : "";
  return normalize(dir === "" ? rel : `${dir}/${rel}`);
}

/**
 * Collapse `.` and `..`. A relative path that climbs out of the worktree root has
 * nowhere to resolve to on this side, so it is `null`; the daemon would refuse it
 * anyway, and saying nothing beats a pane that says "not found" about a path the
 * user never typed.
 */
function normalize(path: string): string | null {
  const absolute = path.startsWith("/");
  const out: string[] = [];
  for (const segment of path.split("/")) {
    if (segment === "" || segment === ".") continue;
    if (segment === "..") {
      if (out.length === 0) {
        if (absolute) continue;
        return null;
      }
      out.pop();
      continue;
    }
    out.push(segment);
  }
  if (out.length === 0) return null;
  return `${absolute ? "/" : ""}${out.join("/")}`;
}

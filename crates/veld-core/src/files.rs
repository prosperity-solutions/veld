//! Which local files a browser pane may show, and what to call them on the wire.
//!
//! Two questions live here, and keeping them apart is the point of the module:
//!
//! - **Viewable** — would the user want this *opened* in a pane? That is policy,
//!   answered from settings ([`ViewPolicy`]), and it gates the two places a file
//!   becomes a pane: the `open` shim's interception and the recently-edited list.
//! - **Servable** — may these bytes be sent at all? That is a much wider set,
//!   because a slide deck's `<link>`, `<script src>`, fonts and images have to load
//!   or the deck renders as unstyled text. It is gated by [`servable_type`] alone.
//!
//! Conflating them fails in both directions: gate serving on the viewable set and
//! every deck loses its stylesheet; gate opening on the servable set and a pane
//! offers to "view" a `.woff2`.
//!
//! # What confines a read is the grant, not this module
//!
//! Bytes are only ever served from inside a worktree root, resolved server-side
//! from an unguessable grant (see `veld-daemon/src/files.rs`). That bound is what
//! makes the threat model tolerable: a page served this way is same-origin with the
//! file server, so its scripts can fetch its siblings and send them anywhere — but
//! its siblings are files in a worktree where the agent that wrote the page already
//! had read access and a network. The pane grants no new reach.
//!
//! [`is_sensitive`] is therefore **defence in depth, not the boundary**. It exists
//! for the narrower case the grant does not cover: HTML somebody else wrote, sitting
//! in a worktree, whose author never had code execution there.

use std::path::Path;

/// The user's answer to "which files should Veld offer to open?".
///
/// Resolved from settings by the daemon and passed in, rather than read here: this
/// crate is compiled into the CLI too, and a policy that reads a database from a
/// pure predicate is a policy with two sources.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewPolicy {
    /// `files.viewWebPages` — `.html`, `.htm`.
    pub web_pages: bool,
    /// `files.viewImages` — raster and vector images Chromium renders directly.
    pub images: bool,
    /// `files.viewPdfs` — `.pdf`, via Chromium's built-in viewer.
    pub pdfs: bool,
    /// `files.viewPlainText` — text Chromium shows verbatim rather than rendering.
    pub plain_text: bool,
    /// `files.viewPatterns` — extra globs, for the kinds no group covers.
    pub patterns: Vec<String>,
}

/// Extensions covered by `files.viewWebPages`.
const WEB_PAGE_EXTS: &[&str] = &["html", "htm"];

/// Extensions covered by `files.viewImages`.
///
/// `svg` is here rather than with the web pages deliberately. It renders as an
/// image, and a user enabling "images" means "show me pictures" — but note that an
/// SVG opened as a *document* can carry script, which is why it is served with its
/// own type and never sniffed.
const IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "avif", "svg", "ico", "bmp",
];

/// Extensions covered by `files.viewPdfs`.
const PDF_EXTS: &[&str] = &["pdf"];

/// Extensions covered by `files.viewPlainText`.
const PLAIN_TEXT_EXTS: &[&str] = &[
    "txt", "log", "json", "csv", "tsv", "md", "yaml", "yml", "toml", "xml",
];

/// Whether a path is one the user wants opened in a pane.
///
/// Takes the relative path rather than just the extension because a pattern may
/// speak about location (`reports/*.xml`), which an extension cannot.
#[must_use]
pub fn is_viewable(rel_path: &str, policy: &ViewPolicy) -> bool {
    if is_sensitive(rel_path) {
        return false;
    }
    let ext = extension_of(rel_path);
    let by_group = match ext.as_deref() {
        Some(e) if WEB_PAGE_EXTS.contains(&e) => policy.web_pages,
        Some(e) if IMAGE_EXTS.contains(&e) => policy.images,
        Some(e) if PDF_EXTS.contains(&e) => policy.pdfs,
        Some(e) if PLAIN_TEXT_EXTS.contains(&e) => policy.plain_text,
        _ => false,
    };
    if by_group {
        return true;
    }
    policy
        .patterns
        .iter()
        .any(|p| glob_matches(p.trim(), rel_path))
}

/// The lowercased extension of a path, if it has one.
#[must_use]
pub fn extension_of(path: &str) -> Option<String> {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// The `Content-Type` to serve a file as, or `None` if Veld will not serve it.
///
/// A closed table rather than a sniffer, and the absence of a fallback to
/// `application/octet-stream` is the security-relevant half: an extension nobody
/// listed is a 404, so a stray `id_rsa` or `.sqlite` is never a download. Paired
/// with `X-Content-Type-Options: nosniff` at the response, since a wrong-but-listed
/// type must not be re-guessed by the renderer either.
#[must_use]
pub fn servable_type(path: &str) -> Option<&'static str> {
    let ext = extension_of(path)?;
    Some(match ext.as_str() {
        // Documents a pane can be pointed at.
        "html" | "htm" => "text/html; charset=utf-8",
        "pdf" => "application/pdf",
        // Images.
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        // Text, and the data a report loads about itself.
        "txt" | "log" => "text/plain; charset=utf-8",
        "md" => "text/plain; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "tsv" => "text/tab-separated-values; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "yaml" | "yml" => "text/plain; charset=utf-8",
        "toml" => "text/plain; charset=utf-8",
        // Subresources. Not viewable, always servable — a deck without these is a
        // deck that renders as a wall of unstyled text.
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        // Media a deck embeds.
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "ogg" | "oga" => "audio/ogg",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        _ => return None,
    })
}

/// Paths never served, whatever the extension table says.
///
/// Defence in depth rather than the boundary — see the module docs. Deliberately
/// small and about *categories of secret*, because a deny list that tries to be
/// exhaustive is a deny list that reads as a guarantee.
///
/// `.git` is here for a different reason than the rest: nothing in it is servable
/// by extension anyway, but a repository's object store is the one directory whose
/// accidental exposure leaks every file the worktree ever had, including the ones
/// somebody deleted for exactly that reason.
#[must_use]
pub fn is_sensitive(rel_path: &str) -> bool {
    let lower = rel_path.to_ascii_lowercase();
    lower.split('/').any(|segment| {
        segment == ".git"
            || segment == ".ssh"
            || segment == ".env"
            || segment.starts_with(".env.")
            || segment == ".npmrc"
            || segment == ".netrc"
            || segment == "credentials"
            || segment.starts_with("id_rsa")
            || segment.starts_with("id_ed25519")
            || segment.ends_with(".pem")
            || segment.ends_with(".key")
            || segment.ends_with(".p12")
            || segment.ends_with(".keystore")
    })
}

/// Directory names a recency scan never descends into.
///
/// **Not gitignore.** Using the ignore rules would be more principled and is the
/// obvious suggestion, but it hides exactly the file this feature exists to show:
/// this repo gitignores `/notes/`, and its own `AGENTS.md` tells agents to write
/// analyses and plans there. A hardcoded list of heavy directories keeps the scan
/// fast without deciding that a user's scratch directory does not exist.
pub const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".svelte-kit",
    "vendor",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".cargo",
    ".gradle",
    "Pods",
    ".terraform",
    ".veld",
];

/// Whether a glob matches a relative path.
///
/// A deliberately small grammar, because the setting it serves is an escape hatch
/// and not a query language:
///
/// - `?` — one character, never `/`.
/// - `*` — any run of characters, never crossing `/`.
/// - `**` — any run, including `/`.
/// - a pattern with **no** `/` matches against the file name alone, so `*.mmd`
///   finds one at any depth. A pattern containing `/` matches the whole relative
///   path, so `reports/*.xml` means what it looks like.
///
/// Case-insensitive, matching the extension groups beside it.
#[must_use]
pub fn glob_matches(pattern: &str, rel_path: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    let subject = if pattern.contains('/') {
        rel_path
    } else {
        rel_path.rsplit('/').next().unwrap_or(rel_path)
    };
    let pattern = pattern.to_ascii_lowercase();
    let subject = subject.to_ascii_lowercase();
    glob_here(pattern.as_bytes(), subject.as_bytes())
}

/// Backtracking glob match over bytes.
///
/// Recursive on `*` only, and the recursion is bounded by the subject's length
/// because each step consumes at least one byte of it.
fn glob_here(pattern: &[u8], subject: &[u8]) -> bool {
    match pattern.first() {
        None => subject.is_empty(),
        Some(b'*') => {
            // `**` crosses separators; a single `*` stops at one.
            let (rest, crosses) = match pattern.get(1) {
                Some(b'*') => (&pattern[2..], true),
                _ => (&pattern[1..], false),
            };
            // Try the shortest expansion first, growing one byte at a time.
            for taken in 0..=subject.len() {
                if !crosses && subject[..taken].contains(&b'/') {
                    break;
                }
                if glob_here(rest, &subject[taken..]) {
                    return true;
                }
            }
            false
        }
        Some(b'?') => match subject.first() {
            Some(&c) if c != b'/' => glob_here(&pattern[1..], &subject[1..]),
            _ => false,
        },
        Some(&p) => match subject.first() {
            Some(&c) if c == p => glob_here(&pattern[1..], &subject[1..]),
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> ViewPolicy {
        ViewPolicy {
            web_pages: true,
            images: true,
            pdfs: true,
            plain_text: false,
            patterns: vec![],
        }
    }

    #[test]
    fn the_default_groups_cover_the_deck_case_and_not_the_noise() {
        let p = policy();
        for yes in [
            "deck.html",
            "notes/analysis.HTM",
            "report.pdf",
            "img/diagram.svg",
            "shot.PNG",
        ] {
            assert!(is_viewable(yes, &p), "{yes}");
        }
        // Plain text is off by default, and a subresource is never *viewable*.
        for no in [
            "README.md",
            "data.json",
            "styles.css",
            "app.js",
            "font.woff2",
            "Cargo.lock",
            "binary",
        ] {
            assert!(!is_viewable(no, &p), "{no}");
        }
    }

    #[test]
    fn plain_text_is_one_switch_away() {
        let mut p = policy();
        assert!(!is_viewable("README.md", &p));
        p.plain_text = true;
        assert!(is_viewable("README.md", &p));
        assert!(is_viewable("logs/run.log", &p));
        // Still not a stylesheet: enabling text does not enable subresources.
        assert!(!is_viewable("styles.css", &p));
    }

    #[test]
    fn a_custom_pattern_reaches_what_no_group_covers() {
        let mut p = policy();
        p.patterns = vec!["*.mmd".to_owned(), "reports/*.xml".to_owned()];
        assert!(is_viewable("chart.mmd", &p));
        assert!(
            is_viewable("deep/nested/chart.mmd", &p),
            "bare name, any depth"
        );
        assert!(is_viewable("reports/q3.xml", &p));
        // The pattern has a slash, so it means that location and not any other.
        assert!(!is_viewable("other/q3.xml", &p));
        assert!(
            !is_viewable("reports/deep/q3.xml", &p),
            "* stops at a slash"
        );
    }

    #[test]
    fn a_pattern_can_never_reach_a_sensitive_path() {
        let mut p = policy();
        // Even the most permissive pattern a user could write.
        p.patterns = vec!["**".to_owned()];
        for secret in [
            ".env",
            ".env.local",
            "config/.env.production",
            ".git/config",
            "deploy.pem",
            "certs/server.key",
            ".ssh/id_rsa",
            "aws/credentials",
        ] {
            assert!(!is_viewable(secret, &p), "{secret}");
            assert!(is_sensitive(secret), "{secret}");
        }
        // …while an ordinary file under the same pattern is still reachable.
        assert!(is_viewable("anything.bin", &p));
    }

    #[test]
    fn the_servable_table_is_closed() {
        assert_eq!(servable_type("deck.html"), Some("text/html; charset=utf-8"));
        assert_eq!(
            servable_type("a/b/styles.CSS"),
            Some("text/css; charset=utf-8")
        );
        assert_eq!(servable_type("f.woff2"), Some("font/woff2"));
        // No fallback: an unlisted extension is not a download, it is a 404.
        for refused in ["id_rsa", "db.sqlite", "secrets.env", "Makefile", "a.pem"] {
            assert_eq!(servable_type(refused), None, "{refused}");
        }
    }

    #[test]
    fn the_glob_grammar_is_the_documented_one() {
        assert!(glob_matches("*.html", "deck.html"));
        assert!(
            glob_matches("*.html", "a/b/deck.html"),
            "bare name, any depth"
        );
        assert!(glob_matches("docs/*.html", "docs/deck.html"));
        assert!(
            !glob_matches("docs/*.html", "docs/a/deck.html"),
            "* stops at /"
        );
        assert!(glob_matches("docs/**/*.html", "docs/a/b/deck.html"));
        assert!(glob_matches("deck?.html", "deck1.html"));
        assert!(!glob_matches("deck?.html", "deck12.html"));
        assert!(!glob_matches("", "anything"));
        // Case-insensitive on both sides.
        assert!(glob_matches("*.HTML", "Deck.html"));
    }

    /// A `*`-heavy pattern must not blow the stack on a long path.
    #[test]
    fn a_pathological_pattern_terminates() {
        let pattern = "*a*a*a*a*a*a*a*a*b";
        let subject = "a".repeat(64);
        assert!(!glob_matches(pattern, &subject));
    }
}

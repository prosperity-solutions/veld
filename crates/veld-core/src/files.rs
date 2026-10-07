//! Which local files a browser pane may show, and what to call them on the wire.
//!
//! Two questions live here, and keeping them apart is the point of the module:
//!
//! - **Viewable** — would the user want this *opened* in a pane? That is policy,
//!   answered from settings ([`ViewPolicy`]), and it gates the two places a file
//!   becomes a pane: the `open` shim's interception and the recently-edited list.
//!   Viewable is a **subset** of servable: a `files.viewPatterns` entry chooses among
//!   the types below, it cannot add one — see [`is_viewable`].
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
///
/// Also the file pane's `sourceOnly` kind ([`readable_text_kind`]), so it is in
/// `text_kinds.json` — see the note above [`MARKDOWN_EXTS`].
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
    // **A pattern selects among servable types; it cannot invent one.** Without this
    // gate a pattern for an extension the table has never heard of (`*.mmd`) made a
    // file *viewable* that `servable_type` then refused — so the row was silently
    // dropped from the list, and `open chart.mmd` reported that the file-serving route
    // was not registered, blaming the helper for a decision this table made.
    //
    // The alternative was a fallback content type for pattern-matched files, which
    // would let the setting name any extension. Rejected for one reason: it moves the
    // closed-table property (an unlisted extension is a 404, never a download) behind a
    // user setting, and that property is load-bearing for a server whose whole job is
    // handing bytes to a browser. What remains is the useful half — a pattern scopes by
    // *location* (`reports/*.xml`) or opts one extension in without its whole group
    // (`*.log` with plain text off).
    if servable_type(rel_path).is_none() {
        return false;
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

/// How the file pane renders a text file, decided by name alone.
///
/// The `/ide` bundle mirrors this table to pick a renderer; the daemon and the CLI
/// use it to *route* — a path with a kind opens in the file pane, anything else
/// goes to a browser pane or nowhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TextKind {
    Markdown,
    Csv,
    Tsv,
    /// Source, config and data shown with line numbers and highlighting.
    Code,
    /// Prose or logs with no syntax worth highlighting.
    Plain,
}

// **The text-kind lists below have a second copy** in the `/ide` bundle
// (`veld-daemon/ui/src/files/textKind.ts`), and both are checked against one
// fixture, `text_kinds.json` beside this file: the test
// `text_kind_lists_match_the_shared_fixture` here and "agrees with the shared
// text-kind fixture" in `ui/src/files/files.test.ts`. Change a list, change the
// fixture, and the other side's test says what it is missing.

/// Extensions the file pane renders as Markdown.
const MARKDOWN_EXTS: &[&str] = &["md", "markdown"];

/// Extensions the file pane shows as plain text: prose or logs, no highlighting.
const PLAIN_EXTS: &[&str] = &["txt", "log"];

/// Extensions the file pane renders as code.
///
/// A closed list, like [`servable_type`], for the same reason: the read behind it
/// sends the whole file to a renderer, and "anything that decodes as UTF-8" would
/// include a lockfile's worth of noise and every extensionless secret.
const CODE_EXTS: &[&str] = &[
    "json", "jsonc", "yaml", "yml", "toml", "xml", "ini", "sql", "sh", "bash", "zsh", "rs", "ts",
    "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "py", "rb", "go", "java", "kt", "swift", "c",
    "h", "cpp", "hpp", "cs", "php", "css", "scss", "graphql", "proto", "diff", "patch",
];

/// Extensionless file names the file pane renders as code.
const CODE_NAMES: &[&str] = &["Dockerfile", "Makefile", "justfile"];

/// The kind of text file `path` is, for **routing**: `None` means "not for the file
/// pane".
///
/// HTML is deliberately absent. A page an agent wrote is meant to be *seen*, and
/// that is the browser pane's job; [`readable_text_kind`] is the wider question the
/// file pane asks when somebody explicitly wants a page's source.
#[must_use]
pub fn text_kind(path: &str) -> Option<TextKind> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if CODE_NAMES.contains(&name) {
        return Some(TextKind::Code);
    }
    let ext = extension_of(name)?;
    Some(match ext.as_str() {
        e if MARKDOWN_EXTS.contains(&e) => TextKind::Markdown,
        "csv" => TextKind::Csv,
        "tsv" => TextKind::Tsv,
        e if PLAIN_EXTS.contains(&e) => TextKind::Plain,
        e if CODE_EXTS.contains(&e) => TextKind::Code,
        _ => return None,
    })
}

/// [`text_kind`], plus the kinds the file pane will show as source when it is asked
/// for one by name — today only `.html`/`.htm`.
///
/// The split exists so that "open this page" keeps meaning the rendered page
/// everywhere a path is routed, while a file pane that was pointed at one (from the
/// changed-files list, say) can still read it rather than refuse.
#[must_use]
pub fn readable_text_kind(path: &str) -> Option<TextKind> {
    text_kind(path).or_else(|| {
        extension_of(path)
            .is_some_and(|e| WEB_PAGE_EXTS.contains(&e.as_str()))
            .then_some(TextKind::Code)
    })
}

/// Whether bytes look like a binary file rather than text: a NUL in the first 8 KB.
///
/// The heuristic git and most editors use. It misreads UTF-16, which is rare enough
/// in a worktree that refusing it with a clear message beats rendering mojibake.
#[must_use]
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8 * 1024).any(|b| *b == 0)
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
            // `credentials` alone could never fire on a *servable* path: an
            // extensionless file has no content type, so it is already a 404. The
            // spelling that is servable is `credentials.json` — the standard OAuth
            // client-secret filename — which is `application/json` and would otherwise
            // be fetchable by any page served from the same grant.
            || segment == "credentials"
            || segment.starts_with("credentials.")
            || segment.starts_with("secrets.")
            || segment.starts_with("service-account")
            || segment.starts_with("id_rsa")
            || segment.starts_with("id_ed25519")
            || segment.ends_with(".pem")
            || segment.ends_with(".key")
            || segment.ends_with(".p12")
            || segment.ends_with(".keystore")
    })
}

/// Home-relative prefixes a file pane never reads **outside a worktree**, whatever
/// grant or `files.extraFolders` entry names them. See [`is_home_credential`].
const HOME_CREDENTIAL_DIRS: &[&str] = &[
    ".ssh/",
    ".gnupg/",
    ".aws/",
    ".kube/",
    ".docker/",
    ".codex/",
    ".config/gh/",
    ".config/gcloud/",
];

/// Home-relative files refused for the same reason as [`HOME_CREDENTIAL_DIRS`].
const HOME_CREDENTIAL_FILES: &[&str] = &[
    ".netrc",
    ".npmrc",
    ".yarnrc.yml",
    ".pypirc",
    ".git-credentials",
];

/// File names that hold a login token wherever they sit.
const CREDENTIAL_NAMES: &[&str] = &["auth.json", "hosts.yml", "token", "tokens.json"];

/// Whether `full` — canonical, absolute, **outside** the worktree — is one of the
/// places a CLI keeps a login, on top of [`is_sensitive`].
///
/// `is_sensitive` is about categories of secret anywhere in a checkout. This is
/// about the home folder, where a grant or an extra folder can point and where the
/// secrets have fixed, well-known homes with harmless-looking extensions:
/// `~/.config/gh/hosts.yml`, `~/.aws/config`, `~/.docker/config.json`. A click on
/// a path an agent printed is enough to grant a read, so these are refused however
/// the file was named.
///
/// `~/.claude` is the one folder split rather than refused whole, because it is
/// also where Claude Code writes the plans and memories an agent prints the path
/// of: only Markdown under `.claude/plans/` and `.claude/projects/` reads, and
/// everything else there (settings, `.credentials.json`, transcripts) does not.
///
/// `home` is the canonical home folder, passed in so the rule is testable.
#[must_use]
pub fn is_home_credential(full: &Path, home: Option<&Path>) -> bool {
    let name = full
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.contains("credential") || CREDENTIAL_NAMES.contains(&name.as_str()) {
        return true;
    }
    let Some(rel) = home
        .and_then(|h| full.strip_prefix(h).ok())
        .and_then(Path::to_str)
    else {
        return false;
    };
    let rel = rel.to_ascii_lowercase();
    if HOME_CREDENTIAL_FILES.contains(&rel.as_str())
        || HOME_CREDENTIAL_DIRS.iter().any(|d| rel.starts_with(d))
    {
        return true;
    }
    if let Some(inner) = rel.strip_prefix(".claude/") {
        let plan_or_memory = inner.starts_with("plans/") || inner.starts_with("projects/");
        return !(plan_or_memory && extension_of(&rel).as_deref() == Some("md"));
    }
    false
}

/// The folders under `$XDG_CONFIG_HOME` a CLI keeps a login in — the
/// `.config/…` entries of [`HOME_CREDENTIAL_DIRS`], for a config folder that is
/// not `~/.config`.
const CONFIG_CREDENTIAL_DIRS: &[&str] = &["gh/", "gcloud/"];

/// Whether `full` is under one of [`CONFIG_CREDENTIAL_DIRS`] in `config`, the
/// user's `$XDG_CONFIG_HOME`. [`is_home_credential`] covers `~/.config`; `gh` and
/// `gcloud` follow the variable when it is set, so their logins can live anywhere.
#[must_use]
pub fn is_config_credential(full: &Path, config: &Path) -> bool {
    full.strip_prefix(config)
        .ok()
        .and_then(Path::to_str)
        .map(str::to_ascii_lowercase)
        .is_some_and(|rel| CONFIG_CREDENTIAL_DIRS.iter().any(|d| rel.starts_with(d)))
}

/// `p` in lexical normal form: `.` segments dropped and `..` applied to the
/// spelling, without asking the filesystem. **Not** what the kernel would open
/// when a symlink sits before a `..` — so use it to judge a spelling (a guard that
/// refuses), never to pick the file that is read.
#[must_use]
pub fn lexical(p: &Path) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for part in p.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether a `files.extraFolders` entry would let a file pane read everything:
/// `/`, the home folder, or any folder above it.
///
/// Judged on the **lexical** normal form — `.` segments dropped, `..` applied,
/// trailing `/` ignored — so `/.`, `~/./` and `/Users/me/..` are the folders they
/// name rather than strings that happen not to equal `/` or `~`. `folder` has its
/// `~` expanded already; `home` is the home folder in the same form.
#[must_use]
pub fn folder_too_broad(folder: &Path, home: Option<&Path>) -> bool {
    let folder = lexical(folder);
    folder.parent().is_none() || home.is_some_and(|h| lexical(h).starts_with(&folder))
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

/// One element of a compiled pattern.
///
/// Compiled rather than walked as bytes so the matcher below can index tokens by
/// position — which is what makes a table possible, and a table is what makes the
/// cost bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    /// `*` — any run within one path segment. `**` when `crosses`.
    Star {
        crosses: bool,
    },
    /// `?` — one character, never `/`.
    Any,
    Lit(u8),
}

fn compile(pattern: &[u8]) -> Vec<Tok> {
    let mut out = Vec::with_capacity(pattern.len());
    let mut i = 0;
    while i < pattern.len() {
        match pattern[i] {
            b'*' => {
                let crosses = pattern.get(i + 1) == Some(&b'*');
                // Any further run of `*` adds nothing, and collapsing it here is what
                // keeps `****` from being four table rows.
                while pattern.get(i) == Some(&b'*') {
                    i += 1;
                }
                out.push(Tok::Star { crosses });
            }
            b'?' => {
                out.push(Tok::Any);
                i += 1;
            }
            c => {
                out.push(Tok::Lit(c));
                i += 1;
            }
        }
    }
    out
}

/// Glob match, in time proportional to pattern length times subject length.
///
/// The glob grammar `files.viewPatterns` speaks, deliberately small — it is an escape
/// hatch, not a query language:
///
/// - `?` — one character, never `/`.
/// - `*` — any run of characters, never crossing `/`.
/// - `**` — any run, including `/`.
/// - a pattern with **no** `/` matches against the file name alone, so `*.log` finds
///   one at any depth. A pattern containing `/` matches the whole relative path, so
///   `reports/*.xml` means what it looks like.
///
/// Case-insensitive, matching the extension groups beside it.
///
/// **A pattern reaches only what the walk reaches.** `SKIP_DIRS` is applied before a
/// directory is descended into, so no pattern can name anything under `target`,
/// `node_modules`, `dist`, `build` or `vendor` — `target/doc/**/*.html` matches
/// nothing, however well-formed it looks. That asymmetry is deliberate (those trees
/// are where a scan's time goes) and is stated here because a location-shaped example
/// invites exactly that pattern.
///
/// # Why this is a table and not the obvious recursion
///
/// The first version recursed on `*`, trying each split in turn. That is the textbook
/// implementation and it backtracks **combinatorially**: measured on this exact
/// grammar with subject `"a" * 64`, one added `*` multiplied the work by ~7.3 —
/// 5 stars 0.08s, 6 stars 0.78s, 7 stars 6.2s, 8 stars 53.8s, 9 stars 319s
/// (4.3e10 calls), in release. `files.viewPatterns` accepts 64 patterns of
/// 256 bytes, so ~85 stars was
/// reachable, and `is_viewable` runs every pattern against every candidate in a scan.
/// The daemon's scan bounds could not save it: they are checked *between* directory
/// entries, never inside a match, so one pattern pinned a blocking thread for as long
/// as it liked and `open-file` hung with it.
///
/// The table has no such cliff: `tokens × (subject + 1)` booleans, filled once. The
/// worst case a setting can now express is ~256 × ~4096 ≈ 1M steps, which is a
/// millisecond — so the scan's own budget is once again the thing that bounds a scan.
///
/// Rows are filled from the end, and a `Star` row reads *itself* at `j + 1` (that is
/// the "consume one more character" case), which is why `j` descends.
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
    let toks = compile(pattern.as_bytes());
    let s = subject.as_bytes();
    let n = s.len();

    // `next[j]` = "the tokens after this one match `s[j..]`". Seeded with the empty
    // pattern, which matches only the empty remainder.
    let mut next: Vec<bool> = (0..=n).map(|j| j == n).collect();
    let mut cur: Vec<bool> = vec![false; n + 1];

    for tok in toks.iter().rev() {
        match *tok {
            Tok::Star { crosses } => {
                for j in (0..=n).rev() {
                    // Consume nothing, or one more character and stay on this token.
                    let more = j < n && (crosses || s[j] != b'/') && cur[j + 1];
                    cur[j] = next[j] || more;
                }
            }
            Tok::Any => {
                for j in 0..=n {
                    cur[j] = j < n && s[j] != b'/' && next[j + 1];
                }
            }
            Tok::Lit(c) => {
                for j in 0..=n {
                    cur[j] = j < n && s[j] == c && next[j + 1];
                }
            }
        }
        std::mem::swap(&mut cur, &mut next);
    }
    next[0]
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

    /// A pattern scopes by location, or opts one extension in without its group.
    #[test]
    fn a_custom_pattern_selects_among_servable_types() {
        let mut p = policy();
        // Plain text is off, so `.xml` and `.log` are not viewable by group…
        assert!(!is_viewable("reports/q3.xml", &p));
        assert!(!is_viewable("run.log", &p));
        // …and a pattern brings back exactly the ones asked for.
        p.patterns = vec!["reports/*.xml".to_owned(), "*.log".to_owned()];
        assert!(is_viewable("reports/q3.xml", &p));
        assert!(is_viewable("run.log", &p));
        assert!(
            is_viewable("deep/nested/run.log", &p),
            "bare name, any depth"
        );
        // The pattern has a slash, so it means that location and not any other.
        assert!(!is_viewable("other/q3.xml", &p));
        assert!(
            !is_viewable("reports/deep/q3.xml", &p),
            "* stops at a slash"
        );
    }

    /// A pattern cannot make a file viewable that the server would not serve.
    ///
    /// This is the pairing that was broken: `is_viewable` said yes, `servable_type`
    /// said no, and the caller then reported the *route* as unregistered. Whatever the
    /// two answer, they must agree in this direction — viewable implies servable.
    #[test]
    fn a_pattern_cannot_invent_a_servable_type() {
        let mut p = policy();
        p.patterns = vec!["*.mmd".to_owned(), "*.bin".to_owned(), "**".to_owned()];
        for unservable in ["chart.mmd", "blob.bin", "Makefile", "db.sqlite", "id_rsa"] {
            assert!(!is_viewable(unservable, &p), "{unservable}");
            assert!(servable_type(unservable).is_none(), "{unservable}");
        }
        // The invariant, over the whole surface rather than one example.
        for path in [
            "deck.html",
            "a/b.pdf",
            "x.png",
            "notes.md",
            "chart.mmd",
            "z.bin",
        ] {
            if is_viewable(path, &p) {
                assert!(
                    servable_type(path).is_some(),
                    "{path} is viewable but not servable"
                );
            }
        }
    }

    /// The deny list has to name shapes that are *servable*, or it is decoration.
    ///
    /// `credentials` matched only an extensionless file, which the closed type table
    /// already refuses — so the arm could never fire on anything reachable, while
    /// `credentials.json` sailed through as `application/json`. The lesson generalises:
    /// an entry here is only load-bearing if `servable_type` says yes to it.
    #[test]
    fn the_deny_list_covers_the_servable_spellings() {
        for path in [
            "credentials.json",
            "config/credentials.json",
            "secrets.json",
            "service-account.json",
            "service-account-key.json",
        ] {
            assert!(
                servable_type(path).is_some(),
                "{path} is servable, so the deny list is what has to stop it"
            );
            assert!(is_sensitive(path), "{path}");
        }
        // Not over-broad: an ordinary file whose name merely starts similarly is fine.
        assert!(!is_sensitive("credential-report.html"));
        assert!(!is_sensitive("secretsanta.html"));
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
        // …while an ordinary *servable* file under the same pattern is reachable.
        assert!(is_viewable("anything.html", &p));
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

    /// A `*`-heavy pattern must cost time proportional to its size, not exponential.
    ///
    /// **This test used to be the bug.** It ran the old backtracking matcher on nine
    /// stars against 64 characters, which takes 319 seconds in release and longer in
    /// debug — so `cargo test --workspace` (which CI runs with no `timeout-minutes`)
    /// hung on it, and every local run of this crate's suite took ten minutes for a
    /// reason nobody had attributed correctly.
    ///
    /// It is now sixteen stars against a longer subject — far past where the old
    /// implementation stopped finishing at all — and it completes in microseconds. The
    /// wall-clock assertion is deliberately loose (a second, against a real cost of
    /// well under a millisecond): it exists to fail if the exponential shape ever comes
    /// back, not to measure this machine.
    #[test]
    fn a_pathological_pattern_costs_no_more_than_its_size() {
        let subject = format!("{}b", "a".repeat(200));
        let pattern = format!("{}b", "*a".repeat(16));
        let started = std::time::Instant::now();
        // Matches: every `*a` finds an `a`, and the final `b` lands on the last byte.
        assert!(glob_matches(&pattern, &subject));
        // And the non-matching case, which is the one that used to explode — the
        // matcher has to prove no arrangement works.
        let no_match = format!("{}c", "*a".repeat(16));
        assert!(!glob_matches(&no_match, &subject));
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "glob matching took {elapsed:?} — the exponential backtracker is back"
        );
    }

    #[test]
    fn text_kind_routes_by_name_and_leaves_pages_to_the_browser() {
        assert_eq!(text_kind("docs/plan.md"), Some(TextKind::Markdown));
        assert_eq!(text_kind("README.MARKDOWN"), Some(TextKind::Markdown));
        assert_eq!(text_kind("data/q3.csv"), Some(TextKind::Csv));
        assert_eq!(text_kind("q3.tsv"), Some(TextKind::Tsv));
        assert_eq!(text_kind("run.log"), Some(TextKind::Plain));
        assert_eq!(text_kind("src/main.rs"), Some(TextKind::Code));
        assert_eq!(text_kind("app/Dockerfile"), Some(TextKind::Code));
        assert_eq!(text_kind("justfile"), Some(TextKind::Code));
        // TypeScript's ESM/CJS spellings.
        assert_eq!(text_kind("vite.config.mts"), Some(TextKind::Code));
        assert_eq!(text_kind("jest.config.cts"), Some(TextKind::Code));
        // A page is for looking at, so routing sends it to a browser pane…
        assert_eq!(text_kind("deck.html"), None);
        // …while the file pane can still show its source when asked by name.
        assert_eq!(readable_text_kind("deck.html"), Some(TextKind::Code));
        for no in [
            "shot.png",
            "report.pdf",
            "id_rsa",
            "db.sqlite",
            "Cargo.lock",
        ] {
            assert_eq!(text_kind(no), None, "{no}");
            assert_eq!(readable_text_kind(no), None, "{no}");
        }
    }

    /// The text-kind lists against `text_kinds.json`, which the `/ide` bundle's
    /// `files.test.ts` checks `textKind.ts` against too — so a kind added on one
    /// side only fails one of the two.
    #[test]
    fn text_kind_lists_match_the_shared_fixture() {
        let fixture: std::collections::BTreeMap<String, Vec<String>> =
            serde_json::from_str(include_str!("text_kinds.json")).unwrap();
        let sorted = |list: &[&str]| {
            let mut v: Vec<String> = list.iter().map(|s| (*s).to_owned()).collect();
            v.sort();
            v
        };
        let want = |kind: &str| {
            let mut v = fixture[kind].clone();
            v.sort();
            v
        };
        assert_eq!(sorted(MARKDOWN_EXTS), want("markdown"));
        assert_eq!(sorted(PLAIN_EXTS), want("plain"));
        assert_eq!(sorted(CODE_EXTS), want("code"));
        assert_eq!(sorted(CODE_NAMES), want("codeNames"));
        assert_eq!(sorted(WEB_PAGE_EXTS), want("sourceOnly"));
        // And each entry routes to its kind — which is what pins `csv`/`tsv`, the
        // two kinds spelled as match arms rather than lists.
        let kinds = [
            ("markdown", TextKind::Markdown),
            ("csv", TextKind::Csv),
            ("tsv", TextKind::Tsv),
            ("plain", TextKind::Plain),
            ("code", TextKind::Code),
        ];
        for (key, kind) in kinds {
            for ext in &fixture[key] {
                assert_eq!(text_kind(&format!("a.{ext}")), Some(kind), "{ext}");
            }
        }
        for name in &fixture["codeNames"] {
            assert_eq!(text_kind(&format!("dir/{name}")), Some(TextKind::Code));
        }
        for ext in &fixture["sourceOnly"] {
            assert_eq!(text_kind(&format!("a.{ext}")), None, "{ext}");
            assert_eq!(
                readable_text_kind(&format!("a.{ext}")),
                Some(TextKind::Code)
            );
        }
        let mut keys: Vec<&str> = fixture.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "code",
                "codeNames",
                "csv",
                "markdown",
                "plain",
                "sourceOnly",
                "tsv"
            ],
            "a new key in the fixture needs a list here to check it against"
        );
    }

    #[test]
    fn a_config_home_credential_is_refused_wherever_xdg_puts_it() {
        let config = Path::new("/data/cfg");
        assert!(is_config_credential(&config.join("gh/hosts.yml"), config));
        assert!(is_config_credential(&config.join("GCloud/x.json"), config));
        assert!(!is_config_credential(&config.join("nvim/init.lua"), config));
        assert!(!is_config_credential(
            Path::new("/elsewhere/gh/x.yml"),
            config
        ));
    }

    #[test]
    fn home_credentials_are_refused_but_claude_plans_are_not() {
        let home = Path::new("/home/me");
        let at = |rel: &str| home.join(rel);
        for refused in [
            ".ssh/config",
            ".aws/config",
            ".kube/config",
            ".docker/config.json",
            ".gnupg/gpg.conf",
            ".codex/config.toml",
            ".config/gh/config.yml",
            ".config/gcloud/configurations/config_default",
            ".netrc",
            ".npmrc",
            ".yarnrc.yml",
            ".pypirc",
            ".git-credentials",
            ".claude/settings.json",
            ".claude/.credentials.json",
            ".claude/projects/x/session.jsonl",
            ".claude/plans/notes.txt",
            ".claude/CLAUDE.md",
        ] {
            assert!(is_home_credential(&at(refused), Some(home)), "{refused}");
        }
        // By name, wherever the file sits — inside home or not.
        for refused in [
            "/srv/app/auth.json",
            "/srv/app/hosts.yml",
            "/srv/app/token",
            "/srv/app/tokens.json",
            "/srv/app/aws-credentials.txt",
        ] {
            assert!(
                is_home_credential(Path::new(refused), Some(home)),
                "{refused}"
            );
        }
        for allowed in [
            ".claude/plans/refactor.md",
            ".claude/projects/-Users-me-repo/memory/MEMORY.md",
            "notes/today.md",
            ".config/nvim/init.lua",
            ".sshfoo/readme.md",
        ] {
            assert!(!is_home_credential(&at(allowed), Some(home)), "{allowed}");
        }
        assert!(!is_home_credential(Path::new("/srv/app/config.yml"), None));
    }

    #[test]
    fn an_extra_folder_naming_root_or_home_in_disguise_is_too_broad() {
        let home = Path::new("/Users/me");
        for broad in [
            "/",
            "/.",
            "/./",
            "//",
            "/Users",
            "/Users/",
            "/Users/me",
            "/Users/me/.",
            "/Users/me/./",
            "/Users/me/notes/..",
            "/Users/me/..",
            "/tmp/..",
        ] {
            assert!(folder_too_broad(Path::new(broad), Some(home)), "{broad}");
        }
        for fine in [
            "/Users/me/notes",
            "/Users/me/./notes/",
            "/tmp/scratch",
            "/Users/other",
        ] {
            assert!(!folder_too_broad(Path::new(fine), Some(home)), "{fine}");
        }
        assert!(folder_too_broad(Path::new("/."), None));
    }

    #[test]
    fn a_nul_early_on_is_binary_and_one_past_8kb_is_not_looked_for() {
        assert!(!looks_binary(b"plain text\n"));
        assert!(looks_binary(b"PK\x03\x04\0\0"));
        let mut late = vec![b'a'; 9 * 1024];
        late.push(0);
        assert!(!looks_binary(&late));
    }

    /// `**` collapses, so a run of stars cannot multiply the table's rows.
    #[test]
    fn a_run_of_stars_is_one_token() {
        assert!(glob_matches("****.html", "deck.html"));
        assert!(glob_matches("a/****/b.html", "a/x/y/b.html"), "**/ crosses");
        // A single `*` still stops at a separator even when doubled up oddly.
        assert!(!glob_matches("docs/*.html", "docs/a/b.html"));
    }
}

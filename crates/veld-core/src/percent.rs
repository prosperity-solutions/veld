//! Percent-encoding for the two places veld puts a filesystem path somewhere
//! that only accepts a narrow character set: a URL path segment and an HTTP
//! header value.
//!
//! Hand-rolled rather than pulled in, for the reason [`crate::ide::parse_origin`]
//! and [`crate::db::parse_search_template`] are: the rule is small, and a
//! dependency shared between the CLI, the daemon, the privileged helper and the
//! gateway is not free.
//!
//! **One function per rule, however many callers, deliberately.** `veld settings`
//! encodes a setting key into a `DELETE` path and its database path into
//! `X-Veld-Db`; the daemon encodes *its own* database path to compare against that
//! header, and encodes each segment of a relative path into a file-serving URL
//! (`veld-daemon/src/files.rs`). All four are the same rule and all four call
//! [`encode_component`]. Two encoders that agree today are two encoders that
//! disagree after somebody "fixes" one, and the failure would be a settings guard
//! that silently stops guarding — which is the bug this module was extracted during.
//! Adding a caller is fine; adding a second spelling of a rule that already lives
//! here is the thing to refuse.
//!
//! [`encode_in_url`] is a genuinely different rule, not a second spelling of that
//! one: it preserves `/` because its inputs are multi-segment (a `feat/foo` branch
//! interpolated into `…/tree/feat/foo`), which is the exact property
//! [`encode_component`] exists to deny. Keeping them apart is what stops a caller
//! reaching for the one whose escaping is wrong for its destination — and it is
//! also why neither delegates to the other: a shared inner function parameterised
//! by an allow-list would make the difference a call-site argument rather than a
//! choice of name, and a wrong argument there is silent.

/// Percent-encode a string so it is safe as a URL path segment **and** as an HTTP
/// header value.
///
/// Allow-list, over **bytes**, of RFC 3986's unreserved set — so every multi-byte
/// UTF-8 sequence, every control character, `/`, `?`, `#`, `%` and space come out
/// as `%XX`. Encoding more than a path segment strictly needs is the point: the
/// same output has to survive `HeaderValue`, whose readable range is
/// `32..=126` (`http`'s `is_visible_ascii`), and a header carrying a raw
/// `/Users/José/…` is one a daemon can accept but not read back.
///
/// Not a general URL encoder — it does not know about `+` for spaces in query
/// strings, and must not be used for one.
pub fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Percent-encode a string for interpolation into a **URL a config author wrote**,
/// preserving `/`.
///
/// The same allow-list as [`encode_component`] plus `/`, and that one difference is
/// the whole reason this is a second function rather than a caller of it. The values
/// this encodes are multi-segment by nature — a branch name (`feat/foo`) whose
/// slashes are *path* in `…/tree/feat/foo`, a worktree path — so the segment
/// encoder's `a%2Fb` would produce a URL that resolves to nothing on every code
/// host. Preserving `/` is safe against traversal because `git check-ref-format`
/// refuses the two-character sequence `..` anywhere in a refname, refuses a leading
/// or trailing `/` and refuses `//`; and `/` is legal unencoded in a query string as
/// well as in a path, so one rule serves both places a template can put a value.
///
/// Everything else outside RFC 3986's unreserved set still goes out as `%XX` —
/// which is what closes the interesting hole: git *does* allow `#` in a branch
/// name, and a raw one would truncate the URL at a fragment and quietly open the
/// repo's front page instead of the branch.
///
/// Not a general URL encoder, for the same reason [`encode_component`] is not: it
/// does not know `+` for spaces in a query string, and must not be used for one.
pub fn encode_in_url(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Reverse [`encode_component`], for **display only**.
///
/// The encoded form is what gets compared — decoding to compare would accept two
/// spellings of one string. This exists because a comparison that fails has to
/// tell a human *which two paths* disagree, and
/// `%2FUsers%2Fyou%2F.veld-dev%2Fveld-cargo.db` names a path nobody typed.
///
/// Never fails, but it is lossy in **two different ways** and they are worth not
/// confusing:
///
/// - a `%` not followed by two hex digits is passed through **verbatim** — `100%`
///   decodes to `100%`, which is what somebody who never encoded the string
///   expects to read back;
/// - bytes that are valid `%XX` but do not form valid UTF-8 become the
///   replacement character — `%FF` decodes to `U+FFFD`, not to `%FF`.
///
/// Neither can panic, which is the property that matters: a diagnostic sentence
/// must never be the thing that fails.
pub fn decode_component(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(byte) = hex {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_setting_key_survives_except_its_dots() {
        // `.` is unreserved, so a key round-trips readably — which matters
        // because these end up in a daemon log line and in a 409 body.
        assert_eq!(encode_component("terminal.shell"), "terminal.shell");
        assert_eq!(
            encode_component("browser.quickSwitch.responsive"),
            "browser.quickSwitch.responsive"
        );
    }

    /// The path-segment half: nothing may escape the segment it was written into.
    #[test]
    fn separators_and_traversal_cannot_escape_a_path_segment() {
        assert_eq!(encode_component("../etc"), "..%2Fetc");
        assert_eq!(encode_component("a/b"), "a%2Fb");
        assert_eq!(encode_component("a?b#c"), "a%3Fb%23c");
        assert_eq!(encode_component("a b"), "a%20b");
        assert_eq!(encode_component("100%"), "100%25");
    }

    /// The URL encoder's one difference, and the reason it is a second function.
    ///
    /// A branch name's slashes are *path*: `…/tree/feat/foo` is the address on every
    /// code host and `…/tree/feat%2Ffoo` is a 404 on all of them. Everything else
    /// outside the unreserved set still goes out escaped — `#` being the one that
    /// matters, because git permits it in a refname and a raw one truncates the URL
    /// at a fragment, silently opening the repo's front page instead of the branch.
    #[test]
    fn a_url_keeps_its_path_separators_and_escapes_everything_else() {
        assert_eq!(encode_in_url("feat/foo"), "feat/foo");
        assert_eq!(encode_in_url("git-status-visual"), "git-status-visual");
        assert_eq!(encode_in_url("feat#2"), "feat%232");
        assert_eq!(encode_in_url("100%"), "100%25");
        assert_eq!(encode_in_url("a b"), "a%20b");
        assert_eq!(encode_in_url("feat/ümlaut"), "feat/%C3%BCmlaut");
        // The two encoders must disagree here, and only here. If a change ever
        // makes them agree, one of them has stopped doing its job.
        assert_ne!(encode_in_url("a/b"), encode_component("a/b"));
    }

    /// **This encoder does not stop path traversal, and is not what makes it safe.**
    /// Pinned as an assertion rather than left as a comment, because the tempting
    /// "fix" is to start escaping `.` — which would break every `feat/foo` branch
    /// link this function exists to produce.
    ///
    /// Two things carry the safety instead. First, `git check-ref-format` refuses
    /// the two-character sequence `..` anywhere in a refname, refuses `//`, and
    /// refuses `?` and space (verified against git 2.50.1; it *accepts* `#` and `/`,
    /// which is why `#` is escaped above and `/` is not) — so the one value here an
    /// outsider chooses, a branch name on somebody else's pull request, cannot
    /// contain a traversal to begin with. Second, traversal cannot cross an origin:
    /// `https://host/o/r/tree/../../x` normalises to `https://host/x`, the same
    /// host, and the scheme and host of the template are not interpolated. So the
    /// worst a hostile value could reach is another path on a host the repo's own
    /// config already named.
    #[test]
    fn dot_segments_pass_through_and_git_is_what_refuses_them() {
        assert_eq!(encode_in_url("../../evil"), "../../evil");
        assert_eq!(encode_in_url("a/../b"), "a/../b");
        // The characters git *does* allow in a branch name and a URL reads as
        // structure — the actual job.
        assert_eq!(encode_in_url("feat#2"), "feat%232");
    }

    /// The header half, and the bug this module exists for.
    ///
    /// `HeaderValue::to_str` refuses any byte >= 127, while the builder accepts
    /// them — so a database path with one accented character produced a header
    /// the daemon could not read, which its guard treated as *absent* and waved
    /// through. Everything this emits is inside `is_visible_ascii`'s range.
    #[test]
    fn output_is_always_readable_as_a_header_value() {
        for input in [
            "/Users/José/Library/Application Support/veld/veld.db",
            "/Users/日本語/veld.db",
            "/tmp/veld\u{7f}.db",
            "/tmp/veld\u{1}.db",
        ] {
            let encoded = encode_component(input);
            assert!(
                encoded.bytes().all(|b| (32..127).contains(&b)),
                "{input:?} encoded to {encoded:?}, which a header cannot carry"
            );
            assert!(
                axum_style_header_readable(&encoded),
                "{encoded:?} is not readable back"
            );
        }
    }

    /// `http`'s `is_visible_ascii`, restated so this test does not need the crate.
    fn axum_style_header_readable(s: &str) -> bool {
        s.bytes().all(|b| (32..127).contains(&b) || b == b'\t')
    }

    #[test]
    fn decoding_round_trips_what_encoding_produced() {
        for input in [
            "/Users/José/Library/Application Support/veld/veld.db",
            "/Users/日本語/veld.db",
            "terminal.shell",
            "/tmp/100% sure/veld.db",
            "",
        ] {
            assert_eq!(decode_component(&encode_component(input)), input);
        }
    }

    /// A diagnostic must not be the thing that fails — in either lossy direction.
    #[test]
    fn decoding_something_that_was_never_encoded_is_lossy_not_fatal() {
        // Malformed `%` syntax: passed through as written.
        assert_eq!(decode_component("100%"), "100%");
        assert_eq!(decode_component("%zz"), "%zz");
        assert_eq!(decode_component("%2"), "%2");
        assert_eq!(decode_component("plain/path"), "plain/path");
        // Well-formed `%XX` that is not valid UTF-8: the replacement character,
        // **not** the text as written. The doc claimed otherwise until a review
        // ran it.
        assert_eq!(decode_component("%FF"), "\u{FFFD}");
        assert_eq!(decode_component("/tmp/%FF/x"), "/tmp/\u{FFFD}/x");
    }

    /// Distinct inputs must not collide, or the daemon's comparison would accept
    /// a database it should refuse.
    #[test]
    fn encoding_is_injective_for_paths_that_differ() {
        assert_ne!(encode_component("/a/b"), encode_component("/a%2Fb"));
        assert_ne!(encode_component("/tmp/x"), encode_component("/tmp/y"));
    }
}

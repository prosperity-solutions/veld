//! Percent-encoding for the two places veld puts a filesystem path somewhere
//! that only accepts a narrow character set: a URL path segment and an HTTP
//! header value.
//!
//! Hand-rolled rather than pulled in, for the reason [`crate::ide::parse_origin`]
//! and [`crate::db::parse_search_template`] are: the rule is small, and a
//! dependency shared between the CLI, the daemon, the privileged helper and the
//! gateway is not free.
//!
//! **One function, two callers, deliberately.** `veld settings` encodes a setting
//! key into a `DELETE` path and its database path into `X-Veld-Db`; the daemon
//! encodes *its own* database path to compare against that header. Two encoders
//! that agree today are two encoders that disagree after somebody "fixes" one, and
//! the failure would be a settings guard that silently stops guarding — which is
//! the bug this module was extracted during.

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

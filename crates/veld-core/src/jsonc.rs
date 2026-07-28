//! JSONC support for veld config files: `//` line comments, `/* */` block
//! comments, and trailing commas.
//!
//! The approach is deliberately the boring one. [`strip`] rewrites comment and
//! trailing-comma bytes **in place as spaces** — never deleting anything — and
//! hands the result to `serde_json`. Because nothing shifts, the byte length,
//! line numbers, and columns of everything that remains are unchanged, so
//! `serde_json::Error::line()` / `column()` still point at the real position in
//! the file the user is editing. That is the whole reason not to reach for a CST
//! or a span map: nothing veld does today *rewrites* an existing `veld.json`, so
//! there is no comment-preserving writer to feed. (`veld init` writes one, but
//! only when none exists.) The one planned exception is `veld config --migrate
//! --write`, which MUST NOT round-trip through `serde` — that would silently
//! delete every comment this module exists to allow.
//!
//! There is no `.jsonc` extension requirement: the file is always `veld.json` and
//! it accepts comments as it is. Note that editors do not know that — a
//! `veld.json` carrying a `$schema` is validated by their strict JSON parser, so
//! a `files.associations` mapping to `jsonc` is needed to stop the red squiggles.
//!
//! [`reject_duplicate_keys`] closes the other half of the hole: `serde_json` is
//! silently last-wins, so a config with two `variants` blocks (or two nodes of
//! the same name in one file) would drop one without a word.

use std::collections::HashSet;

use serde::Deserialize;
use serde::de::{self, MapAccess, SeqAccess, Visitor};

/// A structural problem in a JSONC document that `serde_json` could not have
/// reported, because it would have choked on the comment first.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum JsoncError {
    #[error("unterminated block comment starting at line {line}, column {column}")]
    UnterminatedBlockComment { line: usize, column: usize },
}

/// Blank out comments and trailing commas, returning plain JSON of exactly the
/// same byte length so all positions still line up with the source file.
pub fn strip(input: &str) -> Result<String, JsoncError> {
    let mut bytes = input.as_bytes().to_vec();
    blank_comments(&mut bytes)?;
    blank_trailing_commas(&mut bytes);
    // Only whole comment regions were overwritten, and only with ASCII spaces,
    // so every remaining byte sequence is the UTF-8 it already was.
    Ok(String::from_utf8(bytes).expect("blanking writes ASCII over whole comments"))
}

/// Overwrite every comment byte with a space, keeping newlines so line numbers
/// survive. String literals (and escapes inside them) are left alone.
fn blank_comments(bytes: &mut [u8]) -> Result<(), JsoncError> {
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;

    while i < bytes.len() {
        let b = bytes[i];

        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        match b {
            b'"' => {
                in_string = true;
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    bytes[i] = b' ';
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let start = i;
                bytes[i] = b' ';
                bytes[i + 1] = b' ';
                i += 2;
                loop {
                    if i >= bytes.len() {
                        let (line, column) = position_of(bytes, start);
                        return Err(JsoncError::UnterminatedBlockComment { line, column });
                    }
                    if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        bytes[i] = b' ';
                        bytes[i + 1] = b' ';
                        i += 2;
                        break;
                    }
                    // Newlines stay so the reported line number is still right.
                    if bytes[i] != b'\n' {
                        bytes[i] = b' ';
                    }
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    Ok(())
}

/// Replace a comma that is followed only by whitespace before a closing `}` or
/// `]` with a space. Runs after [`blank_comments`], so a comment between the
/// comma and the brace already reads as whitespace.
fn blank_trailing_commas(bytes: &mut [u8]) {
    let mut in_string = false;
    let mut escaped = false;

    for i in 0..bytes.len() {
        let b = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        if b == b'"' {
            in_string = true;
            continue;
        }
        if b != b',' {
            continue;
        }
        let next = bytes[i + 1..]
            .iter()
            .find(|c| !c.is_ascii_whitespace())
            .copied();
        if matches!(next, Some(b'}') | Some(b']')) {
            bytes[i] = b' ';
        }
    }
}

/// 1-based line and column of `offset`, counting bytes.
fn position_of(bytes: &[u8], offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for &b in &bytes[..offset] {
        if b == b'\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

// ---------------------------------------------------------------------------
// Duplicate keys
// ---------------------------------------------------------------------------

/// Reject a document containing two entries with the same key in one object.
///
/// `serde_json` is last-wins and silent, which would quietly discard one of two
/// `variants` blocks, or — once a file may define several nodes — one of two
/// nodes with the same name. The returned error carries `serde_json`'s own line
/// and column, which [`strip`] has kept accurate.
pub fn reject_duplicate_keys(json: &str) -> Result<(), serde_json::Error> {
    serde_json::from_str::<NoDupKeys>(json).map(|_| ())
}

/// A whole JSON document, parsed only far enough to notice a repeated key.
/// Values are recursed into (not stored) so nested objects are checked too.
struct NoDupKeys;

impl<'de> Deserialize<'de> for NoDupKeys {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(NoDupVisitor)
    }
}

struct NoDupVisitor;

impl<'de> Visitor<'de> for NoDupVisitor {
    type Value = NoDupKeys;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            // Error BEFORE consuming the value: serde's reported position is
            // wherever the parser currently sits, so recursing first would point
            // at the object's closing brace — hundreds of lines from the
            // duplicate in a large `nodes` block, which defeats the whole
            // blank-don't-delete premise of this module.
            if !seen.insert(key.clone()) {
                return Err(de::Error::custom(format!(
                    "duplicate key \"{key}\" — the later value would silently win"
                )));
            }
            map.next_value::<NoDupKeys>()?;
        }
        Ok(NoDupKeys)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        while seq.next_element::<NoDupKeys>()?.is_some() {}
        Ok(NoDupKeys)
    }

    fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
        Ok(NoDupKeys)
    }
    fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
        Ok(NoDupKeys)
    }
    fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
        Ok(NoDupKeys)
    }
    fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
        Ok(NoDupKeys)
    }
    fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
        Ok(NoDupKeys)
    }
    // `visit_unit` is what serde_json's `deserialize_any` calls for `null`.
    // `visit_none`/`visit_some` are deliberately absent: they belong to
    // `deserialize_option`, which this type never goes through.
    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(NoDupKeys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_line_and_block_comments() {
        let src = "{ // a comment\n  \"a\": 1, /* inline */ \"b\": 2\n}";
        let out = strip(src).unwrap();
        assert_eq!(out.len(), src.len(), "byte length must be preserved");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], 2);
    }

    #[test]
    fn keeps_comment_like_sequences_inside_strings() {
        let src = r#"{ "url": "https://example.com//x", "glob": "/* not a comment */" }"#;
        let out = strip(src).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["url"], "https://example.com//x");
        assert_eq!(v["glob"], "/* not a comment */");
    }

    #[test]
    fn keeps_escaped_quote_from_ending_the_string() {
        // The `\"` must not close the string, or the `//` after it would be
        // treated as a comment and eat the rest of the line.
        let src = r#"{ "a": "say \"hi\" // not a comment", "b": 2 }"#;
        let out = strip(src).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["a"], r#"say "hi" // not a comment"#);
        assert_eq!(v["b"], 2);
    }

    #[test]
    fn strips_trailing_commas_in_objects_and_arrays() {
        let src = "{\n  \"a\": [1, 2, 3,],\n  \"b\": { \"c\": 1, },\n}";
        let out = strip(src).unwrap();
        assert_eq!(out.len(), src.len());
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["a"][2], 3);
        assert_eq!(v["b"]["c"], 1);
    }

    #[test]
    fn strips_trailing_comma_separated_from_brace_by_a_comment() {
        let src = "{ \"a\": 1, // trailing\n}";
        let out = strip(src).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn keeps_commas_inside_strings() {
        let src = r#"{ "a": "x,]", "b": "y,}" }"#;
        let out = strip(src).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["a"], "x,]");
        assert_eq!(v["b"], "y,}");
    }

    /// The point of blanking rather than deleting: a syntax error after a
    /// comment must still report the line and column the editor shows.
    #[test]
    fn error_positions_survive_stripping() {
        //                        1   2                                   3       4                        5              6
        let src = "{\n  // a comment that would shift things\n  /* and\n     a multi-line one */\n  \"a\": nope\n}";
        let out = strip(src).unwrap();
        let err = serde_json::from_str::<serde_json::Value>(&out).unwrap_err();
        assert_eq!(
            (err.line(), err.column()),
            (5, 9),
            "position must point at `nope` in the original source, not at a \
             shifted offset: {err}"
        );
    }

    #[test]
    fn unterminated_block_comment_is_an_error() {
        let err = strip("{\n  /* never closed\n  \"a\": 1\n}").unwrap_err();
        assert_eq!(
            err,
            JsoncError::UnterminatedBlockComment { line: 2, column: 3 }
        );
    }

    #[test]
    fn multibyte_comment_contents_are_handled() {
        let src = "{ \"a\": 1 /* — em dash and 😀 */ }";
        let out = strip(src).unwrap();
        assert_eq!(out.len(), src.len());
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn duplicate_keys_are_rejected_at_any_depth() {
        let err = reject_duplicate_keys(r#"{ "a": 1, "a": 2 }"#).unwrap_err();
        assert!(err.to_string().contains("duplicate key \"a\""), "{err}");

        let nested = reject_duplicate_keys(
            r#"{ "nodes": { "api": { "variants": {"x": 1}, "variants": {"y": 2} } } }"#,
        )
        .unwrap_err();
        assert!(
            nested.to_string().contains("duplicate key \"variants\""),
            "{nested}"
        );

        // Inside an array element too.
        let in_array = reject_duplicate_keys(r#"{ "xs": [ { "k": 1, "k": 2 } ] }"#).unwrap_err();
        assert!(in_array.to_string().contains("duplicate key \"k\""));
    }

    #[test]
    fn same_key_in_sibling_objects_is_fine() {
        reject_duplicate_keys(r#"{ "a": { "k": 1 }, "b": { "k": 2 } }"#).unwrap();
        reject_duplicate_keys(r#"[ { "k": 1 }, { "k": 2 } ]"#).unwrap();
    }
}

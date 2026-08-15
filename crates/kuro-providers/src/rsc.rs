//! Reading data out of a React Server Components payload.
//!
//! Next.js App Router sites ship page data as a "flight" stream rather than as
//! markup or a JSON document: numbered rows of mixed component references and
//! embedded JSON. Neither a CSS selector nor a JSON parser can read it whole, but
//! the parts worth having are ordinary JSON values sitting behind a known key.
//!
//! Two shapes arrive in practice. Requesting a page normally returns HTML with the
//! stream split across `self.__next_f.push([1,"…"])` calls, which have to be
//! concatenated *before* parsing — records routinely straddle two calls. Requesting
//! it with `?_rsc=` returns the stream directly. [`reassemble`] accepts either.

use serde_json::Value;

/// Recover the flight stream from a page response.
///
/// Passing an already-raw stream through is deliberate: the caller should not have
/// to know which of the two shapes it received.
pub fn reassemble(body: &str) -> String {
    if !body.contains("__next_f.push") {
        return body.to_string();
    }

    let mut out = String::new();
    let mut rest = body;
    // Each call carries one JSON string literal; decoding it undoes the escaping
    // that would otherwise hide the payload's own quotes and slashes.
    while let Some(start) = rest.find("__next_f.push([1,") {
        rest = &rest[start + "__next_f.push([1,".len()..];
        let Some(quote) = rest.find('"') else { break };
        let after = &rest[quote..];
        let Some(end) = closing_quote(after) else {
            break;
        };
        if let Ok(Value::String(decoded)) = serde_json::from_str::<Value>(&after[..=end]) {
            out.push_str(&decoded);
        }
        rest = &after[end..];
    }
    out
}

/// Index of the quote closing a JSON string literal that starts at byte 0.
fn closing_quote(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// The JSON value stored under `"key":` somewhere in `text`.
///
/// Scans for the key and then walks the value with a brace/bracket counter that
/// respects strings and escapes, so nested objects and any punctuation inside
/// string values are handled. Returns the first well-formed match, since a flight
/// stream also mentions keys in places that are not data.
pub fn value_after_key(text: &str, key: &str) -> Option<Value> {
    let needle = format!("\"{key}\":");
    let mut from = 0;

    while let Some(offset) = text[from..].find(&needle) {
        let start = from + offset + needle.len();
        from = start;

        let rest = text[start..].trim_start();
        let skipped = text[start..].len() - rest.len();
        let Some(end) = balanced_end(rest) else {
            continue;
        };

        if let Ok(value) = serde_json::from_str::<Value>(&rest[..end]) {
            return Some(value);
        }
        from = start + skipped + end;
    }
    None
}

/// Length of the JSON array or object beginning at byte 0.
fn balanced_end(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let open = *bytes.first()?;
    let close = match open {
        b'[' => b']',
        b'{' => b'}',
        _ => return None,
    };

    let mut depth = 0usize;
    let mut in_string = false;
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            match b {
                b'\\' => i += 1,
                b'"' => in_string = false,
                _ => {}
            }
        } else {
            match b {
                b'"' => in_string = true,
                _ if b == open => depth += 1,
                _ if b == close => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_raw_stream_passes_through_untouched() {
        let raw = r#"0:{"a":1}"#;
        assert_eq!(reassemble(raw), raw);
    }

    #[test]
    fn pushed_fragments_are_concatenated_in_order() {
        let html = r#"<script>self.__next_f.push([1,"{\"a\":"])</script>
                     <script>self.__next_f.push([1,"1}"])</script>"#;
        assert_eq!(reassemble(html), r#"{"a":1}"#);
    }

    #[test]
    fn a_record_split_across_two_pushes_is_recovered() {
        // The reason concatenation happens before parsing: the site really does
        // split objects mid-key across script tags.
        let html = concat!(
            r#"<script>self.__next_f.push([1,"{\"episodes\":[{\"id\":\"ep-"])</script>"#,
            r#"<script>self.__next_f.push([1,"1\",\"number\":1}]}"])</script>"#
        );
        let value = value_after_key(&reassemble(html), "episodes").expect("episodes");
        assert_eq!(value[0]["id"], "ep-1");
        assert_eq!(value[0]["number"], 1);
    }

    #[test]
    fn a_nested_value_keeps_its_structure() {
        let text = r#"junk "primaryTabs":[{"seasons":[{"episodes":[{"number":3}]}]}] junk"#;
        let v = value_after_key(text, "primaryTabs").expect("primaryTabs");
        assert_eq!(v[0]["seasons"][0]["episodes"][0]["number"], 3);
    }

    #[test]
    fn braces_inside_strings_do_not_end_the_value() {
        let text = r#""a":[{"title":"a } b ] c","n":1}]"#;
        let v = value_after_key(text, "a").expect("a");
        assert_eq!(v[0]["title"], "a } b ] c");
        assert_eq!(v[0]["n"], 1);
    }

    #[test]
    fn an_escaped_quote_does_not_end_a_string() {
        let text = r#""a":[{"title":"say \"hi\" ]","n":2}]"#;
        let v = value_after_key(text, "a").expect("a");
        assert_eq!(v[0]["n"], 2);
    }

    #[test]
    fn a_mention_that_is_not_data_is_skipped() {
        // Flight streams name keys in prose and component props too; the first
        // well-formed value is the one that counts.
        let text = r#"the "episodes":label here, then "episodes":[{"number":7}]"#;
        let v = value_after_key(text, "episodes").expect("episodes");
        assert_eq!(v[0]["number"], 7);
    }

    #[test]
    fn a_missing_key_is_none() {
        assert!(value_after_key(r#"{"a":1}"#, "b").is_none());
    }

    #[test]
    fn an_unterminated_value_is_none_rather_than_a_panic() {
        assert!(value_after_key(r#""a":[{"n":1}"#, "a").is_none());
    }
}

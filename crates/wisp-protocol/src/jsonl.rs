use serde::Serialize;

pub fn hidden_format(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'
            | '\u{FEFF}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{061C}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{2060}'..='\u{2064}'
            | '\u{206A}'..='\u{206F}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{E0000}'..='\u{E007F}'
    )
}

pub fn escape_controls(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        if c == '\u{7f}' || ('\u{80}'..='\u{9f}').contains(&c) || hidden_format(c) {
            for unit in c.encode_utf16(&mut [0; 2]) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn line<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<String> {
    let json = serde_json::to_string(value)?;
    let mut line = escape_controls(&json);
    line.push('\n');
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn del_c1_and_hidden_format_are_escaped_and_the_rest_is_kept() {
        let text = "a\u{1b}]0;t\u{7}b\u{7f}c\u{80}d\u{9b}e\u{9f}f\u{a0}\u{ad}\u{180e}\u{200c}\u{200d}g\u{85}é\n\u{202e}\u{2066}\u{2028}\u{200b}\u{e0041}";
        let got = line(&serde_json::json!({ "t": text })).unwrap();
        assert!(got.ends_with('\n'));
        assert_eq!(got.matches('\n').count(), 1);
        assert!(
            !got.trim_end_matches('\n')
                .chars()
                .any(|c| c.is_control() || hidden_format(c)),
            "{got:?}"
        );
        for escaped in [
            "\\u001b", "\\u0007", "\\u007f", "\\u0080", "\\u009b", "\\u009f", "\\u0085",
        ] {
            assert!(got.contains(escaped), "{escaped} missing from {got:?}");
        }
        let hidden = "\\u202e\\u2066\\u2028\\u200b\\udb40\\udc41";
        assert!(got.contains(hidden), "{hidden} missing from {got:?}");
        let kept = "f\u{a0}\u{ad}\u{180e}\u{200c}\u{200d}g";
        assert!(got.contains(kept) && got.contains('é'), "{got:?}");
        let back: serde_json::Value = serde_json::from_str(&got).unwrap();
        assert_eq!(back["t"], text);
    }

    #[test]
    fn escaping_leaves_plain_json_untouched() {
        let json = r#"{"a":"plain ✓ text","b":[1,2]}"#;
        assert_eq!(escape_controls(json), json);
    }
}

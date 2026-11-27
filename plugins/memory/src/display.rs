use std::fmt::Write as _;
use std::sync::LazyLock;

use regex::Regex;
use wisp_tool_text::terminal::FOLD_SOURCES;

static INVISIBLE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"[\p{C}\p{Default_Ignorable_Code_Point}\p{White_Space}\p{Z}]").ok()
});
static MARK: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\p{M}").ok());
const NO_LF: &str = "\\ No final newline";
const EMPTY_LINE: &str = "\\n";

pub(super) fn format_file(raw: &str) -> Vec<String> {
    let lines: Vec<_> = raw.split_terminator('\n').collect();
    let first = lines.iter().position(|line| !line.is_empty());
    let last = lines.iter().rposition(|line| !line.is_empty());
    let mut encoded: Vec<_> = lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            if line.is_empty() && !first.zip(last).is_some_and(|(a, b)| (a..=b).contains(&i)) {
                EMPTY_LINE.to_owned()
            } else {
                format_line(line)
            }
        })
        .collect();
    if !raw.ends_with('\n') {
        encoded.push(NO_LF.to_owned());
    }
    encoded
}

fn format_line(line: &str) -> String {
    let mut out = String::new();
    let mut base = false;
    let mut previous = None;
    for (i, ch) in line.char_indices() {
        let rest = &line[i..];
        let mut buf = [0; 4];
        let scalar = ch.encode_utf8(&mut buf);
        let mark = MARK.as_ref().is_some_and(|regex| regex.is_match(scalar));
        let literal = !INVISIBLE
            .as_ref()
            .is_none_or(|regex| regex.is_match(scalar))
            && (!mark || base)
            && !FOLD_SOURCES.iter().any(|source| rest.starts_with(source));
        base = match ch {
            '\\' => {
                out.push_str(r"\\");
                false
            }
            '\t' => {
                out.push_str(r"\t");
                false
            }
            '\r' => {
                out.push_str(r"\r");
                false
            }
            ' ' => {
                if i == 0 || i + 1 == line.len() || previous == Some(' ') || rest.starts_with("  ")
                {
                    out.push_str(r"\s");
                } else {
                    out.push(' ');
                }
                false
            }
            _ if literal => {
                out.push(ch);
                true
            }
            _ => {
                if ch.is_ascii() {
                    let _ = write!(out, "\\x{:02X}", u32::from(ch));
                } else {
                    let _ = write!(out, "\\u{{{:x}}}", u32::from(ch));
                }
                false
            }
        };
        previous = Some(ch);
    }
    out
}

#[cfg(test)]
pub(super) fn decode(lines: &[String]) -> String {
    let mut raw = String::new();
    for line in lines {
        if line == NO_LF {
            raw.pop();
            break;
        }
        if line != EMPTY_LINE {
            let mut chars = line.chars();
            while let Some(ch) = chars.next() {
                if ch != '\\' {
                    raw.push(ch);
                    continue;
                }
                raw.push(match chars.next().unwrap() {
                    '\\' => '\\',
                    's' => ' ',
                    't' => '\t',
                    'r' => '\r',
                    'x' => {
                        let hex: String = chars.by_ref().take(2).collect();
                        char::from(u8::from_str_radix(&hex, 16).unwrap())
                    }
                    'u' => {
                        assert_eq!(chars.next(), Some('{'));
                        let hex: String = chars.by_ref().take_while(|ch| *ch != '}').collect();
                        char::from_u32(u32::from_str_radix(&hex, 16).unwrap()).unwrap()
                    }
                    _ => panic!("invalid escape"),
                });
            }
        }
        raw.push('\n');
    }
    raw
}

#[cfg(test)]
#[test]
fn exact_file_bytes_round_trip_without_renderer_sensitive_scalars() {
    for (raw, lines) in [
        ("", vec![NO_LF]),
        ("\n", vec![EMPTY_LINE]),
        ("\n\n", vec![EMPTY_LINE, EMPTY_LINE]),
        ("\na\n\n", vec![EMPTY_LINE, "a", EMPTY_LINE]),
        ("a\n\nz", vec!["a", "", "z", NO_LF]),
        (
            "\\n\n\\ No final newline\n",
            vec![r"\\n", r"\\ No final newline"],
        ),
        (" \ta  b \r\n", vec![r"\s\ta\s\sb \r"]),
        (
            "a\u{a0}\u{202f}\x1b\u{ad}\u{fe0f}\u{378}\u{e000}\n",
            vec![r"a\u{a0}\u{202f}\x1B\u{ad}\u{fe0f}\u{378}\u{e000}"],
        ),
        (
            "青いタイル。\na\u{301}\u{327}\n",
            vec!["青いタイル。", "a\u{301}\u{327}"],
        ),
        (
            "\u{301} \\\u{301} \u{301}  \u{301}\n",
            vec![r"\u{301} \\\u{301} \u{301}\s\s\u{301}"],
        ),
    ] {
        let encoded = format_file(raw);
        assert_eq!(encoded, lines);
        assert_eq!(decode(&encoded), raw);
    }
    let folded = FOLD_SOURCES.concat();
    let encoded = format_file(&folded);
    assert!(
        !FOLD_SOURCES
            .iter()
            .any(|source| encoded[0].contains(source))
    );
    assert_eq!(decode(&encoded), folded);
}

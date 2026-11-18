use std::{fmt, sync::LazyLock};

pub const SECRET_FIELDS: [&str; 4] = ["key", "api_key", "token", "secret"];

macro_rules! key_max {
    () => {
        4096
    };
}

pub const KEY_MAX: usize = key_max!();

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Ends {
    pub start: u8,
    pub end: u8,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl serde::Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyProblem {
    Empty,
    TooLong,
    NotPrintable,
    ManyLines,
    Control,
}

impl KeyProblem {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Empty => "is empty",
            Self::TooLong => concat!("is longer than ", key_max!(), " characters"),
            Self::NotPrintable => {
                "has a space, a control character or a non-ASCII character, so it can't be sent in a header"
            }
            Self::ManyLines => "has more than one line",
            Self::Control => "has a control character in it",
        }
    }

    pub fn of(self, noun: &str) -> String {
        format!("That {noun} {}.", self.describe())
    }
}

impl fmt::Display for KeyProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.of("key"))
    }
}

fn sized(text: &str) -> Result<&str, KeyProblem> {
    if text.is_empty() {
        Err(KeyProblem::Empty)
    } else if text.chars().count() > KEY_MAX {
        Err(KeyProblem::TooLong)
    } else {
        Ok(text)
    }
}

pub fn check_key(text: &str) -> Result<&str, KeyProblem> {
    let key = text.trim();
    if !printable(key) {
        return Err(KeyProblem::NotPrintable);
    }
    sized(key)
}

pub fn check_secret(text: &str) -> Result<&str, KeyProblem> {
    let line = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix(['\n', '\r']))
        .unwrap_or(text);
    let line = sized(line)?;
    if line.contains(['\n', '\r']) {
        return Err(KeyProblem::ManyLines);
    }
    if line.chars().any(char::is_control) {
        return Err(KeyProblem::Control);
    }
    Ok(line)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeProblem {
    Empty,
    NoState,
    Repeated,
    Spaced,
    TooLong,
}

impl fmt::Display for CodeProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "nothing was pasted",
            Self::NoState => "only part of the code was pasted",
            Self::Repeated => "the code was pasted more than once",
            Self::Spaced => "the code has spaces or line breaks in it",
            Self::TooLong => "that is longer than any code",
        })
    }
}

fn printable(text: &str) -> bool {
    text.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

pub fn split_code_state(text: &str) -> Result<(&str, &str), CodeProblem> {
    let text = text.trim();
    if text.is_empty() {
        return Err(CodeProblem::Empty);
    }
    if text.len() > KEY_MAX {
        return Err(CodeProblem::TooLong);
    }
    if !printable(text) {
        return Err(CodeProblem::Spaced);
    }
    let mut parts = text.split('#');
    match (parts.next(), parts.next(), parts.next()) {
        (_, _, Some(_)) => Err(CodeProblem::Repeated),
        (Some(code), Some(state), None) if !code.is_empty() && !state.is_empty() => {
            Ok((code, state))
        }
        _ => Err(CodeProblem::NoState),
    }
}

pub static KEY_PREFIXES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    include_str!("../data/key-prefixes.txt")
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect()
});

pub const MASK_SHORT: usize = 16;

const MASK_END: usize = 4;

const MASK_MIN_STARS: usize = 3;

const MASK_FORMS: [(usize, bool); 4] = [(12, true), (6, true), (6, false), (MASK_MIN_STARS, false)];

fn stars(count: usize) -> String {
    "*".repeat(count)
}

fn only_stars(count: usize, counted: bool, room: usize, total: &str) -> String {
    let total_width = total.chars().count();
    if counted && count + total_width <= room {
        stars(count) + total
    } else if count <= room {
        stars(count)
    } else if room >= total_width + MASK_MIN_STARS {
        stars(room - total_width) + total
    } else {
        stars(room)
    }
}

pub(crate) fn known_prefix(value: &str) -> &'static str {
    KEY_PREFIXES
        .iter()
        .copied()
        .find(|prefix| value.starts_with(prefix))
        .unwrap_or_default()
}

pub fn has_known_key_prefix(value: &str) -> bool {
    value.chars().count() >= crate::KEY_SHAPED_MIN && !known_prefix(value).is_empty()
}

fn capped(count: usize, ends: Ends) -> Ends {
    let start = usize::from(ends.start);
    let end = usize::from(ends.end);
    let fits = |shown| shown < count && count >= MASK_SHORT && shown * 3 <= count;
    if fits(start + end) {
        ends
    } else if fits(end) {
        Ends { start: 0, ..ends }
    } else {
        Ends::default()
    }
}

fn key_ends(value: &str) -> (usize, Ends) {
    let public = known_prefix(value).len();
    (
        public,
        Ends {
            start: if public == 0 { MASK_END as u8 } else { 0 },
            end: MASK_END as u8,
        },
    )
}

fn shown(value: &str, public: usize, ends: Ends) -> Ends {
    if printable(value) {
        capped(value.len() - public, ends)
    } else {
        Ends::default()
    }
}

pub fn key_tail(value: &str) -> Option<&str> {
    let (public, ends) = key_ends(value);
    let end = usize::from(shown(value, public, ends).end);
    (end > 0).then(|| &value[value.len() - end..])
}

pub fn masked(value: &str, room: usize, ellipsis: &str) -> String {
    let (public, ends) = key_ends(value);
    drawn(value, public, ends, false, room, ellipsis)
}

pub fn masked_ends(value: &str, ends: Ends, counted: bool, room: usize, ellipsis: &str) -> String {
    drawn(value, 0, ends, counted, room, ellipsis)
}

fn drawn(
    value: &str,
    public: usize,
    ends: Ends,
    counted: bool,
    room: usize,
    ellipsis: &str,
) -> String {
    let count = value.chars().count();
    let total = format!("  {count} characters");
    let ends = shown(value, public, ends);
    if ends == Ends::default() {
        return only_stars(count, counted, room, &total);
    }
    let start = &value[..public + usize::from(ends.start)];
    let tail = &value[value.len() - usize::from(ends.end)..];
    let lead = if start.is_empty() {
        String::new()
    } else {
        format!("{start}{ellipsis}")
    };
    let end = if tail.is_empty() {
        String::new()
    } else {
        format!("{ellipsis}{tail}")
    };
    let width = lead.chars().count() + end.chars().count();
    MASK_FORMS
        .iter()
        .map(|&(hidden, counted)| (hidden, if counted { total.as_str() } else { "" }))
        .find(|(hidden, suffix)| width + hidden + suffix.chars().count() <= room)
        .map(|(hidden, suffix)| format!("{lead}{}{end}{suffix}", stars(hidden)))
        .or_else(|| {
            let tight = format!("{}{}{end}", &value[..public], stars(MASK_MIN_STARS));
            (public > 0 && tight.chars().count() <= room).then_some(tight)
        })
        .unwrap_or_else(|| only_stars(count, counted, room, &total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_never_prints_its_value() {
        let secret = Secret::new("sk-ant-CANARY0001");
        assert_eq!(format!("{secret:?}"), "<redacted>");
        assert_eq!(format!("{secret:#?}"), "<redacted>");
        assert_eq!(secret.expose(), "sk-ant-CANARY0001");
    }

    #[test]
    fn the_local_key_check_takes_printable_ascii_without_whitespace() {
        assert_eq!(check_key("  sk-ant-abc123\n"), Ok("sk-ant-abc123"));
        assert_eq!(check_key("!~"), Ok("!~"));
        let longest = "k".repeat(KEY_MAX);
        assert_eq!(check_key(&longest), Ok(longest.as_str()));
        for (bad, problem) in [
            ("sk-ant CANARY", KeyProblem::NotPrintable),
            ("sk-ant\tCANARY", KeyProblem::NotPrintable),
            ("sk-ant\nCANARY", KeyProblem::NotPrintable),
            ("sk-ant\u{7}CANARY", KeyProblem::NotPrintable),
            ("sk-ant\u{7f}CANARY", KeyProblem::NotPrintable),
            ("sk-ant-k\u{e9}yCANARY", KeyProblem::NotPrintable),
            ("", KeyProblem::Empty),
            (" \n\t", KeyProblem::Empty),
        ] {
            assert_eq!(check_key(bad), Err(problem), "{bad:?}");
            assert!(!problem.to_string().contains("CANARY"));
        }
        assert_eq!(
            check_key(&"k".repeat(KEY_MAX + 1)),
            Err(KeyProblem::TooLong)
        );
        assert_eq!(
            check_key(&format!("{} k", "k".repeat(KEY_MAX))),
            Err(KeyProblem::NotPrintable)
        );
    }

    #[test]
    fn each_key_problem_has_its_exact_reason() {
        assert_eq!(
            KeyProblem::NotPrintable.to_string(),
            "That key has a space, a control character or a non-ASCII character, so it can't be sent in a header."
        );
        assert_eq!(KeyProblem::Empty.to_string(), "That key is empty.");
        assert_eq!(
            KeyProblem::TooLong.to_string(),
            format!("That key is longer than {KEY_MAX} characters.")
        );
        for (problem, sentence) in [
            (KeyProblem::Empty, "That secret is empty."),
            (KeyProblem::ManyLines, "That secret has more than one line."),
            (
                KeyProblem::Control,
                "That secret has a control character in it.",
            ),
        ] {
            assert_eq!(problem.of("secret"), sentence);
        }
        assert_eq!(
            KeyProblem::TooLong.of("secret"),
            format!("That secret is longer than {KEY_MAX} characters.")
        );
        assert_eq!(
            KeyProblem::ManyLines.to_string(),
            "That key has more than one line."
        );
    }

    #[test]
    fn a_secret_is_one_line_of_anything_but_control_characters() {
        for (pasted, line) in [
            ("hunter2", "hunter2"),
            ("hunter2\n", "hunter2"),
            ("hunter2\r\n", "hunter2"),
            ("hunter2\r", "hunter2"),
            ("  two words  ", "  two words  "),
            (
                "p\u{e4}ssw\u{f6}rd \u{1f510}",
                "p\u{e4}ssw\u{f6}rd \u{1f510}",
            ),
        ] {
            assert_eq!(check_secret(pasted), Ok(line), "{pasted:?}");
        }
        for (bad, problem) in [
            ("", KeyProblem::Empty),
            ("\n", KeyProblem::Empty),
            ("\r\n", KeyProblem::Empty),
            ("one\ntwo", KeyProblem::ManyLines),
            ("one\r\ntwo", KeyProblem::ManyLines),
            ("one\rtwo", KeyProblem::ManyLines),
            ("one\n\n", KeyProblem::ManyLines),
            ("one\ttwo", KeyProblem::Control),
            ("one\u{7}two", KeyProblem::Control),
            ("one\u{7f}two", KeyProblem::Control),
            ("one\u{85}two", KeyProblem::Control),
            ("one\u{1b}[31m", KeyProblem::Control),
        ] {
            assert_eq!(check_secret(bad), Err(problem), "{bad:?}");
        }
        let longest = "s".repeat(KEY_MAX);
        assert_eq!(check_secret(&longest), Ok(longest.as_str()));
        let wide = "\u{e9}".repeat(KEY_MAX);
        assert_eq!(check_secret(&wide), Ok(wide.as_str()));
        assert_eq!(check_secret(&format!("{longest}\n")), Ok(longest.as_str()));
        for over in [format!("{longest}s"), "\u{e9}".repeat(KEY_MAX + 1)] {
            assert_eq!(check_secret(&over), Err(KeyProblem::TooLong));
        }
        for problem in [KeyProblem::ManyLines, KeyProblem::Control] {
            assert!(!problem.of("secret").contains("CANARY"));
        }
    }

    #[test]
    fn a_pasted_code_splits_once_into_code_and_state_or_names_its_problem() {
        for (pasted, expected) in [
            ("", Err(CodeProblem::Empty)),
            (" \r\n", Err(CodeProblem::Empty)),
            ("code", Err(CodeProblem::NoState)),
            ("code#", Err(CodeProblem::NoState)),
            ("#state", Err(CodeProblem::NoState)),
            ("#", Err(CodeProblem::NoState)),
            ("c#sc#s", Err(CodeProblem::Repeated)),
            ("c#s#", Err(CodeProblem::Repeated)),
            ("c#s x", Err(CodeProblem::Spaced)),
            ("c#s\nx", Err(CodeProblem::Spaced)),
            ("c#s\u{7}", Err(CodeProblem::Spaced)),
            ("c\u{e9}#s", Err(CodeProblem::Spaced)),
            (" c#s \r\n", Ok(("c", "s"))),
            (
                "CODE_CANARY#STATE_CANARY",
                Ok(("CODE_CANARY", "STATE_CANARY")),
            ),
        ] {
            assert_eq!(split_code_state(pasted), expected, "{pasted:?}");
        }
        let long = format!("{}#s", "c".repeat(KEY_MAX));
        assert_eq!(split_code_state(&long), Err(CodeProblem::TooLong));
        for problem in [
            CodeProblem::Empty,
            CodeProblem::NoState,
            CodeProblem::Repeated,
            CodeProblem::Spaced,
            CodeProblem::TooLong,
        ] {
            assert!(!problem.to_string().contains("CANARY"));
            assert_eq!(
                serde_json::from_value::<CodeProblem>(serde_json::to_value(problem).unwrap())
                    .unwrap(),
                problem
            );
        }
        assert_eq!(
            serde_json::to_value(CodeProblem::NoState).unwrap(),
            "no_state"
        );
    }

    #[test]
    fn a_key_shows_its_known_prefix_stars_and_its_last_four() {
        let key = format!("sk-ant-api03-{}a3F9", "x".repeat(40));
        assert_eq!(
            masked(&key, 31, "\u{2026}"),
            "sk-ant-api03-\u{2026}******\u{2026}a3F9"
        );
        assert_eq!(
            masked(&key, 200, "\u{2026}"),
            "sk-ant-api03-\u{2026}************\u{2026}a3F9  57 characters"
        );
        for room in [31, 200] {
            assert_eq!(
                masked_ends(&key, Ends { start: 13, end: 4 }, true, room, "…"),
                masked(&key, room, "…")
            );
        }
        assert_eq!(masked(&key, 25, "..."), "sk-ant-api03-***...a3F9");
        assert_eq!(masked(&key, 23, "..."), "sk-ant-api03-***...a3F9");
        let project = format!("sk-proj-{}WXYZ", "q".repeat(30));
        assert!(masked(&project, 80, "\u{2026}").starts_with("sk-proj-\u{2026}***"));
        assert!(masked(&project, 80, "\u{2026}").ends_with("\u{2026}WXYZ  42 characters"));
        let code = format!("ye5b{}he2E", "k".repeat(79));
        assert_eq!(
            masked(&code, 34, "\u{2026}"),
            "ye5b\u{2026}******\u{2026}he2E  87 characters"
        );
        assert_eq!(masked(&code, 34, "..."), "ye5b...******...he2E");
        assert_eq!(masked(&code, 16, "..."), "*".repeat(16));
    }

    #[test]
    fn asked_ends_show_at_most_a_third_and_nothing_of_a_short_secret() {
        for count in 0..=60 {
            let value = "A".repeat(count);
            for start in (0..=20).chain([255]) {
                for end in (0..=20).chain([255]) {
                    let mask = masked_ends(&value, Ends { start, end }, true, 200, "…");
                    let shown = mask.chars().filter(|&c| c == 'A').count();
                    assert!(shown == 0 || shown < count);
                    assert!(shown * 3 <= count);
                    assert!(count >= MASK_SHORT || shown == 0);
                }
            }
        }
        let long = format!("sk-proj-{}WXYZ", "q".repeat(152));
        for (value, ends, expected) in [
            (
                long.as_str(),
                Ends { start: 8, end: 4 },
                "sk-proj-…************…WXYZ  164 characters",
            ),
            (
                "ABCDEFGHIJKLMNOPWXYZ",
                Ends { start: 4, end: 4 },
                "************…WXYZ  20 characters",
            ),
            (
                "CANARY12WXYZ",
                Ends { start: 8, end: 4 },
                "************  12 characters",
            ),
            (
                "ABCDEFGH-CANARY-fake-keys-WXYZ",
                Ends { start: 8, end: 0 },
                "ABCDEFGH…************  30 characters",
            ),
            (
                "ABCDEFGHIJKLMNOPQRST",
                Ends { start: 0, end: 4 },
                "************…QRST  20 characters",
            ),
        ] {
            assert_eq!(masked_ends(value, ends, true, 200, "…"), expected);
            assert_eq!(
                masked_ends(value, ends, true, 200, "..."),
                expected.replace('…', "...")
            );
        }
        let value = "CANARY-placeholders-WXYZ";
        assert_eq!(
            masked_ends(value, Ends::default(), true, 80, "…"),
            format!("{}  24 characters", "*".repeat(24))
        );
        assert_eq!(
            masked_ends(value, Ends::default(), true, 28, "…"),
            "*".repeat(24)
        );
        assert_eq!(
            masked_ends(value, Ends::default(), false, 80, "…"),
            "*".repeat(24)
        );
    }

    #[test]
    fn the_shipped_key_prefixes_name_their_vendor_and_never_overlap() {
        let lines: Vec<_> = include_str!("../data/key-prefixes.txt").lines().collect();
        assert_eq!(lines.len(), KEY_PREFIXES.len());
        for (index, line) in lines.iter().enumerate() {
            let mut words = line.split_whitespace();
            let prefix = words.next().unwrap();
            assert_eq!(prefix, KEY_PREFIXES[index]);
            assert!(words.next().is_some());
            assert!(words.next().is_none());
            assert!(prefix.len() >= 4);
            assert!(
                prefix
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            );
            for (other, candidate) in KEY_PREFIXES.iter().enumerate() {
                if other != index {
                    assert!(!prefix.starts_with(candidate));
                }
            }
        }
    }

    #[test]
    fn a_mask_never_shows_more_than_eight_secret_characters() {
        let secret = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghij";
        for value in [
            secret.to_owned(),
            format!("sk-ant-api03-{secret}"),
            format!("sk-{secret}"),
            secret[..MASK_SHORT].to_owned(),
            secret[..MASK_SHORT + 7].to_owned(),
            secret[..MASK_SHORT + 8].to_owned(),
            format!("sk-proj-{}", &secret[..MASK_SHORT]),
            format!("sk-proj-{}", &secret[..4]),
        ]
        .into_iter()
        .chain(
            KEY_PREFIXES
                .iter()
                .map(|prefix| format!("{prefix}CANARY{secret}")),
        ) {
            for room in [0, 5, 12, 20, 34, 80, 200] {
                for ellipsis in ["\u{2026}", "..."] {
                    let mask = masked(&value, room, ellipsis);
                    let open = KEY_PREFIXES
                        .iter()
                        .find(|p| mask.starts_with(**p))
                        .map_or(mask.as_str(), |p| &mask[p.len()..]);
                    let shown = open
                        .split("  ")
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                        .count();
                    let secret = value.len()
                        - KEY_PREFIXES
                            .iter()
                            .find(|p| value.starts_with(**p))
                            .map_or(0, |p| p.len());
                    assert!(shown <= 8, "{value} {room}: {mask}");
                    assert!(shown * 3 <= secret, "{value} {room}: {mask}");
                    assert!(mask.chars().count() <= room, "{value} {room}: {mask}");
                }
            }
        }
    }

    #[test]
    fn one_tail_rule_for_the_echo_and_the_listing() {
        for length in 0..40 {
            let key = format!("{}WXYZ", "k".repeat(length));
            let prefixed = format!("sk-ant-api03-{key}");
            for value in [key, prefixed] {
                let shows = masked(&value, 80, "\u{2026}").contains("WXYZ");
                assert_eq!(key_tail(&value).is_some(), shows, "{value}");
                assert_eq!(shows, length + 4 >= MASK_SHORT, "{value}");
            }
        }
        assert_eq!(key_tail("ABCDEFGHIJKLMNOP"), Some("MNOP"));
        assert_eq!(key_tail("ABCDEFGHIJKLMNO"), None);
        assert_eq!(key_tail("sk-ant-api03-CANARYGHIJ"), None);
        assert_eq!(key_tail("\u{4e2d}".repeat(20).as_str()), None);
    }

    #[test]
    fn a_short_or_unusual_value_shows_only_stars() {
        for value in [
            "CANARY",
            "contains a space CANARY WXYZ",
            "非ASCII-CANARY-xxxxxxxx-WXYZ",
            "control\t-CANARY-xxxxxxxx-WXYZ",
        ] {
            assert_eq!(
                masked_ends(value, Ends { start: 8, end: 4 }, false, 80, "…"),
                "*".repeat(value.chars().count())
            );
        }
        assert_eq!(
            masked_ends(
                &"x".repeat(108),
                Ends { start: 16, end: 4 },
                true,
                28,
                "..."
            ),
            masked_ends(&"x".repeat(108), Ends::default(), true, 28, "...")
        );

        assert_eq!(masked("", 40, "\u{2026}"), "");
        assert_eq!(masked("abc", 40, "\u{2026}"), "***");
        assert_eq!(masked(&"k".repeat(15), 40, "\u{2026}"), "*".repeat(15));
        assert_eq!(masked("sk-ant-api03-abcd", 40, "\u{2026}"), "*".repeat(17));
        let after_prefix = format!("sk-ant-api03-{}", "s".repeat(15));
        assert_eq!(masked(&after_prefix, 40, "\u{2026}"), "*".repeat(28));
        let sixteen = "ABCDEFGHIJKLMNOP";
        assert_eq!(
            masked(sixteen, 40, "\u{2026}"),
            "************\u{2026}MNOP  16 characters"
        );
        let prefixed_sixteen = format!("sk-ant-api03-{sixteen}");
        assert_eq!(
            masked(&prefixed_sixteen, 60, "\u{2026}"),
            "sk-ant-api03-\u{2026}************\u{2026}MNOP  29 characters"
        );
        let twenty_three = "ABCDEFGHIJKLMNOPQRSTUVW";
        assert!(masked(twenty_three, 60, "\u{2026}").starts_with("************\u{2026}TUVW  23"));
        let wide = "\u{4e2d}".repeat(20);
        assert_eq!(masked(&wide, 40, "\u{2026}"), "*".repeat(20));
        let control = format!("{}\u{1b}[2J", "k".repeat(20));
        assert_eq!(masked(&control, 80, "\u{2026}"), "*".repeat(24));
        assert_eq!(masked(&"k".repeat(15), 10, "\u{2026}"), "*".repeat(10));
        assert_eq!(
            masked(&"k".repeat(200), 20, "\u{2026}"),
            "kkkk\u{2026}******\u{2026}kkkk"
        );
    }

    #[test]
    fn a_mask_fits_forty_columns_after_its_label() {
        let code = format!("ye5b{}he2E", "k".repeat(79));
        let key = format!("sk-ant-api03-{}a3F9", "x".repeat(95));
        for (label, value) in [("Code: ", &code), ("", &key)] {
            for ellipsis in ["\u{2026}", "..."] {
                let line = format!("{label}{}", masked(value, 40 - label.len() - 1, ellipsis));
                assert!(line.chars().count() < 40, "{line}");
                assert!(line.contains("***"), "{line}");
                assert!(
                    label.is_empty() || ellipsis == "..." || line.contains("characters"),
                    "{line}"
                );
            }
        }
    }

    #[test]
    fn aiza_gemini_keys_show_their_known_prefix_stars_and_last_four() {
        let gemini = "AIzaCANARY_gemini_key_with_underscre_1";
        assert!(has_known_key_prefix(gemini));
        assert!(has_known_key_prefix("AIzaCANARY_123456789"));
        assert!(!has_known_key_prefix("AIzaCANARY_12345678"));
        assert!(!has_known_key_prefix("claude-sonnet-4-5-20250929"));
        assert_eq!(key_tail(gemini), Some("re_1"));
        assert_eq!(
            masked(gemini, 80, "\u{2026}"),
            "AIza\u{2026}************\u{2026}re_1  38 characters"
        );
        assert_eq!(masked(gemini, 20, "..."), "AIza...******...re_1");
        assert_eq!(masked(gemini, 16, "..."), "AIza***...re_1");
        assert_eq!(masked("AIzaShort12345", 40, "\u{2026}"), "*".repeat(14));
    }
}

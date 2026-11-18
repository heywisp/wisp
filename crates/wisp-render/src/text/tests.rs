use super::*;
use crate::styled::{StyledLine, StyledSpan};
use crate::testing::{line_text, texts};

#[test]
fn sanitizer_keeps_text_and_shows_controls() {
    assert_eq!(sanitize("a\tb\nc é 😀"), "a\tb\nc é 😀");
    assert_eq!(
        sanitize("\x1b\r\x07\x00\x7f\u{80}\u{9b}\u{9f}"),
        "^[^M^G^@^?M-^@M-^[M-^_"
    );
}

#[test]
fn sanitize_keeps_the_caret_notation() {
    assert_eq!(
        sanitize("\x1b]0;title\x07\r\x7f\u{9b}2J"),
        "^[]0;title^G^M^?M-^[2J"
    );
    assert_eq!(sanitize("a\tb\nc"), "a\tb\nc");
}

#[test]
fn sanitize_shows_every_bidi_control() {
    let controls = [
        '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}',
        '\u{2068}', '\u{2069}', '\u{200E}', '\u{200F}', '\u{061C}',
    ];
    for c in controls {
        let shown = sanitize(&format!("a{c}b"));
        assert_eq!(shown, format!("a<U+{:04X}>b", u32::from(c)));
    }
    assert_eq!(sanitize("user\u{202E}gnp.exe"), "user<U+202E>gnp.exe");
    let hidden = [
        '\u{2028}',
        '\u{2029}',
        '\u{2060}',
        '\u{2061}',
        '\u{2062}',
        '\u{2063}',
        '\u{2064}',
        '\u{206A}',
        '\u{206B}',
        '\u{206C}',
        '\u{206D}',
        '\u{206E}',
        '\u{206F}',
        '\u{FFF9}',
        '\u{FFFA}',
        '\u{FFFB}',
        '\u{E0000}',
        '\u{E0001}',
        '\u{E0041}',
        '\u{E007F}',
    ];
    for c in hidden {
        let shown = sanitize(&format!("a{c}b"));
        assert_eq!(shown, format!("a<U+{:04X}>b", u32::from(c)));
    }
    let england = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}";
    let scotland = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}";
    let wales = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0077}\u{E006C}\u{E0073}\u{E007F}";
    for flag in [england, scotland, wales] {
        assert_eq!(sanitize(flag), flag);
        assert_eq!(sanitize(&format!("a{flag}b")), format!("a{flag}b"));
    }
    assert_eq!(
        sanitize("\u{1F3F4}\u{E0067}\u{E0062}"),
        "\u{1F3F4}<U+E0067><U+E0062>"
    );
    assert_eq!(sanitize("\u{1F3F4}\u{E007F}"), "\u{1F3F4}<U+E007F>");
    assert_eq!(
        sanitize("\u{1F3F4}\u{E0048}\u{E0049}\u{E0020}\u{E0021}\u{E007F}"),
        "\u{1F3F4}<U+E0048><U+E0049><U+E0020><U+E0021><U+E007F>"
    );
    assert_eq!(
        sanitize(
            "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E0067}\u{E0067}\u{E007F}"
        ),
        "\u{1F3F4}<U+E0067><U+E0062><U+E0065><U+E006E><U+E0067><U+E0067><U+E0067><U+E007F>"
    );
    assert_eq!(
        sanitize("\u{1F3F4}\u{E0067}\u{E007F}"),
        "\u{1F3F4}<U+E0067><U+E007F>"
    );
    assert_eq!(sanitize("x\u{E0067}\u{E007F}"), "x<U+E0067><U+E007F>");
    assert_eq!(
        sanitize("ok\u{E0049}\u{E0047}\u{E004E}"),
        "ok<U+E0049><U+E0047><U+E004E>"
    );
}

#[test]
fn sanitize_drops_zwsp_and_bom_and_keeps_joiners() {
    assert_eq!(sanitize("\u{FEFF}a\u{200B}b"), "ab");
    let family = "\u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    assert_eq!(sanitize(family), family);
    assert_eq!(sanitize("a\u{200C}b"), "a\u{200C}b");
    assert_eq!(sanitize("\u{2764}\u{FE0F}"), "\u{2764}\u{FE0F}");
    assert_eq!(sanitize("\u{845B}\u{E0100}"), "\u{845B}\u{E0100}");
}

#[test]
fn sanitize_is_idempotent() {
    let corpus = [
        "\x1b[31mred\x1b[0m",
        "\u{202E}abc\u{2066}",
        "\u{200B}\u{FEFF}x",
        "FAKE PROMPT>\r$ ",
        "\x1bP+q\x1b\\",
        "\u{9b}31m\u{9d}52;c;aGVsbG8=\u{9c}",
    ];
    for text in corpus {
        let once = sanitize(text);
        assert_eq!(sanitize(&once), once, "{text:?}");
        assert!(
            !once
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t'),
            "{once:?}"
        );
    }
}

#[test]
fn tabs_expand_to_stops_per_line() {
    assert_eq!(expand_tabs("\tx", 4, WidthMode::Normal), "    x");
    assert_eq!(expand_tabs("ab\tx", 4, WidthMode::Normal), "ab  x");
    assert_eq!(expand_tabs("abcd\tx", 4, WidthMode::Normal), "abcd    x");
    assert_eq!(expand_tabs("a\n\tb", 4, WidthMode::Normal), "a\n    b");
    assert_eq!(
        expand_tabs("\u{4E2D}\tx", 4, WidthMode::Normal),
        "\u{4E2D}  x"
    );
    assert_eq!(expand_tabs("\u{B7}\tx", 4, WidthMode::Normal), "\u{B7}   x");
    assert_eq!(
        expand_tabs("\u{B7}\tx", 4, WidthMode::AmbiguousWide),
        "\u{B7}  x"
    );
    assert_eq!(expand_tabs("\tx", 0, WidthMode::Normal), " x");
}

#[test]
fn width_in_both_modes() {
    let cases: &[(&str, usize, usize)] = &[
        ("hello", 5, 5),
        ("\u{4E2D}\u{6587}", 4, 4),
        ("\u{3053}\u{3093}\u{306B}\u{3061}\u{306F}", 10, 10),
        ("\u{1F600}", 2, 2),
        ("\u{1F469}\u{200D}\u{1F4BB}", 2, 2),
        (
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
            2,
            2,
        ),
        ("\u{1F1EC}\u{1F1E7}", 2, 2),
        ("\u{2764}\u{FE0F}", 2, 2),
        ("e\u{301}", 1, 1),
        ("\u{B7}", 1, 2),
        ("\u{2500}", 1, 2),
        ("\u{2026}", 1, 2),
        ("\u{2219}", 1, 1),
        ("\u{254E}", 1, 1),
        ("\u{2836}", 1, 1),
        ("a\u{200D}b", 2, 2),
        ("", 0, 0),
    ];
    for (text, normal, wide) in cases {
        assert_eq!(width(text, WidthMode::Normal), *normal, "{text:?}");
        assert_eq!(width(text, WidthMode::AmbiguousWide), *wide, "{text:?}");
    }
}

#[test]
fn width_agrees_with_ratatui() {
    use ratatui_core::text::Span;
    for text in [
        "plain",
        "\u{4E2D}\u{6587}",
        "\u{1F469}\u{200D}\u{1F4BB} ok",
        "e\u{301}\u{B7}",
    ] {
        assert_eq!(
            width(text, WidthMode::Normal),
            Span::raw(text).width(),
            "{text:?}"
        );
    }
}

#[test]
fn the_width_mode_follows_the_glyph_set() {
    use crate::theme::glyphs::GlyphSet;
    assert_eq!(WidthMode::from(GlyphSet::Unicode), WidthMode::Normal);
    assert_eq!(
        WidthMode::from(GlyphSet::AmbiguousWide),
        WidthMode::AmbiguousWide
    );
    assert_eq!(WidthMode::from(GlyphSet::Ascii), WidthMode::Normal);
}

fn rows(text: &str, width: usize) -> Vec<String> {
    texts(&wrap(&[StyledSpan::plain(text)], width, WidthMode::Normal))
}

#[test]
fn wrap_is_greedy_and_trims() {
    assert_eq!(
        rows("the quick brown fox jumps", 10),
        ["the quick", "brown fox", "jumps"]
    );
    assert_eq!(rows("a  b", 10), ["a  b"]);
    assert_eq!(rows("abc   ", 10), ["abc"]);
    assert_eq!(rows("abcde fgh", 5), ["abcde", "fgh"]);
    assert_eq!(rows("ab cdefghijkl", 5), ["ab", "cdefg", "hijkl"]);
    assert_eq!(rows("one\ntwo", 10), ["one", "two"]);
    assert_eq!(rows("one\n\n  two", 10), ["one", "", "  two"]);
    assert_eq!(rows("", 10), [""]);
    assert_eq!(rows("abc", 0), ["a", "b", "c"]);
}

#[test]
fn soft_wrapped_rows_say_how_they_join_the_row_before() {
    let joins = |text: &str, width: usize| -> Vec<Option<(bool, usize)>> {
        wrap(&[StyledSpan::plain(text)], width, WidthMode::Normal)
            .iter()
            .map(|l| l.joined.map(|j| (j.space, j.lead)))
            .collect()
    };
    assert_eq!(
        joins("the quick brown fox jumps", 10),
        [None, Some((true, 0)), Some((true, 0))]
    );
    assert_eq!(joins("abcdefghij", 5), [None, Some((false, 0))]);
    assert_eq!(joins("one\ntwo", 10), [None, None]);
    let wrapped = wrap(&[StyledSpan::plain("aa bb")], 3, WidthMode::Normal);
    assert_eq!(wrapped[1], StyledLine::new(vec![StyledSpan::plain("bb")]));
    assert_eq!(
        wrapped[1]
            .clone()
            .led_by(&[StyledSpan::plain("  ")])
            .joined
            .map(|j| j.lead),
        Some(2)
    );
}

#[test]
fn wrap_breaks_between_graphemes_never_inside() {
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let text = format!("{family}{family}{family}");
    assert_eq!(
        rows(&text, 4),
        [format!("{family}{family}"), family.to_owned()]
    );
    assert_eq!(
        rows("e\u{301}e\u{301}e\u{301}", 2),
        ["e\u{301}e\u{301}", "e\u{301}"]
    );
    assert_eq!(
        rows("\u{4E2D}\u{6587}\u{4E2D}", 5),
        ["\u{4E2D}\u{6587}", "\u{4E2D}"]
    );
    assert_eq!(rows("\u{4E2D}", 1), [TOO_WIDE]);
    assert_eq!(rows("\u{4E2D}", 2), ["\u{4E2D}"]);
}

#[test]
fn wrap_keeps_styles_and_merges_neighbours() {
    use crate::styled::Look;
    use crate::theme::Token;
    let spans = [
        StyledSpan::plain("one "),
        StyledSpan::new("two", Look::default().bold()),
        StyledSpan::plain(" three"),
        StyledSpan::new(" four", Token::Accent),
    ];
    let out = wrap(&spans, 9, WidthMode::Normal);
    assert_eq!(texts(&out), ["one two", "three", "four"]);
    assert_eq!(
        out[0].spans[1],
        StyledSpan::new("two", Look::default().bold())
    );
    assert_eq!(out[2].spans, [StyledSpan::new("four", Token::Accent)]);
}

#[test]
fn rows_never_exceed_the_width() {
    let corpus = [
        "the quick brown fox jumps over the lazy dog",
        "\u{4E2D}\u{6587}\u{5B57}\u{7B26}\u{4E32}\u{6D4B}\u{8BD5}\u{4E2D}\u{6587}",
        "\u{1F600}\u{1F600} \u{1F469}\u{200D}\u{1F4BB}\u{1F469}\u{200D}\u{1F4BB} x",
        "averyveryveryverylongwordwithoutanyspacesatall and more",
    ];
    for text in corpus {
        for w in 2..40 {
            for mode in [WidthMode::Normal, WidthMode::AmbiguousWide] {
                for row in wrap(&[StyledSpan::plain(text)], w, mode) {
                    let row = line_text(&row);
                    assert!(width(&row, mode) <= w, "{w} {row:?}");
                    assert_eq!(row.trim_end(), row);
                }
            }
        }
    }
}

fn prefix_stable(
    text: &str,
    w: usize,
    mode: WidthMode,
    how: fn(&[StyledSpan], usize, WidthMode) -> Vec<StyledLine>,
) {
    let full = how(&[StyledSpan::plain(text)], w, mode);
    for (cut, _) in text.char_indices() {
        let part = how(&[StyledSpan::plain(&text[..cut])], w, mode);
        let settled = part.len().saturating_sub(1);
        assert_eq!(
            part[..settled],
            full[..settled],
            "width {w}, prefix {:?}",
            &text[..cut]
        );
    }
}

#[test]
fn wrapping_is_prefix_stable() {
    let corpus = [
        "the quick brown fox jumps over the lazy dog and keeps running",
        "a bb ccc dddd eeeee ffffff ggggggg hhhhhhhh iiiiiiiii",
        "short averyveryverylongwordthatmustbesplit tail words here",
        "  leading spaces\nand a hard break  then   gaps",
        "\u{4E2D}\u{6587} \u{5B57}\u{7B26}\u{4E32}\u{6D4B}\u{8BD5} mixed \u{1F469}\u{200D}\u{1F4BB} e\u{301}x",
        "https://example.com/a/very/long/path/that/keeps/going/and/going ok",
        "\u{B7} a \u{2026}\u{2026} \u{2500}\u{2500}\u{2500} \u{3B1}\u{3B2}\u{3B3} word \u{2764}\u{FE0F}",
    ];
    for text in corpus {
        assert!(!text.contains('\u{FE0E}'));
        for w in 1..30 {
            for mode in [WidthMode::Normal, WidthMode::AmbiguousWide] {
                prefix_stable(text, w, mode, wrap);
                prefix_stable(text, w, mode, split);
            }
        }
    }
}

#[test]
fn split_keeps_spaces_and_breaks_anywhere() {
    let out = split(&[StyledSpan::plain("    let x = 1;")], 6, WidthMode::Normal);
    assert_eq!(texts(&out), ["    le", "t x = ", "1;"]);
}

#[test]
fn wrap_words_keeps_a_word_wider_than_the_row_whole_on_its_own_row() {
    let path = format!("/home/someone/{}/0193.jsonl", "x".repeat(60));
    let text = format!("log {path} after");
    let got = texts(&wrap_words(
        &[StyledSpan::plain(text.as_str())],
        20,
        WidthMode::Normal,
    ));
    assert_eq!(got, vec!["log".to_owned(), path, "after".to_owned()]);
    assert_eq!(
        texts(&wrap_words(
            &[StyledSpan::plain("one two three")],
            7,
            WidthMode::Normal
        )),
        rows("one two three", 7)
    );
}

#[test]
fn strip_ansi_removes_whole_csi_osc_dcs_and_charset_sequences() {
    let cases = [
        ("a\x1b[1;31mred\x1b[0m b", "ared b"),
        ("safe \x1b]52;c;cHduZWQ=\x07text", "safe text"),
        ("\x1b]8;;https://x.test\x1b\\link\x1b]8;;\x1b\\", "link"),
        ("\x1bPq#0;2;0;0;0\x1b\\after", "after"),
        ("\x1b(Bplain\x1b=", "plain"),
        ("\x1b[?2004h\x1b[2J\x1b[Hdone", "done"),
        ("unterminated \x1b]0;title", "unterminated "),
    ];
    for (raw, want) in cases {
        assert_eq!(strip_ansi(raw), want, "{raw:?}");
    }
}

#[test]
fn a_stray_string_start_ends_at_the_line_and_never_hides_later_lines() {
    let cases = [
        ("line1\n\x1bPstray\nline3", "line1\n\nline3"),
        ("a\x1b]0;title\nb\nc\n", "a\nb\nc\n"),
        ("\x1bXsos\nnext", "\nnext"),
        ("\x1b^pm\nnext", "\nnext"),
        ("\x1b_apc\nnext", "\nnext"),
        ("x\x1b]0;t\u{18}after", "xafter"),
        ("x\x1b]0;t\u{1a}after", "xafter"),
        ("x\x1b]0;t\x1b[31mred", "xred"),
        ("x\x1b]0;t\u{9c}after", "xafter"),
        ("x\x1b[12\u{18}after", "xafter"),
        ("x\x1b[1\nafter", "x\nafter"),
    ];
    for (raw, want) in cases {
        assert_eq!(strip_ansi(raw), want, "{raw:?}");
    }
}

#[test]
fn eight_bit_c1_starts_are_sequences_too() {
    let cases = [
        ("a\u{9b}2Jb", "ab"),
        ("a\u{9d}0;title\u{7}b", "ab"),
        ("a\u{90}qdata\u{9c}b", "ab"),
        ("a\u{98}sos\x1b\\b", "ab"),
        ("a\u{9e}pm\u{7}b", "ab"),
        ("a\u{9f}apc\u{7}b", "ab"),
        ("a\u{9d}open\nb", "a\nb"),
    ];
    for (raw, want) in cases {
        assert_eq!(strip_ansi(raw), want, "{raw:?}");
    }
}
const FAMILY: &str = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
const N: WidthMode = WidthMode::Normal;

fn spans(texts: &[&str]) -> Vec<StyledSpan> {
    texts.iter().map(|t| StyledSpan::plain(*t)).collect()
}

#[test]
fn the_ascii_fast_path_measures_spans_like_the_grapheme_path() {
    let slow = |s: &[StyledSpan]| s.iter().map(|s| width(&s.text, N)).sum::<usize>();
    for case in [
        spans(&[]),
        spans(&["", ""]),
        spans(&["hello", " world"]),
        spans(&["tab\there"]),
        spans(&["bell\x07", "x"]),
        spans(&["\r\n"]),
        spans(&["ascii", "界", "e\u{301}", FAMILY]),
    ] {
        assert_eq!(spans_width(&case, N), slow(&case), "{case:?}");
    }
    assert_eq!(spans_width(&[], N), 0);
    assert_eq!(spans_width(&spans(&["\r\n"]), N), width("\r\n", N));
    assert_eq!(spans_text(&spans(&["a", "界", "b"])), "a界b");
}

#[test]
fn fit_end_keeps_the_head_and_never_exceeds_the_room() {
    assert_eq!(fit_end("abcdef", 6, "\u{2026}", N), "abcdef");
    assert_eq!(fit_end("abcdef", 4, "\u{2026}", N), "abc\u{2026}");
    assert_eq!(fit_end("abcdef", 4, "...", N), "a...");
    assert_eq!(fit_end("界界界", 4, "\u{2026}", N), "界\u{2026}");
    assert_eq!(
        fit_end("e\u{301}e\u{301}e\u{301}", 2, "\u{2026}", N),
        "e\u{301}\u{2026}"
    );
    assert_eq!(
        fit_end(&format!("a{FAMILY}bc"), 3, "\u{2026}", N),
        "a\u{2026}"
    );
    assert_eq!(
        fit_end(&format!("a{FAMILY}bc"), 4, "\u{2026}", N),
        format!("a{FAMILY}\u{2026}")
    );
    assert_eq!(fit_end("abc", 1, "\u{2026}", N), "\u{2026}");
    assert_eq!(fit_end("abc", 0, "\u{2026}", N), "");
    assert_eq!(fit_end("abcdef", 2, "...", N), "");
    assert_eq!(fit_end("abc", 1, "", N), "a");
    assert_eq!(fit_end("abc", 0, "", N), "");
}

#[test]
fn fit_start_keeps_the_tail() {
    assert_eq!(fit_start("abcdef", 6, "\u{2026}", N), "abcdef");
    assert_eq!(fit_start("abcdef", 4, "\u{2026}", N), "\u{2026}def");
    assert_eq!(fit_start("abcdef", 4, "...", N), "...f");
    assert_eq!(fit_start("界界界", 4, "\u{2026}", N), "\u{2026}界");
    assert_eq!(
        fit_start("e\u{301}e\u{301}e\u{301}", 2, "\u{2026}", N),
        "\u{2026}e\u{301}"
    );
    assert_eq!(
        fit_start(&format!("ab{FAMILY}c"), 3, "\u{2026}", N),
        "\u{2026}c"
    );
    assert_eq!(
        fit_start(&format!("ab{FAMILY}c"), 4, "\u{2026}", N),
        format!("\u{2026}{FAMILY}c")
    );
    assert_eq!(fit_start("abc", 1, "\u{2026}", N), "\u{2026}");
    assert_eq!(fit_start("abc", 0, "", N), "");
    assert_eq!(fit_start("abc", 1, "", N), "c");
}

#[test]
fn fit_middle_gives_the_head_the_odd_column_and_the_tail_the_rest() {
    assert_eq!(fit_middle("abcdefghij", 6, "\u{2026}", N), "abc\u{2026}ij");
    assert_eq!(fit_middle("abcdefghij", 7, "\u{2026}", N), "abc\u{2026}hij");
    assert_eq!(fit_middle("abcdefghij", 10, "\u{2026}", N), "abcdefghij");
    assert_eq!(fit_middle("abcdefghij", 7, "...", N), "ab...ij");
    assert_eq!(
        fit_middle("e\u{301}bcdefghe\u{301}", 5, "\u{2026}", N),
        "e\u{301}b\u{2026}he\u{301}"
    );
    assert_eq!(
        fit_middle(&format!("{FAMILY}cdefgh{FAMILY}"), 7, "\u{2026}", N),
        format!("{FAMILY}c\u{2026}h{FAMILY}")
    );
    assert_eq!(fit_middle("abcdefghij", 2, "\u{2026}", N), "a\u{2026}");
    assert_eq!(fit_middle("abcdefghij", 1, "\u{2026}", N), "a");
    assert_eq!(fit_middle("abcdefghij", 0, "\u{2026}", N), "");
    assert_eq!(fit_middle("abcdefghij", 3, "...", N), "abc");
    assert_eq!(fit_middle("abcdefghij", 4, "...", N), "a...");
}

#[test]
fn fit_middle_hands_a_cell_the_head_cannot_use_to_the_tail() {
    let shown = fit_middle("ab界cdefgh", 7, "\u{2026}", N);
    assert_eq!(shown, "ab\u{2026}efgh");
    assert_eq!(width(&shown, N), 7);
    for room in 2..10 {
        let shown = fit_middle("界界界界界界", room, "\u{2026}", N);
        assert!(width(&shown, N) <= room, "{room}: {shown}");
    }
}

#[test]
fn cap_chars_counts_chars_not_columns() {
    assert_eq!(cap_chars("abc".to_owned(), 3), "abc");
    assert_eq!(cap_chars("abcd".to_owned(), 3), "abc\u{2026}");
    assert_eq!(cap_chars("界界界界".to_owned(), 2), "界界\u{2026}");
    assert_eq!(cap_chars("abc".to_owned(), 0), "\u{2026}");
}

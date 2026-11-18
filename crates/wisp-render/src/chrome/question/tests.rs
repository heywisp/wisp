use wisp_protocol::{Answer, Question, QuestionOption};

use super::*;
use crate::markdown::Layout;
use crate::testing::line_text;
use crate::text::{WidthMode, width};
use crate::theme::glyphs::GlyphSet;

const HOSTILE: &str = "a\u{1b}]0;pwned\u{7}b\u{1b}[2J\u{9b}31mc\r\nd";

fn question(header: &str, labels: &[(&str, &str)], multi_select: bool) -> Question {
    Question {
        header: header.to_owned(),
        question: format!("Which {header} should it be?"),
        options: labels
            .iter()
            .map(|(label, description)| QuestionOption {
                label: (*label).to_owned(),
                description: (*description).to_owned(),
            })
            .collect(),
        multi_select,
        source: None,
    }
}

fn database() -> Question {
    question(
        "Database",
        &[
            (
                "PostgreSQL (Recommended)",
                "Relational; the billing service already runs it.",
            ),
            ("SQLite", "One file, no server; fine for a single writer."),
        ],
        false,
    )
}

fn texts(rows: &CardRows) -> Vec<String> {
    rows.lines
        .iter()
        .map(|l| line_text(l).trim_end().to_owned())
        .collect()
}

fn view<'a>(
    questions: &'a [Question],
    drafts: &'a [Draft],
    page: CardPage,
    row: usize,
) -> CardView<'a> {
    CardView {
        questions,
        drafts,
        page,
        row,
        cursor: None,
        mode: CardMode::Active,
    }
}

#[test]
fn a_header_is_cut_at_twelve_characters() {
    assert_eq!(cut_header("Database", GlyphSet::Unicode), "Database");
    assert_eq!(
        cut_header("Twelve chars", GlyphSet::Unicode),
        "Twelve chars"
    );
    assert_eq!(
        cut_header("Thirteen char", GlyphSet::Unicode),
        "Thirteen cha\u{2026}"
    );
    assert_eq!(
        cut_header("Thirteen char", GlyphSet::Ascii),
        "Thirteen cha..."
    );
}

#[test]
fn answers_are_named_by_label_and_own_words() {
    let q = question("F", &[("Auth", ""), ("Search", ""), ("Export", "")], true);
    let set = GlyphSet::Unicode;
    assert_eq!(answer_words(&q, &Answer::skipped(), set), "skipped");
    assert_eq!(answer_words(&q, &Answer::choose(1), set), "Search");
    let both = Answer {
        chosen: vec![0, 2],
        own: Some("Webhooks\nand a feed".to_owned()),
    };
    assert_eq!(
        answer_words(&q, &both, set),
        "Auth \u{b7} Export \u{b7} your own: Webhooks \u{2026}"
    );
    assert_eq!(
        answer_words(&q, &both, GlyphSet::Ascii),
        "Auth - Export - your own: Webhooks ..."
    );
}

#[test]
fn one_question_draws_its_options_the_own_row_and_the_keys() {
    let questions = [database()];
    let drafts = [Draft::default()];
    let rows = question_rows(
        &view(&questions, &drafts, CardPage::Question(0), 0),
        20,
        Layout::new(80, GlyphSet::Unicode),
    );
    let shown = texts(&rows);
    assert!(shown[0].starts_with(" ? Database"), "{shown:#?}");
    assert!(shown[0].ends_with("question 1 of 1"), "{shown:#?}");
    assert_eq!(shown[1], "   Which Database should it be?");
    assert_eq!(shown[3], " \u{203A} 1 PostgreSQL (Recommended)");
    assert_eq!(
        shown[4],
        "     Relational; the billing service already runs it."
    );
    assert_eq!(shown[7], "   3 Type your own answer");
    let keys = "   enter send \u{b7} 1\u{2013}3 choose \u{b7} pgup/pgdn scroll \u{b7} \u{2191}\u{2193} move";
    assert_eq!(shown[9], keys);
    let narrow = question_rows(
        &view(&questions, &drafts, CardPage::Question(0), 0),
        20,
        Layout::new(40, GlyphSet::Unicode),
    );
    assert_eq!(
        texts(&narrow).last().map(String::as_str),
        Some("   enter send \u{b7} 1\u{2013}3 choose")
    );
    let many = [question("Many", &[("a", ""); 9], false)];
    let keys = hints(
        &view(&many, &drafts, CardPage::Question(0), 0),
        Layout::new(80, GlyphSet::Unicode),
    );
    assert!(!format!("{keys:?}").contains(copy::CHOOSE_WORD), "{keys:?}");
    assert_eq!(rows.hits[3], Some(CardHit::Option(0)));
    assert_eq!(rows.hits[4], Some(CardHit::Option(0)));
    assert_eq!(rows.hits[7], Some(CardHit::Own));
    assert!(
        rows.lines
            .iter()
            .all(|l| width(&line_text(l), Layout::new(80, GlyphSet::Unicode).width_mode) == 80)
    );
}

#[test]
fn a_long_description_wraps_under_its_label() {
    let long = "word ".repeat(30);
    let questions = [question("H", &[("A", long.trim()), ("B", "")], false)];
    let drafts = [Draft::default()];
    let rows = question_rows(
        &view(&questions, &drafts, CardPage::Question(0), 0),
        30,
        Layout::new(40, GlyphSet::Unicode),
    );
    let shown = texts(&rows);
    let described: Vec<&String> = shown.iter().filter(|l| l.contains("word")).collect();
    assert!(described.len() > 3, "{shown:#?}");
    assert!(
        described.iter().all(|l| l.starts_with("     word")),
        "{shown:#?}"
    );
    assert!(
        rows.lines
            .iter()
            .all(|l| width(&line_text(l), Layout::new(80, GlyphSet::Unicode).width_mode) == 40)
    );
    let broken = format!("first line\n{}", long.trim());
    let mut asked = question("H", &[("A", &broken)], false);
    asked.question = "Asked here?\nThen this line.".to_owned();
    let questions = [asked];
    let rows = question_rows(
        &view(&questions, &drafts, CardPage::Question(0), 0),
        30,
        Layout::new(40, GlyphSet::Unicode),
    );
    let shown = texts(&rows);
    let at = |prefix: &str| {
        shown
            .iter()
            .position(|l| l.trim_start().starts_with(prefix))
    };
    let first = at("first line").unwrap_or_else(|| panic!("{shown:#?}"));
    assert_eq!(shown[first].trim_end(), "     first line", "{shown:#?}");
    assert!(shown[first + 1].starts_with("     word"), "{shown:#?}");
    let asked = at("Asked here?").unwrap_or_else(|| panic!("{shown:#?}"));
    assert!(
        shown[asked + 1].trim_start().starts_with("Then this line."),
        "{shown:#?}"
    );
}

#[test]
fn a_short_view_gets_the_compact_card() {
    let questions = [database()];
    let drafts = [Draft::default()];
    let layout = Layout::new(60, GlyphSet::Unicode);
    let rows = question_rows(
        &view(&questions, &drafts, CardPage::Question(0), 1),
        QUESTION_MIN_ROWS - 1,
        layout,
    );
    let shown = texts(&rows);
    assert_eq!(shown.len(), 3, "{shown:#?}");
    assert!(
        shown[0].starts_with(" ? Database \u{b7} Which Database"),
        "{shown:#?}"
    );
    assert!(shown[1].starts_with(" \u{203A} 2 SQLite"), "{shown:#?}");
    assert!(shown[1].ends_with("\u{2039} 2/3 \u{203A}"), "{shown:#?}");
    assert!(!shown.iter().any(|l| l.contains("One file")), "{shown:#?}");
    assert_eq!(rows.hits[1], Some(CardHit::Option(1)));
    let two = question_rows(
        &view(&questions, &drafts, CardPage::Question(0), 1),
        2,
        layout,
    );
    assert_eq!(two.lines.len(), 2);
    assert_eq!(rows.lines[1], two.lines[0]);
    let long = [Question {
        question: "Which features should ship in the first release of the notes service?"
            .to_owned(),
        ..question("Features", &[("Auth", ""), ("Search", "")], true)
    }];
    let narrow = Layout::new(40, GlyphSet::Unicode);
    let card = view(&long, &drafts, CardPage::Question(0), 0);
    let shown = texts(&question_rows(&card, QUESTION_MIN_ROWS - 1, narrow));
    let words = shown[..shown.len() - 2].join(" ");
    for word in long[0]
        .question
        .split_whitespace()
        .chain(["(choose", "any)"])
    {
        assert!(words.contains(word), "{word} {shown:#?}");
    }
    assert!(shown[shown.len() - 2].contains("Auth"), "{shown:#?}");
    let cut = texts(&question_rows(&card, 3, narrow));
    assert_eq!(cut.len(), 3, "{cut:#?}");
    assert!(cut[0].ends_with("should\u{2026}"), "{cut:#?}");
    let edge_case = [Question {
        question: "Which database should the notes service use first?".to_owned(),
        ..database()
    }];
    let card = view(&edge_case, &drafts, CardPage::Question(0), 0);
    let first = &texts(&question_rows(&card, 3, narrow))[0];
    assert!(first.ends_with('\u{2026}'), "{first:?}");
    assert!(first.chars().count() < 40, "{first:?}");
}

#[test]
fn a_form_whose_title_wraps_in_a_short_room_keeps_its_answers_and_keys() {
    let options = answer_options(&["Yes", "No"]);
    let footer = [
        KeyHint::new("esc", "cancel"),
        KeyHint::new("enter", "choose"),
    ];
    let form = FormView {
        title: "The keychain isn't running. Keep this key in ~/.config/wisp/credentials.json instead?",
        options: &options,
        selected: Some(1),
        footer: &footer,
        ..FormView::default()
    };
    let narrow = Layout::new(40, GlyphSet::Unicode);
    let whole = texts(&form_rows(&form, 12, narrow));
    assert_eq!(whole.len(), 9, "{whole:#?}");
    let cut = texts(&form_rows(&form, 4, narrow));
    assert_eq!(cut.len(), 4, "{cut:#?}");
    assert_eq!(
        cut[0].trim_end(),
        " ? The keychain isn't running. Keep\u{2026}"
    );
    assert!(
        cut[1].contains("1 Yes") && cut[2].contains("2 No"),
        "{cut:#?}"
    );
    assert!(cut[3].contains("enter choose"), "{cut:#?}");
}

#[test]
fn a_tall_card_keeps_the_focused_row_and_the_keys_in_view() {
    let labels: Vec<(String, String)> = (0..4)
        .map(|i| {
            (
                format!("Option {i}"),
                "a fairly long description ".repeat(4),
            )
        })
        .collect();
    let pairs: Vec<(&str, &str)> = labels
        .iter()
        .map(|(l, d)| (l.as_str(), d.as_str()))
        .collect();
    let questions = [question("H", &pairs, false)];
    let drafts = [Draft::default()];
    let rows = question_rows(
        &view(&questions, &drafts, CardPage::Question(0), 4),
        10,
        Layout::new(40, GlyphSet::Unicode),
    );
    let shown = texts(&rows);
    assert_eq!(shown.len(), 10, "{shown:#?}");
    assert!(shown[0].starts_with(" ? H"), "{shown:#?}");
    assert!(
        shown.iter().any(|l| l.contains("Type your own answer")),
        "{shown:#?}"
    );
    assert!(shown[9].starts_with("   enter send"), "{shown:#?}");
}

#[test]
fn several_questions_show_tabs_and_a_review() {
    let questions = [
        database(),
        question("Features", &[("Auth", ""), ("Search", "")], true),
    ];
    let drafts = [
        Draft {
            chosen: vec![0],
            answered: true,
            ..Draft::default()
        },
        Draft {
            chosen: vec![1],
            own: "Webhooks".to_owned(),
            own_chosen: true,
            answered: false,
        },
    ];
    let layout = Layout::new(80, GlyphSet::Unicode);
    let page = question_rows(
        &view(&questions, &drafts, CardPage::Question(1), 2),
        20,
        layout,
    );
    let shown = texts(&page);
    assert!(
        shown[0].ends_with("Database \u{2713} \u{b7} Features \u{2713}  2 of 2"),
        "{shown:#?}"
    );
    assert!(shown[1].ends_with("(choose any)"), "{shown:#?}");
    assert!(
        shown
            .iter()
            .any(|l| l == " \u{203A} [x] 3 Your own: Webhooks"),
        "{shown:#?}"
    );
    assert!(shown.iter().any(|l| l == "   [x] 2 Search"), "{shown:#?}");
    let review = question_rows(&view(&questions, &drafts, CardPage::Review, 0), 20, layout);
    let shown = texts(&review);
    assert!(shown[0].starts_with(" ? Review"), "{shown:#?}");
    assert_eq!(shown[1], "   Database   PostgreSQL (Recommended)");
    assert_eq!(shown[2], "   Features   Search \u{b7} your own: Webhooks");
    assert!(
        shown.contains(&" \u{203A} Send answers".to_owned()),
        "{shown:#?}"
    );
    assert!(review.hits.contains(&Some(CardHit::Back)));
}

#[test]
fn the_own_row_wraps_and_shows_the_caret() {
    let questions = [database()];
    let drafts = [Draft {
        own: "first line\nsecond".to_owned(),
        ..Draft::default()
    }];
    let card = CardView {
        cursor: Some((1, 3)),
        ..view(&questions, &drafts, CardPage::Question(0), 2)
    };
    let rows = question_rows(&card, 20, Layout::new(80, GlyphSet::Unicode));
    let shown = texts(&rows);
    let at = shown
        .iter()
        .position(|l| l.contains("Your own: first line"))
        .unwrap();
    assert_eq!(shown[at + 1].trim(), "second");
    let caret: Vec<&str> = rows.lines[at + 1]
        .spans
        .iter()
        .filter(|s| s.look.reverse)
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(caret, ["o"]);
    let long = [Question {
        question: "Which database should the notes service use for its first release, given \
                   that billing already runs PostgreSQL?"
            .to_owned(),
        ..database()
    }];
    let typed = [Draft {
        own: (1..=24)
            .map(|n| format!("w{n}"))
            .collect::<Vec<_>>()
            .join(" "),
        ..Draft::default()
    }];
    let card = CardView {
        cursor: Some((0, typed[0].own.chars().count())),
        ..view(&long, &typed, CardPage::Question(0), 2)
    };
    let narrow = Layout::new(40, GlyphSet::Unicode);
    for room in QUESTION_MIN_ROWS..=11 {
        let rows = question_rows(&card, room, narrow);
        let shown = texts(&rows);
        assert!(
            shown.iter().any(|l| l.contains("PostgreSQL?")),
            "{room} {shown:#?}"
        );
        assert!(
            rows.lines
                .iter()
                .any(|l| l.spans.iter().any(|s| s.look.reverse)),
            "{room} {shown:#?}"
        );
    }
    let empty = [Draft::default()];
    let picked = view(&long, &empty, CardPage::Question(0), 0);
    for room in QUESTION_MIN_ROWS..=11 {
        let shown = texts(&question_rows(&picked, room, narrow));
        let label = shown.iter().any(|l| l.contains("\u{203A} 1 PostgreSQL"));
        assert!(label, "{room} {shown:#?}");
    }
}

#[test]
fn settling_and_sending_replace_the_keys() {
    let questions = [database()];
    let drafts = [Draft::default()];
    let layout = Layout::new(80, GlyphSet::Unicode);
    for (mode, words) in [
        (CardMode::Settling, "waiting for you to finish typing"),
        (CardMode::Sending, "sending your answers"),
    ] {
        let card = CardView {
            mode,
            ..view(&questions, &drafts, CardPage::Question(0), 0)
        };
        let shown = texts(&question_rows(&card, 20, layout));
        assert!(shown.iter().any(|l| l.contains(words)), "{shown:#?}");
    }
}

#[test]
fn model_text_never_reaches_a_span_with_a_control_character() {
    let questions = [Question {
        header: HOSTILE.to_owned(),
        question: format!("{HOSTILE}\tx"),
        options: vec![
            QuestionOption {
                label: HOSTILE.to_owned(),
                description: HOSTILE.to_owned(),
            },
            QuestionOption {
                label: "b".to_owned(),
                description: String::new(),
            },
        ],
        multi_select: false,
        source: None,
    }];
    let drafts = [Draft {
        own: HOSTILE.to_owned(),
        own_chosen: true,
        answered: true,
        ..Draft::default()
    }];
    for room in [3, 20] {
        for page in [CardPage::Question(0), CardPage::Review] {
            for row in 0..3 {
                let rows = question_rows(
                    &view(&questions, &drafts, page, row),
                    room,
                    Layout::new(60, GlyphSet::Unicode),
                );
                for line in &rows.lines {
                    for span in &line.spans {
                        assert!(
                            !span.text.chars().any(|c| c.is_control()),
                            "{:?}",
                            span.text
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn the_focused_option_and_its_description_take_the_selection_band() {
    let questions = [database()];
    let drafts = [Draft::default()];
    for w in [40, 80, 120, 200] {
        let layout = Layout::new(w, GlyphSet::Unicode);
        let rows = question_rows(
            &view(&questions, &drafts, CardPage::Question(0), 0),
            20,
            layout,
        );
        let shown = texts(&rows);
        let label = shown.iter().position(|l| l.contains("PostgreSQL")).unwrap();
        crate::chrome::band::check::assert_selected(&rows.lines[label], w, "\u{203A}");
        let sqlite = shown.iter().position(|l| l.contains("SQLite")).unwrap();
        for (i, line) in rows.lines.iter().enumerate() {
            if rows.hits[i] == Some(CardHit::Option(0)) {
                assert_eq!(
                    line.spans.first().and_then(|s| s.look.bg),
                    Some(Token::AccentTint.into()),
                    "{line:?}"
                );
            } else {
                crate::chrome::band::check::assert_unselected(line);
            }
        }
        assert!(sqlite > label);
        let own = view(&questions, &drafts, CardPage::Question(0), 2);
        let rows = question_rows(&own, 20, layout);
        let at = rows
            .hits
            .iter()
            .position(|h| *h == Some(CardHit::Own))
            .unwrap();
        crate::chrome::band::check::assert_selected(&rows.lines[at], w, "\u{203A}");
        let settling = CardView {
            mode: CardMode::Settling,
            ..view(&questions, &drafts, CardPage::Question(0), 0)
        };
        for line in &question_rows(&settling, 20, layout).lines {
            crate::chrome::band::check::assert_unselected(line);
        }
    }
}

#[test]
fn the_caret_on_a_banded_row_is_a_hole_in_the_band_without_colour() {
    use crate::theme::{Depth, Mode, Theme};
    use ratatui_core::style::Modifier;
    let questions = [database()];
    let drafts = [Draft {
        own: "first line\nsecond".to_owned(),
        ..Draft::default()
    }];
    let card = CardView {
        cursor: Some((1, 3)),
        ..view(&questions, &drafts, CardPage::Question(0), 2)
    };
    let rows = question_rows(&card, 20, Layout::new(80, GlyphSet::Unicode));
    let at = texts(&rows)
        .iter()
        .position(|l| l.trim() == "second")
        .unwrap();
    let line = &rows.lines[at];
    let caret = line.spans.iter().find(|s| s.look.reverse).unwrap();
    assert_eq!(caret.look.bg, Some(Token::AccentTint.into()));
    for mode in [Mode::Dark, Mode::Light] {
        for depth in [Depth::Ansi16, Depth::None] {
            let theme = Theme::new(mode, depth);
            let reversed = |look: &crate::styled::Look| {
                theme.style(look).add_modifier.contains(Modifier::REVERSED)
            };
            assert!(!reversed(&caret.look), "{mode:?} {depth:?}");
            let band: Vec<_> = line.spans.iter().filter(|s| !s.look.reverse).collect();
            assert!(band.iter().all(|s| reversed(&s.look)), "{mode:?} {depth:?}");
        }
        let theme = Theme::new(mode, Depth::TrueColor);
        assert!(
            theme
                .style(&caret.look)
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }
}

#[test]
fn the_header_and_the_compact_position_stop_at_the_content_edge() {
    let questions = [database()];
    let drafts = [Draft::default()];
    for (screen, end) in [(80, 79), (120, 119), (200, 119)] {
        let layout = Layout::new(screen, GlyphSet::Unicode);
        let full = question_rows(
            &view(&questions, &drafts, CardPage::Question(0), 1),
            20,
            layout,
        );
        let header = &texts(&full)[0];
        assert_eq!(header.chars().count(), end, "{screen}: {header:?}");
        let compact = question_rows(
            &view(&questions, &drafts, CardPage::Question(0), 1),
            QUESTION_MIN_ROWS - 1,
            layout,
        );
        let second = &texts(&compact)[1];
        assert!(second.ends_with("\u{2039} 2/3 \u{203A}"), "{second:?}");
        assert_eq!(second.chars().count(), end, "{screen}: {second:?}");
    }
}

#[test]
fn the_focused_option_wraps_every_word_inside_the_band_at_every_width() {
    use crate::chrome::band::band_cols;
    use crate::chrome::band::check::{
        PAST_THE_BAND, assert_inside_band, assert_selected, assert_tinted_to, assert_unselected,
    };
    let long = (1..=60).map(|n| format!("word{n}")).collect::<Vec<_>>();
    let label = "A label long enough to run past the band edge ".repeat(4);
    let questions = [question(
        "Long",
        &[(label.trim(), &long.join(" ")), ("Other", &long.join(" "))],
        false,
    )];
    let drafts = [Draft::default()];
    for w in PAST_THE_BAND {
        let rows = question_rows(
            &view(&questions, &drafts, CardPage::Question(0), 0),
            200,
            Layout::new(w, GlyphSet::Unicode),
        );
        let focused: Vec<usize> = (0..rows.len())
            .filter(|&i| rows.hits[i] == Some(CardHit::Option(0)))
            .collect();
        let shown = texts(&rows);
        let words: Vec<&str> = focused
            .iter()
            .flat_map(|&i| shown[i].split_whitespace())
            .collect();
        for word in &long {
            assert!(words.contains(&word.as_str()), "{w}: {word} {shown:#?}");
        }
        for (i, line) in rows.lines.iter().enumerate() {
            assert_inside_band(line, w);
            if focused.first() == Some(&i) {
                assert_selected(line, w, "\u{203A}");
            } else if focused.contains(&i) {
                assert_tinted_to(line, band_cols(w));
            } else {
                assert_unselected(line);
            }
        }
    }
}

#[test]
fn form_debug_never_exposes_private_prompts_or_field_text() {
    let options = [wisp_protocol::QuestionOption {
        label: "OPTION_CANARY".into(),
        description: "DESCRIPTION_CANARY".into(),
    }];
    let view = super::FormView {
        title: "TITLE_CANARY",
        title_line: None,
        note: "NOTE_CANARY",
        prompt: "https://example.invalid/?state=PRIVATE_URL_CANARY",
        prompt_lines: &[],
        error: "",
        options: &options,
        option_marks: &[],
        selected: Some(0),
        field: Some(super::FormField {
            label: "FIELD_CANARY",
            value: super::FormValue::Text("VALUE_CANARY"),
        }),
        footer: &[KeyHint::note("FOOTER_CANARY")],
        account: Some(super::FormAccount {
            kind: AccountKind::ClaudeSubscription,
            name: "NAME_CANARY",
            mark: Some(1),
        }),
        done: false,
        link: Some(super::LinkRow {
            text: Some("LINK_CANARY.invalid/path"),
            browser: "BROWSER_CANARY",
            copy: "COPY_CANARY",
            copy_look: Token::Text,
            slot: 10,
        }),
        refused: None,
    };
    let shown = format!("{view:?}");
    assert!(shown.starts_with("FormView"));
    assert!(!shown.contains("CANARY") && !shown.contains(".invalid"));
}

#[test]
fn the_link_row_shortens_by_segment_keeps_the_host_and_stays_inside_the_band() {
    use crate::chrome::band::check::assert_inside_band;
    let hostile = "claude.ai/oauth/au\u{202E}tho\u{200B}rize\u{1b}]52;c;ignored\u{7}";
    let browser = "Opened in your browser";
    let copy = "Link copied";
    let slot = width(browser, WidthMode::Normal) + 3 + width(copy, WidthMode::Normal);
    let code = wisp_protocol::Secret::new("CODE_CANARY_0123456789#STATE");
    for (given, clean) in [("claude.ai/oauth/authorize", true), (hostile, false)] {
        for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
            for width_cols in [40, 60, 70, 80, 120, 200] {
                let form = FormView {
                    title: "Sign in",
                    title_line: None,
                    note: "",
                    prompt: "",
                    prompt_lines: &[],
                    error: "",
                    options: &[],
                    option_marks: &[],
                    selected: Some(0),
                    field: Some(FormField {
                        label: "Code",
                        value: FormValue::Secret(&code),
                    }),
                    footer: &[
                        KeyHint::new("enter", "finish"),
                        KeyHint::new("ctrl-o", "open again"),
                        KeyHint::new("esc", "cancel").as_exit(),
                    ],
                    account: None,
                    done: false,
                    link: Some(LinkRow {
                        text: Some(given),
                        browser,
                        copy,
                        copy_look: Token::TextMuted,
                        slot,
                    }),
                    refused: None,
                };
                let layout = Layout::new(width_cols, glyphs);
                let rows = form_rows(&form, 24, layout);
                let debug = format!("{rows:?}");
                assert!(!debug.contains("claude.ai") && !debug.contains("CODE_CANARY"));
                for line in &rows.lines {
                    assert_inside_band(line, width_cols);
                }
                let shown = texts(&rows);
                let all = shown.join("\n");
                assert!(!all.contains("CODE_CANARY"), "{all}");
                assert!(
                    !all.contains(['\u{1b}', '\u{7}', '\u{202E}', '\u{200B}']),
                    "{all}"
                );
                if !clean {
                    continue;
                }
                let cells = rows.link.unwrap();
                assert_eq!(rows.hits[cells.line], Some(CardHit::Link));
                let row = &shown[cells.line];
                let more = Glyphs::of(glyphs).more;
                let text: String = row.chars().skip(cells.start).collect();
                let text: String = text.chars().take(cells.width).collect();
                assert!(
                    text.contains("claude.ai/") && text.ends_with(more),
                    "{width_cols}: {text}"
                );
                let beside = row.contains(browser);
                assert_eq!(beside, row.contains(copy), "{width_cols}: {row}");
                assert_eq!(beside, width_cols >= 60, "{width_cols}: {shown:#?}");
                if width_cols >= 120 {
                    assert_eq!(
                        text,
                        format!(
                            "{} claude.ai/oauth/authorize{more}",
                            Glyphs::of(glyphs).arrow
                        )
                    );
                }
                if !beside {
                    let below = shown[cells.line + 1..].join("\n");
                    assert!(
                        below.contains(browser) && below.contains(copy),
                        "{shown:#?}"
                    );
                }
                assert!(shown.last().unwrap().contains("esc cancel"), "{shown:#?}");
            }
        }
    }
}

#[test]
fn a_secret_field_is_masked_in_the_room_left_after_its_label() {
    let code = wisp_protocol::Secret::new(format!("ye5b{}he2E", "x".repeat(79)));
    let empty = wisp_protocol::Secret::new("");
    let field = |secret, width, glyphs| {
        let form = FormView {
            title: "Sign in",
            title_line: None,
            note: "",
            prompt: "",
            prompt_lines: &[],
            error: "",
            options: &[],
            option_marks: &[],
            selected: Some(0),
            field: Some(FormField {
                label: "Code",
                value: FormValue::Secret(secret),
            }),
            footer: &[KeyHint::new("esc", "cancel").as_exit()],
            account: None,
            done: false,
            link: None,
            refused: None,
        };
        let rows = form_rows(&form, 24, Layout::new(width, glyphs));
        let mask = rows
            .lines
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.look.fg == Look::fg(Token::Secret).fg);
        let line = texts(&rows)
            .into_iter()
            .find(|line| line.contains("Code  "))
            .map(|line| line.trim_end().to_owned())
            .unwrap();
        let shown = line.split_once("Code  ").map(|(_, shown)| shown);
        assert_eq!(
            mask.map(|s| s.text.as_str()),
            shown.filter(|_| !secret.expose().is_empty())
        );
        line
    };
    assert_eq!(
        field(&code, 40, GlyphSet::Unicode),
        " \u{203a} Code  ye5b\u{2026}******\u{2026}he2E"
    );
    assert_eq!(
        field(&code, 80, GlyphSet::Unicode),
        " \u{203a} Code  ye5b\u{2026}************\u{2026}he2E  87 characters"
    );
    assert_eq!(
        field(&code, 40, GlyphSet::Ascii),
        " > Code  ye5b...******...he2E"
    );
    assert_eq!(
        field(&empty, 80, GlyphSet::Unicode),
        format!(" \u{203a} Code  {SECRET_PLACEHOLDER}")
    );
}

fn secret_box(value: &wisp_protocol::Secret) -> SecretView<'_> {
    SecretView {
        asked_by: None,
        name: "OpenAI API key",
        reason: "To add it to Wisp as the account \"work\".",
        destination: SecretGoes::Process {
            id: 4,
            command: "wisp account add work --key-stdin\nnot drawn",
        },
        value,
        ends: wisp_protocol::Ends { start: 8, end: 4 },
        pasted: true,
        hidden: false,
        settling: false,
        sent: false,
        problem: "",
    }
}

#[test]
fn the_secret_box_at_40_80_and_120_in_unicode_and_ascii() {
    let key = wisp_protocol::Secret::new(format!("sk-proj-{}WXYZ", "q".repeat(152)));
    let empty = wisp_protocol::Secret::new("");
    for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
        let g = Glyphs::of(glyphs);
        for width in [40, 80, 120] {
            let layout = Layout::new(width, glyphs);
            let mut view = secret_box(&key);
            let rows = |view: &SecretView<'_>| texts(&secret_rows(view, 24, layout));
            let lines = rows(&view);
            let shown = if width == 40 {
                format!("sk-proj-{}******{}WXYZ", g.more, g.more)
            } else {
                format!(
                    "sk-proj-{}************{}WXYZ  164 characters",
                    g.more, g.more
                )
            };
            let field = format!(" {} Secret  {shown}", g.prompt);
            assert!(lines.contains(&field), "{width} {glyphs:?}: {lines:?}");
            assert_eq!(lines[0], " ? OpenAI API key");
            assert!(lines.iter().any(|l| l.starts_with("   Goes to process 4")));
            assert!(!lines.iter().any(|l| l.contains("not drawn")));
            let clear = if width == 40 {
                String::new()
            } else {
                format!(" {} ctrl-u clear", g.separator)
            };
            assert_eq!(
                lines.last().unwrap(),
                &format!(
                    "   enter send {} tab hide ends{clear} {} esc stop",
                    g.separator, g.separator
                )
            );
            view.hidden = true;
            assert!(rows(&view).contains(&format!(
                " {} Secret  {}  164 characters",
                g.prompt,
                "*".repeat(width - 28)
            )));
            assert!(rows(&view).last().unwrap().contains("tab show ends"));
            view.value = &empty;
            assert!(rows(&view).contains(&format!(" {} Secret  {SECRET_PLACEHOLDER}", g.prompt)));
            assert_eq!(rows(&view).last().unwrap(), "   esc stop");
            view.settling = true;
            assert!(rows(&view).contains(&format!("   Secret  {SECRET_PLACEHOLDER}")));
            assert_eq!(
                rows(&view).last().unwrap(),
                "   waiting for you to finish typing"
            );
            view.settling = false;
            view.sent = true;
            assert!(rows(&view).contains(&"   Secret".to_owned()));
            assert_eq!(
                rows(&view).last().unwrap(),
                &format!("   sending{}", g.more)
            );
            view.sent = false;
            view.problem = "That secret has more than one line.";
            let lines = rows(&view);
            let field = lines
                .iter()
                .position(|l| l.contains(SECRET_PLACEHOLDER))
                .unwrap();
            assert_eq!(lines[field + 1], "   That secret has more than one line.");
            let password = wisp_protocol::Secret::new("CANARY-placeholders-WXYZ");
            view = secret_box(&password);
            view.ends = wisp_protocol::Ends::default();
            assert!(!view.two_forms(layout));
            let count = if width == 40 { "" } else { "  24 characters" };
            assert!(rows(&view).contains(&format!(
                " {} Secret  {}{count}",
                g.prompt,
                "*".repeat(24)
            )));
            let long_start = wisp_protocol::Secret::new("x".repeat(108));
            view = secret_box(&long_start);
            view.ends = wisp_protocol::Ends { start: 16, end: 4 };
            assert_eq!(
                view.two_forms(layout),
                width != 40 || glyphs != GlyphSet::Ascii
            );
        }
    }
}

#[test]
fn the_secret_box_never_draws_the_value() {
    for value in [
        "CANARY",
        "CANARY with spaces and some more WXYZ",
        "CANARY非ASCIIxxxxxxxxxxxxxxxxxxxxWXYZ",
    ] {
        let value = wisp_protocol::Secret::new(value);
        for hidden in [false, true] {
            for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
                for width in [40, 80, 120] {
                    let view = SecretView {
                        hidden,
                        ..secret_box(&value)
                    };
                    let rows = texts(&secret_rows(&view, 24, Layout::new(width, glyphs)));
                    let text = rows.join("\n");
                    assert!(!text.contains("CANARY"));
                    assert!(!text.contains("WXYZ"));
                    assert!(!view.two_forms(Layout::new(width, glyphs)));
                    assert!(!format!("{view:?}").contains("CANARY"));
                }
            }
        }
    }
}

#[test]
fn a_servers_secret_destination_wraps_whole_and_a_process_keeps_its_cut() {
    let value = wisp_protocol::Secret::new("");
    let command = "npx -y @sentry/mcp-server";
    for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
        let layout = Layout::new(40, glyphs);
        let view = SecretView {
            name: "SENTRY_TOKEN",
            reason: "Your agent asks for the key of the server sentry. Wisp keeps it for that server and never shows it to your agent.",
            destination: SecretGoes::Server {
                name: "sentry",
                command: Some(command),
            },
            ..secret_box(&value)
        };
        let rows = texts(&secret_rows(&view, 40, layout));
        let joined = rows
            .iter()
            .map(|line| line.trim())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(joined.contains("@sentry/mcp-server"));
        assert!(joined.contains("Goes to the server sentry"));
        assert!(!joined.contains("Goes to process"));
        let process = SecretView {
            destination: SecretGoes::Process { id: 4, command },
            ..secret_box(&value)
        };
        let text = texts(&secret_rows(&process, 40, layout)).join("\n");
        assert!(text.contains("Goes to process 4"));
        assert!(!text.contains("@sentry/mcp-server"));
    }
}

#[test]
fn the_server_asks_on_each_card_page_and_in_the_secret_box() {
    let asker = wisp_protocol::AskedBy::Server {
        name: "linear".into(),
    };
    let mut asked = question("Team", &[("Web", "")], false);
    asked.source = Some(wisp_protocol::QuestionSource {
        asked_by: asker.clone(),
        typed: false,
    });
    let questions = [asked.clone(), asked];
    let drafts = [Draft::default(), Draft::default()];
    let secret = wisp_protocol::Secret::new("");
    for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for width in [40, 80, 120] {
            let layout = Layout::new(width, glyphs);
            for page in 0..2 {
                let shown = texts(&question_rows(
                    &view(&questions, &drafts, CardPage::Question(page), 0),
                    30,
                    layout,
                ));
                assert!(
                    shown
                        .iter()
                        .any(|line| line.trim() == "Asked by linear, one of your servers"),
                    "{shown:?}"
                );
            }
            let compact = texts(&question_rows(
                &view(&questions, &drafts, CardPage::Question(0), 0),
                QUESTION_MIN_ROWS - 1,
                layout,
            ));
            assert!(
                compact
                    .iter()
                    .any(|line| line.contains("Asked by linear, one of your servers")),
                "{compact:?}"
            );
            let mut box_view = secret_box(&secret);
            box_view.asked_by = Some(&asker);
            box_view.destination = SecretGoes::Server {
                name: "linear",
                command: None,
            };
            let shown = texts(&secret_rows(&box_view, 30, layout));
            let ask = shown
                .iter()
                .position(|line| line.contains("Asked by linear"))
                .unwrap();
            let goes = shown
                .iter()
                .position(|line| line.contains("Goes to the server linear"))
                .unwrap();
            assert!(ask < goes);
            let long = wisp_protocol::AskedBy::Server {
                name: "a".repeat(80),
            };
            box_view.asked_by = Some(&long);
            assert!(
                texts(&secret_rows(&box_view, 30, layout))
                    .iter()
                    .filter(|line| line.contains('a'))
                    .count()
                    > 1
            );
        }
    }
    assert_eq!(copy::asked_by(&wisp_protocol::AskedBy::Unknown), None);
}

#[test]
fn a_typed_server_question_uses_its_typing_row_and_move_hint() {
    let mut asked = question("Needs you", &[("Not now", "")], false);
    asked.source = Some(wisp_protocol::QuestionSource {
        asked_by: wisp_protocol::AskedBy::Server {
            name: "linear".into(),
        },
        typed: true,
    });
    for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for width in [40, 80, 120] {
            let layout = Layout::new(width, glyphs);
            let drafts = [Draft::default()];
            let shown = texts(&question_rows(
                &view(
                    std::slice::from_ref(&asked),
                    &drafts,
                    CardPage::Question(0),
                    1,
                ),
                24,
                layout,
            ));
            assert!(shown.iter().any(|line| line.contains("Type your answer")));
            let keys = hints(
                &view(
                    std::slice::from_ref(&asked),
                    &drafts,
                    CardPage::Question(0),
                    1,
                ),
                layout,
            );
            let text = format!("{keys:?}");
            assert!(text.find("move").unwrap() < text.find("scroll").unwrap());
            let drafts = [Draft {
                own: "Flaky test in ci.yml".into(),
                own_chosen: true,
                ..Draft::default()
            }];
            let shown = texts(&question_rows(
                &view(
                    std::slice::from_ref(&asked),
                    &drafts,
                    CardPage::Question(0),
                    1,
                ),
                24,
                layout,
            ));
            assert!(
                shown
                    .iter()
                    .any(|line| line.contains("Flaky test in ci.yml"))
            );
            assert!(!shown.iter().any(|line| line.contains("Your own:")));
        }
    }
}

#[test]
fn a_typed_answer_is_shown_alone() {
    let mut asked = question("Needs you", &[("Not now", "")], false);
    asked.source = Some(wisp_protocol::QuestionSource {
        asked_by: wisp_protocol::AskedBy::Server {
            name: "linear".into(),
        },
        typed: true,
    });
    for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
        assert_eq!(
            answer_words(&asked, &Answer::own("Flaky test in ci.yml"), glyphs),
            "Flaky test in ci.yml"
        );
    }
}

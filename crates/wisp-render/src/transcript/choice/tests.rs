use std::time::Duration;

use wisp_protocol::{AgentEvent, CacheTtl, ErrorKind};

use super::super::{Config, Detail, Transcript, UserPart};
use super::{Choice, ChoiceRow};
use crate::events::{End, Ending, turn_ended};
use crate::markdown::{Layout, Renderer};
use crate::testing::{line_text, texts};
use crate::theme::glyphs::GlyphSet;

fn failed() -> Transcript {
    failed_with(ErrorKind::Server)
}

fn failed_with(kind: ErrorKind) -> Transcript {
    let mut t = Transcript::new(Config {
        provider: "Anthropic".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        logging: true,
    });
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    t.deliver(vec![UserPart::Text("go".to_owned())], None);
    t.apply(
        &turn_ended(End {
            ending: Ending::Error(kind),
            message: "Upstream provider stream was interrupted.".into(),
            committed_steps: 18,
            attempts: 6,
            gave_up: Some(wisp_protocol::GaveUp::StepLimit),
            ..End::default()
        }),
        Duration::ZERO,
    );
    t
}

fn lines(t: &mut Transcript, width: usize, set: GlyphSet) -> Vec<String> {
    texts(&t.rows(&Renderer::new(Layout::new(width, set)), Detail::Scrollback))
}

fn open(focus: usize, dimmed: bool) -> Choice {
    Choice::Open {
        rows: vec![
            ChoiceRow {
                label: "Retry".into(),
                text: "from step 19 · cache warm ~$0.06".into(),
            },
            ChoiceRow {
                label: "Switch account".into(),
                text: "pick one, then retry · cache rebuild".into(),
            },
        ],
        focus,
        dimmed,
    }
}

#[test]
fn the_choice_is_the_last_part_of_the_error_cell() {
    let mut t = failed();
    let turn = t.turn();
    assert!(t.set_choice(turn, Some(open(0, false))));
    assert!(!t.set_choice(turn, Some(open(0, false))));
    let shown = lines(&mut t, 80, GlyphSet::Unicode);
    let at = shown
        .iter()
        .position(|l| l.contains("Upstream provider stream"))
        .unwrap();
    assert_eq!(shown[at + 1], "");
    assert_eq!(
        shown[at + 2..at + 5],
        [
            " › Retry             from step 19 · cache warm ~$0.06",
            "   Switch account    pick one, then retry · cache rebuild",
            "   enter choose · ↑↓ move · esc dismiss",
        ]
    );
}

#[test]
fn a_dimmed_choice_has_no_hint_and_a_used_one_is_one_line() {
    let mut t = failed();
    let turn = t.turn();
    t.set_choice(turn, Some(open(1, true)));
    let shown = lines(&mut t, 80, GlyphSet::Ascii).join("\n");
    assert!(shown.contains("\n > Switch account"), "{shown}");
    assert!(!shown.contains("dismisses"), "{shown}");
    t.set_choice(turn, Some(Choice::Used("Retried on direct".into())));
    let shown = lines(&mut t, 80, GlyphSet::Unicode).join("\n");
    assert!(shown.contains("\n   Retried on direct\n"), "{shown}");
    assert!(!shown.contains("Switch account"), "{shown}");
    t.set_choice(turn, None);
    assert!(
        !lines(&mut t, 80, GlyphSet::Unicode)
            .join("\n")
            .contains("Retried")
    );
}

fn styled(t: &mut Transcript, width: usize) -> Vec<crate::styled::StyledLine> {
    t.rows(
        &Renderer::new(Layout::new(width, GlyphSet::Unicode)),
        Detail::Scrollback,
    )
}

#[test]
fn the_focused_row_takes_the_selection_band_and_a_dimmed_card_has_none() {
    for width in [40, 80, 120, 200] {
        let mut t = failed();
        let turn = t.turn();
        t.set_choice(turn, Some(open(1, false)));
        let rows = styled(&mut t, width);
        let at = rows
            .iter()
            .position(|l| line_text(l).contains("Switch account"))
            .unwrap();
        crate::chrome::band::check::assert_banded_to(&rows[at], width, "\u{203A}");
        let retry = rows
            .iter()
            .position(|l| l.spans.iter().any(|s| s.text.trim() == "Retry"))
            .unwrap();
        crate::chrome::band::check::assert_unselected(&rows[retry]);
        t.set_choice(turn, Some(open(1, true)));
        for row in styled(&mut t, width) {
            crate::chrome::band::check::assert_unselected(&row);
            if line_text(&row).contains("Switch account") {
                let text_ink = Some(crate::theme::Token::Text.into());
                assert!(row.spans.iter().all(|s| s.look.fg != text_ink), "{row:?}");
            }
        }
    }
}

#[test]
fn a_long_row_wraps_under_its_text_column() {
    let mut t = failed();
    let turn = t.turn();
    t.set_choice(
        turn,
        Some(Choice::Open {
            rows: vec![ChoiceRow {
                label: "Retry".into(),
                text: "on direct · converts the summary · turns 11–30 kept only as your messages and the ledger · ~$0.14".into(),
            }],
            focus: 0,
            dimmed: false,
        }),
    );
    let shown = lines(&mut t, 60, GlyphSet::Unicode);
    let at = shown.iter().position(|l| l.contains("› Retry")).unwrap();
    assert_eq!(text_starts(&shown[at + 1]), Some(3 + 9), "{shown:?}");
    assert_eq!(shown[at + 3], "   enter choose · esc dismiss");
}

#[test]
fn a_choice_set_for_another_turn_changes_nothing() {
    let mut t = failed();
    let turn = t.turn();
    assert!(!t.set_choice(turn + 1, Some(open(0, false))));
}

fn text_starts(line: &str) -> Option<usize> {
    line.chars()
        .position(|c| c.is_alphabetic() || c == '\u{b7}')
}

#[test]
fn a_wrapped_retry_note_continues_under_its_text() {
    let note = "Retrying step 19 on direct · claude-opus-5-5 at medium · cache rebuild ~$1.52 · earlier Sonnet 5.5 reasoning may not carry over";
    for (set, column) in [(GlyphSet::Unicode, 5), (GlyphSet::Ascii, 3)] {
        let mut t = failed();
        t.retrying(note);
        let rows = lines(&mut t, 60, set);
        let at = rows
            .iter()
            .position(|l| l.contains("Retrying step 19"))
            .unwrap();
        assert_eq!(text_starts(&rows[at]), Some(column), "{rows:#?}");
        assert_eq!(text_starts(&rows[at + 1]), Some(column), "{rows:#?}");
        assert_eq!(text_starts(&rows[at + 2]), Some(column), "{rows:#?}");
    }
}

#[test]
fn only_the_cross_and_the_first_sentence_of_the_headline_are_red() {
    for width in [40, 80, 120, 200] {
        let mut t = failed();
        let rows = t.rows(
            &Renderer::new(Layout::new(width, GlyphSet::Unicode)),
            Detail::Scrollback,
        );
        let at = rows
            .iter()
            .position(|l| l.spans.iter().any(|s| s.text.contains('\u{2A2F}')))
            .unwrap();
        let headline: Vec<&crate::styled::StyledSpan> = rows[at..]
            .iter()
            .take_while(|l| {
                !l.spans
                    .iter()
                    .any(|s| s.text.contains("Gave up") || s.text.contains("Upstream"))
            })
            .flat_map(|l| &l.spans)
            .collect();
        let error = Some(crate::theme::Token::Error.into());
        let red: String = headline
            .iter()
            .filter(|s| s.look.fg == error)
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let red = red.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(red, "\u{2A2F} Anthropic had a server error.", "{width}");
        let rest: String = headline
            .iter()
            .filter(|s| s.look.fg != error && !s.text.trim().is_empty())
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let rest = rest.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(rest.contains("18 steps were kept"), "{width}: {rest:?}");
        assert!(
            headline
                .iter()
                .filter(|s| s.look.fg != error && !s.text.trim().is_empty())
                .all(|s| s.look.fg == Some(crate::theme::Token::Text.into())),
            "{width}: {headline:?}"
        );
    }
}

const KINDS: [ErrorKind; 14] = [
    ErrorKind::Auth,
    ErrorKind::RateLimit,
    ErrorKind::Overloaded,
    ErrorKind::InvalidRequest,
    ErrorKind::Network,
    ErrorKind::Refusal,
    ErrorKind::InvalidResponse,
    ErrorKind::ThinkingBinding,
    ErrorKind::Server,
    ErrorKind::Timeout,
    ErrorKind::Billing,
    ErrorKind::Interrupted,
    ErrorKind::ContextOverflow,
    ErrorKind::Other,
];

#[test]
fn every_error_kind_paints_only_its_first_sentence_red() {
    use super::super::copy;
    for kind in KINDS {
        let (sentence, rest) = copy::error_headline("Anthropic", kind, None);
        assert!(sentence.ends_with('.'), "{kind:?}: {sentence}");
        assert!(
            !sentence.trim_end_matches('.').contains(". "),
            "{kind:?}: {sentence}"
        );
        let mut t = failed_with(kind);
        let rows = t.rows(
            &Renderer::new(Layout::new(200, GlyphSet::Unicode)),
            Detail::Scrollback,
        );
        let at = rows
            .iter()
            .position(|l| l.spans.iter().any(|s| s.text.contains('\u{2A2F}')))
            .unwrap();
        let spans: Vec<&crate::styled::StyledSpan> = rows[at..]
            .iter()
            .take_while(|l| !line_text(l).trim().is_empty())
            .flat_map(|l| &l.spans)
            .collect();
        let error = Some(crate::theme::Token::Error.into());
        let red: String = spans
            .iter()
            .filter(|s| s.look.fg == error)
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let red = red.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(red, format!("\u{2A2F} {sentence}"), "{kind:?}");
        let plain: String = spans
            .iter()
            .filter(|s| s.look.fg == Some(crate::theme::Token::Text.into()))
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let plain = plain.split_whitespace().collect::<Vec<_>>().join(" ");
        if let Some(rest) = rest {
            assert!(plain.trim_start().starts_with(rest), "{kind:?}: {plain:?}");
        }
        assert!(plain.contains("18 steps were kept"), "{kind:?}: {plain:?}");
    }
    assert_eq!(
        copy::error_headline("Anthropic", ErrorKind::ContextOverflow, None).1,
        Some(copy::COMPACT_TO_FIT)
    );
    assert_eq!(
        copy::error_sentence("Anthropic", ErrorKind::ContextOverflow),
        format!(
            "Anthropic said the conversation is too long for the model. {}",
            copy::COMPACT_TO_FIT
        )
    );
}

#[test]
fn a_refused_request_names_the_model_and_a_404_says_it_was_not_found() {
    use super::super::copy;
    use wisp_protocol::{ErrorDetail, FailurePhase, FailureSource, RetryClass};
    let headline = |status, error_type: Option<&str>, model: &str| {
        let detail = ErrorDetail {
            status: Some(status),
            error_type: error_type.map(str::to_owned),
            model: Some(model.to_owned()),
            ..ErrorDetail::new(
                FailurePhase::Response,
                FailureSource::Endpoint,
                RetryClass::Never,
            )
        };
        copy::error_headline("Anthropic", ErrorKind::InvalidRequest, Some(&detail))
    };
    let rejected = |model: &str| (format!("Anthropic rejected the request for {model}."), None);
    assert_eq!(
        headline(404, Some("not_found_error"), "nova-7"),
        (
            "Anthropic could not find the model nova-7.".to_owned(),
            Some(copy::MODEL_LISTS)
        )
    );
    let invalid = Some("invalid_request_error");
    assert_eq!(
        headline(400, invalid, "claude-nova-7"),
        rejected("claude-nova-7")
    );
    assert_eq!(headline(404, None, "nova-7"), rejected("nova-7"));
    let key = "sk-ant-CANARY0123456789abcdef";
    assert_eq!(
        headline(400, invalid, key),
        rejected(wisp_protocol::NOT_SHOWN)
    );
}

#[test]
fn the_details_are_one_muted_block_and_the_key_row_has_muted_keys_and_faint_words() {
    use crate::theme::{Ink, Token};
    let mut t = failed();
    let turn = t.turn();
    t.set_choice(turn, Some(open(0, false)));
    let rows = styled(&mut t, 80);
    let head = rows
        .iter()
        .position(|l| line_text(l).contains("Upstream"))
        .unwrap();
    let blank = rows
        .iter()
        .skip(head)
        .position(|l| line_text(l).trim().is_empty())
        .map(|i| head + i)
        .unwrap();
    assert!(blank > head);
    for row in &rows[head..blank] {
        let inks: Vec<_> = row
            .spans
            .iter()
            .filter(|s| !s.text.trim().is_empty())
            .map(|s| s.look.fg)
            .collect();
        assert!(!inks.is_empty(), "{row:?}");
        assert!(
            inks.iter()
                .all(|i| *i == Some(Ink::Token(Token::TextMuted))),
            "{row:?}"
        );
        assert!(line_text(row).starts_with("   "), "{row:?}");
    }
    assert!(line_text(&rows[blank + 1]).contains("Retry"));
    let keys = rows
        .iter()
        .find(|l| line_text(l).contains("dismiss"))
        .unwrap();
    let ink = |want: &str| {
        keys.spans
            .iter()
            .find(|s| s.text.split(['\u{b7}', ' ']).any(|w| w == want))
            .and_then(|s| s.look.fg)
    };
    let muted = Some(Ink::Token(Token::TextMuted));
    let faint = Some(Ink::Token(Token::TextFaint));
    assert_eq!(ink("\u{2191}\u{2193}"), muted, "{keys:?}");
    assert_eq!(ink("enter"), muted, "{keys:?}");
    assert_eq!(ink("esc"), muted, "{keys:?}");
    assert_eq!(ink("choose"), faint, "{keys:?}");
    assert_eq!(ink("dismiss"), faint, "{keys:?}");
    assert!(
        keys.spans
            .iter()
            .filter(|s| s.text.contains('\u{b7}'))
            .all(|s| s.look.fg == faint),
        "{keys:?}"
    );
}

#[test]
fn an_unfocused_option_reads_its_name_in_text_and_its_detail_in_muted() {
    use crate::theme::{Ink, Token};
    let mut t = failed();
    let turn = t.turn();
    t.set_choice(turn, Some(open(0, false)));
    let rows = styled(&mut t, 80);
    let row = rows
        .iter()
        .find(|l| l.spans.iter().any(|s| s.text == "Switch account"))
        .unwrap();
    let ink = |want: &str| {
        row.spans
            .iter()
            .find(|s| s.text.contains(want))
            .and_then(|s| s.look.fg)
    };
    assert_eq!(ink("Switch account"), Some(Ink::Token(Token::Text)));
    assert_eq!(ink("pick one"), Some(Ink::Token(Token::TextMuted)));
}

#[test]
fn below_60_columns_each_detail_wraps_under_its_name() {
    for width in [40, 59] {
        let mut t = failed();
        let turn = t.turn();
        t.set_choice(turn, Some(open(1, false)));
        let shown = lines(&mut t, width, GlyphSet::Unicode);
        let retry = shown
            .iter()
            .position(|l| l.trim_end() == "   Retry")
            .unwrap_or_else(|| panic!("{shown:#?}"));
        assert_eq!(text_starts(&shown[retry + 1]), Some(3), "{shown:#?}");
        assert!(shown[retry + 1].contains("from step 19"), "{shown:#?}");
        let switch = shown
            .iter()
            .position(|l| l.trim_end() == " \u{203A} Switch account")
            .unwrap_or_else(|| panic!("{shown:#?}"));
        assert!(shown[switch + 1].starts_with("   pick one"), "{shown:#?}");
        let rows = styled(&mut t, width);
        let at = |needle: &str| {
            rows.iter()
                .position(|l| l.spans.iter().any(|s| s.text.contains(needle)))
                .unwrap()
        };
        crate::chrome::band::check::assert_banded_to(
            &rows[at("Switch account")],
            width,
            "\u{203A}",
        );
        let tint = Some(crate::theme::Ink::Token(crate::theme::Token::AccentTint));
        assert!(rows[at("pick one")].spans.iter().all(|s| s.look.bg == tint));
    }
    let mut t = failed();
    let turn = t.turn();
    t.set_choice(turn, Some(open(1, false)));
    let shown = lines(&mut t, 60, GlyphSet::Unicode);
    assert!(
        shown.iter().any(|l| l.starts_with("   Retry    ")),
        "{shown:#?}"
    );
}

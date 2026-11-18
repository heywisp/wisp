use super::*;
use crate::theme::glyphs::GlyphSet;

const NATIVE: SearchForm = SearchForm::Native;
const UNSUPPORTED: SearchForm = SearchForm::None(NoSearch::NotSupported);
const OFF: SearchForm = SearchForm::None(NoSearch::TurnedOff);
const NOT_STARTED: SearchForm = SearchForm::None(NoSearch::NotStarted);
const REFUSED: SearchForm = SearchForm::None(NoSearch::Refused);

fn uni() -> Glyphs {
    Glyphs::of(GlyphSet::Unicode)
}

fn ascii() -> Glyphs {
    Glyphs::of(GlyphSet::Ascii)
}

fn forms() -> SearchForms {
    SearchForms {
        requested: true,
        chat_only: false,
        pairs: vec![
            SearchPair {
                endpoint: "direct".to_owned(),
                model: "claude-opus-5-5".to_owned(),
                form: NATIVE,
            },
            SearchPair {
                endpoint: "gateway".to_owned(),
                model: "claude-opus-5-5".to_owned(),
                form: OFF,
            },
        ],
        refused: Vec::new(),
        kept: None,
    }
}

#[test]
fn each_form_reads_in_the_kernels_words() {
    assert_eq!(NATIVE.words(), "on");
    assert_eq!(UNSUPPORTED.words(), "off (not on this model)");
    assert_eq!(OFF.words(), "off (off for this account)");
    assert_eq!(NOT_STARTED.words(), "off (not in this chat)");
    assert_eq!(REFUSED.words(), "off (not on this account)");
    assert_eq!(SearchForm::None(NoSearch::NotRequested).words(), "off");
}

#[test]
fn a_listed_pair_keeps_its_form_and_any_other_pair_has_no_native_search() {
    let forms = forms();
    assert_eq!(forms.form("direct", "claude-opus-5-5"), Some(NATIVE));
    assert_eq!(forms.form("gateway", "claude-opus-5-5"), Some(OFF));
    assert_eq!(forms.form("direct", "my-model"), Some(UNSUPPORTED));
    assert_eq!(
        forms.form("elsewhere", "claude-opus-5-5"),
        Some(UNSUPPORTED)
    );
}

#[test]
fn a_refused_endpoint_has_no_search_for_any_model() {
    let forms = SearchForms {
        refused: vec!["direct".to_owned()],
        ..forms()
    };
    assert_eq!(forms.form("direct", "claude-opus-5-5"), Some(REFUSED));
    assert_eq!(forms.form("direct", "my-model"), Some(REFUSED));
    assert_eq!(forms.form("gateway", "claude-opus-5-5"), Some(OFF));
}

#[test]
fn search_left_out_of_the_tools_has_no_form_to_show() {
    let forms = SearchForms {
        requested: false,
        refused: vec!["direct".to_owned()],
        ..forms()
    };
    assert_eq!(forms.form("direct", "claude-opus-5-5"), None);
    assert_eq!(SearchForms::default().form("env", "claude-opus-5-5"), None);
}

#[test]
fn a_change_of_form_names_both_forms_the_full_reason_and_the_reasoning_it_drops() {
    assert_eq!(
        search_change(NATIVE, UNSUPPORTED, true, &uni()).as_deref(),
        Some("web search: on \u{2192} off (not on this model) \u{b7} earlier reasoning dropped")
    );
    assert_eq!(
        search_change(NATIVE, OFF, false, &uni()).as_deref(),
        Some("web search: on \u{2192} off (off for this account)")
    );
    assert_eq!(
        search_change(NATIVE, REFUSED, false, &ascii()).as_deref(),
        Some("web search: on -> off (not on this account)")
    );
    assert_eq!(
        search_change(OFF, NATIVE, false, &uni()).as_deref(),
        Some("web search: off \u{2192} on")
    );
    assert_eq!(search_change(NATIVE, NATIVE, true, &uni()), None);
    assert_eq!(search_change(OFF, REFUSED, true, &uni()), None);
}

#[test]
fn a_row_names_a_change_in_short_and_says_why_a_pair_stays_without_search() {
    assert_eq!(search_row(NATIVE, NATIVE, &uni()), None);
    assert_eq!(
        search_row(NATIVE, OFF, &uni()).as_deref(),
        Some("web search: on \u{2192} off")
    );
    assert_eq!(
        search_row(REFUSED, NATIVE, &ascii()).as_deref(),
        Some("web search: off -> on")
    );
    assert_eq!(
        search_row(OFF, OFF, &uni()).as_deref(),
        Some("web search: off (off for this account)")
    );
    assert_eq!(
        search_row(UNSUPPORTED, REFUSED, &uni()).as_deref(),
        Some("web search: off (not on this account)")
    );
    assert!(changes_search(NATIVE, REFUSED));
    assert!(changes_search(OFF, NATIVE));
    assert!(!changes_search(OFF, REFUSED));
    assert!(!changes_search(NATIVE, NATIVE));
}

#[test]
fn the_banner_says_why_search_is_off_and_when_that_leaves_chat_only() {
    assert_eq!(search_banner(NATIVE, true, &uni()), None);
    assert_eq!(
        search_banner(OFF, false, &uni()).as_deref(),
        Some("web search: off (off for this account)")
    );
    assert_eq!(
        search_banner(UNSUPPORTED, true, &ascii()).as_deref(),
        Some("web search: off (not on this model) - chat only")
    );
}

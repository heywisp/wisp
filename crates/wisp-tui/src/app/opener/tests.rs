use wisp_render::styled::{StyledLine, StyledSpan};

use super::*;
use crate::term::{Key, KeyCode};

fn entry(target: OpenTarget, filter: &str) -> Openable {
    Openable {
        target,
        line: StyledLine::new(vec![StyledSpan::plain(filter)]),
        filter: filter.to_owned(),
        live: false,
    }
}

fn list(cells: &[(usize, &str)]) -> Vec<Openable> {
    let mut out = vec![entry(
        OpenTarget::Transcript,
        "Full transcript every cell in full",
    )];
    out.extend(
        cells
            .iter()
            .map(|(cell, text)| entry(OpenTarget::Cell(*cell), text)),
    );
    out
}

fn five() -> Vec<Openable> {
    list(&[
        (9, "✓ Ran cargo test -p wisp-core"),
        (
            7,
            "╎ Thought for 4 s Placeholder thought line five, medium.",
        ),
        (5, "⨯ Ran exit 101 cargo build --release"),
        (3, "✓ Read 214 lines crates/wisp-core/src/cache.rs"),
    ])
}

fn press(opener: &mut Opener, code: KeyCode) -> Act {
    opener.key(Key::plain(code))
}

fn typed(opener: &mut Opener, text: &str) {
    for c in text.chars() {
        opener.key(Key::plain(KeyCode::Char(c)));
    }
}

#[test]
fn it_starts_on_the_given_cell_or_on_the_full_transcript() {
    let opener = Opener::new(five(), OpenTarget::Cell(7), Anchor::Follow);
    assert_eq!(opener.selected(), Some(OpenTarget::Cell(7)));
    let opener = Opener::new(five(), OpenTarget::Cell(8), Anchor::Follow);
    assert_eq!(opener.selected(), Some(OpenTarget::Transcript));
}

#[test]
fn the_arrows_stop_at_the_ends_and_pages_home_and_end_move_as_far_as_they_say() {
    let mut opener = Opener::new(five(), OpenTarget::Transcript, Anchor::Follow);
    assert_eq!(press(&mut opener, KeyCode::Up), Act::Stay);
    assert_eq!(opener.selected(), Some(OpenTarget::Transcript));
    press(&mut opener, KeyCode::Down);
    assert_eq!(opener.selected(), Some(OpenTarget::Cell(9)));
    for _ in 0..9 {
        press(&mut opener, KeyCode::Down);
    }
    assert_eq!(opener.selected(), Some(OpenTarget::Cell(3)));
    opener.set_page(2);
    press(&mut opener, KeyCode::PageUp);
    assert_eq!(opener.selected(), Some(OpenTarget::Cell(7)));
    press(&mut opener, KeyCode::PageDown);
    assert_eq!(opener.selected(), Some(OpenTarget::Cell(3)));
    press(&mut opener, KeyCode::Home);
    assert_eq!(opener.selected(), Some(OpenTarget::Transcript));
    press(&mut opener, KeyCode::End);
    assert_eq!(opener.selected(), Some(OpenTarget::Cell(3)));
    press(&mut opener, KeyCode::PageUp);
    press(&mut opener, KeyCode::PageUp);
    press(&mut opener, KeyCode::PageUp);
    assert_eq!(opener.selected(), Some(OpenTarget::Transcript));
}

#[test]
fn typing_filters_by_every_word_in_any_case_and_backspace_widens() {
    let mut opener = Opener::new(five(), OpenTarget::Cell(7), Anchor::Follow);
    typed(&mut opener, "RAN cargo");
    let shown: Vec<OpenTarget> = opener.shown().iter().map(|e| e.target).collect();
    assert_eq!(shown, [OpenTarget::Cell(9), OpenTarget::Cell(5)]);
    assert_eq!(opener.selected(), Some(OpenTarget::Cell(9)));
    typed(&mut opener, " release");
    assert_eq!(opener.selected(), Some(OpenTarget::Cell(5)));
    let menu = opener.menu();
    assert_eq!(menu.choices.len(), 1);
    assert!(
        menu.notes
            .iter()
            .any(|(n, _)| n.contains("RAN cargo release"))
    );
    typed(&mut opener, "zzz");
    assert!(opener.shown().is_empty());
    assert_eq!(opener.selected(), None);
    assert_eq!(press(&mut opener, KeyCode::Enter), Act::Stay);
    assert!(
        opener
            .menu()
            .notes
            .iter()
            .any(|(n, _)| n == copy::NOTHING_MATCHES)
    );
    for _ in 0.."zzz release".len() {
        press(&mut opener, KeyCode::Backspace);
    }
    assert_eq!(opener.filter(), "RAN cargo");
    assert_eq!(opener.shown().len(), 2);
}

#[test]
fn a_filter_that_hides_the_selection_selects_the_first_match() {
    let mut opener = Opener::new(five(), OpenTarget::Cell(3), Anchor::Follow);
    opener.paste("thought");
    assert_eq!(opener.selected(), Some(OpenTarget::Cell(7)));
    assert_eq!(
        press(&mut opener, KeyCode::Enter),
        Act::Open(OpenTarget::Cell(7))
    );
}

#[test]
fn enter_opens_ctrl_o_opens_transcript_mode_and_esc_or_ctrl_c_close() {
    let mut opener = Opener::new(five(), OpenTarget::Cell(5), Anchor::Follow);
    assert_eq!(
        press(&mut opener, KeyCode::Enter),
        Act::Open(OpenTarget::Cell(5))
    );
    assert_eq!(
        opener.key(Key::ctrl('o')),
        Act::Transcript(OpenTarget::Cell(5))
    );
    press(&mut opener, KeyCode::Home);
    assert_eq!(
        press(&mut opener, KeyCode::Enter),
        Act::Open(OpenTarget::Transcript)
    );
    assert_eq!(
        opener.key(Key::ctrl('o')),
        Act::Transcript(OpenTarget::Transcript)
    );
    typed(&mut opener, "zzz");
    assert_eq!(
        opener.key(Key::ctrl('o')),
        Act::Transcript(OpenTarget::Transcript)
    );
    assert_eq!(press(&mut opener, KeyCode::Esc), Act::Close);
    assert_eq!(opener.key(Key::ctrl('c')), Act::Close);
    assert_eq!(opener.key(Key::ctrl('d')), Act::Stay);
    assert_eq!(opener.filter(), "zzz");
}

#[test]
fn a_new_cell_joins_at_the_top_and_a_removed_selection_moves_to_the_next_older() {
    let mut opener = Opener::new(five(), OpenTarget::Cell(7), Anchor::Follow);
    let mut grown = list(&[(11, "╎ Thinking more")]);
    grown.extend(five().into_iter().skip(1));
    opener.refresh(grown);
    assert_eq!(opener.entries()[1].target, OpenTarget::Cell(11));
    assert_eq!(opener.selected(), Some(OpenTarget::Cell(7)));
    opener.refresh(list(&[(9, "✓ Ran"), (3, "✓ Read")]));
    assert_eq!(opener.selected(), Some(OpenTarget::Cell(3)));
    opener.refresh(list(&[(9, "✓ Ran")]));
    assert_eq!(opener.selected(), Some(OpenTarget::Cell(9)));
    opener.refresh(list(&[]));
    assert_eq!(opener.selected(), Some(OpenTarget::Transcript));
}

#[test]
fn the_chat_goes_back_unless_a_wheel_moved_it() {
    let at = Anchor::At { item: 4, offset: 2 };
    let mut opener = Opener::new(five(), OpenTarget::Cell(7), at);
    assert_eq!(opener.back(), Some(at));
    opener.wheeled();
    assert_eq!(opener.back(), None);
}

#[test]
fn a_click_opens_the_shown_entry_and_an_entry_draws_its_own_line() {
    let mut opener = Opener::new(five(), OpenTarget::Cell(7), Anchor::Follow);
    assert_eq!(opener.click(2), Act::Open(OpenTarget::Cell(7)));
    assert_eq!(opener.click(9), Act::Stay);
    let menu = opener.menu();
    assert_eq!(menu.selected, Some(2));
    assert_eq!(menu.choices[2].line, Some(five()[2].line.clone()));
    assert!(menu.titles.iter().all(String::is_empty));
    let dock = Opener::dock();
    assert!(dock.column.is_empty());
    assert_eq!(dock.keys[2].key(), copy::KEY_CTRL_O);
}

#[test]
fn a_paste_drops_control_characters_turns_whitespace_into_spaces_and_is_capped() {
    let mut opener = Opener::new(five(), OpenTarget::Transcript, Anchor::Follow);
    opener.paste("ca\u{7}rgo\ntest\t-p");
    assert_eq!(opener.filter(), "cargo test -p");
    opener.paste(&"x".repeat(MAX_FILTER * 2));
    assert_eq!(opener.filter().chars().count(), MAX_FILTER);
    press(&mut opener, KeyCode::Char('y'));
    assert_eq!(opener.filter().chars().count(), MAX_FILTER);
    assert!(!opener.filter().contains('y'));
}

#[test]
fn the_start_entry_and_opening_at_the_top_are_set_apart() {
    let opener = Opener::new(five(), OpenTarget::Cell(5), Anchor::Follow);
    assert!(!opener.top());
    let opener = opener.opening_at_top(true);
    assert!(opener.top());
    assert_eq!(opener.selected(), Some(OpenTarget::Cell(5)));
    let opener = opener.starting_on_full_transcript();
    assert!(opener.top());
    assert_eq!(opener.selected(), Some(OpenTarget::Transcript));
}

#[test]
fn building_the_entries_is_contained_when_it_panics() {
    let inside = std::cell::Cell::new(false);
    let built = entries_of(|| -> Vec<Openable> {
        inside.set(crate::term::contained());
        panic!("the picker blew up")
    });
    assert_eq!(built, None);
    assert!(inside.get());
    assert!(!crate::term::contained());
    assert_eq!(entries_of(five), Some(five()));
}

use super::*;

fn key(code: KeyCode) -> Key {
    Key::plain(code)
}

#[test]
fn a_held_row_is_moved_to_by_a_digit_or_click_and_taken_by_enter_or_space() {
    let mut pick = Pick {
        rows: 2,
        held: vec![true, false],
        focus: None,
    };
    assert_eq!(pick.key(key(KeyCode::Char('y'))), Picked::Not);
    assert_eq!(pick.key(key(KeyCode::Enter)), Picked::Not);
    assert_eq!(pick.key(key(KeyCode::Char(' '))), Picked::Not);
    assert_eq!(pick.key(key(KeyCode::Char('1'))), Picked::Moved);
    assert_eq!(pick.focus, Some(0));
    assert_eq!(pick.click(0), Picked::Moved);
    assert_eq!(pick.key(key(KeyCode::Enter)), Picked::Took(0));
    assert_eq!(pick.key(key(KeyCode::Char(' '))), Picked::Took(0));
    assert_eq!(pick.key(key(KeyCode::Char('2'))), Picked::Took(1));
    assert_eq!(pick.click(1), Picked::Took(1));
    assert_eq!(pick.key(key(KeyCode::Char('3'))), Picked::Not);
    assert_eq!(pick.key(Key::ctrl('2')), Picked::Not);
}

#[test]
fn the_arrows_and_tab_stop_at_the_ends_and_start_from_no_focus() {
    let mut pick = Pick {
        rows: 2,
        ..Pick::default()
    };
    assert_eq!(pick.key(key(KeyCode::Down)), Picked::Moved);
    assert_eq!(pick.focus, Some(0));
    pick.focus = None;
    pick.key(key(KeyCode::Up));
    assert_eq!(pick.focus, Some(1));
    pick.key(key(KeyCode::Tab));
    assert_eq!(pick.focus, Some(1));
    pick.key(key(KeyCode::BackTab));
    assert_eq!(pick.focus, Some(0));
    pick.key(key(KeyCode::Up));
    assert_eq!(pick.focus, Some(0));
    assert_eq!(pick.key(key(KeyCode::PageDown)), Picked::Not);
    let mut long = Pick {
        rows: 10,
        ..Pick::default()
    };
    assert_eq!(long.key(key(KeyCode::Char('1'))), Picked::Not);
}

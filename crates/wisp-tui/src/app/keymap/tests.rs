use super::*;
use crate::app::pager::{PagerAction, pager_action, reader_action};

fn idle(empty: bool) -> Context {
    Context {
        running: false,
        commanding: false,
        empty,
        blank: empty,
        armed: false,
        first_row: true,
        last_row: true,
        scrolled: false,
        selection: false,
        tab_queues: !empty,
        held: false,
        editing: false,
        rows: false,
        browsing: false,
        primed: false,
        movable: false,
        agents: false,
    }
}

fn answering(empty: bool) -> Context {
    Context {
        running: true,
        ..idle(empty)
    }
}

#[test]
fn enter_sends_when_idle_and_steers_while_answering() {
    let enter = Key::plain(KeyCode::Enter);
    assert_eq!(action(enter, idle(false)), Action::Send);
    assert_eq!(action(enter, idle(true)), Action::Nothing);
    assert_eq!(action(enter, answering(false)), Action::Steer);
    assert_eq!(action(enter, answering(true)), Action::Nothing);
}

#[test]
fn enter_on_an_empty_composer_releases_held_messages_only_when_idle() {
    let enter = Key::plain(KeyCode::Enter);
    let held = Context {
        held: true,
        ..idle(true)
    };
    assert_eq!(action(enter, held), Action::Release);
    let blank = Context {
        empty: false,
        ..held
    };
    assert_eq!(action(enter, blank), Action::Release);
    assert_eq!(
        action(
            enter,
            Context {
                held: true,
                ..idle(false)
            }
        ),
        Action::Send
    );
    let busy = Context {
        running: true,
        ..held
    };
    assert_eq!(action(enter, busy), Action::Nothing);
}

#[test]
fn tab_queues_only_while_busy_with_the_cursor_where_it_acts() {
    let tab = Key::plain(KeyCode::Tab);
    assert_eq!(action(tab, answering(false)), Action::Queue);
    assert_eq!(action(tab, idle(false)), Action::Edit(tab));
    let mid_text = Context {
        tab_queues: false,
        ..answering(false)
    };
    assert_eq!(action(tab, mid_text), Action::Edit(tab));
    assert_eq!(action(tab, answering(true)), Action::Edit(tab));
    let shifted = Key { shift: true, ..tab };
    assert_eq!(action(shifted, answering(false)), Action::Edit(shifted));
    assert_eq!(
        action(Key::plain(KeyCode::BackTab), answering(false)),
        Action::CycleEffort
    );
}

#[test]
fn up_on_the_first_line_enters_the_rows_while_any_show_and_history_otherwise() {
    let up = Key::plain(KeyCode::Up);
    let down = Key::plain(KeyCode::Down);
    let busy = Context {
        rows: true,
        ..answering(false)
    };
    assert_eq!(action(up, busy), Action::EnterRows);
    assert_eq!(action(down, busy), Action::HistoryNext);
    let idle_held = Context {
        rows: true,
        held: true,
        ..idle(true)
    };
    assert_eq!(action(up, idle_held), Action::EnterRows);
    assert_eq!(action(up, answering(true)), Action::HistoryPrevious);
    let browsing = Context {
        browsing: true,
        ..busy
    };
    assert_eq!(action(up, browsing), Action::HistoryPrevious);
    let lower = Context {
        first_row: false,
        ..busy
    };
    assert_eq!(action(up, lower), Action::Edit(up));
    assert_eq!(
        action(Key::alt(KeyCode::Up), busy),
        Action::Edit(Key::alt(KeyCode::Up))
    );
}

#[test]
fn while_editing_enter_saves_esc_cancels_and_tab_and_arrows_stay_in_the_message() {
    let editing = Context {
        editing: true,
        rows: true,
        held: true,
        ..answering(false)
    };
    let enter = Key::plain(KeyCode::Enter);
    assert_eq!(action(enter, editing), Action::SaveEdit);
    let idle_editing = Context {
        running: false,
        ..editing
    };
    assert_eq!(action(enter, idle_editing), Action::SaveEdit);
    let blank = Context {
        blank: true,
        ..idle_editing
    };
    assert_eq!(action(enter, blank), Action::Nothing, "never a release");
    assert_eq!(
        action(Key::plain(KeyCode::Esc), editing),
        Action::CancelEdit
    );
    for code in [KeyCode::Tab, KeyCode::Up, KeyCode::Down] {
        let key = Key::plain(code);
        assert_eq!(action(key, editing), Action::Edit(key), "{code:?}");
    }
    let newline = Key {
        shift: true,
        ..enter
    };
    assert_eq!(action(newline, editing), Action::Newline);
    assert_eq!(action(Key::ctrl('c'), editing), Action::Stop);
}

#[test]
fn keys_in_the_rows() {
    for (key, want) in [
        (Key::plain(KeyCode::Up), RowAction::Up),
        (Key::plain(KeyCode::Down), RowAction::Down),
        (Key::plain(KeyCode::Enter), RowAction::Edit),
        (Key::plain(KeyCode::Char('e')), RowAction::Edit),
        (Key::plain(KeyCode::Delete), RowAction::Remove),
        (Key::plain(KeyCode::Backspace), RowAction::Remove),
        (Key::plain(KeyCode::Char('x')), RowAction::Remove),
        (Key::plain(KeyCode::Tab), RowAction::Switch),
        (Key::plain(KeyCode::Esc), RowAction::Leave),
        (Key::ctrl('c'), RowAction::Stay),
        (Key::plain(KeyCode::Char('q')), RowAction::Pass),
        (Key::ctrl('o'), RowAction::Pass),
        (Key::plain(KeyCode::BackTab), RowAction::Pass),
        (Key::alt(KeyCode::Enter), RowAction::Pass),
    ] {
        assert_eq!(row_action(key), want, "{key:?}");
    }
}

#[test]
fn enter_on_whitespace_only_input_sends_nothing() {
    let cx = Context {
        blank: true,
        ..idle(false)
    };
    assert_eq!(action(Key::plain(KeyCode::Enter), cx), Action::Nothing);
}

#[test]
fn shift_alt_enter_and_ctrl_j_add_a_newline_always() {
    for cx in [idle(false), answering(true)] {
        assert_eq!(action(Key::shift(KeyCode::Enter), cx), Action::Newline);
        assert_eq!(action(Key::alt(KeyCode::Enter), cx), Action::Newline);
        assert_eq!(action(Key::ctrl('j'), cx), Action::Newline);
    }
}

#[test]
fn esc_and_ctrl_c_stop_a_reply() {
    assert_eq!(
        action(Key::plain(KeyCode::Esc), answering(false)),
        Action::Stop
    );
    assert_eq!(action(Key::ctrl('c'), answering(true)), Action::Stop);
    assert_eq!(
        action(Key::plain(KeyCode::Esc), idle(false)),
        Action::Nothing
    );
}

#[test]
fn ctrl_c_clears_then_must_be_pressed_twice_to_quit() {
    assert_eq!(action(Key::ctrl('c'), idle(false)), Action::Clear);
    assert_eq!(action(Key::ctrl('c'), idle(true)), Action::ArmQuit);
    let armed = Context {
        armed: true,
        ..idle(true)
    };
    assert_eq!(action(Key::ctrl('c'), armed), Action::Quit);
}

#[test]
fn ctrl_d_quits_only_on_empty_idle_input() {
    assert_eq!(action(Key::ctrl('d'), idle(true)), Action::Quit);
    assert_eq!(action(Key::ctrl('d'), answering(true)), Action::Nothing);
    assert_eq!(
        action(Key::ctrl('d'), idle(false)),
        Action::Edit(Key::ctrl('d'))
    );
}

#[test]
fn with_agents_busy_ctrl_d_arms_first_and_ctrl_t_opens_the_list() {
    let busy = Context {
        agents: true,
        ..idle(true)
    };
    assert_eq!(action(Key::ctrl('d'), busy), Action::ArmQuit);
    assert_eq!(action(Key::ctrl('c'), busy), Action::ArmQuit);
    let armed = Context {
        armed: true,
        ..busy
    };
    assert_eq!(action(Key::ctrl('d'), armed), Action::Quit);
    assert_eq!(action(Key::ctrl('c'), armed), Action::Quit);
    assert_eq!(action(Key::ctrl('t'), idle(false)), Action::Agents);
    assert_eq!(action(Key::ctrl('t'), answering(true)), Action::Agents);
}

#[test]
fn arrows_walk_history_only_at_the_edges_of_the_input() {
    assert_eq!(
        action(Key::plain(KeyCode::Up), idle(true)),
        Action::HistoryPrevious
    );
    assert_eq!(
        action(Key::plain(KeyCode::Down), idle(true)),
        Action::HistoryNext
    );
    let middle = Context {
        first_row: false,
        last_row: false,
        ..idle(false)
    };
    assert_eq!(
        action(Key::plain(KeyCode::Up), middle),
        Action::Edit(Key::plain(KeyCode::Up))
    );
}

#[test]
fn control_keys_map_to_their_commands() {
    assert_eq!(action(Key::ctrl('o'), idle(true)), Action::Pager);
    assert_eq!(action(Key::ctrl('l'), answering(true)), Action::Repaint);
    assert_eq!(action(Key::ctrl('z'), idle(true)), Action::Suspend);
}

#[test]
fn scrolling_keys_move_the_view_and_home_and_end_only_when_the_input_is_empty() {
    let page_up = Key::plain(KeyCode::PageUp);
    let page_down = Key::plain(KeyCode::PageDown);
    for cx in [idle(true), idle(false), answering(false)] {
        assert_eq!(action(page_up, cx), Action::Scroll(ViewScroll::HalfUp));
        assert_eq!(action(page_down, cx), Action::Scroll(ViewScroll::HalfDown));
        let top = Key {
            ctrl: true,
            ..Key::plain(KeyCode::Home)
        };
        let bottom = Key {
            ctrl: true,
            ..Key::plain(KeyCode::End)
        };
        assert_eq!(action(top, cx), Action::Scroll(ViewScroll::Top));
        assert_eq!(action(bottom, cx), Action::Scroll(ViewScroll::Bottom));
    }
    let home = Key::plain(KeyCode::Home);
    let end = Key::plain(KeyCode::End);
    assert_eq!(action(home, idle(true)), Action::Scroll(ViewScroll::Top));
    assert_eq!(action(end, idle(true)), Action::Scroll(ViewScroll::Bottom));
    assert_eq!(action(home, idle(false)), Action::Edit(home));
    assert_eq!(action(end, idle(false)), Action::Edit(end));
    let up = Key::plain(KeyCode::Up);
    assert_eq!(action(up, idle(true)), Action::HistoryPrevious);
}

#[test]
fn esc_stops_a_reply_then_returns_to_the_bottom_when_scrolled() {
    let esc = Key::plain(KeyCode::Esc);
    let scrolled = Context {
        scrolled: true,
        ..idle(true)
    };
    assert_eq!(action(esc, scrolled), Action::Scroll(ViewScroll::Bottom));
    let both = Context {
        running: true,
        ..scrolled
    };
    assert_eq!(action(esc, both), Action::Stop);
    assert_eq!(action(esc, idle(true)), Action::PrimeRewind);
}

#[test]
fn ctrl_c_copies_a_selection_whether_idle_or_answering() {
    let selected = Context {
        selection: true,
        ..idle(false)
    };
    let shifted = Key {
        shift: true,
        ..Key::ctrl('C')
    };
    for cx in [
        selected,
        Context {
            running: true,
            ..selected
        },
    ] {
        assert_eq!(action(Key::ctrl('c'), cx), Action::Copy);
        assert_eq!(action(shifted, cx), Action::Copy);
    }
    assert_eq!(action(Key::ctrl('c'), answering(true)), Action::Stop);
    assert_eq!(action(Key::ctrl('c'), idle(false)), Action::Clear);
}

#[test]
fn esc_clears_a_selection_while_idle_and_still_stops_a_reply() {
    let esc = Key::plain(KeyCode::Esc);
    let selected = Context {
        selection: true,
        scrolled: true,
        ..idle(true)
    };
    assert_eq!(action(esc, selected), Action::Deselect);
    let running = Context {
        running: true,
        ..selected
    };
    assert_eq!(action(esc, running), Action::Stop);
}

#[test]
fn alt_v_and_ctrl_v_attach_an_image_even_while_answering() {
    for cx in [idle(true), idle(false), answering(true)] {
        assert_eq!(
            action(Key::alt(KeyCode::Char('v')), cx),
            Action::AttachImage
        );
        assert_eq!(action(Key::ctrl('v'), cx), Action::AttachImage);
        assert_eq!(
            action(Key::alt(KeyCode::Char('V')), cx),
            Action::AttachImage
        );
    }
    let both = Key {
        ctrl: true,
        ..Key::alt(KeyCode::Char('v'))
    };
    assert_eq!(action(both, idle(true)), Action::Edit(both));
    let plain = Key::plain(KeyCode::Char('v'));
    assert_eq!(action(plain, idle(true)), Action::Edit(plain));
}

#[test]
fn esc_and_ctrl_c_stop_the_turn_while_messages_wait() {
    let waiting = Context {
        held: true,
        ..answering(false)
    };
    assert_eq!(action(Key::plain(KeyCode::Esc), waiting), Action::Stop);
    assert_eq!(action(Key::ctrl('c'), waiting), Action::Stop);
    let idle_held = Context {
        held: true,
        ..idle(true)
    };
    assert_eq!(
        action(Key::plain(KeyCode::Esc), idle_held),
        Action::PrimeRewind
    );
}

#[test]
fn esc_primes_a_rewind_on_an_empty_idle_message_and_a_second_esc_opens_it() {
    let esc = Key::plain(KeyCode::Esc);
    assert_eq!(action(esc, idle(true)), Action::PrimeRewind);
    let primed = Context {
        primed: true,
        ..idle(true)
    };
    assert_eq!(action(esc, primed), Action::Rewind);
    assert_eq!(action(esc, idle(false)), Action::Nothing);
    assert_eq!(
        action(
            esc,
            Context {
                primed: true,
                ..idle(false)
            }
        ),
        Action::Nothing
    );
}

#[test]
fn esc_while_answering_stops_and_never_primes_or_opens_a_rewind() {
    let esc = Key::plain(KeyCode::Esc);
    assert_eq!(action(esc, answering(true)), Action::Stop);
    let primed = Context {
        primed: true,
        ..answering(true)
    };
    assert_eq!(action(esc, primed), Action::Stop);
}

#[test]
fn esc_with_a_selection_or_scrolled_keeps_its_meaning_and_never_primes() {
    let esc = Key::plain(KeyCode::Esc);
    for cx in [
        Context {
            selection: true,
            primed: true,
            ..idle(true)
        },
        Context {
            scrolled: true,
            primed: true,
            ..idle(true)
        },
    ] {
        assert!(!matches!(
            action(esc, cx),
            Action::PrimeRewind | Action::Rewind
        ));
    }
    let modified = Key { alt: true, ..esc };
    assert_eq!(action(modified, idle(true)), Action::Nothing);
}

#[test]
fn question_mark_opens_help_only_on_an_empty_new_message() {
    let ask = Key::plain(KeyCode::Char('?'));
    let shifted = Key { shift: true, ..ask };
    for key in [ask, shifted] {
        assert_eq!(action(key, idle(true)), Action::Help);
        assert_eq!(action(key, answering(true)), Action::Help);
        assert_eq!(action(key, idle(false)), Action::Edit(key));
        assert_eq!(action(key, answering(false)), Action::Edit(key));
        let editing = Context {
            editing: true,
            ..idle(true)
        };
        assert_eq!(action(key, editing), Action::Edit(key));
    }
    let alt = Key::alt(KeyCode::Char('?'));
    assert_eq!(action(alt, idle(true)), Action::Edit(alt));
    let ctrl = Key { ctrl: true, ..ask };
    assert_eq!(action(ctrl, idle(true)), Action::Edit(ctrl));
}

#[test]
fn question_mark_in_the_waiting_rows_passes_back_to_the_composer() {
    assert_eq!(row_action(Key::plain(KeyCode::Char('?'))), RowAction::Pass);
}

#[test]
fn ctrl_b_moves_a_movable_call_and_is_cursor_back_otherwise() {
    let movable = Context {
        movable: true,
        ..answering(false)
    };
    assert_eq!(action(Key::ctrl('b'), movable), Action::Background);
    assert_eq!(
        action(Key::ctrl('b'), answering(false)),
        Action::Edit(Key::ctrl('b'))
    );
    assert_eq!(
        action(Key::ctrl('b'), idle(false)),
        Action::Edit(Key::ctrl('b'))
    );
    let left = Key::plain(KeyCode::Left);
    assert_eq!(action(left, movable), Action::Edit(left));
}

#[test]
fn a_running_plugin_command_takes_esc_and_ctrl_c_and_nothing_else() {
    let commanding = |empty: bool| Context {
        commanding: true,
        held: true,
        ..idle(empty)
    };
    for empty in [true, false] {
        assert_eq!(
            action(Key::plain(KeyCode::Esc), commanding(empty)),
            Action::Stop
        );
        assert_eq!(action(Key::ctrl('c'), commanding(empty)), Action::Stop);
    }
    let tab = Key::plain(KeyCode::Tab);
    let others = [
        tab,
        Key::ctrl('d'),
        Key::plain(KeyCode::Enter),
        Key::plain(KeyCode::Up),
    ];
    for key in others {
        for empty in [true, false] {
            let plain = Context {
                held: true,
                ..idle(empty)
            };
            assert_eq!(
                action(key, commanding(empty)),
                action(key, plain),
                "{key:?}"
            );
        }
    }
    assert_eq!(action(Key::ctrl('d'), commanding(true)), Action::Quit);
    assert_eq!(
        action(Key::plain(KeyCode::Enter), commanding(true)),
        Action::Release
    );
    let editing = Context {
        editing: true,
        ..commanding(false)
    };
    assert_eq!(
        action(Key::plain(KeyCode::Esc), editing),
        Action::CancelEdit
    );
}

#[test]
fn f12_ctrl_bracket_and_ctrl_5_take_over_and_f12_is_bound_nowhere_else() {
    let f12 = Key::plain(KeyCode::F(12));
    for key in [f12, Key::ctrl(']'), Key::ctrl('5')] {
        assert!(take_over_key(key), "{key:?}");
    }
    let others = [
        Key { ctrl: true, ..f12 },
        Key { alt: true, ..f12 },
        Key { shift: true, ..f12 },
        Key::plain(KeyCode::Char(']')),
    ];
    for key in others {
        assert!(!take_over_key(key), "{key:?}");
    }
    for cx in [idle(true), idle(false), answering(true), answering(false)] {
        assert_eq!(action(f12, cx), Action::TakeOver);
    }
    assert_eq!(row_action(f12), RowAction::Pass);
    for running in [false, true] {
        assert_eq!(pager_action(f12, running), PagerAction::Nothing);
        assert_eq!(reader_action(f12, running), PagerAction::Nothing);
    }
}

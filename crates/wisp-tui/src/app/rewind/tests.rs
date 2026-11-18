use jiff::tz::TimeZone;
use wisp_protocol::{PointInfo, RequestId, RewindPoint, RewindQuote, Warmth};

use super::*;
use crate::shared::fixed_now;
use crate::term::{Key, KeyCode};

fn point(turn: u64, at: &str) -> PointInfo {
    PointInfo {
        point: RewindPoint::Turn { turn },
        label: format!("message {turn}"),
        at: at.to_owned(),
        inputs_preview: Vec::new(),
        quote: RewindQuote {
            prefix_tokens: 1000,
            warm: Warmth::Warm,
            read_micros: Some(100),
            rewrite_micros: Some(2000),
        },
        compaction: None,
        processes: Vec::new(),
    }
}

fn three() -> Vec<PointInfo> {
    vec![
        point(1, "2026-10-26T14:02:00Z"),
        point(2, "2026-10-26T14:11:00Z"),
        point(3, "2026-10-26T14:20:00Z"),
    ]
}

fn opened() -> Rewind {
    let mut rewind = Rewind::asked(RequestId(4));
    assert_eq!(
        rewind.answered(RequestId(4), &three(), &TimeZone::UTC, fixed_now()),
        Answered::Shown
    );
    rewind
}

fn press(rewind: &mut Rewind, code: KeyCode) -> Act {
    rewind.key(Key::plain(code))
}

#[test]
fn the_picker_waits_for_its_own_answer_and_opens_on_the_newest_point() {
    let mut rewind = Rewind::asked(RequestId(4));
    assert_eq!(rewind.phase(), Phase::Loading);
    assert_eq!(
        rewind.answered(RequestId(3), &three(), &TimeZone::UTC, fixed_now()),
        Answered::Other
    );
    assert_eq!(rewind.phase(), Phase::Loading);
    assert_eq!(
        rewind.answered(RequestId(4), &three(), &TimeZone::UTC, fixed_now()),
        Answered::Shown
    );
    assert_eq!(rewind.phase(), Phase::Picking);
    assert_eq!(rewind.selected(), 2);
    assert_eq!(rewind.times(), ["14:02", "14:11", "14:20"]);
    assert_eq!(
        rewind.answered(RequestId(4), &three(), &TimeZone::UTC, fixed_now()),
        Answered::Other
    );
}

#[test]
fn an_empty_answer_says_there_is_nothing_to_go_back_to() {
    let mut rewind = Rewind::asked(RequestId(1));
    assert_eq!(
        rewind.answered(RequestId(1), &[], &TimeZone::UTC, fixed_now()),
        Answered::Empty
    );
}

#[test]
fn up_down_home_and_end_move_within_the_points_without_wrapping() {
    let mut rewind = opened();
    assert_eq!(press(&mut rewind, KeyCode::Down), Act::Stay);
    assert_eq!(rewind.selected(), 2);
    press(&mut rewind, KeyCode::Up);
    press(&mut rewind, KeyCode::Up);
    press(&mut rewind, KeyCode::Up);
    assert_eq!(rewind.selected(), 0);
    press(&mut rewind, KeyCode::End);
    assert_eq!(rewind.selected(), 2);
    press(&mut rewind, KeyCode::Home);
    assert_eq!(rewind.selected(), 0);
}

#[test]
fn enter_confirms_then_sends_and_esc_steps_back_then_closes() {
    let mut rewind = opened();
    press(&mut rewind, KeyCode::Up);
    assert_eq!(press(&mut rewind, KeyCode::Enter), Act::Stay);
    assert_eq!(rewind.phase(), Phase::Confirming);
    assert_eq!(press(&mut rewind, KeyCode::Up), Act::Stay);
    assert_eq!(rewind.selected(), 1);
    assert_eq!(press(&mut rewind, KeyCode::Esc), Act::Stay);
    assert_eq!(rewind.phase(), Phase::Picking);
    press(&mut rewind, KeyCode::Enter);
    assert_eq!(
        press(&mut rewind, KeyCode::Enter),
        Act::Send(RewindPoint::Turn { turn: 2 })
    );
    let mut rewind = opened();
    assert_eq!(press(&mut rewind, KeyCode::Esc), Act::Close);
}

#[test]
fn ctrl_c_closes_from_any_phase_and_other_keys_stay() {
    let mut loading = Rewind::asked(RequestId(1));
    assert_eq!(press(&mut loading, KeyCode::Enter), Act::Stay);
    assert_eq!(loading.key(Key::ctrl('c')), Act::Close);
    let mut rewind = opened();
    press(&mut rewind, KeyCode::Enter);
    assert_eq!(rewind.key(Key::ctrl('C')), Act::Close);
    let mut rewind = opened();
    assert_eq!(press(&mut rewind, KeyCode::Char('x')), Act::Stay);
    assert_eq!(rewind.key(Key::alt(KeyCode::Enter)), Act::Stay);
    assert_eq!(rewind.phase(), Phase::Picking);
}

#[test]
fn a_click_selects_a_row_and_a_click_on_the_selected_row_confirms() {
    let mut rewind = opened();
    assert_eq!(rewind.select(0), Act::Stay);
    assert_eq!(rewind.selected(), 0);
    assert_eq!(rewind.phase(), Phase::Picking);
    rewind.select(0);
    assert_eq!(rewind.phase(), Phase::Confirming);
    assert_eq!(rewind.select(1), Act::Stay);
    assert_eq!(rewind.selected(), 0);
    let mut rewind = opened();
    rewind.select(9);
    assert_eq!(rewind.selected(), 2);
}

#[test]
fn a_point_time_is_the_clock_time_today_and_gains_the_date_on_other_days() {
    let utc = TimeZone::UTC;
    assert_eq!(
        clock_time("2026-10-26T14:02:59Z", &utc, fixed_now()),
        "14:02"
    );
    assert_eq!(
        clock_time("2026-10-25T14:02:00Z", &utc, fixed_now()),
        "Oct 25 14:02"
    );
    assert_eq!(
        clock_time("2026-09-05T08:15:00Z", &utc, fixed_now()),
        "Sep 5 08:15"
    );
    let plus_two = TimeZone::fixed(jiff::tz::offset(2));
    assert_eq!(
        clock_time("2026-10-26T23:30:00Z", &plus_two, fixed_now()),
        "Oct 27 01:30"
    );
    let late = "2026-10-26T23:00:00Z".parse().unwrap();
    assert_eq!(clock_time("2026-10-26T23:30:00Z", &plus_two, late), "01:30");
    assert_eq!(clock_time("not a time", &utc, fixed_now()), "");
}

use std::time::Duration;

use wisp_protocol::{Answer, Question, QuestionOption, check_answers};
use wisp_render::chrome::{CardHit, CardMode, CardPage};

use super::{Card, SETTLE, Settle, Step};
use crate::term::{Key, KeyCode};

fn question(header: &str, labels: &[&str], multi_select: bool) -> Question {
    Question {
        header: header.to_owned(),
        question: format!("Which {header}?"),
        options: labels
            .iter()
            .map(|l| QuestionOption {
                label: (*l).to_owned(),
                description: format!("About {l}."),
            })
            .collect(),
        multi_select,
        source: None,
    }
}

fn one(multi: bool) -> Card {
    Card::new(
        "c1".to_owned(),
        vec![question(
            "Database",
            &["Postgres", "SQLite", "Dynamo"],
            multi,
        )],
    )
}

fn three() -> Card {
    Card::new(
        "c3".to_owned(),
        vec![
            question("Database", &["Postgres", "SQLite"], false),
            question("Features", &["Auth", "Search", "Export"], true),
            question("Deploy", &["Fly", "VPS"], false),
        ],
    )
}

fn press(card: &mut Card, code: KeyCode) -> Step {
    card.key(Key::plain(code))
}

fn typed(card: &mut Card, text: &str) {
    for c in text.chars() {
        assert_eq!(press(card, KeyCode::Char(c)), Step::Stay);
    }
}

fn sent(step: Step) -> Vec<Answer> {
    match step {
        Step::Send(answers) => answers,
        other => panic!("expected a send, got {other:?}"),
    }
}

#[test]
fn arrows_move_between_rows_and_off_the_own_row_keeping_its_text() {
    let mut card = one(false);
    assert_eq!(card.row(), 0);
    press(&mut card, KeyCode::Up);
    assert_eq!(card.row(), 0);
    for _ in 0..5 {
        press(&mut card, KeyCode::Down);
    }
    assert_eq!(card.row(), 3, "the own-answer row is the last");
    typed(&mut card, "mine");
    press(&mut card, KeyCode::Up);
    assert_eq!(card.row(), 2);
    assert_eq!(card.shown().drafts[0].own, "mine");
    press(&mut card, KeyCode::Down);
    assert_eq!(card.shown().cursor, Some((0, 4)));
}

#[test]
fn a_number_picks_and_sends_a_lone_single_select_question() {
    let mut card = one(false);
    let answers = sent(press(&mut card, KeyCode::Char('2')));
    assert_eq!(answers, vec![Answer::choose(1)]);
    assert_eq!(check_answers(card.questions(), &answers), Ok(()));
}

#[test]
fn the_last_number_focuses_the_own_row_and_numbers_there_are_text() {
    let mut card = one(false);
    assert_eq!(press(&mut card, KeyCode::Char('4')), Step::Stay);
    assert_eq!(card.row(), 3);
    typed(&mut card, "12 replicas");
    assert_eq!(card.shown().drafts[0].own, "12 replicas");
    let answers = sent(press(&mut card, KeyCode::Enter));
    assert_eq!(answers, vec![Answer::own("12 replicas")]);
}

#[test]
fn numbers_toggle_on_a_multi_select_question() {
    let mut card = one(true);
    press(&mut card, KeyCode::Char('3'));
    press(&mut card, KeyCode::Char('1'));
    press(&mut card, KeyCode::Char('3'));
    press(&mut card, KeyCode::Char('2'));
    assert_eq!(card.shown().drafts[0].chosen, vec![0, 1]);
    assert_eq!(card.row(), 1);
}

#[test]
fn space_picks_on_single_select_toggles_on_multi_and_types_on_the_own_row() {
    let mut card = one(false);
    press(&mut card, KeyCode::Down);
    assert_eq!(
        sent(press(&mut card, KeyCode::Char(' '))),
        vec![Answer::choose(1)]
    );
    let mut card = one(true);
    press(&mut card, KeyCode::Char(' '));
    press(&mut card, KeyCode::Down);
    press(&mut card, KeyCode::Char(' '));
    press(&mut card, KeyCode::Char(' '));
    assert_eq!(card.shown().drafts[0].chosen, vec![0]);
    press(&mut card, KeyCode::Char('4'));
    typed(&mut card, "a b");
    assert_eq!(card.shown().drafts[0].own, "a b");
}

#[test]
fn enter_picks_on_single_select_and_goes_on_on_multi() {
    let mut card = three();
    press(&mut card, KeyCode::Down);
    assert_eq!(press(&mut card, KeyCode::Enter), Step::Stay);
    assert_eq!(card.page(), CardPage::Question(1));
    press(&mut card, KeyCode::Char('1'));
    press(&mut card, KeyCode::Char('3'));
    assert_eq!(press(&mut card, KeyCode::Enter), Step::Stay);
    assert_eq!(card.page(), CardPage::Question(2));
    assert_eq!(press(&mut card, KeyCode::Enter), Step::Stay);
    assert_eq!(card.page(), CardPage::Review);
    let answers = sent(press(&mut card, KeyCode::Enter));
    assert_eq!(
        answers,
        vec![
            Answer::choose(1),
            Answer {
                chosen: vec![0, 2],
                own: None
            },
            Answer::choose(0)
        ]
    );
    assert_eq!(check_answers(card.questions(), &answers), Ok(()));
    assert_eq!(sent(press(&mut card, KeyCode::Char(' '))), answers);
}

#[test]
fn enter_on_the_own_row_sends_single_text_and_keeps_multi_text() {
    let mut card = three();
    press(&mut card, KeyCode::Char('3'));
    typed(&mut card, "MySQL");
    press(&mut card, KeyCode::Enter);
    assert_eq!(card.page(), CardPage::Question(1));
    press(&mut card, KeyCode::Char('2'));
    typed(&mut card, "webhooks");
    press(&mut card, KeyCode::Enter);
    assert_eq!(card.page(), CardPage::Question(2));
    press(&mut card, KeyCode::Tab);
    let answers = sent(press(&mut card, KeyCode::Enter));
    assert_eq!(answers[0], Answer::own("MySQL"));
    assert_eq!(
        answers[1],
        Answer {
            chosen: vec![1],
            own: Some("webhooks".to_owned())
        }
    );
    assert_eq!(answers[2], Answer::skipped());
    assert_eq!(check_answers(card.questions(), &answers), Ok(()));
}

#[test]
fn a_blank_own_row_never_sends_and_a_lone_question_cannot_be_skipped() {
    let mut card = one(false);
    press(&mut card, KeyCode::Char('4'));
    typed(&mut card, "  ");
    assert_eq!(press(&mut card, KeyCode::Enter), Step::Stay);
    let mut card = one(true);
    assert_eq!(press(&mut card, KeyCode::Enter), Step::Stay);
    press(&mut card, KeyCode::Char('2'));
    assert_eq!(
        sent(press(&mut card, KeyCode::Enter)),
        vec![Answer::choose(1)]
    );
}

#[test]
fn a_printable_key_moves_to_the_own_row_and_starts_typing() {
    let mut card = three();
    press(&mut card, KeyCode::Char('x'));
    assert_eq!(card.row(), 2);
    assert_eq!(card.shown().drafts[0].own, "x");
}

#[test]
fn newline_keys_add_a_line_only_on_the_own_row() {
    let mut card = one(false);
    let shift_enter = Key {
        shift: true,
        ..Key::plain(KeyCode::Enter)
    };
    assert_eq!(card.key(shift_enter), Step::Stay);
    assert_eq!(card.shown().drafts[0].own, "");
    press(&mut card, KeyCode::Char('4'));
    typed(&mut card, "a");
    card.key(shift_enter);
    typed(&mut card, "b");
    card.key(Key::alt(KeyCode::Enter));
    typed(&mut card, "c");
    card.key(Key::ctrl('j'));
    typed(&mut card, "d");
    assert_eq!(card.shown().drafts[0].own, "a\nb\nc\nd");
    assert_eq!(
        sent(press(&mut card, KeyCode::Enter)),
        vec![Answer::own("a\nb\nc\nd")]
    );
}

#[test]
fn tab_and_arrows_page_between_questions_but_arrows_edit_on_the_own_row() {
    let mut card = three();
    press(&mut card, KeyCode::Tab);
    assert_eq!(card.page(), CardPage::Question(1));
    press(&mut card, KeyCode::Right);
    assert_eq!(card.page(), CardPage::Question(2));
    press(&mut card, KeyCode::Tab);
    assert_eq!(card.page(), CardPage::Review);
    press(&mut card, KeyCode::BackTab);
    assert_eq!(card.page(), CardPage::Question(2));
    press(&mut card, KeyCode::Left);
    assert_eq!(card.page(), CardPage::Question(1));
    card.key(Key {
        shift: true,
        ..Key::plain(KeyCode::Tab)
    });
    assert_eq!(card.page(), CardPage::Question(0));
    press(&mut card, KeyCode::Char('3'));
    typed(&mut card, "ab");
    press(&mut card, KeyCode::Left);
    assert_eq!(card.page(), CardPage::Question(0));
    assert_eq!(card.shown().cursor, Some((0, 1)));
    press(&mut card, KeyCode::Tab);
    assert_eq!(card.page(), CardPage::Question(1));
    let mut lone = one(false);
    press(&mut lone, KeyCode::Tab);
    press(&mut lone, KeyCode::BackTab);
    assert_eq!(lone.page(), CardPage::Question(0));
}

#[test]
fn esc_and_ctrl_c_stop_and_typed_text_is_kept_for_the_composer() {
    let mut card = three();
    press(&mut card, KeyCode::Char('3'));
    typed(&mut card, "MySQL");
    assert_eq!(press(&mut card, KeyCode::Esc), Step::Stop);
    assert_eq!(card.key(Key::ctrl('c')), Step::Stop);
    let texts: Vec<String> = card
        .typed_texts()
        .iter()
        .map(|s| s.text().to_owned())
        .collect();
    assert_eq!(texts, ["MySQL"]);
    for typed_field in [true, false] {
        let mut question = question("Server", &["Not now"], false);
        question.source = Some(wisp_protocol::QuestionSource {
            asked_by: wisp_protocol::AskedBy::Server {
                name: "linear".into(),
            },
            typed: typed_field,
        });
        let mut server = Card::new("server".into(), vec![question]);
        server.click(CardHit::Own);
        typed(&mut server, "server-only words");
        assert_eq!(press(&mut server, KeyCode::Esc), Step::Stop);
        assert_eq!(server.key(Key::ctrl('c')), Step::Stop);
        assert_eq!(server.typed_texts().is_empty(), typed_field);
    }
}

#[test]
fn clicks_pick_toggle_and_focus() {
    let mut card = three();
    assert_eq!(card.click(CardHit::Option(1)), Step::Stay);
    assert_eq!(card.page(), CardPage::Question(1));
    card.click(CardHit::Option(2));
    card.click(CardHit::Option(0));
    assert_eq!(card.shown().drafts[1].chosen, vec![0, 2]);
    card.click(CardHit::Own);
    assert_eq!(card.row(), 3);
    card.click(CardHit::Send);
    assert_eq!(card.page(), CardPage::Question(1));
    press(&mut card, KeyCode::Tab);
    press(&mut card, KeyCode::Tab);
    assert_eq!(card.click(CardHit::Back), Step::Stay);
    assert_eq!(card.page(), CardPage::Question(2));
    press(&mut card, KeyCode::Tab);
    assert_eq!(card.page(), CardPage::Review);
    assert!(matches!(card.click(CardHit::Send), Step::Send(_)));
}

#[test]
fn the_review_goes_back_to_the_first_unanswered_question() {
    let mut card = three();
    press(&mut card, KeyCode::Char('1'));
    press(&mut card, KeyCode::Tab);
    press(&mut card, KeyCode::Tab);
    assert_eq!(card.page(), CardPage::Review);
    press(&mut card, KeyCode::Down);
    assert_eq!(card.row(), 1);
    press(&mut card, KeyCode::Enter);
    assert_eq!(card.page(), CardPage::Question(1));
}

#[test]
fn going_on_skips_answered_questions() {
    let mut card = three();
    press(&mut card, KeyCode::Tab);
    press(&mut card, KeyCode::Tab);
    press(&mut card, KeyCode::Char('2'));
    assert_eq!(card.page(), CardPage::Question(0));
    press(&mut card, KeyCode::Char('1'));
    assert_eq!(card.page(), CardPage::Question(1));
    press(&mut card, KeyCode::Enter);
    assert_eq!(card.page(), CardPage::Review);
}

#[test]
fn a_paste_goes_to_the_own_row_whole() {
    let mut card = one(false);
    card.paste("line one\nline two");
    assert_eq!(card.row(), 3);
    assert_eq!(card.shown().drafts[0].own, "line one\nline two");
}

#[test]
fn a_settle_ends_half_a_second_after_the_last_hold() {
    let at = Duration::from_secs(10);
    let typed = at - Duration::from_millis(100);
    let mut settle = Settle::after(Some(typed), at);
    assert_eq!(settle.due(at), Some(typed + SETTLE));
    assert!(!settle.settled(at) && settle.settled(typed + SETTLE));
    assert_eq!(Settle::after(None, at).due(at), None);
    settle.hold(at);
    settle.hold(typed);
    assert_eq!(settle.due(at), Some(at + SETTLE));
}

#[test]
fn a_sent_card_ignores_keys_until_refused() {
    let mut card = one(false);
    assert_eq!(card.mode(false), CardMode::Settling);
    assert_eq!(card.mode(true), CardMode::Active);
    card.sent();
    assert_eq!(card.mode(true), CardMode::Sending);
    assert_eq!(press(&mut card, KeyCode::Char('2')), Step::Stay);
    assert_eq!(press(&mut card, KeyCode::Esc), Step::Stop);
    card.refused();
    assert!(matches!(
        press(&mut card, KeyCode::Char('2')),
        Step::Send(_)
    ));
}

#[test]
fn own_words_alone_answer_a_multi_select_question_on_every_side() {
    let mut card = three();
    press(&mut card, KeyCode::Char('1'));
    assert_eq!(card.page(), CardPage::Question(1));
    press(&mut card, KeyCode::Char('4'));
    typed(&mut card, "Webhooks");
    press(&mut card, KeyCode::Tab);
    press(&mut card, KeyCode::Tab);
    assert_eq!(card.page(), CardPage::Review);
    let shown = card.shown();
    assert!(shown.drafts[1].has_answer());
    assert!(!shown.drafts[2].has_answer());
    assert_eq!(card.click(CardHit::Back), Step::Stay);
    assert_eq!(card.page(), CardPage::Question(2));
    press(&mut card, KeyCode::Char('2'));
    assert_eq!(card.page(), CardPage::Review);
    let answers = sent(card.click(CardHit::Send));
    assert_eq!(answers[1], Answer::own("Webhooks".to_owned()));
}

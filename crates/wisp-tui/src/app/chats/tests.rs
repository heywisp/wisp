use wisp_render::chrome::mockup_chats;

use super::*;

fn press(chats: &mut Chats, code: KeyCode) -> Act {
    chats.key(Key::plain(code))
}

fn typed(chats: &mut Chats, text: &str) {
    for c in text.chars() {
        press(chats, KeyCode::Char(c));
    }
}

#[test]
fn the_focus_starts_on_the_newest_chat_that_can_be_opened() {
    let mut chats = Chats::listed(mockup_chats(), None);
    assert_eq!(chats.focused(), Some(1));
    assert_eq!(press(&mut chats, KeyCode::Enter), Act::Pick("b".into()));
    press(&mut chats, KeyCode::Up);
    assert_eq!(chats.focused(), Some(0));
    assert_eq!(press(&mut chats, KeyCode::Enter), Act::Stay);
    press(&mut chats, KeyCode::End);
    assert_eq!(chats.chosen().map(|c| c.id.as_str()), Some("f"));
    for (code, at) in [
        (KeyCode::Home, 0),
        (KeyCode::PageDown, 5),
        (KeyCode::PageUp, 0),
        (KeyCode::Tab, 1),
        (KeyCode::BackTab, 0),
    ] {
        press(&mut chats, code);
        assert_eq!(chats.focused(), Some(at));
    }
}

#[test]
fn typing_filters_by_topic_account_and_model_and_esc_clears_then_closes() {
    let mut chats = Chats::listed(mockup_chats(), None);
    typed(&mut chats, "usage");
    let ids: Vec<&str> = chats.shown().iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["a", "e"]);
    assert_eq!(chats.chosen().map(|c| c.id.as_str()), Some("e"));
    for word in ["narrow", "trim"] {
        press(&mut chats, KeyCode::Esc);
        typed(&mut chats, word);
        assert_eq!(chats.chosen().map(|c| c.id.as_str()), Some("e"));
    }
    press(&mut chats, KeyCode::Esc);
    typed(&mut chats, "usage");
    chats.paste("x\n");
    assert_eq!(chats.view(ChatsPlace::Start, Vec::new()).query, "usagex ");
    assert!(chats.shown().is_empty());
    assert_eq!(press(&mut chats, KeyCode::Enter), Act::Stay);
    assert_eq!(press(&mut chats, KeyCode::Esc), Act::Stay);
    assert_eq!(chats.view(ChatsPlace::Start, Vec::new()).query, "");
    assert_eq!(chats.shown().len(), 6);
    typed(&mut chats, "SONNET");
    assert_eq!(chats.chosen().map(|c| c.id.as_str()), Some("c"));
    press(&mut chats, KeyCode::Esc);
    typed(&mut chats, "chatgpt");
    assert_eq!(chats.chosen().map(|c| c.id.as_str()), Some("d"));
    press(&mut chats, KeyCode::Esc);
    chats.chats[3].typed = "then tag the zebra build".to_owned();
    typed(&mut chats, "zebra");
    let zebra = Query::new("zebra");
    assert!(
        chats
            .chosen()
            .is_some_and(|c| c.id == "d" && zebra.found(&c.topic).is_empty())
    );
    press(&mut chats, KeyCode::Esc);
    assert_eq!(press(&mut chats, KeyCode::Esc), Act::Close);
}

#[test]
fn a_pick_is_checked_against_a_fresh_list_before_anything_closes() {
    let mut chats = Chats::asked(RequestId(1), "review");
    assert!(chats.loading());
    assert_eq!(press(&mut chats, KeyCode::Enter), Act::Stay);
    assert_eq!(
        chats.answered(RequestId(9), &mockup_chats()),
        Answered::Other
    );
    assert_eq!(
        chats.answered(RequestId(1), &mockup_chats()),
        Answered::Shown
    );
    let Act::Pick(id) = press(&mut chats, KeyCode::Enter) else {
        panic!("no pick");
    };
    chats.checking(RequestId(2), id);
    assert!(!chats.loading());
    let mut taken = mockup_chats();
    taken[1].open_elsewhere = true;
    assert_eq!(chats.answered(RequestId(2), &taken), Answered::Shown);
    assert!(chats.chosen().is_some_and(|c| c.open_elsewhere));
    assert_eq!(press(&mut chats, KeyCode::Enter), Act::Stay);
    chats.checking(RequestId(3), "c".into());
    assert_eq!(
        chats.answered(RequestId(3), &mockup_chats()),
        Answered::Open("c".into())
    );
    let mut empty = Chats::asked(RequestId(4), "");
    assert_eq!(empty.answered(RequestId(4), &[]), Answered::Empty);
}

#[test]
fn keys_typed_while_the_list_loads_filter_it() {
    let mut chats = Chats::asked(RequestId(1), "");
    typed(&mut chats, "revx");
    press(&mut chats, KeyCode::Backspace);
    assert_eq!(press(&mut chats, KeyCode::Down), Act::Stay);
    assert_eq!(
        chats.answered(RequestId(1), &mockup_chats()),
        Answered::Shown
    );
    let shown = chats.shown();
    assert!(!shown.is_empty());
    let mut rev = Chats::listed(mockup_chats(), None);
    typed(&mut rev, "rev");
    assert_eq!(shown, rev.shown());
    assert!(shown.len() < mockup_chats().len());
}

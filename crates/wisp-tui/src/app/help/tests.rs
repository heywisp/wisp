use super::*;
use crate::app::commands::CommandId;
use crate::app::keymap::{Context, action, row_action};
use crate::app::pager::{pager_action, reader_action};
use crate::term::clipboard::{SELECT_KEY, SELECT_KEY_REMOTE};
use crate::term::{Key, KeyCode};

fn next_action(action: Action) -> Option<Action> {
    match action {
        Action::Send => Some(Action::Steer),
        Action::Steer => Some(Action::Queue),
        Action::Queue => Some(Action::Release),
        Action::Release => Some(Action::Newline),
        Action::Newline => Some(Action::Stop),
        Action::Stop => Some(Action::Clear),
        Action::Clear => Some(Action::ArmQuit),
        Action::ArmQuit => Some(Action::Quit),
        Action::Quit => Some(Action::HistoryPrevious),
        Action::HistoryPrevious => Some(Action::HistoryNext),
        Action::HistoryNext => Some(Action::EnterRows),
        Action::EnterRows => Some(Action::SaveEdit),
        Action::SaveEdit => Some(Action::CancelEdit),
        Action::CancelEdit => Some(Action::Pager),
        Action::Pager => Some(Action::Help),
        Action::Help => Some(Action::Repaint),
        Action::Repaint => Some(Action::Suspend),
        Action::Suspend => Some(Action::Scroll(ViewScroll::HalfUp)),
        Action::Scroll(scroll) => match scroll {
            ViewScroll::HalfUp => Some(Action::Scroll(ViewScroll::HalfDown)),
            ViewScroll::HalfDown => Some(Action::Scroll(ViewScroll::Top)),
            ViewScroll::Top => Some(Action::Scroll(ViewScroll::Bottom)),
            ViewScroll::Bottom => Some(Action::CycleEffort),
        },
        Action::CycleEffort => Some(Action::AttachImage),
        Action::AttachImage => Some(Action::Background),
        Action::Background => Some(Action::TakeOver),
        Action::TakeOver => Some(Action::Copy),
        Action::Copy => Some(Action::Deselect),
        Action::Deselect => Some(Action::PrimeRewind),
        Action::PrimeRewind => Some(Action::Rewind),
        Action::Rewind => Some(Action::Agents),
        Action::Agents => Some(Action::Edit(Key::plain(KeyCode::Char('a')))),
        Action::Edit(_) => Some(Action::Nothing),
        Action::Nothing => None,
    }
}

fn action_exempt(action: Action) -> Option<&'static str> {
    match action {
        Action::Nothing => Some("the key does nothing"),
        Action::Edit(_) => Some("typing; the editing keys are rows with no action of their own"),
        Action::Send
        | Action::Steer
        | Action::Queue
        | Action::Release
        | Action::Newline
        | Action::Stop
        | Action::Clear
        | Action::ArmQuit
        | Action::Quit
        | Action::HistoryPrevious
        | Action::HistoryNext
        | Action::EnterRows
        | Action::SaveEdit
        | Action::CancelEdit
        | Action::Pager
        | Action::Help
        | Action::Repaint
        | Action::Suspend
        | Action::Scroll(_)
        | Action::CycleEffort
        | Action::AttachImage
        | Action::Background
        | Action::TakeOver
        | Action::Copy
        | Action::Deselect
        | Action::PrimeRewind
        | Action::Rewind
        | Action::Agents => None,
    }
}

fn next_row(action: RowAction) -> Option<RowAction> {
    match action {
        RowAction::Up => Some(RowAction::Down),
        RowAction::Down => Some(RowAction::Edit),
        RowAction::Edit => Some(RowAction::Remove),
        RowAction::Remove => Some(RowAction::Switch),
        RowAction::Switch => Some(RowAction::Leave),
        RowAction::Leave => Some(RowAction::Stay),
        RowAction::Stay => Some(RowAction::Pass),
        RowAction::Pass => None,
    }
}

fn row_exempt(action: RowAction) -> Option<&'static str> {
    match action {
        RowAction::Pass => Some("the key goes back to the composer"),
        RowAction::Up
        | RowAction::Down
        | RowAction::Edit
        | RowAction::Remove
        | RowAction::Switch
        | RowAction::Leave
        | RowAction::Stay => None,
    }
}

fn next_pager(action: PagerAction) -> Option<PagerAction> {
    match action {
        PagerAction::Close => Some(PagerAction::Stop),
        PagerAction::Stop => Some(PagerAction::Scroll(Scroll::Lines(-1))),
        PagerAction::Scroll(scroll) => match scroll {
            Scroll::Lines(n) if n < 0 => Some(PagerAction::Scroll(Scroll::Lines(1))),
            Scroll::Lines(_) => Some(PagerAction::Scroll(Scroll::Pages(-1))),
            Scroll::Pages(n) if n < 0 => Some(PagerAction::Scroll(Scroll::Pages(1))),
            Scroll::Pages(_) => Some(PagerAction::Scroll(Scroll::Halves(-1))),
            Scroll::Halves(n) if n < 0 => Some(PagerAction::Scroll(Scroll::Halves(1))),
            Scroll::Halves(_) => Some(PagerAction::Scroll(Scroll::Top)),
            Scroll::Top => Some(PagerAction::Scroll(Scroll::Bottom)),
            Scroll::Bottom => Some(PagerAction::Repaint),
        },
        PagerAction::Repaint => Some(PagerAction::Suspend),
        PagerAction::Suspend => Some(PagerAction::HandOff),
        PagerAction::HandOff => Some(PagerAction::Detail),
        PagerAction::Detail => Some(PagerAction::Screen),
        PagerAction::Screen => Some(PagerAction::Copy),
        PagerAction::Copy => Some(PagerAction::Search { back: false }),
        PagerAction::Search { back: false } => Some(PagerAction::Search { back: true }),
        PagerAction::Search { back: true } => Some(PagerAction::Next),
        PagerAction::Next => Some(PagerAction::Previous),
        PagerAction::Previous => Some(PagerAction::Nothing),
        PagerAction::Nothing => None,
    }
}

fn pager_exempt(action: PagerAction) -> Option<&'static str> {
    match action {
        PagerAction::Nothing => Some("the key does nothing"),
        PagerAction::Close
        | PagerAction::Stop
        | PagerAction::Scroll(_)
        | PagerAction::Repaint
        | PagerAction::Suspend
        | PagerAction::HandOff
        | PagerAction::Detail
        | PagerAction::Screen
        | PagerAction::Copy
        | PagerAction::Search { .. }
        | PagerAction::Next
        | PagerAction::Previous => None,
    }
}

fn chain<T: Copy>(first: T, next: fn(T) -> Option<T>) -> Vec<T> {
    let mut all = vec![first];
    while let Some(after) = all.last().copied().and_then(next) {
        all.push(after);
    }
    all
}

fn documented(doc: Doc) -> bool {
    KEYS.iter()
        .flat_map(|s| s.rows.iter())
        .any(|row| row.docs.contains(&doc))
}

#[test]
fn every_keymap_action_is_on_the_help_screen_or_exempt_with_a_reason() {
    let actions = chain(Action::Send, next_action);
    assert_eq!(actions.len(), 33);
    let exempt: Vec<Action> = actions
        .iter()
        .copied()
        .filter(|&a| action_exempt(a).is_some())
        .collect();
    assert_eq!(exempt.len(), 2, "{exempt:?}");
    for action in actions {
        match action_exempt(action) {
            Some(reason) => assert!(!documented(Doc::Main(action)), "{action:?}: {reason}"),
            None => assert!(documented(Doc::Main(action)), "{action:?} has no row"),
        }
    }
    let rows = chain(RowAction::Up, next_row);
    assert_eq!(rows.len(), 8);
    for action in rows {
        match row_exempt(action) {
            Some(reason) => assert!(!documented(Doc::Rows(action)), "{action:?}: {reason}"),
            None => assert!(documented(Doc::Rows(action)), "{action:?} has no row"),
        }
    }
    let pager = chain(PagerAction::Close, next_pager);
    assert_eq!(pager.len(), 21);
    for action in pager {
        let doc = match action {
            PagerAction::Detail | PagerAction::Screen | PagerAction::Copy => Doc::Reader(action),
            _ => Doc::Pager(action),
        };
        match pager_exempt(action) {
            Some(reason) => assert!(!documented(doc), "{action:?}: {reason}"),
            None => assert!(documented(doc), "{action:?} has no row"),
        }
    }
}

fn key_of(word: &str) -> Option<Key> {
    let plain = |code| Some(Key::plain(code));
    let ctrl = |code| {
        Some(Key {
            ctrl: true,
            ..Key::plain(code)
        })
    };
    match word {
        "enter" => plain(KeyCode::Enter),
        "tab" => plain(KeyCode::Tab),
        "esc" => plain(KeyCode::Esc),
        "space" => plain(KeyCode::Char(' ')),
        "pgup" => plain(KeyCode::PageUp),
        "pgdn" => plain(KeyCode::PageDown),
        "home" => plain(KeyCode::Home),
        "end" => plain(KeyCode::End),
        "del" => plain(KeyCode::Delete),
        "shift-tab" => plain(KeyCode::BackTab),
        "\u{2191}" | "up" => plain(KeyCode::Up),
        "\u{2193}" | "down" => plain(KeyCode::Down),
        "ctrl-home" => ctrl(KeyCode::Home),
        "ctrl-end" => ctrl(KeyCode::End),
        "alt-enter" => Some(Key::alt(KeyCode::Enter)),
        "shift-enter" => Some(Key {
            shift: true,
            ..Key::plain(KeyCode::Enter)
        }),
        "alt-v" => Some(Key::alt(KeyCode::Char('v'))),
        "f12" => plain(KeyCode::F(12)),
        _ => match word.strip_prefix("ctrl-") {
            Some(rest) if rest.chars().count() == 1 => rest.chars().next().map(Key::ctrl),
            Some(_) => None,
            None if word.chars().count() == 1 => {
                word.chars().next().map(|c| plain(KeyCode::Char(c)))?
            }
            None => None,
        },
    }
}

const NOT_KEYS: &[&str] = &[
    "wheel", "drag", "while", "editing", "empty", "message", "twice",
];

fn contexts() -> Vec<Context> {
    (0u32..1 << 16)
        .map(|bits| {
            let on = |n: u32| bits & (1 << n) != 0;
            Context {
                running: on(0),
                commanding: false,
                empty: on(1),
                blank: on(2),
                armed: on(3),
                first_row: on(4),
                last_row: on(5),
                scrolled: on(6),
                selection: on(7),
                tab_queues: on(8),
                held: on(9),
                editing: on(10),
                rows: on(11),
                browsing: on(12),
                primed: on(13),
                movable: on(14),
                agents: on(15),
            }
        })
        .collect()
}

fn reaches(key: Key, doc: Doc, contexts: &[Context]) -> bool {
    match doc {
        Doc::Main(want) => contexts.iter().any(|&cx| action(key, cx) == want),
        Doc::Rows(want) => row_action(key) == want,
        Doc::Pager(want) => [false, true]
            .into_iter()
            .any(|running| pager_action(key, running) == want),
        Doc::Reader(want) => [false, true]
            .into_iter()
            .any(|running| reader_action(key, running) == want),
    }
}

#[test]
fn every_key_a_row_names_does_what_the_row_says() {
    let contexts = contexts();
    let terminal = Terminal {
        glyphs: GlyphSet::Unicode,
        shift_enter: true,
        mouse: true,
        select_key: SELECT_KEY,
        right_paste: true,
    };
    let mut checked = 0;
    for section in KEYS {
        for row in section.rows {
            if row.docs.is_empty() {
                continue;
            }
            let keys = terminal.keys(row.keys);
            for word in keys.split([',', ' ']).filter(|w| !w.is_empty()) {
                if NOT_KEYS.contains(&word) {
                    continue;
                }
                let key = key_of(word)
                    .unwrap_or_else(|| panic!("{word:?} in {:?} is not a known key", row.keys));
                checked += 1;
                assert!(
                    row.docs.iter().any(|&doc| reaches(key, doc, &contexts)),
                    "{word:?} in {:?} does none of {:?}",
                    row.keys,
                    row.docs
                );
            }
        }
    }
    assert!(checked >= 50, "{checked}");
}

fn unicode(mouse: bool, shift_enter: bool) -> Terminal {
    Terminal {
        glyphs: GlyphSet::Unicode,
        shift_enter,
        mouse,
        select_key: SELECT_KEY,
        right_paste: false,
    }
}

fn all_keys(terminal: Terminal) -> Vec<String> {
    sections(terminal, &[])
        .into_iter()
        .flat_map(|s| s.rows.into_iter().map(|r| r.keys))
        .collect()
}

fn row_for(terminal: Terminal, text: &str) -> Option<HelpRow> {
    sections(terminal, &[])
        .into_iter()
        .flat_map(|s| s.rows)
        .find(|r| r.text == text)
}

#[test]
fn every_registry_command_is_listed_first_in_registry_order() {
    let sections = sections(unicode(true, false), &[]);
    let commands = &sections[0];
    assert_eq!(commands.title, copy::HELP_COMMANDS);
    assert_eq!(commands.lead, None);
    assert_eq!(commands.rows.len(), COMMANDS.len());
    for (row, command) in commands.rows.iter().zip(COMMANDS) {
        assert!(
            row.keys.starts_with(&format!("/{}", command.name)),
            "{row:?}"
        );
        assert_eq!(row.text, command.summary);
        assert_eq!(format!("/{}{}", command.name, row.args), synopsis(command));
    }
    let exit = commands
        .rows
        .iter()
        .find(|r| r.keys.starts_with("/exit"))
        .unwrap();
    assert_eq!(exit.keys, "/exit, /quit");
    assert_eq!(exit.args, "");
    let model = crate::app::commands::get(CommandId::Model).unwrap();
    assert_eq!(command_row(model).args, " [id]");
}

#[test]
fn the_keys_follow_the_commands_in_six_sections() {
    let sections = sections(unicode(true, false), &[]);
    let titles: Vec<&str> = sections[1..].iter().map(|s| s.title.as_str()).collect();
    assert_eq!(
        titles,
        [
            copy::KEYS_WRITING,
            copy::KEYS_WORKING,
            copy::KEYS_WAITING,
            copy::KEYS_READING,
            copy::KEYS_TRANSCRIPT,
            copy::KEYS_ANYWHERE,
        ]
    );
    assert!(
        sections[1..]
            .iter()
            .all(|s| s.lead.as_deref() == Some(copy::KEYS_LEAD))
    );
    assert_eq!(sections[FIRST_KEYS].title, copy::KEYS_WRITING);
}

#[test]
fn the_newline_row_names_only_the_keys_this_terminal_sends() {
    let plain = row_for(unicode(true, false), "Add a line").unwrap();
    assert_eq!(plain.keys, NEWLINE_ROW);
    let kitty = row_for(unicode(true, true), "Add a line").unwrap();
    assert_eq!(kitty.keys, NEWLINE_ROW_KITTY);
    assert!(
        !all_keys(unicode(true, false))
            .iter()
            .any(|k| k.contains("shift-enter"))
    );
}

#[test]
fn the_selection_rows_follow_mouse_capture_and_remoteness() {
    let captured = row_for(unicode(true, false), "Select in the terminal").unwrap();
    assert_eq!(captured.keys, SELECT_KEY);
    assert!(row_for(unicode(true, false), "Select, then copy").is_some());
    assert!(row_for(unicode(true, false), "Clear the selection").is_some());
    let native = row_for(unicode(false, false), "Select in the terminal").unwrap();
    assert_eq!(native.keys, "drag");
    assert!(row_for(unicode(false, false), "Select, then copy").is_none());
    assert!(row_for(unicode(false, false), "Clear the selection").is_none());
    let remote = Terminal {
        select_key: SELECT_KEY_REMOTE,
        ..unicode(true, false)
    };
    assert_eq!(
        row_for(remote, "Select in the terminal").unwrap().keys,
        SELECT_KEY_REMOTE
    );
}

#[test]
fn arrows_follow_the_glyph_set() {
    let unicode_keys = all_keys(unicode(true, false));
    assert!(unicode_keys.contains(&"\u{2191} \u{2193}".to_owned()));
    assert!(unicode_keys.contains(&"\u{2191}".to_owned()));
    for glyphs in [GlyphSet::Ascii, GlyphSet::AmbiguousWide] {
        let terminal = Terminal {
            glyphs,
            ..unicode(true, false)
        };
        let keys = all_keys(terminal);
        assert!(keys.contains(&"up down".to_owned()), "{keys:#?}");
        assert!(keys.contains(&"up down, j k".to_owned()), "{keys:#?}");
        assert!(keys.iter().all(|k| k.is_ascii()), "{keys:#?}");
    }
}

#[test]
fn no_placeholder_survives_and_the_image_row_keeps_its_wording() {
    for mouse in [false, true] {
        for shift_enter in [false, true] {
            for row in sections(unicode(mouse, shift_enter), &[])
                .into_iter()
                .flat_map(|s| s.rows)
            {
                assert!(!row.keys.contains(['{', '}']), "{row:?}");
                assert!(!row.text.contains(['{', '}']), "{row:?}");
            }
        }
    }
    let image = row_for(unicode(true, false), "Attach the clipboard's image").unwrap();
    assert_eq!(image.keys, "ctrl-v, alt-v");
}

#[test]
fn the_right_click_row_says_what_this_terminal_does() {
    let pastes = Terminal {
        right_paste: true,
        ..unicode(true, false)
    };
    let row = row_for(pastes, RIGHT_CLICK_PASTES).unwrap();
    assert_eq!(row.keys, RIGHT_CLICK);
    assert!(row_for(pastes, RIGHT_CLICK_COPIES).is_none());
    let copies = row_for(unicode(true, false), RIGHT_CLICK_COPIES).unwrap();
    assert_eq!(copies.keys, RIGHT_CLICK);
    assert!(row_for(unicode(true, false), RIGHT_CLICK_PASTES).is_none());
    for right_paste in [false, true] {
        let native = Terminal {
            right_paste,
            ..unicode(false, false)
        };
        assert!(!all_keys(native).contains(&RIGHT_CLICK.to_owned()));
    }
    assert_eq!(
        RIGHT_CLICK_PASTES,
        "Copy the selection; paste when nothing is selected"
    );
    assert_eq!(RIGHT_CLICK_COPIES, "Copy the selection");
}

#[test]
fn plugin_commands_follow_the_built_ins_and_question_mark_still_finds_the_keys() {
    let plugins = [
        PluginCommand {
            plugin: "testkit".to_owned(),
            name: "go".to_owned(),
            aliases: vec!["g".to_owned()],
            args: vec![wisp_protocol::plugin::CommandArg {
                name: "speed".to_owned(),
                values: vec![],
            }],
            summary: "Go \u{202e}now".to_owned(),
            between_turns: None,
            short: true,
        },
        PluginCommand {
            plugin: "other".to_owned(),
            name: "stop".to_owned(),
            aliases: vec![],
            args: vec![],
            summary: "Stop it".to_owned(),
            between_turns: None,
            short: false,
        },
    ];
    let with = sections(unicode(true, false), &plugins);
    let without = sections(unicode(true, false), &[]);
    assert_eq!(with.len(), without.len() + 1);
    assert_eq!(with[0], without[0]);
    assert_eq!(with[1].title, copy::HELP_PLUGINS);
    assert_eq!(with[1].lead, None);
    let rows: Vec<(&str, &str, &str)> = with[1]
        .rows
        .iter()
        .map(|r| (r.keys.as_str(), r.args.as_str(), r.text.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            ("/go, /g", " [speed]", "Go \u{202e}now"),
            ("/other:stop", "", "Stop it"),
        ]
    );
    assert_eq!(with[first_keys(&plugins)].title, copy::KEYS_WRITING);
    assert_eq!(first_keys(&[]), FIRST_KEYS);
}

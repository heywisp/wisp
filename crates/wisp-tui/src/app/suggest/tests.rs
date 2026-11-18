use wisp_protocol::{CacheTtl, Effort, valid_model_id};
use wisp_render::chrome::Endpoint;

use super::*;
use crate::app::commands::{Arg, Busy, CONTEXT_WORDS, CommandId, InChat, Source};
use crate::app::fixture::{endpoint_facts, facts, with_levels};

fn names(listing: &Listing) -> Vec<String> {
    listing.commands.iter().map(Entry::key).collect()
}

fn ctrl(c: char) -> Key {
    Key::ctrl(c)
}

fn plain(code: KeyCode) -> Key {
    Key::plain(code)
}

fn shown(word: &str) -> Suggest {
    let mut suggest = Suggest::default();
    suggest.edited(Some(word));
    suggest
}

#[test]
fn the_list_opens_only_on_a_typed_slash_word_with_the_cursor_at_the_end() {
    let cases: &[(&str, bool, bool, Option<&str>)] = &[
        ("/", false, true, Some("")),
        ("/e", false, true, Some("e")),
        ("/Mo", false, true, Some("Mo")),
        ("/hepl", false, true, Some("hepl")),
        ("/end-point2", false, true, Some("end-point2")),
        ("/e", false, false, None),
        ("/e", true, true, None),
        (" /e", false, true, None),
        ("/model ", false, true, None),
        ("/model x", false, true, None),
        ("/e\nmore", false, true, None),
        ("/e\r", false, true, None),
        ("/2x", false, true, None),
        ("/--", false, true, None),
        ("/etc/hosts", false, true, None),
        ("see /model", false, true, None),
        ("", false, true, None),
    ];
    for (text, attachments, at_end, want) in cases {
        assert_eq!(
            command_word(text, *attachments, *at_end),
            *want,
            "{text:?} attachments={attachments} at_end={at_end}"
        );
    }
}

#[test]
fn it_shows_after_an_edit_and_not_after_a_recall_or_a_card() {
    let mut suggest = Suggest::default();
    assert!(!suggest.shows("e"));
    suggest.edited(Some("e"));
    assert!(suggest.shows("e"));
    suggest.quiet();
    assert!(!suggest.shows("e"));
    suggest.edited(Some("e"));
    assert!(suggest.shows("e"));
}

#[test]
fn esc_closes_it_until_the_word_changes() {
    let mut suggest = shown("e");
    suggest.dismiss("e");
    assert!(!suggest.shows("e"));
    suggest.edited(Some("e"));
    assert!(!suggest.shows("e"));
    suggest.edited(Some("en"));
    assert!(suggest.shows("en"));
    suggest.edited(Some("e"));
    assert!(suggest.shows("e"));
}

#[test]
fn rows_match_by_prefix_with_an_exact_match_first_in_registry_order() {
    let suggest = shown("");
    let all = suggest.listing("");
    assert_eq!(
        names(&all),
        COMMANDS.iter().map(|c| c.name).collect::<Vec<_>>()
    );
    assert_eq!(all.selected, Some(0));
    assert_eq!(all.highlighted().map(Entry::key).as_deref(), Some("model"));
    assert_eq!(names(&suggest.listing("e")), ["effort", "exit"]);
    assert_eq!(names(&suggest.listing("E")), ["effort", "exit"]);
    assert!(names(&suggest.listing("endpoint")).is_empty());
    assert_eq!(names(&suggest.listing("q")), ["exit"]);
    assert_eq!(names(&suggest.listing("QUIT")), ["exit"]);
    let none = suggest.listing("fo");
    assert!(none.commands.is_empty());
    assert_eq!(none.selected, None);
    assert_eq!(none.highlighted(), None);
}

#[test]
fn up_and_down_and_ctrl_p_and_ctrl_n_move_and_wrap() {
    let mut suggest = shown("c");
    let at = |s: &Suggest| s.listing("c").highlighted().map(Entry::key);
    assert_eq!(at(&suggest).as_deref(), Some("context"));
    for (key, want) in [
        (plain(KeyCode::Down), "compact"),
        (plain(KeyCode::Down), "config"),
        (plain(KeyCode::Down), "context"),
        (plain(KeyCode::Up), "config"),
        (ctrl('p'), "compact"),
        (ctrl('n'), "config"),
        (ctrl('n'), "context"),
    ] {
        let listing = suggest.listing("c");
        assert_eq!(suggest.key(key, &listing), Step::Stay, "{key:?}");
        assert_eq!(at(&suggest).as_deref(), Some(want), "{key:?}");
    }
}

#[test]
fn the_highlight_stays_while_the_word_does_and_resets_when_it_changes() {
    let mut suggest = shown("e");
    let listing = suggest.listing("e");
    suggest.key(plain(KeyCode::Down), &listing);
    assert_eq!(
        suggest
            .listing("e")
            .highlighted()
            .map(Entry::key)
            .as_deref(),
        Some("exit")
    );
    suggest.edited(Some("e"));
    assert_eq!(
        suggest
            .listing("e")
            .highlighted()
            .map(Entry::key)
            .as_deref(),
        Some("exit")
    );
    suggest.edited(Some("ex"));
    suggest.edited(Some("e"));
    assert_eq!(
        suggest
            .listing("e")
            .highlighted()
            .map(Entry::key)
            .as_deref(),
        Some("effort")
    );
}

#[test]
fn moving_on_no_match_does_nothing() {
    let mut suggest = shown("fo");
    let listing = suggest.listing("fo");
    for key in [plain(KeyCode::Up), plain(KeyCode::Down), ctrl('n')] {
        assert_eq!(suggest.key(key, &listing), Step::Stay);
        assert_eq!(suggest.listing("fo").selected, None);
    }
}

#[test]
fn tab_completes_adding_a_space_only_for_commands_with_arguments() {
    let mut suggest = shown("e");
    let listing = suggest.listing("e");
    assert_eq!(
        suggest.key(plain(KeyCode::Tab), &listing),
        Step::Complete("/effort ".to_owned())
    );
    let listing = suggest.listing("h");
    assert_eq!(
        suggest.key(plain(KeyCode::Tab), &listing),
        Step::Complete("/help".to_owned())
    );
    let listing = suggest.listing("q");
    assert_eq!(
        suggest.key(plain(KeyCode::Tab), &listing),
        Step::Complete("/exit".to_owned())
    );
    let listing = suggest.listing("fo");
    assert_eq!(suggest.key(plain(KeyCode::Tab), &listing), Step::Stay);
}

#[test]
fn enter_runs_the_highlighted_command_and_passes_with_no_match() {
    let mut suggest = shown("mo");
    let listing = suggest.listing("mo");
    assert_eq!(
        suggest.key(plain(KeyCode::Enter), &listing),
        Step::Run("/model".to_owned())
    );
    let listing = suggest.listing("e");
    suggest.key(plain(KeyCode::Down), &listing);
    let listing = suggest.listing("e");
    assert_eq!(
        suggest.key(plain(KeyCode::Enter), &listing),
        Step::Run("/exit".to_owned())
    );
    let listing = suggest.listing("hepl");
    assert_eq!(suggest.key(plain(KeyCode::Enter), &listing), Step::Pass);
}

#[test]
fn enter_on_a_name_or_alias_typed_in_full_sends_the_line_as_typed() {
    let mut suggest = shown("quit");
    for word in ["quit", "QUIT", "exit", "MODEL", "Help", "context"] {
        let listing = suggest.listing(word);
        assert!(listing.highlighted().is_some(), "{word}");
        assert_eq!(
            suggest.key(plain(KeyCode::Enter), &listing),
            Step::Pass,
            "{word}"
        );
    }
    let listing = suggest.listing("qui");
    assert_eq!(
        suggest.key(plain(KeyCode::Enter), &listing),
        Step::Run("/exit".to_owned())
    );
}

#[test]
fn reset_forgets_the_highlight_and_the_dismissal() {
    let mut suggest = shown("e");
    let listing = suggest.listing("e");
    suggest.key(plain(KeyCode::Down), &listing);
    suggest.dismiss("e");
    suggest.reset();
    assert!(!suggest.shows("e"));
    suggest.edited(Some("e"));
    assert!(suggest.shows("e"));
    assert_eq!(
        suggest
            .listing("e")
            .highlighted()
            .map(Entry::key)
            .as_deref(),
        Some("effort")
    );
}

#[test]
fn esc_closes_and_other_keys_go_to_the_composer() {
    let mut suggest = shown("e");
    let listing = suggest.listing("e");
    assert_eq!(suggest.key(plain(KeyCode::Esc), &listing), Step::Close);
    let shift_enter = Key {
        shift: true,
        ..plain(KeyCode::Enter)
    };
    for key in [
        shift_enter,
        Key::alt(KeyCode::Enter),
        ctrl('j'),
        ctrl('c'),
        plain(KeyCode::BackTab),
        plain(KeyCode::Char('x')),
        plain(KeyCode::Backspace),
        plain(KeyCode::Left),
        plain(KeyCode::PageUp),
    ] {
        assert_eq!(suggest.key(key, &listing), Step::Pass, "{key:?}");
    }
}

#[test]
fn rows_show_names_aliases_arguments_and_summaries_with_matched_letters_bold() {
    let suggest = shown("e");
    let view = suggest.listing("e").view(false);
    assert_eq!(view.selected, Some(0));
    let effort = &view.rows[0];
    assert_eq!(effort.name, "/effort");
    assert_eq!(&effort.name[effort.bold.clone()], "e");
    assert_eq!(effort.args, " [level]");
    assert_eq!(effort.summary, "Set how hard the model thinks");
    assert!(effort.enabled);
    let exit = &view.rows[1];
    assert_eq!(exit.name, "/exit, /quit");
    assert_eq!(&exit.name[exit.bold.clone()], "e");
    assert_eq!(exit.bold, 1..2);
    let quit = &suggest.listing("qu").view(false).rows[0];
    assert_eq!(&quit.name[quit.bold.clone()], "qu");
    assert_eq!(quit.bold, 8..10);
    let upper = &suggest.listing("EF").view(false).rows[0];
    assert_eq!(&upper.name[upper.bold.clone()], "ef");
    let all = suggest.listing("").view(false);
    assert!(all.rows.iter().all(|r| r.bold.is_empty()));
    let none = suggest.listing("fo").view(false);
    assert!(none.rows.is_empty());
    assert_eq!(none.empty, "no command matches /fo");
}

static WITH_COMPACT: &[Command] = &[
    Command {
        id: CommandId::Context,
        name: "compact",
        aliases: &[],
        args: &[Arg {
            name: "focus",
            source: Source::Free,
        }],
        summary: "Summarise to free up context",
        busy: Busy::BetweenTurns("/compact runs between turns. Send it after this one."),
        chat: InChat::Main,
    },
    Command {
        id: CommandId::Help,
        name: "help",
        aliases: &[],
        args: &[],
        summary: "Show commands and keys",
        busy: Busy::Runs,
        chat: InChat::Main,
    },
];

#[test]
fn a_between_turns_command_is_dimmed_while_busy_and_can_still_be_highlighted() {
    let mut suggest = Suggest::new(WITH_COMPACT);
    suggest.edited(Some(""));
    let listing = suggest.listing("");
    let busy = listing.view(true);
    assert_eq!(busy.rows[0].summary, copy::BETWEEN_TURNS);
    assert!(!busy.rows[0].enabled);
    assert!(busy.rows[1].enabled);
    assert_eq!(busy.rows[1].summary, "Show commands and keys");
    assert_eq!(busy.selected, Some(0));
    let idle = listing.view(false);
    assert_eq!(idle.rows[0].summary, "Summarise to free up context");
    assert!(idle.rows[0].enabled);
    assert_eq!(
        suggest.key(plain(KeyCode::Enter), &listing),
        Step::Run("/compact".to_owned())
    );
}

fn official() -> Facts {
    facts(Endpoint::Official, CacheTtl::OneHour)
}

fn cx<'a>(facts: &'a Facts, tracker: &'a Tracker, glyphs: GlyphSet) -> Context<'a> {
    Context {
        facts,
        tracker,
        at: Duration::ZERO,
        glyphs,
        sections: vec![],
    }
}

fn opened(suggest: &Suggest, line: &str, facts: &Facts) -> Values {
    let tracker = Tracker::new(facts);
    match suggest.open(line, &cx(facts, &tracker, GlyphSet::Unicode)) {
        Some(Open::Values(values)) => values,
        other => panic!("{line:?} opened {other:?}"),
    }
}

fn menu_of(values: &Values) -> &Menu {
    match &values.view {
        ValuesView::Menu(menu) => menu,
        ValuesView::List(list) => panic!("a list, not a menu: {list:?}"),
    }
}

fn list_of(values: &Values) -> &Suggestions {
    match &values.view {
        ValuesView::List(list) => list,
        ValuesView::Menu(menu) => panic!("a menu, not a list: {menu:?}"),
    }
}

fn texts(values: &Values) -> Vec<&str> {
    values.values.iter().map(|v| v.text.as_str()).collect()
}

fn highlighted(values: &Values) -> Option<&str> {
    values.highlighted().map(|v| v.text.as_str())
}

fn typing(suggest: &mut Suggest, line: &str, facts: &Facts) -> Values {
    suggest.edited(Some(line));
    opened(suggest, line, facts)
}

fn press(suggest: &mut Suggest, line: &str, facts: &Facts, key: Key) -> Step {
    let values = opened(suggest, line, facts);
    suggest.value_key(key, &values)
}

fn picker_menu(picker: &Picker, facts: &Facts) -> Menu {
    let tracker = Tracker::new(facts);
    let menu = picker.menu(facts, &tracker, Duration::ZERO, GlyphSet::Unicode);
    Menu {
        selected: None,
        ..menu
    }
}

#[test]
fn the_argument_stage_starts_after_a_known_command_and_one_space() {
    let model = &COMMANDS[0];
    let context = COMMANDS
        .iter()
        .find(|c| c.id == CommandId::Context)
        .unwrap();
    let cases: &[(&str, Option<Stage>)] = &[
        ("", Some(Stage::Commands(""))),
        ("mo", Some(Stage::Commands("mo"))),
        (
            "model ",
            Some(Stage::Values {
                command: Entry::BuiltIn(model),
                word: "",
            }),
        ),
        (
            "Model son",
            Some(Stage::Values {
                command: Entry::BuiltIn(model),
                word: "son",
            }),
        ),
        (
            "context a",
            Some(Stage::Values {
                command: Entry::BuiltIn(context),
                word: "a",
            }),
        ),
        ("context all extra", None),
        ("model  son", None),
        ("help me", None),
        ("exit now", None),
        ("hepl me", Some(Stage::Unknown("hepl"))),
        ("he me", Some(Stage::Unknown("he"))),
        ("2x y", None),
        ("etc/hosts is wrong", None),
        ("etc/hosts", None),
    ];
    for (line, want) in cases {
        assert_eq!(&stage(COMMANDS, &[], line), want, "{line:?}");
    }
    assert_eq!(typed_line("/model x", false, true), Some("model x"));
    assert_eq!(typed_line("/model x", false, false), None);
    assert_eq!(typed_line("/model x", true, true), None);
    assert_eq!(typed_line("/model x\ny", false, true), None);
    assert_eq!(typed_line(" /model x", false, true), None);
}

#[test]
fn an_unknown_name_followed_by_words_lists_no_command_and_takes_no_tab() {
    let f = official();
    let t = Tracker::new(&f);
    let mut suggest = shown("he me");
    for (line, word) in [("he me", "he"), ("hepl me", "hepl")] {
        let Some(Open::Commands(listing)) = suggest.open(line, &cx(&f, &t, GlyphSet::Unicode))
        else {
            panic!("{line:?}");
        };
        assert!(listing.commands.is_empty(), "{line:?}");
        assert_eq!(
            listing.view(false).empty,
            format!("no command matches /{word}")
        );
        assert_eq!(suggest.key(plain(KeyCode::Tab), &listing), Step::Stay);
        assert_eq!(suggest.key(plain(KeyCode::Enter), &listing), Step::Pass);
    }
}

#[test]
fn model_rows_are_the_model_pickers_rows_for_the_same_query() {
    let f = official();
    let suggest = Suggest::default();
    for query in ["", "opus", "SON", "claude-x9", "5-5"] {
        let values = opened(&suggest, &format!("model {query}"), &f);
        let mut typed = Picker::models();
        typed.paste(query);
        assert_eq!(
            Menu {
                selected: None,
                ..menu_of(&values).clone()
            },
            picker_menu(&typed, &f),
            "{query:?}"
        );
        assert_eq!(
            values.values,
            typed.values(&f, &Tracker::new(&f)),
            "{query:?}"
        );
        assert!(!values.closed);
    }
    let values = opened(&suggest, "model ", &f);
    assert_eq!(
        texts(&values),
        [
            "",
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "add"
        ]
    );
    assert!(!values.values[0].enabled);
    assert!(!values.values[4].enabled);
}

#[test]
fn a_free_text_model_id_is_its_own_row_and_runs_as_typed() {
    let f = official();
    let mut suggest = Suggest::default();
    let values = typing(&mut suggest, "model claude-x9", &f);
    assert!(valid_model_id("claude-x9"));
    assert_eq!(texts(&values), ["claude-x9"]);
    assert_eq!(menu_of(&values).choices[0].detail, copy::FREE_TEXT);
    assert_eq!(highlighted(&values), Some("claude-x9"));
    assert_eq!(
        suggest.value_key(plain(KeyCode::Enter), &values),
        Step::Run("/model claude-x9".to_owned())
    );
    let values = typing(&mut suggest, "model bad!id", &f);
    assert_eq!(texts(&values), ["bad!id"]);
    assert!(!values.values[0].enabled);
    assert_eq!(highlighted(&values), None);
    assert_eq!(
        suggest.value_key(plain(KeyCode::Enter), &values),
        Step::Pass
    );
    assert_eq!(suggest.value_key(plain(KeyCode::Tab), &values), Step::Stay);
}

#[test]
fn enter_never_guesses_a_model_and_takes_one_only_when_chosen_or_exact() {
    let f = official();
    let mut suggest = Suggest::default();
    let values = typing(&mut suggest, "model sonnet", &f);
    assert_eq!(
        texts(&values),
        ["", "claude-sonnet-5-5", "claude-sonnet-4-6"]
    );
    assert_eq!(values.selected, None);
    assert_eq!(menu_of(&values).selected, None);
    assert_eq!(
        suggest.value_key(plain(KeyCode::Enter), &values),
        Step::Pass
    );
    assert_eq!(
        press(&mut suggest, "model sonnet", &f, plain(KeyCode::Down)),
        Step::Stay
    );
    let values = opened(&suggest, "model sonnet", &f);
    assert_eq!(highlighted(&values), Some("claude-sonnet-5-5"));
    assert_eq!(menu_of(&values).selected, Some(1));
    assert_eq!(
        suggest.value_key(plain(KeyCode::Enter), &values),
        Step::Run("/model claude-sonnet-5-5".to_owned())
    );
    let values = typing(&mut suggest, "model claude-sonnet-5-5", &f);
    assert_eq!(highlighted(&values), Some("claude-sonnet-5-5"));
    assert_eq!(
        suggest.value_key(plain(KeyCode::Enter), &values),
        Step::Run("/model claude-sonnet-5-5".to_owned())
    );
    let values = typing(&mut suggest, "model sonnet-5-5", &f);
    assert_eq!(highlighted(&values), Some("claude-sonnet-5-5"));
    assert_eq!(
        suggest.value_key(plain(KeyCode::Enter), &values),
        Step::Run("/model claude-sonnet-5-5".to_owned())
    );
    for line in [
        "model CLAUDE-OPUS-5-5",
        "model OPUS-5-5",
        "model opus-4-5",
        "model sonnet-5",
    ] {
        let values = typing(&mut suggest, line, &f);
        assert!(!values.values.is_empty(), "{line:?}");
        assert_eq!(highlighted(&values), None, "{line:?}");
        assert_eq!(
            suggest.value_key(plain(KeyCode::Enter), &values),
            Step::Pass,
            "{line:?}"
        );
    }
    let values = typing(&mut suggest, "model ", &f);
    assert_eq!(highlighted(&values), None);
    assert_eq!(
        suggest.value_key(plain(KeyCode::Enter), &values),
        Step::Pass
    );
}

#[test]
fn moving_skips_dimmed_models_and_wraps_and_tab_completes_the_first_enabled() {
    let f = official();
    let mut suggest = Suggest::default();
    let values = typing(&mut suggest, "model opus", &f);
    assert_eq!(
        texts(&values),
        ["", "claude-opus-5-5", "claude-opus-4-5", "claude-opus-4-8"]
    );
    assert!(!values.values[2].enabled);
    assert_eq!(
        suggest.value_key(plain(KeyCode::Tab), &values),
        Step::Complete("/model claude-opus-5-5".to_owned())
    );
    for (key, want) in [
        (plain(KeyCode::Down), "claude-opus-5-5"),
        (plain(KeyCode::Down), "claude-opus-4-8"),
        (ctrl('n'), "claude-opus-5-5"),
        (plain(KeyCode::Up), "claude-opus-4-8"),
        (ctrl('p'), "claude-opus-5-5"),
    ] {
        press(&mut suggest, "model opus", &f, key);
        assert_eq!(
            highlighted(&opened(&suggest, "model opus", &f)),
            Some(want),
            "{key:?}"
        );
    }
    let values = typing(&mut suggest, "model claude-opus-4-5", &f);
    assert_eq!(highlighted(&values), None, "a dimmed model is never taken");
    suggest.edited(Some("model opus"));
    press(&mut suggest, "model opus", &f, plain(KeyCode::Up));
    assert_eq!(
        highlighted(&opened(&suggest, "model opus", &f)),
        Some("claude-opus-4-8")
    );
    suggest.edited(Some("model opu"));
    assert_eq!(highlighted(&opened(&suggest, "model opus", &f)), None);
}

#[test]
fn effort_rows_are_every_level_and_the_only_prefix_match_is_taken() {
    let f = official();
    let mut suggest = Suggest::default();
    let values = typing(&mut suggest, "effort x", &f);
    assert_eq!(texts(&values), ["low", "medium", "high", "xhigh", "max"]);
    assert!(values.closed);
    let effort = Picker::efforts("claude-opus-5-5".to_owned(), false, None);
    assert_eq!(
        Menu {
            selected: None,
            ..menu_of(&values).clone()
        },
        picker_menu(&effort, &f)
    );
    assert_eq!(highlighted(&values), Some("xhigh"));
    assert_eq!(menu_of(&values).selected, Some(3));
    assert_eq!(
        suggest.value_key(plain(KeyCode::Enter), &values),
        Step::Run("/effort xhigh".to_owned())
    );
    let values = typing(&mut suggest, "Effort X", &f);
    assert_eq!(
        suggest.value_key(plain(KeyCode::Enter), &values),
        Step::Run("/Effort xhigh".to_owned())
    );
    let values = typing(&mut suggest, "effort m", &f);
    assert_eq!(highlighted(&values), None);
    assert_eq!(
        suggest.value_key(plain(KeyCode::Enter), &values),
        Step::Pass
    );
    assert_eq!(
        suggest.value_key(plain(KeyCode::Tab), &values),
        Step::Complete("/effort medium".to_owned())
    );
    press(&mut suggest, "effort m", &f, plain(KeyCode::Up));
    assert_eq!(highlighted(&opened(&suggest, "effort m", &f)), Some("max"));
    suggest.reset();
    suggest.edited(Some("effort m"));
    press(&mut suggest, "effort m", &f, plain(KeyCode::Down));
    assert_eq!(
        highlighted(&opened(&suggest, "effort m", &f)),
        Some("medium")
    );
    let values = typing(&mut suggest, "effort zz", &f);
    assert_eq!(suggest.value_key(plain(KeyCode::Tab), &values), Step::Stay);
    for line in ["effort ", "effort zz"] {
        let values = typing(&mut suggest, line, &f);
        assert_eq!(highlighted(&values), None, "{line:?}");
        assert_eq!(
            suggest.value_key(plain(KeyCode::Enter), &values),
            Step::Pass,
            "{line:?}"
        );
    }
}

#[test]
fn a_model_with_no_effort_levels_shows_one_muted_note() {
    let mut f = official();
    with_levels(&mut f, "claude-opus-5-5", &[], Effort::High);
    let mut suggest = Suggest::default();
    let values = typing(&mut suggest, "effort h", &f);
    assert!(values.values.is_empty());
    let list = list_of(&values);
    assert!(list.rows.is_empty());
    assert_eq!(list.empty, copy::no_effort_levels("opus-5-5"));
    for key in [
        plain(KeyCode::Tab),
        plain(KeyCode::Down),
        plain(KeyCode::Up),
    ] {
        assert_eq!(suggest.value_key(key, &values), Step::Stay, "{key:?}");
    }
    assert_eq!(
        suggest.value_key(plain(KeyCode::Enter), &values),
        Step::Pass
    );
}

#[test]
fn endpoint_rows_match_ignoring_case_and_never_guess() {
    let f = endpoint_facts();
    let mut suggest = Suggest::default();
    let values = typing(&mut suggest, "endpoint MAN", &f);
    assert_eq!(texts(&values), ["manifest"]);
    let mut typed = Picker::endpoints();
    typed.paste("MAN");
    assert_eq!(
        Menu {
            selected: None,
            ..menu_of(&values).clone()
        },
        picker_menu(&typed, &f)
    );
    assert_eq!(highlighted(&values), None);
    assert_eq!(
        suggest.value_key(plain(KeyCode::Enter), &values),
        Step::Pass
    );
    press(&mut suggest, "endpoint MAN", &f, plain(KeyCode::Down));
    let values = opened(&suggest, "endpoint MAN", &f);
    assert_eq!(
        suggest.value_key(plain(KeyCode::Enter), &values),
        Step::Run("/endpoint manifest".to_owned())
    );
    let values = typing(&mut suggest, "endpoint ", &f);
    assert_eq!(
        texts(&values),
        ["direct", "gateway", "manifest", "work", "add"]
    );
    assert!(!values.values[3].enabled);
    let values = typing(&mut suggest, "endpoint work", &f);
    assert_eq!(highlighted(&values), None);
    let values = typing(&mut suggest, "endpoint gateway", &f);
    assert_eq!(highlighted(&values), Some("gateway"));
}

#[test]
fn context_words_come_from_the_registry_and_an_empty_word_runs_as_typed() {
    let f = official();
    let mut suggest = Suggest::default();
    let values = typing(&mut suggest, "context ", &f);
    let words: Vec<&str> = CONTEXT_WORDS.iter().map(|(w, _)| *w).collect();
    assert_eq!(texts(&values), words);
    let list = list_of(&values);
    assert_eq!(
        list.rows
            .iter()
            .map(|r| (r.name.as_str(), r.summary.as_str()))
            .collect::<Vec<_>>(),
        CONTEXT_WORDS.to_vec()
    );
    assert!(
        list.rows
            .iter()
            .all(|r| r.bold.is_empty() && r.args.is_empty())
    );
    assert_eq!(list.selected, None);
    assert_eq!(
        suggest.value_key(plain(KeyCode::Enter), &values),
        Step::Pass
    );
    assert_eq!(
        suggest.value_key(plain(KeyCode::Tab), &values),
        Step::Complete(format!("/context {}", words[0]))
    );
    for word in &words {
        let values = typing(&mut suggest, &format!("context {word}"), &f);
        assert_eq!(highlighted(&values), Some(*word));
        assert_eq!(
            suggest.value_key(plain(KeyCode::Enter), &values),
            Step::Run(format!("/context {word}"))
        );
    }
    let values = typing(&mut suggest, "context A", &f);
    assert_eq!(highlighted(&values), Some("all"));
    let all = list_of(&values)
        .rows
        .iter()
        .find(|r| r.name == "all")
        .unwrap();
    assert_eq!(&all.name[all.bold.clone()], "a");
    let values = typing(&mut suggest, "context zz", &f);
    assert_eq!(highlighted(&values), None);
    assert_eq!(suggest.value_key(plain(KeyCode::Tab), &values), Step::Stay);
    assert_eq!(
        suggest.value_key(plain(KeyCode::Enter), &values),
        Step::Pass
    );
}

#[test]
fn a_free_argument_shows_its_name_and_takes_no_value() {
    let f = official();
    let t = Tracker::new(&f);
    let suggest = Suggest::new(WITH_COMPACT);
    for (glyphs, want) in [
        (GlyphSet::Unicode, "focus (optional) \u{b7} enter run"),
        (GlyphSet::Ascii, "focus (optional) - enter run"),
    ] {
        let Some(Open::Values(values)) = suggest.open("compact ", &cx(&f, &t, glyphs)) else {
            panic!("no values");
        };
        assert!(values.values.is_empty());
        assert_eq!(list_of(&values).empty, want);
        let mut suggest = suggest.clone();
        assert_eq!(suggest.value_key(plain(KeyCode::Tab), &values), Step::Stay);
        assert_eq!(
            suggest.value_key(plain(KeyCode::Enter), &values),
            Step::Pass
        );
    }
    let Some(Open::Values(values)) =
        Suggest::default().open("logout ", &cx(&f, &t, GlyphSet::Unicode))
    else {
        panic!("no values for logout");
    };
    assert!(values.values.is_empty());
    assert_eq!(list_of(&values).empty, "name (optional) \u{b7} enter run");
}

#[test]
fn setting_names_match_a_prefix_of_the_name_or_any_part_of_the_label() {
    let f = official();
    let mut suggest = Suggest::default();
    let values = typing(&mut suggest, "config comp", &f);
    assert_eq!(
        texts(&values),
        [
            "context.auto_compact",
            "compact_at",
            "context.background_compaction",
            "context.strategy"
        ]
    );
    assert!(!values.closed);
    let list = list_of(&values);
    assert_eq!(list.rows[0].summary, "Compact automatically");
    assert_eq!(list.rows[0].bold, 0..0);
    assert_eq!(list.rows[1].summary, "Compact at");
    assert_eq!(list.rows[1].bold, 0..4);
    let values = typing(&mut suggest, "config lifetime", &f);
    assert_eq!(texts(&values), ["cache_ttl"]);
    assert_eq!(list_of(&values).rows[0].bold, 0..0);
    let values = typing(&mut suggest, "config display.", &f);
    assert!(texts(&values).iter().all(|t| t.starts_with("display.")));
    assert_eq!(texts(&values).len(), 13);
}

#[test]
fn every_setting_on_the_screen_is_listed_in_screen_order() {
    let f = official();
    let mut suggest = Suggest::default();
    let values = typing(&mut suggest, "config ", &f);
    let want: Vec<&str> = crate::app::config_screen::named_settings()
        .map(|spec| spec.name)
        .collect();
    assert_eq!(texts(&values), want);
    assert_eq!(highlighted(&values), None);
    for name in ["context_window", "pricing"] {
        assert!(texts(&values).contains(&name));
        let exact = typing(&mut suggest, &format!("config {name}"), &f);
        assert_eq!(highlighted(&exact), Some(name));
    }
}

#[test]
fn a_word_no_setting_matches_says_so_and_enter_runs_it_as_typed() {
    let f = official();
    let mut suggest = Suggest::default();
    let values = typing(&mut suggest, "config zzz", &f);
    assert!(values.values.is_empty());
    assert_eq!(list_of(&values).empty, "no setting matches zzz");
    assert_eq!(
        suggest.value_key(plain(KeyCode::Enter), &values),
        Step::Pass
    );
}

#[test]
fn the_exact_name_is_highlighted_and_a_label_match_only_when_chosen() {
    let f = official();
    let mut suggest = Suggest::default();
    let values = typing(&mut suggest, "config effort", &f);
    assert_eq!(highlighted(&values), Some("effort"));
    let values = typing(&mut suggest, "config cache", &f);
    assert_eq!(highlighted(&values), None);
}

#[test]
fn values_keep_the_command_as_typed_and_esc_closes() {
    let f = official();
    let mut suggest = Suggest::default();
    let values = typing(&mut suggest, "EFFORT hi", &f);
    assert_eq!(values.with("high"), "/EFFORT high");
    assert_eq!(suggest.value_key(plain(KeyCode::Esc), &values), Step::Close);
    for key in [
        plain(KeyCode::Char('x')),
        plain(KeyCode::Backspace),
        ctrl('c'),
    ] {
        assert_eq!(suggest.value_key(key, &values), Step::Pass, "{key:?}");
    }
}

#[test]
fn sidebar_completes_open_and_every_section_that_is_on_with_its_title() {
    let f = official();
    let tracker = Tracker::new(&f);
    let mut context = cx(&f, &tracker, GlyphSet::Unicode);
    context.sections = vec![
        ("agent-viz/topology".to_owned(), "Agents".to_owned()),
        ("demo/notes".to_owned(), "Notes".to_owned()),
    ];
    let open = |line: &str| match Suggest::default().open(line, &context) {
        Some(Open::Values(values)) => values,
        other => panic!("{line:?} opened {other:?}"),
    };
    let all = open("sidebar ");
    assert_eq!(texts(&all), ["open", "agent-viz/topology", "demo/notes"]);
    let summaries: Vec<&str> = list_of(&all)
        .rows
        .iter()
        .map(|r| r.summary.as_str())
        .collect();
    assert_eq!(summaries[1..], ["Agents", "Notes"]);
    assert_eq!(open("sidebar de").selected, Some(2));
    assert_eq!(open("sidebar o").selected, Some(0));
}

fn plugin_command(name: &str, short: bool, values: &[&str], busy: Option<&str>) -> PluginCommand {
    PluginCommand {
        plugin: "testkit".to_owned(),
        name: name.to_owned(),
        aliases: vec![format!("{name}-x")],
        args: if values.is_empty() && busy.is_none() {
            vec![]
        } else {
            vec![wisp_protocol::plugin::CommandArg {
                name: "speed".to_owned(),
                values: values.iter().map(|v| (*v).to_owned()).collect(),
            }]
        },
        summary: format!("Run {name}"),
        between_turns: busy.map(str::to_owned),
        short,
    }
}

fn with_plugins(word: &str) -> Suggest {
    let mut suggest = shown(word);
    suggest.set_plugins(vec![
        plugin_command("go", true, &["fast", "slow"], None),
        plugin_command("wait", false, &[], Some("/wait runs between turns")),
    ]);
    suggest
}

#[test]
fn plugin_rows_follow_the_built_ins_in_the_form_being_typed() {
    let suggest = with_plugins("");
    let all = suggest.listing("").view(true);
    let rows: Vec<(&str, &str, &str, bool)> = all.rows[COMMANDS.len()..]
        .iter()
        .map(|r| {
            (
                r.name.as_str(),
                r.args.as_str(),
                r.summary.as_str(),
                r.enabled,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("/go, /go-x", " [speed]", "Run go", true),
            (
                "/testkit:wait, /testkit:wait-x",
                " [speed]",
                copy::BETWEEN_TURNS,
                false
            ),
        ]
    );
    let full = &suggest.listing("testkit:g").view(false).rows[0];
    assert_eq!(full.name, "/testkit:go, /testkit:go-x");
    assert_eq!(&full.name[full.bold.clone()], "testkit:g");
    let short = &suggest.listing("go-").view(false).rows[0];
    assert_eq!(&short.name[short.bold.clone()], "go-");
    assert_eq!(short.bold, 6..9);
}

#[test]
fn tab_and_enter_complete_and_run_a_plugin_command_in_the_typed_form() {
    let mut suggest = with_plugins("g");
    let listing = suggest.listing("g");
    assert_eq!(
        suggest.key(plain(KeyCode::Tab), &listing),
        Step::Complete("/go ".to_owned())
    );
    let listing = suggest.listing("testkit:g");
    assert_eq!(
        suggest.key(plain(KeyCode::Enter), &listing),
        Step::Run("/testkit:go".to_owned())
    );
    let listing = suggest.listing("wa");
    assert!(listing.commands.is_empty());
    let listing = suggest.listing("testkit:w");
    assert_eq!(
        suggest.key(plain(KeyCode::Tab), &listing),
        Step::Complete("/testkit:wait ".to_owned())
    );
    let listing = suggest.listing("go-x");
    assert_eq!(suggest.key(plain(KeyCode::Enter), &listing), Step::Pass);
}

#[test]
fn a_plugin_arguments_values_are_offered_and_a_free_one_shows_its_name() {
    let f = official();
    let mut suggest = with_plugins("");
    let values = typing(&mut suggest, "go f", &f);
    assert_eq!(texts(&values), ["fast", "slow"]);
    assert_eq!(highlighted(&values), Some("fast"));
    assert_eq!(values.with("fast"), "/go fast");
    let values = typing(&mut suggest, "testkit:go-x ", &f);
    assert_eq!(texts(&values), ["fast", "slow"]);
    let values = typing(&mut suggest, "testkit:wait ", &f);
    assert!(values.values.is_empty());
    assert_eq!(list_of(&values).empty, "speed (optional) \u{b7} enter run");
    assert_eq!(suggest.stage("wait x"), Some(Stage::Unknown("wait")));
}

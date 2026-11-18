use wisp_protocol::settings::ENABLED_KEY;

use super::plugins::{opened_with, sent};
use super::*;
use crate::shared::switch_placed;

const COST: &str = "cache rebuild \u{2248}20k tokens";

pub(super) fn search_plugin(
    on: bool,
    enabled: bool,
    why: Option<&str>,
) -> wisp_protocol::PluginEntry {
    let mut tool = catalog_tool("web_search", None, true);
    (tool.on, tool.native, tool.unavailable) = (on, true, why.map(str::to_owned));
    let mut plugin = catalog_plugin("web-search", vec![tool, catalog_tool("run", None, true)]);
    plugin.enabled = enabled;
    plugin
}

fn saved_at(place: Place, name: &str, value: &str) -> SavedValue {
    let spec = find(name).unwrap();
    let path = SitePath::new(place, spec);
    SavedValue::of(path, spec.parse(value).unwrap(), Some(4))
}

fn with_list(plugin: Plugin, switch: Option<bool>, list: Option<(&str, &str)>) -> Screen {
    let placed = switch.map(|on| switch_placed("web-search", ENABLED_KEY, on));
    let (mut settings, mut saved) = (defaults(), Vec::new());
    if let Some((held, current)) = list {
        put(&mut settings, "tools", Layer::Session, current, "/tools");
        saved.push(saved_global("tools", held));
    }
    let plugin = search_plugin(plugin.0, plugin.1, plugin.2);
    let mut screen = opened_with(plugin, placed.into_iter().collect(), settings, saved);
    screen.selected = Some(Spot::Search);
    screen
}

type Plugin = (bool, bool, Option<&'static str>);

fn row(plugin: Plugin, switch: Option<bool>, list: Option<&str>) -> Screen {
    with_list(plugin, switch, list.map(|list| (list, list)))
}

fn keys_text(screen: &Screen, quote: &Quote) -> String {
    hints_text(&screen.detail(quote).unwrap().keys)
}

fn lines(act: Act) -> Vec<String> {
    let line = |(path, value): (String, Option<SettingValue>)| match value {
        Some(value) => format!("{path}={}", value.text()),
        None => format!("{path} unset"),
    };
    sent(act).into_iter().map(line).collect()
}

const ON: &str = "plugins.web-search.enabled=on";
const OFF: &str = "plugins.web-search.enabled=off";

#[test]
fn the_web_search_row_reads_the_switch_what_waits_a_reason_or_this_chat() {
    let (on, off) = ((true, true, None), (false, false, None));
    let reason = |why| (true, true, Some(why));
    let cases: Vec<(&str, Plugin, _, _, &str, &str, &[&str])> = vec![
        ("on", on, None, None, "on", "enter turn off", &[OFF]),
        ("off", off, Some(false), None, "off", "enter turn on", &[ON]),
        (
            "turned off, waiting",
            on,
            Some(false),
            None,
            "off \u{b7} pending \u{b7} next turn",
            "enter turn on",
            &[ON],
        ),
        (
            "turned on, waiting",
            off,
            Some(true),
            None,
            "on \u{b7} pending \u{b7} next turn",
            "enter turn off",
            &[OFF],
        ),
        (
            "put back in a saved list, waiting",
            (false, true, None),
            None,
            Some("run,web_search"),
            "on \u{b7} pending \u{b7} next turn",
            "enter turn off",
            &[OFF],
        ),
        (
            "left out of a saved list",
            (false, true, None),
            None,
            Some("run"),
            "off",
            "enter turn on",
            &[ON, "tools=run,web_search"],
        ),
        (
            "left out of a saved list and switched off",
            off,
            Some(false),
            Some("run"),
            "off",
            "enter turn on",
            &[ON, "tools=run,web_search"],
        ),
        (
            "kept by a resumed chat while saved off",
            (true, false, None),
            Some(false),
            None,
            "on",
            "",
            &[],
        ),
        (
            "no search on the model",
            reason("not on this model"),
            None,
            None,
            "not on this model",
            "",
            &[],
        ),
        (
            "refused by the account",
            reason("not on this account"),
            Some(true),
            None,
            "not on this account",
            "",
            &[],
        ),
        (
            "began without it",
            (false, true, Some("not in this chat")),
            None,
            None,
            "not in this chat",
            "",
            &[],
        ),
    ];
    for (case, plugin, switch, list, value, keys, sends) in cases {
        let mut screen = row(plugin, switch, list);
        assert_eq!(value_of(&screen, "Web search"), value, "{case}");
        assert_eq!(keys_text(&screen, &Quote::default()), keys, "{case}");
        let act = press(&mut screen, KeyCode::Enter);
        match sends {
            [] => assert_eq!(act, Act::Stay, "{case}"),
            _ => assert_eq!(lines(act), sends, "{case}"),
        }
    }
    let mut screen = row(on, None, None);
    screen.open_page(Page::Plugins);
    screen.selected = Some(Spot::Plugin("web-search".to_owned()));
    let tool = |word: &str| {
        let layers = screen.detail(&Quote::default()).unwrap().layers;
        layers.into_iter().find(|l| l.word == word).unwrap().value
    };
    assert_eq!(tool("web_search"), "on \u{b7} switch in Web search");
    assert_eq!(tool("run"), "on \u{b7} switch in Tools");
}

#[test]
fn a_tools_flag_that_leaves_search_out_is_this_chat_not_a_put_back_waiting() {
    let screen = with_list((false, true, None), None, Some(("run,web_search", "run")));
    assert_eq!(value_of(&screen, "Web search"), "off");
    let pane = screen.detail(&Quote::default()).unwrap();
    assert_eq!(pane.notes, ["This chat: off. New chats: on."]);
    assert_eq!(hints_text(&pane.keys), "");
}

#[test]
fn the_row_says_what_this_chat_and_new_chats_get_and_what_it_is() {
    let pane = row((true, false, None), Some(false), None)
        .detail(&Quote::default())
        .unwrap();
    assert_eq!(pane.notes, ["This chat: on. New chats: off."]);
    assert_eq!(pane.title, "lets the model look things up on the web");
}

#[test]
fn d_removes_the_saved_switch_that_stops_search_and_the_row_keeps_its_reason() {
    let endpoint = || "manifest".to_owned();
    let by_account = Place::Endpoint(endpoint());
    let by_pair = Place::EndpointModel {
        endpoint: endpoint(),
        model: MODEL.to_owned(),
    };
    let cases = [
        (
            "off for this account",
            by_account,
            "native_web_search",
            "off",
            "account",
        ),
        (
            "not on this model",
            Place::Model(MODEL.to_owned()),
            "web_search",
            "none",
            "model",
        ),
        ("not on this model", by_pair, "web_search", "none", "model"),
    ];
    for (why, place, name, held, whose) in cases {
        let saved = saved_at(place, name, held);
        let path = saved.path.to_string();
        let plugin = search_plugin(true, true, Some(why));
        let mut screen = opened_with(plugin, Vec::new(), defaults(), vec![saved]);
        screen.selected = Some(Spot::Search);
        let pane = screen.detail(&Quote::default()).unwrap();
        assert_eq!(pane.notes, [format!("Saved for this {whose}: off.")]);
        assert_eq!(
            hints_text(&pane.keys),
            "d back to Wisp's own \u{b7} new chats"
        );
        let act = press(&mut screen, KeyCode::Char('d'));
        let Act::SaveMany(many) = &act else {
            panic!("{path}");
        };
        screen.sent_many(RequestId(7), many);
        assert_eq!(lines(act), [format!("{path} unset")]);
        assert_eq!(
            value_of(&screen, "Web search"),
            format!("{why}{SEP}{SAVING}")
        );
        assert!(screen.saved(&changed(RequestId(7), name, When::NextSession), false));
        assert_eq!(
            value_of(&screen, "Web search"),
            format!("{why} \u{b7} new chats")
        );
    }
    let nothing = |plugin: Plugin, saved: Vec<SavedValue>| {
        let plugin = search_plugin(plugin.0, plugin.1, plugin.2);
        let mut screen = opened_with(plugin, Vec::new(), defaults(), saved);
        screen.selected = Some(Spot::Search);
        (
            press(&mut screen, KeyCode::Char('d')),
            screen.detail(&Quote::default()).unwrap().notes,
        )
    };
    let held = saved_at(Place::Endpoint(endpoint()), "native_web_search", "off");
    assert_eq!(
        nothing((true, true, Some("off for this account")), Vec::new()),
        (Act::Stay, Vec::new())
    );
    assert_eq!(
        nothing((true, true, None), vec![held]),
        (Act::Stay, Vec::new())
    );
}

#[test]
fn the_cost_follows_enter_and_the_reasoning_line_stands_in_for_it() {
    let quote = Quote {
        tools: COST.to_owned(),
        ..Quote::default()
    };
    let screen = row((true, true, None), None, None);
    assert_eq!(
        keys_text(&screen, &quote),
        format!("enter turn off \u{b7} {COST}")
    );
    let mut plugin = screen.clone();
    plugin.open_page(Page::Plugins);
    plugin.selected = Some(Spot::Plugin("web-search".to_owned()));
    for screen in [&screen, &plugin] {
        let pane = screen.detail(&quote).unwrap();
        for width in [40, 80] {
            let lines = wisp_render::chrome::config_screen::detail_block(
                &pane,
                wisp_render::markdown::Layout::new(width, GlyphSet::Unicode),
            );
            let text: Vec<_> = lines.iter().map(crate::testing::line_text).collect();
            assert!(
                text[if width == 40 { 1 } else { 2 }].contains(COST),
                "{width}: {text:#?}"
            );
            assert!(text[2].contains("enter turn off"));
            if width == 40 && matches!(screen.selected_spot(), Some(Spot::Plugin(_))) {
                assert_eq!(text[2].trim(), "enter turn off · → its settings");
            }
        }
    }
    let held = Quote {
        drops: true,
        ..quote.clone()
    };
    let pane = screen.detail(&held).unwrap();
    assert_eq!(hints_text(&pane.keys), "enter turn off");
    assert_eq!(
        pane.notes,
        ["switching web search on or off drops earlier reasoning"]
    );
    let mut waiting = row((true, true, None), Some(false), None);
    assert_eq!(keys_text(&waiting, &quote), "enter turn on");
    waiting.open_page(Page::Plugins);
    waiting.selected = Some(Spot::Plugin("web-search".to_owned()));
    assert!(!keys_text(&waiting, &quote).contains(COST));
    let reason = row((true, true, Some("not on this model")), None, None);
    assert_eq!(keys_text(&reason, &quote), "");
}

#[test]
fn the_tools_box_lists_the_search_tool_without_a_tick_and_keeps_it_as_the_list_had_it() {
    let cases = [
        (None, vec!["run", "web_search"]),
        (Some("run,web_search"), vec!["run", "web_search"]),
        (Some("run"), vec!["run"]),
    ];
    for (list, kept) in cases {
        let mut screen = with_list((true, true, None), None, list.map(|list| (list, list)));
        screen.selected = Some(Spot::Setting("tools"));
        assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
        press(&mut screen, KeyCode::Right);
        assert!(!screen.edit().unwrap().toggles());
        press(&mut screen, KeyCode::Char(' '));
        let pane = screen.detail(&Quote::default()).unwrap();
        let Some(EditView::Choices { choices, .. }) = &pane.edit else {
            panic!("{:?}", pane.edit);
        };
        assert_eq!(choices, &["[x] run", "web_search"], "{list:?}");
        assert_eq!(
            pane.notes.first().map(String::as_str),
            Some("Switched by the Web search row.")
        );
        assert_eq!(pane.keys, hints(FIXED_CHECK_KEYS));
        let Some(SettingValue::List(names)) =
            saved(press(&mut screen, KeyCode::Enter)).change.value
        else {
            panic!("{list:?}");
        };
        assert_eq!(names, kept, "{list:?}");
    }
}

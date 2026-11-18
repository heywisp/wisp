use wisp_protocol::settings::{ENABLED_KEY, INSTALLED_KEY, PlacedValue, SettingValue};

use super::*;
use crate::shared::{defined_plugin, switch_placed};

pub(super) fn declared_plugin(id: &str) -> wisp_protocol::PluginEntry {
    let plugin = defined_plugin(id);
    wisp_protocol::Catalog {
        plugins: vec![plugin.clone()],
    }
    .declare();
    plugin
}

pub(super) fn opened_with(
    plugin: wisp_protocol::PluginEntry,
    placed: Vec<PlacedValue>,
    settings: Vec<ResolvedSetting>,
    saved: Vec<SavedValue>,
) -> Screen {
    let mut screen = Screen::opening(RequestId(1), "");
    let mut shot = snapshot(settings, saved, ok());
    shot.plugins = vec![plugin];
    shot.placed = placed;
    assert!(screen.answered(&shot, &Display::default()));
    screen
}

fn opened(plugin: wisp_protocol::PluginEntry, placed: Vec<PlacedValue>) -> Screen {
    let mut screen = opened_with(plugin, placed, defaults(), Vec::new());
    screen.open_page(Page::Plugins);
    screen
}

fn row_of(screen: &Screen, id: &str) -> wisp_render::chrome::config_screen::ListRow {
    let (entries, map) = screen.entries();
    let spot = Spot::Plugin(id.to_owned());
    let at = map.iter().position(|s| s.as_ref() == Some(&spot)).unwrap();
    let Entry::Row(row) = &entries[at] else {
        panic!("{:?}", entries[at]);
    };
    row.clone()
}

fn keys_of(screen: &mut Screen, id: &str) -> Vec<String> {
    screen.selected = Some(Spot::Plugin(id.to_owned()));
    screen
        .detail(&Quote::default())
        .unwrap()
        .keys
        .iter()
        .map(|h| format!("{} {}", h.key(), h.does()))
        .collect()
}

fn entry(installed: bool, enabled: bool) -> wisp_protocol::PluginEntry {
    let mut plugin = catalog_plugin("runner", vec![catalog_tool("run", None, true)]);
    plugin.installed = installed;
    plugin.enabled = enabled;
    plugin
}

type Case = (
    &'static str,
    wisp_protocol::PluginEntry,
    Vec<PlacedValue>,
    &'static str,
    Vec<&'static str>,
    (&'static str, bool),
);

#[test]
fn a_plugin_rows_state_comes_from_its_placed_switches_and_the_catalog() {
    let on = || vec![switch_placed("runner", ENABLED_KEY, true)];
    let cases: Vec<Case> = vec![
        (
            "on",
            entry(true, true),
            Vec::new(),
            "on",
            vec!["enter turn off", "→ its settings", "d remove"],
            (ENABLED_KEY, false),
        ),
        (
            "switched off, not applied yet",
            entry(true, true),
            vec![switch_placed("runner", ENABLED_KEY, false)],
            "off \u{b7} pending \u{b7} next turn",
            vec!["enter turn on", "→ its settings", "d remove"],
            (ENABLED_KEY, true),
        ),
        (
            "off and applied",
            entry(true, false),
            vec![switch_placed("runner", ENABLED_KEY, false)],
            "off",
            vec!["enter turn on", "→ its settings", "d remove"],
            (ENABLED_KEY, true),
        ),
        (
            "switched on, not applied yet",
            entry(true, false),
            on(),
            "on \u{b7} pending \u{b7} next turn",
            vec!["enter turn off", "→ its settings", "d remove"],
            (ENABLED_KEY, false),
        ),
        (
            "removed in this session",
            entry(true, true),
            vec![switch_placed("runner", INSTALLED_KEY, false)],
            "removed \u{b7} pending \u{b7} next turn",
            vec!["enter add back", "→ its settings"],
            (INSTALLED_KEY, true),
        ),
        (
            "removed when the session started",
            entry(false, false),
            vec![switch_placed("runner", INSTALLED_KEY, false)],
            "removed",
            vec!["enter add back", "→ its settings"],
            (INSTALLED_KEY, true),
        ),
        (
            "added back, not applied yet",
            entry(false, false),
            vec![switch_placed("runner", INSTALLED_KEY, true)],
            "on \u{b7} pending \u{b7} next turn",
            vec!["enter turn off", "→ its settings", "d remove"],
            (ENABLED_KEY, false),
        ),
    ];
    for (case, plugin, switches, state, keys, sent) in cases {
        let mut screen = opened(plugin, switches);
        assert_eq!(row_of(&screen, "runner").value, state, "{case}");
        assert_eq!(keys_of(&mut screen, "runner"), keys, "{case}");
        let detail = screen.detail(&Quote::default()).unwrap();
        assert_eq!(
            (
                detail.layers[0].word.as_str(),
                detail.layers[0].value.as_str()
            ),
            ("State", state),
            "{case}"
        );
        assert_eq!(enter_sends(&mut screen), sent_change(sent), "{case}");
    }
}

fn sent_change((key, on): (&str, bool)) -> (String, Option<SettingValue>) {
    (
        SitePath::plugin_switch("runner", key).unwrap().to_string(),
        Some(SettingValue::Bool(on)),
    )
}

pub(super) fn sent(act: Act) -> Vec<(String, Option<SettingValue>)> {
    let Act::SaveMany(many) = act else {
        panic!("{act:?}");
    };
    let line = |c: &SettingChange| (c.target.to_string(), c.value.clone());
    many.changes.iter().map(line).collect()
}

fn enter_sends(screen: &mut Screen) -> (String, Option<SettingValue>) {
    screen.selected = Some(Spot::Plugin("runner".to_owned()));
    let [change] = sent(press(screen, KeyCode::Enter))
        .try_into()
        .unwrap_or_else(|c| panic!("{c:?}"));
    change
}

#[test]
fn a_plugin_left_out_at_start_shows_no_mark() {
    let mut plugin = entry(true, true);
    plugin.problems = vec!["it failed to describe its tools".to_owned()];
    let screen = opened(plugin, Vec::new());
    assert_eq!(row_of(&screen, "runner").value, "on");
}

#[test]
fn the_pending_mark_goes_with_the_applying_catalog_and_none_shows_after_off_then_on() {
    let mut screen = opened(
        entry(true, true),
        vec![switch_placed("runner", ENABLED_KEY, false)],
    );
    assert_eq!(
        row_of(&screen, "runner").value,
        "off \u{b7} pending \u{b7} next turn"
    );
    let catalog = wisp_protocol::Catalog {
        plugins: vec![entry(true, false)],
    };
    assert!(screen.catalog_changed(&catalog));
    assert_eq!(row_of(&screen, "runner").value, "off");
    let screen = opened(entry(true, true), on_again());
    assert_eq!(row_of(&screen, "runner").value, "on");
}

fn on_again() -> Vec<PlacedValue> {
    vec![switch_placed("runner", ENABLED_KEY, true)]
}

#[test]
fn enter_on_a_removed_row_adds_it_back() {
    let mut screen = opened(
        entry(true, true),
        vec![switch_placed("runner", INSTALLED_KEY, false)],
    );
    screen.selected = Some(Spot::Plugin("runner".to_owned()));
    let Act::SaveMany(many) = press(&mut screen, KeyCode::Enter) else {
        panic!("enter saved nothing");
    };
    let [change] = many.changes.as_slice() else {
        panic!("{:?}", many.changes);
    };
    let SettingTarget::Path(path) = &change.target else {
        panic!("{change:?}");
    };
    assert_eq!(path.to_string(), "plugins.runner.installed");
    assert_eq!(change.value, Some(SettingValue::Bool(true)));
    assert_eq!(press(&mut screen, KeyCode::Char('d')), Act::Stay);
    assert!(screen.asking().is_none());
}

#[test]
fn d_asks_before_removing() {
    let mut screen = opened(entry(true, true), Vec::new());
    screen.selected = Some(Spot::Plugin("runner".to_owned()));
    assert_eq!(press(&mut screen, KeyCode::Char('d')), Act::Stay);
    let ask = screen.asking().unwrap();
    assert_eq!(ask.question, "Remove runner?");
}

#[test]
fn a_tool_of_an_off_or_removed_plugin_is_locked_in_the_tools_checklist_with_a_note() {
    for (key, short, note) in [
        (
            ENABLED_KEY,
            "runner is off",
            "run is a tool of runner, which is off; turn it on in Plugins",
        ),
        (
            INSTALLED_KEY,
            "runner is removed",
            "run is a tool of runner, which is removed; add it back in Plugins",
        ),
    ] {
        let mut shot = snapshot(defaults(), Vec::new(), ok());
        shot.plugins = vec![entry(true, true)];
        shot.placed = vec![switch_placed("runner", key, false)];
        let checks = crate::app::config_screen::edit::tool_checks(&shot);
        let run = checks.iter().find(|c| c.name == "run").unwrap();
        assert_eq!(run.locked.as_deref(), Some(note), "{key}");
        assert_eq!(run.note.as_deref(), Some(short), "{key}");
    }
}

#[test]
fn removing_a_plugin_hides_the_setting_rows_it_owns() {
    let defined = declared_plugin("removed-row");
    for (id, setting) in [
        ("removed-row", "plugins.removed-row.minutes"),
        ("time", "time"),
        ("compaction", "context.verbatim_user_tokens"),
        ("instructions", "instructions.source"),
        ("instructions", "instructions.warn_tokens"),
    ] {
        let shown = |placed: Vec<PlacedValue>| {
            let mut screen = Screen::opening(RequestId(1), "");
            let mut shot = snapshot(defaults(), Vec::new(), ok());
            shot.plugins = vec![if id == defined.id {
                defined.clone()
            } else {
                catalog_plugin(id, Vec::new())
            }];
            shot.placed = placed;
            assert!(screen.answered(&shot, &Display::default()));
            screen.open_page(Page::setting(setting));
            let (_, map) = screen.entries();
            map.iter()
                .any(|s| matches!(s, Some(Spot::Setting(name)) if *name == setting))
        };
        assert!(shown(Vec::new()), "{setting}");
        assert!(
            !shown(vec![switch_placed(id, INSTALLED_KEY, false)]),
            "{setting}"
        );
        assert!(
            shown(vec![switch_placed(id, ENABLED_KEY, false)]),
            "{setting}"
        );
    }
}

#[test]
fn a_plugins_page_lists_its_settings_before_its_parts() {
    let mut plugin = catalog_plugin("instructions", Vec::new());
    plugin.settings = vec![wisp_protocol::plugin::Setting::Name("time".into())];
    plugin.hooks = vec![hook_entry(
        "instructions/read",
        "before-turn",
        0,
        None,
        None,
    )];
    let mut screen = opened(plugin, Vec::new());
    screen.open_page(Page::Plugin("instructions".into()));
    assert_eq!(
        screen.shown(),
        [
            Spot::Setting(names::INSTRUCTIONS_SOURCE),
            Spot::Setting(names::INSTRUCTIONS_WARN_TOKENS),
            Spot::Hook("instructions/read".into()),
        ]
    );
    screen.spaced(30, 5);
    assert_eq!(screen.entries().0[3], Entry::Gap);
    screen.spaced(16, 4);
    assert!(!screen.entries().0.contains(&Entry::Gap));
    assert!(
        screen
            .page_spots(&Page::Group(Section::Chat))
            .contains(&Spot::Setting(names::INSTRUCTIONS_SOURCE))
    );
}

#[test]
fn a_setting_a_plugin_defines_opens_on_its_page_and_is_filtered_under_plugins() {
    let plugin = declared_plugin("defined-page");
    let name = "plugins.defined-page.minutes";
    let spec = find(name).unwrap();
    let mut shot = snapshot(defaults(), Vec::new(), ok());
    shot.plugins = vec![plugin];
    let mut screen = Screen::opening(RequestId(1), name);
    assert!(screen.answered(&shot, &Display::default()));
    assert_eq!(screen.current, Page::Plugin("defined-page".into()));
    assert_eq!(screen.shown(), [Spot::Setting(spec.name)]);
    assert_eq!(screen.selected_spot(), Some(&Spot::Setting(spec.name)));
    assert!(
        !screen
            .page_spots(&Page::More)
            .contains(&Spot::Open(Page::Group(Section::Plugins)))
    );
    screen.filter = "Idle after".into();
    assert_eq!(screen.shown(), [Spot::Setting(spec.name)]);
    assert_eq!(screen.entries().0[0], Entry::Heading("Plugins".into()));
    screen.filter.clear();
    assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
    assert!(screen.edit().is_some());
    assert_eq!(screen.key(Key::ctrl('u')), Act::Stay);
    typed(&mut screen, "5");
    let save = saved(press(&mut screen, KeyCode::Enter));
    assert_eq!(save.change.target, named(name));
    assert_eq!(save.change.value, Some(SettingValue::Int(5)));
    assert_eq!(screen.current, Page::Plugin("defined-page".into()));
}

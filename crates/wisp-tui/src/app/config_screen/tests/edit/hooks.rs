use wisp_protocol::plugin::{HookDisabled, HookEntry};
use wisp_protocol::settings::{INSTALLED_KEY, Layer, PlacedValue, SettingValue};

use super::*;

fn hook_placed(plugin: &str, hook: &str, on: bool) -> PlacedValue {
    PlacedValue {
        path: SitePath::plugin_hook(plugin, hook).unwrap(),
        value: Some(SettingValue::Bool(on)),
        layer: Layer::Session,
    }
}

fn plugin_with(id: &str, hooks: Vec<HookEntry>) -> wisp_protocol::PluginEntry {
    let mut plugin = catalog_plugin(id, Vec::new());
    plugin.hooks = hooks;
    plugin
}

fn opened_with(
    plugins: Vec<wisp_protocol::PluginEntry>,
    placed: Vec<PlacedValue>,
    rows: Vec<wisp_protocol::settings::ResolvedSetting>,
) -> Screen {
    let mut screen = Screen::opening(RequestId(1), "");
    let mut shot = snapshot(rows, Vec::new(), ok());
    shot.plugins = plugins;
    shot.placed = placed;
    assert!(screen.answered(&shot, &Display::default()));
    screen.open_page(Page::Plugin(shot.plugins[0].id.clone()));
    screen
}

fn hook_row(screen: &Screen, name: &str) -> String {
    let (entries, map) = screen.entries();
    let spot = Spot::Hook(name.to_owned());
    let at = map
        .iter()
        .position(|s| s.as_ref() == Some(&spot))
        .unwrap_or_else(|| panic!("no row {name}"));
    let Entry::Row(row) = &entries[at] else {
        panic!("{:?}", entries[at]);
    };
    row.value.clone()
}

fn shell(disabled: Option<HookDisabled>) -> wisp_protocol::PluginEntry {
    plugin_with(
        "shell",
        vec![hook_entry(
            "shell/processes",
            "before-turn",
            0,
            None,
            disabled,
        )],
    )
}

#[test]
fn a_switchable_hook_rows_mark_is_derived_from_placed_and_the_catalog() {
    let off = || vec![hook_placed("shell", "processes", false)];
    let mut screen = opened_with(vec![shell(None)], off(), defaults());
    assert_eq!(
        hook_row(&screen, "shell/processes"),
        "off \u{b7} pending \u{b7} next turn"
    );
    let catalog = wisp_protocol::Catalog {
        plugins: vec![shell(Some(HookDisabled::SwitchedOff))],
    };
    assert!(screen.catalog_changed(&catalog));
    assert_eq!(hook_row(&screen, "shell/processes"), "off");
    let reopened = opened_with(vec![shell(None)], off(), defaults());
    assert_eq!(
        hook_row(&reopened, "shell/processes"),
        "off \u{b7} pending \u{b7} next turn"
    );
    let back_on = opened_with(
        vec![shell(None)],
        vec![hook_placed("shell", "processes", true)],
        defaults(),
    );
    assert_eq!(hook_row(&back_on, "shell/processes"), "on");
    let waiting_on = opened_with(
        vec![shell(Some(HookDisabled::SwitchedOff))],
        Vec::new(),
        defaults(),
    );
    assert_eq!(
        hook_row(&waiting_on, "shell/processes"),
        "on \u{b7} pending \u{b7} next turn"
    );
}

#[test]
fn a_hook_row_names_why_it_is_off_and_a_left_out_one_takes_no_key() {
    let cases = [
        (Some(HookDisabled::PluginOff), "off (plugin off)"),
        (
            Some(HookDisabled::Cycle {
                cycle: vec!["aa/x".into(), "aa/x".into()],
            }),
            "off (in a cycle)",
        ),
        (
            Some(HookDisabled::Missing {
                requires: vec!["zz/\u{7}gone".into()],
            }),
            "off (requires zz/^Ggone)",
        ),
        (Some(HookDisabled::LeftOut), "off (left out at start)"),
        (None, "on"),
    ];
    for (disabled, state) in cases {
        let left_out = disabled == Some(HookDisabled::LeftOut);
        let mut screen = opened_with(vec![shell(disabled)], Vec::new(), defaults());
        assert_eq!(hook_row(&screen, "shell/processes"), state);
        screen.selected = Some(Spot::Hook("shell/processes".to_owned()));
        let detail = screen.detail(&Quote::default()).unwrap();
        assert_eq!(detail.layers[0].value, state);
        if left_out {
            assert!(detail.keys.is_empty(), "{:?}", detail.keys);
            assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
        } else {
            assert_eq!(detail.notes[0], "before-turn \u{b7} 1 of 1");
        }
    }
}

#[test]
fn enter_on_a_switchable_hook_saves_its_own_switch_the_other_way() {
    for (placed, on) in [
        (Vec::new(), false),
        (vec![hook_placed("shell", "processes", false)], true),
    ] {
        let mut screen = opened_with(vec![shell(None)], placed, defaults());
        screen.selected = Some(Spot::Hook("shell/processes".to_owned()));
        let Act::SaveMany(many) = press(&mut screen, KeyCode::Enter) else {
            panic!("enter saved nothing");
        };
        let [change] = many.changes.as_slice() else {
            panic!("{:?}", many.changes);
        };
        let SettingTarget::Path(path) = &change.target else {
            panic!("{change:?}");
        };
        assert_eq!(path.to_string(), "plugins.shell.hooks.processes");
        assert_eq!(change.value, Some(SettingValue::Bool(on)));
    }
}

fn time_plugin(disabled: Option<HookDisabled>) -> wisp_protocol::PluginEntry {
    plugin_with(
        "time",
        vec![hook_entry(
            "time/sent",
            "before-turn",
            0,
            Some("time"),
            disabled,
        )],
    )
}

#[test]
fn a_hook_with_a_switch_follows_it_and_enter_selects_that_settings_row() {
    let mut screen = opened_with(vec![time_plugin(None)], Vec::new(), defaults());
    assert_eq!(hook_row(&screen, "time/sent"), "on \u{b7} follows time");
    screen.selected = Some(Spot::Hook("time/sent".to_owned()));
    let detail = screen.detail(&Quote::default()).unwrap();
    let keys: Vec<String> = detail
        .keys
        .iter()
        .map(|h| format!("{} {}", h.key(), h.does()))
        .collect();
    assert_eq!(keys, ["enter its setting"]);
    assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
    assert_eq!(screen.selected_spot(), Some(&Spot::Setting("time")));
    assert!(screen.filter.is_empty());
    assert_eq!(screen.current, Page::Plugin("time".into()));
    assert!(screen.shown().contains(&Spot::Setting("time")));
    screen.filter = "time/sent".into();
    screen.selected = Some(Spot::Hook("time/sent".into()));
    assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
    assert_eq!(screen.selected_spot(), Some(&Spot::Setting("time")));
    assert_eq!(screen.current, Page::Plugin("time".into()));
    assert!(screen.filter.is_empty());
}

#[test]
fn a_switch_rows_mark_compares_the_owning_setting_with_the_entry() {
    let mut rows = defaults();
    put(&mut rows, "time", Layer::Session, "off", "session");
    let pending = opened_with(vec![time_plugin(None)], Vec::new(), rows.clone());
    assert_eq!(
        hook_row(&pending, "time/sent"),
        "off \u{b7} pending \u{b7} next turn \u{b7} follows time"
    );
    let applied = opened_with(
        vec![time_plugin(Some(HookDisabled::SwitchedOff))],
        Vec::new(),
        rows,
    );
    assert_eq!(hook_row(&applied, "time/sent"), "off \u{b7} follows time");
    let back_on = opened_with(
        vec![time_plugin(Some(HookDisabled::SwitchedOff))],
        Vec::new(),
        defaults(),
    );
    assert_eq!(
        hook_row(&back_on, "time/sent"),
        "on \u{b7} pending \u{b7} next turn \u{b7} follows time"
    );
}

#[test]
fn enter_on_a_removed_plugins_switch_row_hints_the_add_back() {
    let plugin = plugin_with(
        "self-check",
        vec![hook_entry(
            "self-check/review",
            "before-turn",
            0,
            Some("self_check.before_end"),
            Some(HookDisabled::PluginOff),
        )],
    );
    let removed = PlacedValue {
        path: SitePath::plugin_switch("self-check", INSTALLED_KEY).unwrap(),
        value: Some(SettingValue::Bool(false)),
        layer: Layer::Session,
    };
    let mut screen = opened_with(vec![plugin], vec![removed], defaults());
    screen.selected = Some(Spot::Hook("self-check/review".to_owned()));
    assert_eq!(
        press(&mut screen, KeyCode::Enter),
        Act::Note("add self-check back in Plugins".to_owned())
    );
}

#[test]
fn no_hook_row_with_a_switch_writes_its_own_hook_key() {
    let plugins = vec![
        time_plugin(None),
        shell(None),
        plugin_with(
            "self-check",
            vec![hook_entry(
                "self-check/review",
                "before-turn",
                0,
                Some("self_check.before_end"),
                None,
            )],
        ),
    ];
    let mut screen = opened_with(plugins.clone(), Vec::new(), defaults());
    let mut written = Vec::new();
    for hook in plugins.iter().flat_map(|p| &p.hooks) {
        screen.open_page(Page::Plugin(
            hook.hook.split_once('/').unwrap().0.to_owned(),
        ));
        screen.selected = Some(Spot::Hook(hook.hook.clone()));
        if let Act::SaveMany(many) = press(&mut screen, KeyCode::Enter) {
            for change in &many.changes {
                if let SettingTarget::Path(path) = &change.target {
                    written.push(path.to_string());
                }
            }
        }
        screen.clear_filter();
    }
    assert_eq!(written, ["plugins.shell.hooks.processes"]);
}

#[test]
fn a_hostile_hook_name_point_and_switch_are_cleaned_in_the_row_and_its_detail() {
    let bell = '\u{7}';
    let name = format!("shell/pro{bell}c");
    let mut entry = hook_entry(&name, "before-turn", 0, Some(&format!("sw{bell}x")), None);
    entry.point = format!("be{bell}fore");
    let mut screen = opened_with(
        vec![plugin_with("shell", vec![entry])],
        Vec::new(),
        defaults(),
    );
    let (entries, map) = screen.entries();
    let at = map
        .iter()
        .position(|s| s.as_ref() == Some(&Spot::Hook(name.clone())))
        .unwrap();
    let Entry::Row(row) = &entries[at] else {
        panic!("{:?}", entries[at]);
    };
    assert_eq!(row.label, "pro^Gc");
    assert_eq!(row.value, "on \u{b7} follows sw^Gx");
    screen.selected = Some(Spot::Hook(name));
    let detail = screen.detail(&Quote::default()).unwrap();
    assert_eq!(detail.lead, "pro^Gc");
    assert_eq!(detail.title, "the hook \"be^Gfore\"");
    assert_eq!(detail.notes[0], "be^Gfore \u{b7} 1 of 1");
    assert_eq!(detail.layers[0].value, "on \u{b7} follows sw^Gx");
}

use wisp_protocol::settings::{Applies, Form, Kind, Percent, Place, Site, WindowRoom};

use crate::app::config_screen::edit::CheckItem;
use wisp_protocol::Effort;
use wisp_render::chrome::KeyHint;
use wisp_render::chrome::Wins;
use wisp_render::chrome::config_screen::{Check, EditView};
use wisp_render::theme::glyphs::{GlyphSet, Glyphs};

use super::*;
use crate::events::{Changed, settings_changed};
use crate::shared::{catalog_tool, change, hook_entry, named};

const TOOLS: &[&str] = &["ask", "run", "search", "show", "view"];

fn on_the_keys_file_account(mut snap: SettingsSnapshot) -> SettingsSnapshot {
    snap.context.endpoint = wisp_protocol::CLAUDE_API.to_owned();
    snap.endpoints
        .push(wisp_protocol::settings::EndpointSummary {
            models: Vec::new(),
            mark: None,
            name: wisp_protocol::CLAUDE_API.to_owned(),
            provider: "anthropic".to_owned(),
            credential: None,
            official: Some(true),
            host: None,
            unavailable: None,
            priced: true,
            default: false,
            keys_file: true,
            expired: false,
            key_var: None,
            voice_model: None,
            complete: false,
            default_model: None,
        });
    snap
}

fn hints_text(hints: &[KeyHint]) -> String {
    let gap = format!(" {} ", Glyphs::of(GlyphSet::Unicode).separator);
    hints
        .iter()
        .map(KeyHint::text)
        .collect::<Vec<_>>()
        .join(&gap)
}

const CANARY: &str = "sk9canary4key7shaped2text";
const UNDERSCORED: &str = "sk_ant_canary4key7shaped2text";

fn defaults() -> Vec<ResolvedSetting> {
    placed()
        .map(|(_, spec)| spec)
        .chain([find("context_window").unwrap(), find("pricing").unwrap()])
        .map(|spec| setting(spec.name, spec.default_value(), Layer::Default, "default"))
        .collect()
}

fn put(rows: &mut [ResolvedSetting], name: &str, layer: Layer, value: &str, origin: &str) {
    let parsed = find(name).unwrap().parse(value).unwrap();
    for row in rows.iter_mut().filter(|r| r.name.spec().name == name) {
        if let Some(old) = row.value.take()
            && row.layer != Layer::Default
        {
            row.shadowed.insert(
                0,
                Shadowed {
                    layer: row.layer.clone(),
                    value: old,
                    origin: std::mem::take(&mut row.origin),
                },
            );
        }
        row.value = Some(parsed.clone());
        row.layer = layer.clone();
        row.origin = origin.to_owned();
    }
}

fn saved_global(name: &str, value: &str) -> SavedValue {
    let spec = find(name).unwrap();
    SavedValue::of(global(name), spec.parse(value).unwrap(), Some(2))
}

fn loaded_with(
    settings: Vec<ResolvedSetting>,
    saved: Vec<SavedValue>,
    file: FileState,
    on: &str,
) -> Screen {
    with_window(settings, saved, file, on, None)
}

fn with_window(
    settings: Vec<ResolvedSetting>,
    saved: Vec<SavedValue>,
    file: FileState,
    on: &str,
    window: Option<u64>,
) -> Screen {
    let mut shot = snapshot(settings, saved, file);
    shot.context.window = window;
    let mut screen = Screen::opening(RequestId(1), on);
    assert!(screen.answered(&shot, &Display::default()));
    screen
}

fn loaded(on: &str) -> Screen {
    loaded_with(defaults(), Vec::new(), ok(), on)
}

fn press(screen: &mut Screen, code: KeyCode) -> Act {
    screen.key(Key::plain(code))
}

fn typed(screen: &mut Screen, text: &str) {
    for c in text.chars() {
        assert_eq!(press(screen, KeyCode::Char(c)), Act::Stay);
    }
}

fn saved(act: Act) -> Save {
    match act {
        Act::Save(save) => *save,
        other => panic!("expected a save, got {other:?}"),
    }
}

fn field(screen: &Screen) -> Option<Field> {
    screen.edit().map(|e| e.field.clone())
}

fn changed(request: RequestId, name: &str, when: When) -> AgentEvent {
    settings_changed(Changed {
        answers: Some(request),
        endpoint: "manifest".to_owned(),
        model: MODEL.to_owned(),
        effort: Effort::High,
        changes: vec![change(named(name), None, Layer::Session, when)],
        ..Changed::default()
    })
}

fn value_of(screen: &Screen, label: &str) -> String {
    let (entries, _) = screen.entries();
    entries
        .iter()
        .find_map(|e| match e {
            Entry::Row(row) if row.label == label => Some(row.value.clone()),
            _ => None,
        })
        .unwrap()
}

fn context(window: Option<u64>) -> Context {
    Context {
        window,
        room: None,
        max_tokens: 64_000,
    }
}

#[test]
fn each_row_gets_the_widget_its_kind_gives_or_waits_for_a_later_change() {
    let toggles = [
        "claude_usage",
        "debug_cache",
        "refusal_fallback",
        "message_threads",
        "context.context_note",
        "self_check.before_end",
        "context.count_tokens",
        "context.auto_compact",
        "context.background_compaction",
        "log",
        "display.full_repaint",
        "display.mouse",
        "display.pictures",
        "display.verbose",
        "keep_warm",
    ];
    let rows = [
        "preload",
        "cache_ttl",
        "pricing",
        "time",
        "instructions.source",
        "instructions.extra",
        "keychain",
        "context.compact_effort",
        "display.ambiguous",
        "display.color",
        "display.copy_on_select",
        "display.motion",
        "display.sidebar",
        "display.theme",
    ];
    let texts = [
        "retry.budget_usd",
        "instructions.warn_tokens",
        "compact_at",
        "context.verbatim_user_tokens",
        "context.background_tokens",
        "context_window",
        "display.scroll_speed",
        "agents.max_running",
        "agents.max_tree_running",
        "agents.max_agents",
        "agents.max_depth",
        "agents.wake_when_stuck",
        "agents.warm_refreshes",
        "tool_output.max_age_days",
        "turned_off",
        "context.strategy",
        "voice.instructions",
        "voice.language",
    ];
    let pickers = [
        ("model", Pick::Model),
        ("effort", Pick::Effort),
        ("endpoint", Pick::Endpoint),
    ];
    let checklists: Vec<&str> = placed()
        .filter(|(_, spec)| {
            matches!(
                spec.kind,
                Kind::List(_) | Kind::ToolNames | Kind::SectionNames
            )
        })
        .map(|(_, spec)| spec.name)
        .collect();
    assert_eq!(checklists.len(), 3, "{checklists:?}");
    for (_, spec) in placed() {
        let picker = pickers.iter().find(|(name, _)| *name == spec.name);
        let want = if toggles.contains(&spec.name) {
            Some(Widget::Toggle)
        } else if rows.contains(&spec.name) {
            Some(Widget::Choice)
        } else if texts.contains(&spec.name) {
            Some(Widget::Text)
        } else if let Some((_, pick)) = picker {
            Some(Widget::Picker(*pick))
        } else if checklists.contains(&spec.name) {
            Some(Widget::Checklist)
        } else {
            None
        };
        assert_eq!(widget(spec), want, "{}", spec.name);
    }
}

#[test]
fn enter_on_a_switch_saves_the_other_value_at_once() {
    let mut screen = loaded("debug_cache");
    let save = saved(press(&mut screen, KeyCode::Enter));
    assert_eq!(save.name, "debug_cache");
    assert_eq!(
        save.change,
        SettingChange {
            target: named("debug_cache"),
            value: Some(SettingValue::Bool(true)),
        }
    );
    assert_eq!(save.told.was, "off");
    assert_eq!(save.told.wins, None);
    assert!(screen.edit().is_none());
}

#[test]
fn a_choice_row_moves_with_the_arrows_and_saves_on_enter() {
    let mut screen = loaded("cache_ttl");
    assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
    assert_eq!(field(&screen), Some(Field::Choice { at: 1 }));
    press(&mut screen, KeyCode::Right);
    assert_eq!(field(&screen), Some(Field::Choice { at: 1 }));
    press(&mut screen, KeyCode::Left);
    press(&mut screen, KeyCode::Left);
    assert_eq!(field(&screen), Some(Field::Choice { at: 0 }));
    let save = saved(press(&mut screen, KeyCode::Enter));
    assert_eq!(save.change.value, Some(text("5m")));
    assert!(screen.edit().is_none());
    press(&mut screen, KeyCode::Enter);
    press(&mut screen, KeyCode::Char('h'));
    assert_eq!(field(&screen), Some(Field::Choice { at: 0 }));
    assert_eq!(press(&mut screen, KeyCode::Esc), Act::Stay);
    assert!(screen.edit().is_none());
    press(&mut screen, KeyCode::Enter);
    assert_eq!(press(&mut screen, KeyCode::Char('q')), Act::Stay);
    assert!(screen.edit().is_none());
    assert_eq!(press(&mut screen, KeyCode::Esc), Act::Stay);
    assert_eq!(screen.current, Page::More);
    let mut preload = loaded("preload");
    assert_eq!(press(&mut preload, KeyCode::Enter), Act::Stay);
    assert_eq!(field(&preload), Some(Field::Choice { at: 0 }));
    press(&mut preload, KeyCode::Right);
    let save = saved(press(&mut preload, KeyCode::Enter));
    assert_eq!(
        save.change.value,
        Some(SettingValue::List(vec!["none".to_owned()]))
    );
}

#[test]
fn a_text_editor_starts_from_the_value_and_saves_only_a_valid_entry() {
    let mut screen = loaded("retry.budget_usd");
    press(&mut screen, KeyCode::Enter);
    assert_eq!(
        field(&screen),
        Some(Field::Text {
            text: "none".to_owned(),
            cursor: 4,
        })
    );
    assert_eq!(screen.key(Key::ctrl('u')), Act::Stay);
    typed(&mut screen, "abc");
    let quote = Quote::default();
    let pane = screen.detail(&quote).unwrap();
    let Some(EditView::Text { check, unit, .. }) = pane.edit else {
        panic!("{pane:?}");
    };
    assert_eq!(
        check,
        Check::Invalid("needs a number of dollars, such as 0.50, or none".to_owned())
    );
    assert_eq!(unit, "US dollars, or none");
    assert_eq!(pane.keys, hints(TEXT_KEYS));
    assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
    assert!(screen.edit().is_some());
    screen.key(Key::ctrl('c'));
    assert_eq!(
        field(&screen),
        Some(Field::Text {
            text: String::new(),
            cursor: 0,
        })
    );
    typed(&mut screen, "0.5");
    press(&mut screen, KeyCode::Home);
    press(&mut screen, KeyCode::Delete);
    typed(&mut screen, "$0");
    press(&mut screen, KeyCode::End);
    typed(&mut screen, "0");
    let save = saved(press(&mut screen, KeyCode::Enter));
    assert_eq!(
        save.change.value,
        Some(SettingValue::Usd { micros: 500_000 })
    );
    assert_eq!(save.told.was, "none");
    press(&mut screen, KeyCode::Enter);
    screen.key(Key::ctrl('c'));
    assert!(screen.edit().is_some());
    screen.key(Key::ctrl('c'));
    assert!(screen.edit().is_none());
}

#[test]
fn the_compaction_point_previews_as_the_kernel_works_it_out() {
    let spec = find("compact_at").unwrap();
    let preview_of = |text: &str, context: Context| match check(spec, text, context) {
        Check::Preview(words) => words,
        other => panic!("{other:?}"),
    };
    let million = context(Some(1_000_000));
    assert_eq!(preview_of("60%", million), "600,000 of 1,000,000");
    assert_eq!(
        preview_of("40%", context(Some(333_333))),
        "133,333 of 333,333",
        "share_point, as the kernel works it out"
    );
    assert_eq!(preview_of("95%", million), "850,000 of 1,000,000");
    assert_eq!(preview_of("900k", million), "850,000 of 1,000,000");
    assert_eq!(preview_of("60%", context(None)), "= 60% of the window");
    assert_eq!(check(spec, "  ", million), Check::Empty);
    let Check::Invalid(rule) = check(spec, CANARY, million) else {
        panic!();
    };
    assert!(!rule.contains(CANARY));
    let verbatim = find("context.verbatim_user_tokens").unwrap();
    assert_eq!(
        preview(verbatim, &SettingValue::Tokens(8_000), million),
        "= 8,000 tokens"
    );
    assert_eq!(verbatim.describe(Form::Unit), "tokens, 0 or more");
    assert_eq!(
        find("context_window").unwrap().describe(Form::Unit),
        "tokens, 40,000 or more"
    );
    assert_eq!(
        preview(
            spec,
            &SettingValue::Percent(Percent::new(60).unwrap()),
            million
        ),
        "600,000 of 1,000,000"
    );
}

#[test]
fn the_editor_preview_uses_the_snapshot_s_window_and_reserve() {
    let mut screen = with_window(defaults(), Vec::new(), ok(), "compact_at", Some(1_000_000));
    press(&mut screen, KeyCode::Enter);
    typed(&mut screen, "95%");
    let pane = screen.detail(&Quote::default()).unwrap();
    let Some(EditView::Text { check, .. }) = pane.edit else {
        panic!("{pane:?}");
    };
    assert_eq!(check, Check::Preview("850,000 of 1,000,000".to_owned()));
}

#[test]
fn d_resets_a_value_saved_here_or_set_this_session_and_names_what_wins_next() {
    let mut screen = loaded("time");
    assert_eq!(
        press(&mut screen, KeyCode::Char('d')),
        Act::Hint(NOTHING_TO_RESET)
    );
    let mut settings = defaults();
    put(&mut settings, "time", Layer::Config, "utc", "time");
    put(&mut settings, "time", Layer::Flag, "local", "--time");
    put(&mut settings, "time", Layer::Session, "off", "session");
    let mut screen = loaded_with(
        settings.clone(),
        vec![saved_global("time", "utc")],
        ok(),
        "time",
    );
    let pane = screen.detail(&Quote::default()).unwrap();
    assert_eq!(hints_text(&pane.keys), "enter choose \u{b7} d back to utc");
    let save = saved(press(&mut screen, KeyCode::Char('d')));
    assert!(!save.save);
    assert_eq!(
        save.change.value,
        Some(SettingValue::Text("utc".to_owned()))
    );
    assert_eq!(save.change.target, named("time"));
    assert_eq!(save.told.was, "off");
    assert_eq!(save.told.saved.as_deref(), Some("utc"));
    assert_eq!(save.told.wins, None);
    let mut settings = defaults();
    put(&mut settings, "log", Layer::Config, "off", "log");
    let mut screen = loaded_with(settings, vec![saved_global("log", "off")], ok(), "log");
    let save = saved(press(&mut screen, KeyCode::Char('d')));
    assert_eq!(
        save.told.wins,
        Some(Wins::NewSessions(THE_DEFAULT.to_owned()))
    );
}

#[test]
fn d_returns_a_chat_value_to_new_chats_without_changing_what_is_saved() {
    for flag in [false, true] {
        let mut rows = defaults();
        put(&mut rows, "time", Layer::Config, "utc", "time");
        if flag {
            put(&mut rows, "time", Layer::Flag, "local", "--time");
        }
        put(&mut rows, "time", Layer::Session, "off", "session");
        let mut screen = loaded_with(rows, vec![saved_global("time", "utc")], ok(), "time");
        let save = saved(press(&mut screen, KeyCode::Char('d')));
        assert!(!save.save);
        assert_eq!(save.change.target, named("time"));
        assert_eq!(
            save.change.value,
            flag.then(|| SettingValue::Text("utc".to_owned()))
        );
    }
    for (name, saved_value, flag, window) in [
        ("tools", None, "run", None),
        ("endpoint", None, "work", None),
        ("instructions.extra", None, "autonomous", None),
        ("display.theme", Some("auto"), "light", None),
        ("compact_at", None, "300000", Some(1_000_000)),
        ("compact_at", Some("400000"), "600000", Some(1_000_000)),
        ("compact_at", None, "300000", None),
    ] {
        let mut rows = defaults();
        if let Some(value) = saved_value {
            put(&mut rows, name, Layer::Config, value, name);
        }
        put(&mut rows, name, Layer::Flag, flag, "a start flag");
        let values = saved_value
            .map(|v| saved_global(name, v))
            .into_iter()
            .collect();
        let mut screen = with_window(rows, values, ok(), name, window);
        let row = screen.selected().unwrap();
        assert!(chat_only(row), "{name}");
        assert!(!can_reset(row), "{name}");
        assert_eq!(reset_value(row), None, "{name}");
        assert!(reset_key(row, &Quote::default()).is_none(), "{name}");
        if window.is_some() && saved_value.is_none() {
            assert_eq!(new_chats(row), "850,000");
        }
        if name == "compact_at" && window.is_none() {
            assert_eq!(
                screen.detail(&Quote::default()).unwrap().line,
                "This chat: 300,000."
            );
        }
        assert_eq!(press(&mut screen, KeyCode::Char('d')), Act::Stay, "{name}");
    }
    for (name, value) in [
        ("compact_at", "850000"),
        ("keychain", "off"),
        ("log", "off"),
    ] {
        let mut rows = defaults();
        put(&mut rows, name, Layer::Config, value, name);
        put(&mut rows, name, Layer::Flag, value, "a start flag");
        let mut screen = loaded_with(rows, vec![saved_global(name, value)], ok(), name);
        assert!(!chat_only(screen.selected().unwrap()), "{name}");
        assert!(can_reset(screen.selected().unwrap()), "{name}");
        let change = saved(press(&mut screen, KeyCode::Char('d')));
        assert!(change.save, "{name}");
        assert_eq!(change.change.value, None, "{name}");
    }
    let mut rows = defaults();
    put(&mut rows, "tools", Layer::Config, "run,view", "tools");
    put(&mut rows, "tools", Layer::Flag, "run", "--tools");
    let mut tools = loaded_with(rows, vec![saved_global("tools", "run,view")], ok(), "tools");
    let save = saved(press(&mut tools, KeyCode::Char('d')));
    assert!(!save.save);
    assert_eq!(
        save.change.value,
        Some(SettingValue::List(vec![
            "run".to_owned(),
            "view".to_owned()
        ]))
    );
    let mut instructions = shown("instructions.source", Layer::Flag, None, "none");
    let save = saved(press(&mut instructions, KeyCode::Char('d')));
    assert!(!save.save);
    assert_eq!(
        save.change.value,
        Some(SettingValue::Text("auto".to_owned()))
    );
}

#[test]
fn a_broken_file_says_so_again_on_enter_and_d() {
    let broken = FileState::Broken {
        line: Some(12),
        reason: "expected a value".to_owned(),
    };
    let mut screen = loaded_with(defaults(), Vec::new(), broken, "debug_cache");
    let said = Act::Hint(BROKEN);
    assert_eq!(press(&mut screen, KeyCode::Enter), said);
    assert_eq!(press(&mut screen, KeyCode::Char('d')), said);
}

#[test]
fn the_display_rows_change_here_with_their_widgets() {
    let mut theme = loaded("display.theme");
    assert_eq!(press(&mut theme, KeyCode::Enter), Act::Stay);
    assert!(theme.edit().is_some());
    let pane = theme.detail(&Quote::default()).unwrap();
    assert!(!hints_text(&pane.keys).starts_with("Change it with wisp config set"));
    let mut mouse = loaded("display.mouse");
    let Act::Save(save) = press(&mut mouse, KeyCode::Enter) else {
        panic!("a display switch saves on enter");
    };
    assert_eq!(save.change.value, Some(SettingValue::Bool(false)));
    let mut speed = loaded("display.scroll_speed");
    assert_eq!(press(&mut speed, KeyCode::Enter), Act::Stay);
    assert!(speed.edit().is_some());
}

#[test]
fn enter_on_model_and_effort_opens_their_picker_and_account_opens_its_page() {
    for (name, pick) in [("model", Pick::Model), ("effort", Pick::Effort)] {
        let mut screen = loaded(name);
        assert_eq!(
            press(&mut screen, KeyCode::Enter),
            Act::Pick(pick),
            "{name}"
        );
        assert!(screen.edit().is_none());
        let pane = screen.detail(&Quote::default()).unwrap();
        assert_eq!(
            pane.keys.first(),
            Some(&hint(CHOOSE_KEY)),
            "{:?}",
            pane.keys
        );
    }
    let mut screen = loaded("endpoint");
    assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
    assert_eq!(screen.current, Page::Accounts);
    assert!(screen.edit().is_none());
    let mut shot = on_the_keys_file_account(snapshot(defaults(), Vec::new(), ok()));
    screen.ask(RequestId(1));
    assert!(screen.answered(&shot, &Display::default()));
    press(&mut screen, KeyCode::Home);
    assert_eq!(
        press(&mut screen, KeyCode::Enter),
        Act::UseEndpoint(wisp_protocol::CLAUDE_API.to_owned())
    );
    shot.endpoints[0].default = true;
    screen.ask(RequestId(1));
    assert!(screen.answered(&shot, &Display::default()));
    assert!(
        !screen
            .detail(&Quote::default())
            .unwrap()
            .keys
            .contains(&hint(USE_KEY))
    );
    assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
}

#[test]
fn a_saving_or_locked_row_opens_no_picker_and_no_places() {
    let mut screen = loaded("model");
    screen.sent(RequestId(4), &["model"], true);
    assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
    assert_eq!(press(&mut screen, KeyCode::Char('e')), Act::Stay);
    let mut sited = loaded("compact_at");
    sited.sent(RequestId(5), &["compact_at"], true);
    assert_eq!(press(&mut sited, KeyCode::Char('e')), Act::Stay);
    assert!(sited.edit().is_none());
    let snap = on_the_keys_file_account(snapshot(defaults(), Vec::new(), ok()));
    let mut pricing = Screen::opening(RequestId(1), "pricing");
    assert!(pricing.answered(&snap, &Display::default()));
    assert_eq!(
        press(&mut pricing, KeyCode::Enter),
        Act::Hint(PRICING_PER_ENDPOINT)
    );
    let pane = pricing.detail(&Quote::default()).unwrap();
    assert_eq!(pane.keys, [KeyHint::note(PRICING_PER_ENDPOINT)]);
}

#[test]
fn a_global_setting_has_one_place_to_save() {
    let mut screen = loaded("cache_ttl");
    assert_eq!(press(&mut screen, KeyCode::Char('e')), Act::Stay);
    let pane = screen.detail(&Quote::default()).unwrap();
    assert!(!pane.keys.iter().any(|k| k.text().contains("where to save")));
}

fn with_tools(choices: &[&str], value: Option<&str>) -> Vec<ResolvedSetting> {
    let mut rows = defaults();
    for row in rows.iter_mut().filter(|r| r.name.spec().name == "tools") {
        row.choices = choices.iter().map(|c| (*c).to_owned()).collect();
        if let Some(value) = value {
            row.value = Some(find("tools").unwrap().parse(value).unwrap());
            row.layer = Layer::Config;
            row.origin = "tools".to_owned();
        }
    }
    rows
}

#[test]
fn the_tools_checklist_starts_from_every_built_tool_and_saves_what_is_checked() {
    let mut screen = loaded_with(with_tools(TOOLS, None), Vec::new(), ok(), "tools");
    assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
    assert_eq!(
        field(&screen),
        Some(Field::Checklist {
            items: TOOLS.iter().map(|t| CheckItem::plain(t)).collect(),
            marks: vec![true; TOOLS.len()],
            at: 0,
        })
    );
    let pane = screen.detail(&Quote::default()).unwrap();
    assert_eq!(pane.keys, hints(CHECK_KEYS));
    assert_eq!(
        pane.edit,
        Some(EditView::Choices {
            label: CHECK_LABEL.to_owned(),
            choices: TOOLS.iter().map(|t| format!("[x] {t}")).collect(),
            at: 0,
        })
    );
    press(&mut screen, KeyCode::Right);
    press(&mut screen, KeyCode::Char(' '));
    let save = saved(press(&mut screen, KeyCode::Enter));
    assert_eq!(save.change.target, named("tools"));
    assert_eq!(
        save.change.value,
        Some(SettingValue::List(
            TOOLS
                .iter()
                .filter(|t| **t != TOOLS[1])
                .map(|t| (*t).to_owned())
                .collect()
        ))
    );
    assert!(screen.edit().is_none());
}

fn catalog_plugin(
    id: &str,
    tools: Vec<wisp_protocol::plugin::ToolEntry>,
) -> wisp_protocol::PluginEntry {
    wisp_protocol::PluginEntry {
        prompt_sections: Vec::new(),
        id: id.to_owned(),
        version: "0.1.0".to_owned(),
        summary: format!("The {id} plugin"),
        source: wisp_protocol::PluginSource::Bundled,
        installed: true,
        enabled: true,
        tools,
        commands: Vec::new(),
        hooks: Vec::new(),
        slots: Vec::new(),
        settings: Vec::new(),
        problems: Vec::new(),
    }
}

fn bundle() -> Vec<wisp_protocol::PluginEntry> {
    let mut far = catalog_tool("far_away", None, true);
    far.unavailable = Some("not available on this platform".to_owned());
    vec![
        catalog_plugin(
            "runner",
            vec![
                catalog_tool("run", None, true),
                catalog_tool("job", Some("run"), true),
            ],
        ),
        catalog_plugin("lists", vec![catalog_tool("checklist", None, false)]),
        catalog_plugin("elsewhere", vec![far]),
    ]
}

const CATALOG_TOOLS: [&str; 3] = ["run", "far_away", "checklist"];

fn from_catalog(value: Option<&str>) -> Screen {
    from_catalog_over(with_tools(&CATALOG_TOOLS, value), value)
}

fn from_catalog_over(rows: Vec<ResolvedSetting>, saved: Option<&str>) -> Screen {
    let tools = find("tools").unwrap();
    let saved = saved
        .map(|text| SavedValue::of(SitePath::global(tools), tools.parse(text).unwrap(), Some(1)))
        .into_iter()
        .collect();
    let mut screen = Screen::opening(RequestId(1), "tools");
    let mut shot = snapshot(rows, saved, ok());
    shot.plugins = bundle();
    assert!(screen.answered(&shot, &Display::default()));
    screen
}

#[test]
fn the_tools_checklist_comes_from_the_catalog_with_followers_under_their_leader() {
    let mut screen = from_catalog(None);
    assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
    let Some(Field::Checklist { items, marks, .. }) = field(&screen) else {
        panic!("{:?}", field(&screen));
    };
    let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["checklist", "far_away", "run", "job"]);
    assert_eq!(marks, [false, true, true, false]);
    let pane = screen.detail(&Quote::default()).unwrap();
    assert_eq!(
        pane.edit,
        Some(EditView::Choices {
            label: CHECK_LABEL.to_owned(),
            choices: vec![
                "[ ] checklist".to_owned(),
                "[x] far_away \u{b7} not available on this platform".to_owned(),
                "[x] run".to_owned(),
                "    [x] job (comes with run)".to_owned(),
            ],
            at: 0,
        })
    );
    for _ in 0..3 {
        press(&mut screen, KeyCode::Right);
    }
    press(&mut screen, KeyCode::Char(' '));
    let Some(Field::Checklist { marks, .. }) = field(&screen) else {
        panic!("{:?}", field(&screen));
    };
    assert_eq!(
        marks,
        [false, true, true, false],
        "a follower can't be checked"
    );
    press(&mut screen, KeyCode::Left);
    press(&mut screen, KeyCode::Char(' '));
    let pane = screen.detail(&Quote::default()).unwrap();
    let Some(EditView::Choices { choices, .. }) = pane.edit else {
        panic!("{:?}", pane.edit);
    };
    assert_eq!(choices[3], "    [ ] job (comes with run)");
    let save = saved(press(&mut screen, KeyCode::Enter));
    assert_eq!(
        save.change.value,
        Some(SettingValue::List(vec!["far_away".to_owned()]))
    );
}

#[test]
fn a_turned_off_tool_is_drawn_unchecked_cant_be_toggled_and_keeps_its_place_in_the_list() {
    for (name, at, expected) in [
        (
            "run",
            2,
            ["[ ] run \u{b7} turned off", "    [ ] job (comes with run)"],
        ),
        (
            "job",
            3,
            ["[x] run", "    [ ] job (comes with run) \u{b7} turned off"],
        ),
    ] {
        let mut rows = with_tools(&CATALOG_TOOLS, None);
        for row in rows
            .iter_mut()
            .filter(|r| r.name.spec().name == "turned_off")
        {
            row.value = Some(SettingValue::List(vec![name.to_owned()]));
        }
        let mut screen = from_catalog_over(rows, None);
        press(&mut screen, KeyCode::Enter);
        let Some(Field::Checklist { items, .. }) = field(&screen) else {
            panic!("{:?}", field(&screen));
        };
        assert_eq!(
            items[at].locked.as_deref(),
            Some(format!("{name} is in Turned off; take it out there to turn it on").as_str())
        );
        for _ in 0..at {
            press(&mut screen, KeyCode::Right);
        }
        press(&mut screen, KeyCode::Char(' '));
        let pane = screen.detail(&Quote::default()).unwrap();
        let Some(EditView::Choices { choices, .. }) = pane.edit else {
            panic!("{:?}", pane.edit);
        };
        assert_eq!(choices[2..], expected);
        let save = saved(press(&mut screen, KeyCode::Enter));
        assert_eq!(
            save.change.value,
            Some(SettingValue::List(vec![
                "far_away".to_owned(),
                "run".to_owned()
            ]))
        );
    }
}

fn spawner_screen(saved: Option<&str>) -> Screen {
    let tools = find("tools").unwrap();
    let saved_values = saved
        .map(|text| SavedValue::of(SitePath::global(tools), tools.parse(text).unwrap(), Some(1)))
        .into_iter()
        .collect();
    let mut screen = Screen::opening(RequestId(1), "tools");
    let mut shot = snapshot(with_tools(&["spawn"], saved), saved_values, ok());
    shot.plugins = vec![catalog_plugin(
        "spawner",
        vec![
            catalog_tool("quiet", Some("spawn"), false),
            catalog_tool("report", Some("spawn"), false),
            catalog_tool("mail", Some("spawn"), true),
            catalog_tool("spawn", None, true),
        ],
    )];
    assert!(screen.answered(&shot, &Display::default()));
    screen
}

#[test]
fn a_follower_off_by_default_is_a_note_row_never_checked_with_its_leader() {
    let mut screen = spawner_screen(None);
    press(&mut screen, KeyCode::Enter);
    let Some(Field::Checklist { items, marks, .. }) = field(&screen) else {
        panic!("{:?}", field(&screen));
    };
    let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["spawn", "mail", "quiet", "report"]);
    assert_eq!(marks, [true, false, false, false]);
    let pane = screen.detail(&Quote::default()).unwrap();
    let Some(EditView::Choices { choices, .. }) = pane.edit else {
        panic!("{:?}", pane.edit);
    };
    assert_eq!(
        choices,
        [
            "[x] spawn",
            "    [x] mail (comes with spawn)",
            "    quiet (follows spawn)",
            "    report \u{b7} child agents only",
        ]
    );
    for _ in 0..3 {
        press(&mut screen, KeyCode::Right);
    }
    press(&mut screen, KeyCode::Char(' '));
    let Some(Field::Checklist { marks, .. }) = field(&screen) else {
        panic!("{:?}", field(&screen));
    };
    assert_eq!(
        marks,
        [true, false, false, false],
        "the note row can't be checked"
    );
    let save = saved(press(&mut screen, KeyCode::Enter));
    assert_eq!(
        save.change.value,
        Some(SettingValue::List(vec!["spawn".to_owned()]))
    );
    let mut screen = Screen::opening(RequestId(1), "");
    let mut shot = snapshot(defaults(), Vec::new(), ok());
    shot.plugins = vec![catalog_plugin(
        "spawner",
        vec![
            catalog_tool("report", Some("spawn"), false),
            catalog_tool("spawn", None, true),
        ],
    )];
    assert!(screen.answered(&shot, &Display::default()));
    screen.open_page(Page::Plugins);
    screen.selected = Some(Spot::Plugin("spawner".to_owned()));
    let pane = screen.detail(&Quote::default()).unwrap();
    assert_eq!(pane.layers[1].word, "report");
    assert_eq!(pane.layers[1].value, "off \u{b7} child agents only");
}

#[test]
fn a_saved_list_naming_followers_beside_their_leader_keeps_no_rows_for_them() {
    let mut screen = spawner_screen(Some("mail,report,spawn"));
    press(&mut screen, KeyCode::Enter);
    let Some(Field::Checklist { items, marks, .. }) = field(&screen) else {
        panic!("{:?}", field(&screen));
    };
    let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["spawn", "mail", "quiet", "report"]);
    assert_eq!(marks, [true, false, false, false]);
    let save = saved(press(&mut screen, KeyCode::Enter));
    assert_eq!(
        save.change.value,
        Some(SettingValue::List(vec!["spawn".to_owned()]))
    );
}

#[test]
fn a_saved_report_without_its_leader_is_kept_as_for_child_agents_only() {
    let mut screen = spawner_screen(Some("mail,report"));
    press(&mut screen, KeyCode::Enter);
    let pane = screen.detail(&Quote::default()).unwrap();
    let Some(EditView::Choices { choices, .. }) = pane.edit else {
        panic!("{:?}", pane.edit);
    };
    assert_eq!(
        choices[4..],
        [
            "    mail \u{b7} comes with spawn; kept",
            "    report \u{b7} child agents only; kept",
        ]
    );
}

const PASTED: &str = "opaque_CANARY0123456789abcdefABCDEF";

#[test]
fn a_saved_name_no_plugin_provides_is_kept_when_the_checklist_saves() {
    let mut screen = from_catalog(Some(&format!("run,telepathy,{PASTED}")));
    press(&mut screen, KeyCode::Enter);
    let Some(Field::Checklist { items, marks, .. }) = field(&screen) else {
        panic!("{:?}", field(&screen));
    };
    let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(
        names,
        ["checklist", "far_away", "run", "job", PASTED, "telepathy"]
    );
    assert_eq!(marks, [false, false, true, false, false, false]);
    let pane = screen.detail(&Quote::default()).unwrap();
    let Some(EditView::Choices { choices, .. }) = pane.edit else {
        panic!("{:?}", pane.edit);
    };
    assert_eq!(
        choices[4..],
        [
            "    a saved value that is no tool \u{b7} no plugin provides it; kept",
            "    telepathy \u{b7} no plugin provides it; kept",
        ]
    );
    assert!(!choices.iter().any(|c| c.contains(PASTED)), "{choices:?}");
    for _ in 0..5 {
        press(&mut screen, KeyCode::Right);
    }
    press(&mut screen, KeyCode::Char(' '));
    let pane = screen.detail(&Quote::default()).unwrap();
    assert_eq!(pane.keys, hints(FIXED_CHECK_KEYS));
    for _ in 0..5 {
        press(&mut screen, KeyCode::Left);
    }
    press(&mut screen, KeyCode::Char(' '));
    let pane = screen.detail(&Quote::default()).unwrap();
    assert_eq!(pane.keys, hints(CHECK_KEYS));
    let save = saved(press(&mut screen, KeyCode::Enter));
    assert_eq!(
        save.change.value,
        Some(SettingValue::List(
            ["checklist", PASTED, "run", "telepathy"]
                .map(str::to_owned)
                .to_vec()
        ))
    );
}

#[test]
fn kept_lines_come_from_the_saved_list_not_the_value_in_force() {
    let mut rows = with_tools(&CATALOG_TOOLS, Some("run,checklist"));
    for row in rows.iter_mut().filter(|r| r.name.spec().name == "tools") {
        row.layer = Layer::Flag;
        row.origin = "--tools".to_owned();
    }
    let mut screen = from_catalog_over(rows, Some("job,telepathy"));
    press(&mut screen, KeyCode::Enter);
    let Some(Field::Checklist { items, marks, .. }) = field(&screen) else {
        panic!("{:?}", field(&screen));
    };
    let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(
        names,
        ["checklist", "far_away", "run", "job", "job", "telepathy"]
    );
    assert_eq!(marks, [true, false, true, false, false, false]);
    let pane = screen.detail(&Quote::default()).unwrap();
    let Some(EditView::Choices { choices, .. }) = pane.edit else {
        panic!("{:?}", pane.edit);
    };
    assert_eq!(
        choices[4..],
        [
            "    job \u{b7} comes with run; kept",
            "    telepathy \u{b7} no plugin provides it; kept",
        ]
    );
    let save = saved(press(&mut screen, KeyCode::Enter));
    assert_eq!(
        save.change.value,
        Some(SettingValue::List(
            ["checklist", "job", "run", "telepathy"]
                .map(str::to_owned)
                .to_vec()
        ))
    );
}

#[test]
fn a_saved_tools_list_checks_only_what_it_names() {
    let mut screen = from_catalog(Some("checklist"));
    press(&mut screen, KeyCode::Enter);
    let Some(Field::Checklist { marks, .. }) = field(&screen) else {
        panic!("{:?}", field(&screen));
    };
    assert_eq!(marks, [true, false, false, false]);
}

#[test]
fn the_plugins_section_lists_each_plugin_and_switches_it_by_path() {
    let mut screen = Screen::opening(RequestId(1), "");
    let mut shot = snapshot(defaults(), Vec::new(), ok());
    shot.plugins = bundle();
    assert!(screen.answered(&shot, &Display::default()));
    screen.selected = Some(Spot::Open(Page::Plugins));
    assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
    let (entries, map) = screen.entries();
    let spot = Spot::Plugin("runner".to_owned());
    let at = map.iter().position(|s| s.as_ref() == Some(&spot)).unwrap();
    assert!(
        entries
            .iter()
            .any(|e| matches!(e, Entry::Heading(h) if h == "Plugins")),
        "{entries:?}"
    );
    let Entry::Row(row) = &entries[at] else {
        panic!("{:?}", entries[at]);
    };
    assert_eq!(row.label, "runner");
    assert_eq!(row.value, "on");
    screen.selected = Some(spot);
    let pane = screen.detail(&Quote::default()).unwrap();
    assert_eq!(pane.lead, "runner");
    assert_eq!(pane.title, "the runner plugin");
    assert!(pane.notes.is_empty());
    let tools: Vec<(String, String)> = pane
        .layers
        .iter()
        .map(|l| (l.word.clone(), l.value.clone()))
        .collect();
    assert_eq!(
        tools,
        [
            ("State".to_owned(), "on".to_owned()),
            ("run".to_owned(), "on \u{b7} switch in Tools".to_owned()),
            ("job".to_owned(), "on, with run".to_owned()),
        ]
    );
    assert!(!screen.can_change());
    let keys: Vec<(String, String)> = pane
        .keys
        .iter()
        .map(|h| (h.key().to_owned(), h.does().to_owned()))
        .collect();
    assert_eq!(
        keys,
        [
            ("enter".to_owned(), "turn off".to_owned()),
            ("→".to_owned(), "its settings".to_owned()),
            ("d".to_owned(), "remove".to_owned()),
        ]
    );
    for key in [KeyCode::Char(' '), KeyCode::Char('e')] {
        assert_eq!(press(&mut screen, key), Act::Stay, "{key:?}");
    }
    let switched = |act: Act| {
        let Act::SaveMany(many) = act else {
            panic!("{act:?}");
        };
        let [change] = many.changes.as_slice() else {
            panic!("{:?}", many.changes);
        };
        let SettingTarget::Path(path) = &change.target else {
            panic!("{change:?}");
        };
        (path.to_string(), change.value.clone())
    };
    assert_eq!(
        switched(press(&mut screen, KeyCode::Enter)),
        (
            "plugins.runner.enabled".to_owned(),
            Some(SettingValue::Bool(false))
        )
    );
    assert_eq!(press(&mut screen, KeyCode::Char('d')), Act::Stay);
    assert!(screen.asking().is_some());
    assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
    assert!(screen.asking().is_none());
    assert_eq!(press(&mut screen, KeyCode::Char('d')), Act::Stay);
    assert_eq!(press(&mut screen, KeyCode::Up), Act::Stay);
    assert_eq!(
        switched(press(&mut screen, KeyCode::Enter)),
        (
            "plugins.runner.installed".to_owned(),
            Some(SettingValue::Bool(false))
        )
    );
}

#[test]
fn no_config_row_writes_a_key_another_section_writes() {
    let mut writers: std::collections::BTreeMap<&str, Section> = std::collections::BTreeMap::new();
    for (section, spec) in placed() {
        if let Some(other) = writers.insert(spec.name, section) {
            assert_eq!(other, section, "{} is written from two sections", spec.name);
        }
    }
    assert_eq!(writers.get("tools"), Some(&Section::Chat));
    assert!(!writers.values().any(|s| *s == Section::Plugins));
    assert!(!writers.contains_key(wisp_protocol::settings::PLUGIN_HOOK));
}

#[test]
fn the_tools_checklist_starts_from_the_saved_list_and_may_save_none() {
    let mut screen = loaded_with(with_tools(TOOLS, Some(TOOLS[0])), Vec::new(), ok(), "tools");
    press(&mut screen, KeyCode::Enter);
    let Some(Field::Checklist { marks, .. }) = field(&screen) else {
        panic!("{:?}", field(&screen));
    };
    let first: Vec<bool> = (0..TOOLS.len()).map(|i| i == 0).collect();
    assert_eq!(marks, first);
    press(&mut screen, KeyCode::Char(' '));
    let save = saved(press(&mut screen, KeyCode::Enter));
    assert_eq!(save.change.value, Some(SettingValue::List(Vec::new())));
    press(&mut screen, KeyCode::Enter);
    assert_eq!(press(&mut screen, KeyCode::Char('q')), Act::Stay);
    assert!(screen.edit().is_none());
}

fn on_sites(saved: Vec<SavedValue>) -> Screen {
    with_window(defaults(), saved, ok(), "compact_at", Some(1_000_000))
}

fn endpoint_model(name: &str) -> SitePath {
    SitePath::new(
        Place::EndpointModel {
            endpoint: "manifest".to_owned(),
            model: MODEL.to_owned(),
        },
        find(name).unwrap(),
    )
}

#[test]
fn tab_changes_for_and_enter_saves_the_new_value_there() {
    let spec = find("compact_at").unwrap();
    let model_site = SitePath::new(Place::Model(MODEL.to_owned()), spec);
    let mut screen = on_sites(vec![SavedValue::of(
        model_site,
        spec.parse("60%").unwrap(),
        Some(3),
    )]);
    press(&mut screen, KeyCode::Enter);
    let edit = screen.edit().unwrap();
    assert_eq!(
        edit.sites
            .iter()
            .map(|s| s.label.as_str())
            .collect::<Vec<_>>(),
        [ALL_MODELS, "this model"]
    );
    assert_eq!(edit.site, 1);
    typed(&mut screen, "600000");
    press(&mut screen, KeyCode::Tab);
    let pane = screen.detail(&Quote::default()).unwrap();
    let Some(EditView::Text { sites, site, .. }) = pane.edit else {
        panic!("{pane:?}");
    };
    assert_eq!(sites, [ALL_MODELS, "this model"]);
    assert_eq!(site, 0);
    let save = saved(press(&mut screen, KeyCode::Enter));
    assert!(save.save);
    assert_eq!(
        save.change.target,
        SettingTarget::Path(SitePath::global(spec))
    );
    assert_eq!(save.change.value, Some(SettingValue::Tokens(600000)));
}

#[test]
fn for_offers_an_account_s_model_place_only_when_it_holds_a_saved_number() {
    for held in [false, true] {
        let spec = find("compact_at").unwrap();
        let values = held
            .then(|| {
                SavedValue::of(
                    endpoint_model("compact_at"),
                    spec.parse("90%").unwrap(),
                    Some(7),
                )
            })
            .into_iter()
            .collect();
        let mut screen = on_sites(values);
        press(&mut screen, KeyCode::Enter);
        assert_eq!(screen.edit().unwrap().sites.len(), if held { 3 } else { 2 });
        if held {
            assert_eq!(screen.edit().unwrap().site, 2);
            let save = saved(press(&mut screen, KeyCode::Enter));
            assert_eq!(
                save.change.target,
                SettingTarget::Path(endpoint_model("compact_at"))
            );
            assert_eq!(save.change.value, None);
            assert_eq!(save.told.saved.as_deref(), Some("90%"));
        } else {
            assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
            assert!(screen.edit().is_some());
        }
    }
    let spec = find("context_window").unwrap();
    let legacy = endpoint_model("context_window");
    let mut snap = on_sites(vec![SavedValue::of(
        legacy.clone(),
        spec.parse("900000").unwrap(),
        Some(7),
    )])
    .snapshot()
    .unwrap()
    .clone();
    snap.answers = RequestId(2);
    let mut screen = Screen::opening(RequestId(2), "context_window");
    assert!(screen.answered(&snap, &Display::default()));
    press(&mut screen, KeyCode::Enter);
    let edit = screen.edit().unwrap();
    assert_eq!(edit.sites.len(), 2);
    assert_eq!(edit.site().unwrap().path, legacy);
    assert!(matches!(&edit.field, Field::Text { text, .. } if text == "900000"));
    screen.key(Key::ctrl('u'));
    let save = saved(press(&mut screen, KeyCode::Enter));
    assert_eq!(
        save.change,
        SettingChange {
            target: SettingTarget::Path(legacy),
            value: None
        }
    );
}

#[test]
fn an_empty_value_removes_the_number_only_at_the_place_for_names() {
    let spec = find("compact_at").unwrap();
    let model = SitePath::new(Place::Model(MODEL.to_owned()), spec);
    let mut screen = on_sites(vec![
        saved_global("compact_at", "70%"),
        SavedValue::of(model.clone(), spec.parse("60%").unwrap(), Some(3)),
    ]);
    press(&mut screen, KeyCode::Enter);
    let pane = screen.detail(&Quote::default()).unwrap();
    let Some(EditView::Text { check, .. }) = pane.edit else {
        panic!("{pane:?}");
    };
    assert_eq!(
        check,
        Check::Preview("remove it from this model".to_owned())
    );
    let save = saved(press(&mut screen, KeyCode::Enter));
    assert_eq!(
        save.change,
        SettingChange {
            target: SettingTarget::Path(model),
            value: None
        }
    );
}

#[test]
fn the_site_chosen_is_kept_with_a_set_aside_edit() {
    let mut screen = on_sites(Vec::new());
    press(&mut screen, KeyCode::Enter);
    press(&mut screen, KeyCode::Tab);
    typed(&mut screen, "75%");
    let mut back = Screen::restored(RequestId(5), screen.draft());
    let mut rows = defaults();
    put(
        &mut rows,
        "context_window",
        Layer::Shipped,
        "1000000",
        "shipped",
    );
    let mut answer = snapshot(rows, Vec::new(), ok());
    answer.answers = RequestId(5);
    assert!(back.answered(&answer, &Display::default()));
    let save = saved(press(&mut back, KeyCode::Enter));
    assert_eq!(
        save.change.target,
        SettingTarget::Path(SitePath::new(
            Place::Model(MODEL.to_owned()),
            find("compact_at").unwrap()
        ))
    );
}

#[test]
fn a_pair_save_marks_each_row_it_names_and_a_refusal_lands_under_each() {
    let mut screen = loaded("model");
    screen.sent(RequestId(20), &["model", "effort"], true);
    assert!(value_of(&screen, "Model").ends_with(&format!("{SEP}{SAVING}")));
    assert!(value_of(&screen, "Effort").ends_with(&format!("{SEP}{SAVING}")));
    let refused = AgentEvent::Rejected {
        id: None,
        kind: None,
        request: Some(RequestId(20)),
        reason: "config.toml is locked".to_owned(),
    };
    assert!(screen.saved(&refused, false));
    let (entries, _) = screen.entries();
    let notes = entries
        .iter()
        .filter(|e| matches!(e, Entry::Note(reason) if reason == "config.toml is locked"))
        .count();
    assert_eq!(notes, 2);
}

#[test]
fn a_save_reads_saving_then_its_answer_and_a_refusal_shows_under_its_row() {
    let mut screen = loaded("cache_ttl");
    screen.sent(RequestId(9), &["cache_ttl"], true);
    assert_eq!(
        value_of(&screen, "Cache lifetime"),
        format!("1h{SEP}{SAVING}")
    );
    assert!(!screen.saved(&changed(RequestId(8), "cache_ttl", When::NextTurn), true));
    assert!(screen.saved(&changed(RequestId(9), "cache_ttl", When::NextTurn), true));
    assert_eq!(
        value_of(&screen, "Cache lifetime"),
        format!("1h \u{b7} {}", pending(NEXT_TURN))
    );
    assert!(screen.turn_started());
    assert_eq!(value_of(&screen, "Cache lifetime"), "1h");
    screen.open_page(Page::Group(Section::Chat));
    screen.sent(RequestId(10), &["time"], true);
    assert!(screen.saved(&changed(RequestId(10), "time", When::NextTurn), false));
    assert_eq!(value_of(&screen, "Message time"), "local");
    screen.open_page(Page::Group(Section::Advanced));
    screen.sent(RequestId(11), &["log"], true);
    assert!(screen.saved(&changed(RequestId(11), "log", When::NextSession), false));
    assert_eq!(
        value_of(&screen, "Session log"),
        format!("on \u{b7} {NEW_CHATS}")
    );
    screen.sent(RequestId(12), &["debug_cache"], true);
    let refused = AgentEvent::refused(RequestId(12), "config.toml is locked");
    assert!(screen.saved(&refused, false));
    assert!(!screen.saved(&refused, false));
    assert_eq!(value_of(&screen, "Cache diagnostics"), "off");
    let (entries, map) = screen.entries();
    let at = entries
        .iter()
        .position(|e| matches!(e, Entry::Row(row) if row.label == "Cache diagnostics"))
        .unwrap();
    assert_eq!(
        entries[at + 1],
        Entry::Note("config.toml is locked".to_owned())
    );
    assert_eq!(map[at + 1], None);
    press(&mut screen, KeyCode::Down);
    press(&mut screen, KeyCode::Up);
    assert_eq!(
        screen.selected().map(|r| r.spec.name),
        Some("cache_ttl"),
        "the note is not a row"
    );
}

#[test]
fn an_edit_kept_for_reopening_never_keeps_key_shaped_text() {
    let mut screen = loaded("context.verbatim_user_tokens");
    press(&mut screen, KeyCode::Enter);
    screen.key(Key::ctrl('u'));
    screen.paste(CANARY);
    let draft = screen.draft();
    assert_eq!(draft.edit, None);
    screen.key(Key::ctrl('u'));
    typed(&mut screen, "4k");
    let draft = screen.draft();
    let mut reopened = Screen::restored(RequestId(5), draft);
    assert!(reopened.answered(
        &SettingsSnapshot {
            often: Vec::new(),
            answers: RequestId(5),
            ..snapshot(defaults(), Vec::new(), ok())
        },
        &Display::default()
    ));
    assert_eq!(
        reopened.selected().map(|r| r.spec.name),
        Some("context.verbatim_user_tokens")
    );
    assert_eq!(
        field(&reopened),
        Some(Field::Text {
            text: "4k".to_owned(),
            cursor: 2,
        })
    );
    let save = saved(press(&mut reopened, KeyCode::Enter));
    assert_eq!(save.change.value, Some(SettingValue::Tokens(4_000)));
    let mut filtered = loaded("");
    press(&mut filtered, KeyCode::Char('/'));
    typed(&mut filtered, CANARY);
    assert_eq!(filtered.draft().filter, "");
}

#[test]
fn a_key_with_underscores_is_never_kept_either() {
    let mut screen = loaded("context.verbatim_user_tokens");
    press(&mut screen, KeyCode::Enter);
    screen.key(Key::ctrl('u'));
    screen.paste(UNDERSCORED);
    assert_eq!(screen.draft().edit, None);
    let mut filtered = loaded("");
    press(&mut filtered, KeyCode::Char('/'));
    typed(&mut filtered, UNDERSCORED);
    assert_eq!(filtered.draft().filter, "");
    assert!(!key_shaped("WORK_API_KEY"));
}

#[test]
fn pricing_on_the_implicit_endpoint_points_at_a_named_one() {
    let snap = on_the_keys_file_account(snapshot(defaults(), Vec::new(), ok()));
    let mut screen = Screen::opening(RequestId(1), "pricing");
    assert!(screen.answered(&snap, &Display::default()));
    assert!(!screen.can_change());
    assert!(matches!(screen.opened(), Some(Spot::Endpoint(_))));
    assert!(
        screen
            .rows()
            .iter()
            .all(|row| row.spec.name == names::PRICING)
    );
    let said = Act::Hint(PRICING_PER_ENDPOINT);
    assert_eq!(press(&mut screen, KeyCode::Enter), said);
    assert_eq!(press(&mut screen, KeyCode::Char('d')), said);
    assert!(screen.edit().is_none());
    let pane = screen.detail(&Quote::default()).unwrap();
    assert_eq!(pane.keys, [KeyHint::note(PRICING_PER_ENDPOINT)]);
    let mut named = loaded("pricing");
    assert!(named.can_change());
    assert_eq!(press(&mut named, KeyCode::Enter), Act::Stay);
    assert!(named.edit().is_some());
}

#[test]
fn enter_and_d_wait_while_the_row_is_saving() {
    let mut screen = loaded("debug_cache");
    let save = saved(press(&mut screen, KeyCode::Enter));
    screen.sent(RequestId(4), &[save.name], true);
    assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
    assert_eq!(press(&mut screen, KeyCode::Char('d')), Act::Stay);
    assert!(screen.saved(&changed(RequestId(4), "debug_cache", When::Now), false));
    assert!(matches!(press(&mut screen, KeyCode::Enter), Act::Save(_)));
}

#[test]
fn the_compact_value_stops_at_the_point() {
    let spec = find("compact_at").unwrap();
    let million = context(Some(1_000_000));
    let cap = wisp_protocol::settings::default_point(1_000_000, 64_000);
    assert_eq!(
        preview(spec, &SettingValue::Tokens(cap), million),
        format!("{} of 1,000,000", wisp_protocol::settings::thousands(cap))
    );
    assert_eq!(
        preview(spec, &SettingValue::Tokens(cap + 1), million),
        preview(spec, &SettingValue::Tokens(cap), million)
    );
    let shipped = setting(
        "context_window",
        Some(SettingValue::Tokens(1_050_000)),
        Layer::Shipped,
        "shipped",
    );
    let mut shot = snapshot(vec![shipped], Vec::new(), ok());
    shot.context.window = Some(272_000);
    assert_eq!(
        check(spec, "400k", Context::of(&shot)),
        Check::Preview("208,000 of 272,000".to_owned())
    );
    shot.context.window = Some(470_589);
    shot.context.room = Some(WindowRoom {
        base: 272_000,
        max: 872_000,
    });
    for (typed, want) in [
        ("900k", "741,200 of 872,000"),
        ("90%", "208,000 of 272,000"),
    ] {
        assert_eq!(
            check(spec, typed, Context::of(&shot)),
            Check::Preview(want.to_owned())
        );
    }
}

#[test]
fn a_file_that_breaks_while_editing_stops_the_save() {
    let mut screen = loaded("context.verbatim_user_tokens");
    press(&mut screen, KeyCode::Enter);
    screen.key(Key::ctrl('u'));
    typed(&mut screen, "4k");
    screen.ask(RequestId(2));
    let broken = FileState::Broken {
        line: Some(3),
        reason: "expected a value".to_owned(),
    };
    assert!(screen.answered(
        &SettingsSnapshot {
            often: Vec::new(),
            answers: RequestId(2),
            ..snapshot(defaults(), Vec::new(), broken)
        },
        &Display::default()
    ));
    assert!(screen.edit().is_some());
    assert_eq!(press(&mut screen, KeyCode::Enter), Act::Hint(BROKEN));
}

#[test]
fn control_characters_never_reach_an_editor_or_the_filter() {
    let mut screen = loaded("context.verbatim_user_tokens");
    press(&mut screen, KeyCode::Enter);
    screen.key(Key::ctrl('u'));
    screen.paste("1\n2\u{7}3\t");
    assert_eq!(
        field(&screen),
        Some(Field::Text {
            text: "123".to_owned(),
            cursor: 3,
        })
    );
    let mut filtered = loaded("");
    press(&mut filtered, KeyCode::Char('/'));
    filtered.paste("cache\n\t");
    assert_eq!(filtered.filter(), "cache");
    filtered.key(Key {
        ctrl: true,
        ..Key::plain(KeyCode::Backspace)
    });
    assert_eq!(filtered.filter(), "cache");
}

#[test]
fn the_replaced_value_and_the_default_read_plainly() {
    let mut screen = with_window(defaults(), Vec::new(), ok(), "compact_at", Some(1_000_000));
    press(&mut screen, KeyCode::Enter);
    screen.key(Key::ctrl('u'));
    typed(&mut screen, "60%");
    let save = saved(press(&mut screen, KeyCode::Enter));
    assert_eq!(save.told.was, "850,000 of 1,000,000");
    assert_eq!(unnested("on"), "on");
    let mut settings = defaults();
    put(
        &mut settings,
        "context.verbatim_user_tokens",
        Layer::Session,
        "4000",
        "session",
    );
    let screen = loaded_with(settings, Vec::new(), ok(), "context.verbatim_user_tokens");
    let keys = hints_text(&screen.detail(&Quote::default()).unwrap().keys);
    assert!(keys.contains("d back to 8,000"), "{keys}");
}

#[test]
fn every_next_turn_timing_word_starts_with_the_render_words() {
    let next_turn = |applies: Applies| match applies {
        Applies::NextTurnKept
        | Applies::NextTurnRebuild
        | Applies::NextTurnMayRebuild
        | Applies::NextTurnEffort => true,
        Applies::Now
        | Applies::NextSession
        | Applies::NextCall
        | Applies::Connection
        | Applies::Reserved => false,
    };
    for applies in [
        Applies::Now,
        Applies::NextTurnKept,
        Applies::NextTurnRebuild,
        Applies::NextTurnMayRebuild,
        Applies::NextTurnEffort,
        Applies::NextSession,
        Applies::NextCall,
        Applies::Connection,
        Applies::Reserved,
    ] {
        let starts = applies.words().starts_with(&format!("{NEXT_TURN}{SEP}"));
        assert_eq!(starts, next_turn(applies), "{applies:?}");
    }
}

#[test]
fn a_reset_to_a_rule_default_keeps_its_cost_words_unless_the_cost_is_unknown() {
    let mut settings = defaults();
    put(
        &mut settings,
        "compact_at",
        Layer::Session,
        "60%",
        "session",
    );
    let screen = loaded_with(settings, Vec::new(), ok(), "compact_at");
    let row = screen.selected().unwrap();
    assert!(row.default.is_empty());
    let applies = when_words(row.spec.applies, &Quote::default());
    assert!(applies.is_empty());
    let keys = hints_text(&screen.detail(&Quote::default()).unwrap().keys);
    assert!(keys.ends_with("back to Wisp's own"), "{keys}");
    let unknown = Quote {
        reset: ResetCost::Unknown,
        ..Quote::default()
    };
    let keys = hints_text(&screen.detail(&unknown).unwrap().keys);
    assert!(keys.ends_with("back to Wisp's own"), "{keys}");
    let quoted = Quote {
        reset: ResetCost::Quoted("costs this".to_owned()),
        ..Quote::default()
    };
    let keys = hints_text(&screen.detail(&quoted).unwrap().keys);
    assert!(
        keys.ends_with("back to Wisp's own \u{b7} costs this"),
        "{keys}"
    );
    let pane = screen.detail(&Quote::default()).unwrap();
    assert!(
        pane.keys
            .iter()
            .all(|k| k.text().split('\u{b7}').count() == 1),
        "{:?}",
        pane.keys
    );
}

#[test]
fn the_mouse_leaves_the_edited_row_selected() {
    let mut screen = loaded("cache_ttl");
    press(&mut screen, KeyCode::Enter);
    screen.fit(5);
    screen.click(1);
    screen.scroll(5);
    assert_eq!(screen.selected().map(|r| r.spec.name), Some("cache_ttl"));
    assert!(screen.edit().is_some());
}

#[test]
fn a_restored_edit_waits_for_the_rows_and_esc_still_closes_the_screen() {
    let mut screen = loaded("cache_ttl");
    press(&mut screen, KeyCode::Enter);
    let mut reopened = Screen::restored(RequestId(3), screen.draft());
    assert!(reopened.loading());
    assert_eq!(press(&mut reopened, KeyCode::Enter), Act::Stay);
    assert!(reopened.edit().is_some());
    assert_eq!(press(&mut reopened, KeyCode::Esc), Act::Stay);
    assert_eq!(reopened.current, Page::More);
}

#[test]
fn on_the_implicit_endpoint_no_endpoint_place_is_offered() {
    let snap = on_the_keys_file_account(snapshot(defaults(), Vec::new(), ok()));
    let names: Vec<&str> = placed()
        .map(|(_, spec)| spec)
        .chain([find(names::CONTEXT_WINDOW).unwrap()])
        .filter(|spec| {
            spec.is_built()
                && spec.secrecy == Secrecy::Plain
                && spec.sites.contains(&Site::EndpointModel)
        })
        .map(|spec| spec.name)
        .collect();
    assert_eq!(names.len(), 4, "{names:?}");
    for name in names {
        let mut screen = Screen::opening(RequestId(1), name);
        assert!(screen.answered(&snap, &Display::default()));
        let row = screen.selected().unwrap().clone();
        let places = sites(&row, &snap);
        assert!(
            places.iter().all(|s| matches!(
                s.path.place,
                Place::Global | Place::Model(_) | Place::Agent(_)
            )),
            "{name}: {places:?}"
        );
        if find(name).is_some_and(|spec| spec.sites.contains(&Site::Global)) {
            continue;
        }
        press(&mut screen, KeyCode::Enter);
        assert!(screen.edit().unwrap().sites.is_empty());
    }
}

#[test]
fn the_for_choice_ends_when_the_box_closes() {
    let mut screen = on_sites(Vec::new());
    press(&mut screen, KeyCode::Enter);
    press(&mut screen, KeyCode::BackTab);
    assert_eq!(screen.edit().unwrap().site, 1);
    press(&mut screen, KeyCode::Esc);
    press(&mut screen, KeyCode::Enter);
    assert_eq!(screen.edit().unwrap().site, 0);
}

#[test]
fn a_set_aside_checklist_is_kept_in_the_draft() {
    let mut tools = loaded_with(with_tools(TOOLS, None), Vec::new(), ok(), "tools");
    assert_eq!(press(&mut tools, KeyCode::Enter), Act::Stay);
    assert!(tools.edit().is_some_and(Edit::is_checklist));
    let draft = tools.draft();
    assert!(draft.edit.as_ref().is_some_and(Edit::is_checklist));
    let back = Screen::restored(RequestId(9), draft);
    assert!(back.edit().is_some_and(Edit::is_checklist));
}

#[test]
fn a_key_as_the_model_offers_no_place_under_it_and_locks_its_rows() {
    const KEY: &str = "sk-ant-api03-CANARYMODEL0000";
    let spec = find("context_window").unwrap();
    let mut settings = defaults();
    put(
        &mut settings,
        "context_window",
        Layer::Model,
        "300000",
        &format!("models.\"{KEY}\".context_window"),
    );
    let saved = vec![SavedValue::of(
        SitePath::new(Place::Model(KEY.to_owned()), spec),
        spec.parse("300000").unwrap(),
        Some(3),
    )];
    let mut given = snapshot(settings, saved, ok());
    given.context.model = KEY.to_owned();
    let mut screen = Screen::opening(RequestId(1), "context_window");
    assert!(screen.answered(&given, &Display::default()));
    let row = screen.selected().unwrap().clone();
    assert_eq!(row.changed, None);
    let stored = format!("{:?}", screen.snapshot);
    assert!(sites(&row, screen.snapshot.as_ref().unwrap()).is_empty());
    for key in [KeyCode::Char('d'), KeyCode::Enter] {
        assert_eq!(
            press(&mut screen, key),
            Act::Hint(crate::app::config_screen::edit::HIDDEN_MODEL_LOCK)
        );
        assert!(screen.edit().is_none());
    }
    let pane = screen.detail(&Quote::default()).unwrap();
    assert_eq!(
        pane.keys,
        [KeyHint::note(
            crate::app::config_screen::edit::HIDDEN_MODEL_LOCK
        )]
    );
    assert!(
        pane.layers
            .iter()
            .any(|l| l.origin.starts_with("models.\"MODEL\".context_window")),
        "{pane:?}"
    );
    let all = format!("{screen:?} {pane:?} {stored}");
    assert!(!all.contains("CANARY"), "{all}");
    let mut time = Screen::opening(RequestId(1), "time");
    assert!(time.answered(&given, &Display::default()));
    assert_eq!(press(&mut time, KeyCode::Char('e')), Act::Stay);
}

#[test]
fn an_agent_limit_takes_none_in_the_text_editor() {
    let mut screen = loaded("agents.max_depth");
    press(&mut screen, KeyCode::Enter);
    assert_eq!(screen.key(Key::ctrl('u')), Act::Stay);
    typed(&mut screen, "lots");
    let quote = Quote::default();
    let pane = screen.detail(&quote).unwrap();
    let Some(EditView::Text { check, unit, .. }) = pane.edit else {
        panic!("{pane:?}");
    };
    assert_eq!(
        check,
        Check::Invalid("must be a whole number from 1 to 5, or none".to_owned())
    );
    assert_eq!(unit, "a whole number, 1 to 5, or none");
    screen.key(Key::ctrl('u'));
    typed(&mut screen, "none");
    let save = saved(press(&mut screen, KeyCode::Enter));
    assert_eq!(
        save.change.value,
        Some(SettingValue::Text("none".to_owned()))
    );
    assert_eq!(save.told.was, "2");
}

fn sectioned(
    disabled: Option<wisp_protocol::slot::SlotDisabled>,
) -> Vec<wisp_protocol::PluginEntry> {
    let mut plugin = catalog_plugin("runner", vec![catalog_tool("run", None, true)]);
    plugin.slots = vec![wisp_protocol::slot::SlotEntry {
        name: "runner/usage".to_owned(),
        slot: wisp_protocol::slot::SlotPlace::Sidebar,
        title: "Usage".to_owned(),
        default_on: true,
        order: 0,
        disabled,
    }];
    vec![plugin]
}

fn section_screen(plugins: Vec<wisp_protocol::PluginEntry>, display: &Display) -> Screen {
    let mut screen = Screen::opening(RequestId(1), "");
    let mut shot = snapshot(defaults(), Vec::new(), ok());
    shot.plugins = plugins;
    assert!(screen.answered(&shot, display));
    screen.open_page(Page::Plugin("runner".to_owned()));
    screen
}

fn section_row(screen: &Screen) -> wisp_render::chrome::config_screen::ListRow {
    let (entries, map) = screen.entries();
    let spot = Spot::Section("runner/usage".to_owned());
    let at = map.iter().position(|s| s.as_ref() == Some(&spot)).unwrap();
    let Entry::Row(row) = &entries[at] else {
        panic!("{:?}", entries[at]);
    };
    row.clone()
}

fn toggled(screen: &mut Screen) -> SettingChange {
    screen.selected = Some(Spot::Section("runner/usage".to_owned()));
    let Act::SaveMany(many) = press(screen, KeyCode::Enter) else {
        panic!("enter saved nothing");
    };
    let [change] = many.changes.as_slice() else {
        panic!("{:?}", many.changes);
    };
    change.clone()
}

#[test]
fn a_sidebar_section_row_follows_its_plugin_and_enter_switches_it_by_path() {
    let mut screen = section_screen(sectioned(None), &Display::default());
    let row = section_row(&screen);
    assert_eq!(row.label, "usage");
    assert_eq!(row.value, "on");
    let change = toggled(&mut screen);
    let SettingTarget::Path(path) = &change.target else {
        panic!("{change:?}");
    };
    assert_eq!(path.to_string(), "plugins.runner.slots.usage");
    assert_eq!(change.value, Some(SettingValue::Bool(false)));
    let mut display = Display::default();
    display.set_section("plugins.runner.slots.usage", false, "config.toml");
    let mut screen = section_screen(sectioned(None), &display);
    assert_eq!(toggled(&mut screen).value, Some(SettingValue::Bool(true)));
    let cycle = wisp_protocol::slot::SlotDisabled::Cycle {
        cycle: vec!["runner/usage".to_owned()],
    };
    let screen = section_screen(sectioned(Some(cycle)), &Display::default());
    assert_eq!(section_row(&screen).value, "off (in a cycle)");
}

#[test]
fn a_sidebar_section_row_and_its_detail_show_hostile_strings_inert() {
    let mut plugins = sectioned(None);
    plugins[0].slots[0].name = "runner/us\u{7}[2Jage".to_owned();
    plugins[0].slots[0].title = "Us\u{7}[31mage\u{202e}x".to_owned();
    let mut screen = section_screen(plugins, &Display::default());
    let (entries, map) = screen.entries();
    let spot = Spot::Section("runner/us\u{7}[2Jage".to_owned());
    let at = map.iter().position(|s| s.as_ref() == Some(&spot)).unwrap();
    let Entry::Row(row) = &entries[at] else {
        panic!("{:?}", entries[at]);
    };
    assert_eq!(row.label, "us^G[2Jage");
    assert_eq!(row.value, "on");
    screen.selected = Some(spot);
    let pane = screen.detail(&Quote::default()).unwrap();
    assert_eq!(pane.lead, "us^G[2Jage");
    assert_eq!(pane.title, "the sidebar section \"Us^G[31mage<U+202E>x\"");
    assert_eq!(pane.notes[0], "Us^G[31mage<U+202E>x");
    let shown = format!("{pane:?}");
    assert!(
        !shown.contains('\u{7}') && !shown.contains('\u{202e}'),
        "{shown}"
    );
}

fn with_section(id: &str, name: &str, order: u32) -> wisp_protocol::PluginEntry {
    let mut plugin = catalog_plugin(id, Vec::new());
    plugin.slots = vec![wisp_protocol::slot::SlotEntry {
        name: format!("{id}/{name}"),
        slot: wisp_protocol::slot::SlotPlace::Sidebar,
        title: name.to_owned(),
        default_on: true,
        order,
        disabled: None,
    }];
    plugin
}

#[test]
fn the_fold_checklist_lists_sections_in_order_and_keeps_a_saved_name_no_plugin_provides() {
    let name = "display.sidebar_folded";
    let spec = find(name).unwrap();
    let list = spec.parse("gone/old,zz/first").unwrap();
    let held = vec![SavedValue::of(
        SitePath::global(spec),
        list.clone(),
        Some(1),
    )];
    let mut screen = Screen::opening(RequestId(1), name);
    let mut shot = snapshot(defaults(), held, ok());
    shot.plugins = vec![
        with_section("aa", "second", 1),
        with_section("zz", "first", 0),
    ];
    let mut display = Display::default();
    display.set(name, list, "config.toml display.sidebar_folded");
    assert!(screen.answered(&shot, &display));
    press(&mut screen, KeyCode::Enter);
    let Some(Field::Checklist { items, marks, .. }) = field(&screen) else {
        panic!("{:?}", field(&screen));
    };
    let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["zz/first", "aa/second", "gone/old"]);
    assert_eq!(marks, [true, false, false]);
    let pane = screen.detail(&Quote::default()).unwrap();
    let Some(EditView::Choices { choices, .. }) = pane.edit else {
        panic!("{:?}", pane.edit);
    };
    assert_eq!(
        choices[2..],
        ["    gone/old \u{b7} no plugin provides it; kept"]
    );
    press(&mut screen, KeyCode::Right);
    press(&mut screen, KeyCode::Char(' '));
    let save = saved(press(&mut screen, KeyCode::Enter));
    assert_eq!(
        save.change.value,
        Some(SettingValue::List(
            ["aa/second", "gone/old", "zz/first"]
                .map(str::to_owned)
                .to_vec()
        ))
    );
}

mod hooks;
mod plugins;
mod search;

#[test]
fn a_tool_that_waits_for_the_next_session_says_so_in_its_plugin_row() {
    let mut waiting = catalog_tool("lookup", None, true);
    waiting.waits = true;
    let mut screen = Screen::opening(RequestId(1), "");
    let mut shot = snapshot(defaults(), Vec::new(), ok());
    shot.plugins = vec![catalog_plugin(
        "searcher",
        vec![waiting, catalog_tool("fetch", None, true)],
    )];
    assert!(screen.answered(&shot, &Display::default()));
    screen.open_page(Page::Plugins);
    screen.selected = Some(Spot::Plugin("searcher".to_owned()));
    let pane = screen.detail(&Quote::default()).unwrap();
    assert_eq!(pane.layers[1].word, "lookup");
    assert!(pane.layers[2].value.starts_with("on"));
    assert!(!pane.layers[2].value.contains("pending"));
    assert_eq!(
        pane.layers[1].value,
        pane.layers[2]
            .value
            .replacen("on", "on \u{b7} pending \u{b7} new chats", 1)
    );
}

#[test]
fn every_setting_describes_its_kind_as_a_unit() {
    let rows: Vec<String> = wisp_protocol::settings::SETTINGS
        .iter()
        .map(|s| format!("{}: {}", s.label, s.describe(Form::Unit)))
        .collect();
    insta::assert_snapshot!(rows.join("\n"));
}

fn shown(name: &str, layer: Layer, at: Option<Place>, text: &str) -> Screen {
    let spec = find(name).unwrap();
    let path = at.map(|place| SitePath::new(place, spec));
    let from = path
        .as_ref()
        .map_or_else(|| "elsewhere".to_owned(), SitePath::to_string);
    let mut rows = defaults();
    put(&mut rows, name, layer, text, &from);
    let value = spec.parse(text).unwrap();
    let saved = path
        .into_iter()
        .map(|p| SavedValue::of(p, value.clone(), Some(2)));
    loaded_with(rows, saved.collect(), ok(), name)
}

fn enter_target(mut screen: Screen) -> SettingTarget {
    press(&mut screen, KeyCode::Enter);
    typed(&mut screen, "9");
    saved(press(&mut screen, KeyCode::Enter)).change.target
}

fn d_target(mut screen: Screen) -> SettingTarget {
    saved(press(&mut screen, KeyCode::Char('d'))).change.target
}

#[test]
fn enter_and_d_act_on_the_number_a_row_with_several_places_shows() {
    let model = Place::Model(MODEL.to_owned());
    let path =
        |n: &str, at: &Place| SettingTarget::Path(SitePath::new(at.clone(), find(n).unwrap()));
    for (n, layer, at) in [
        ("compact_at", Layer::Model, &model),
        ("context.background_tokens", Layer::Model, &model),
    ] {
        let screen = || shown(n, layer.clone(), Some(at.clone()), "300000");
        assert_eq!(enter_target(screen()), path(n, at), "{n}");
        assert_eq!(d_target(screen()), path(n, at), "{n}");
    }
    for n in ["compact_at", "context.background_tokens"] {
        assert_eq!(enter_target(loaded(n)), path(n, &Place::Global), "{n}");
    }
    for n in ["compact_at", "context.background_tokens"] {
        let screen = || shown(n, Layer::Session, None, "300000");
        assert_eq!(enter_target(screen()), path(n, &Place::Global), "{n}");
        assert_eq!(d_target(screen()), named(n), "{n}");
    }
    assert_eq!(
        enter_target(shown("compact_at", Layer::Flag, None, "300000")),
        path("compact_at", &Place::Global)
    );
}

#[test]
fn the_row_shows_the_number_that_applies_and_the_pane_names_where_each_is_saved() {
    let m = "compact_at";
    let own = SitePath::new(Place::Model(MODEL.to_owned()), find(m).unwrap());
    let cases = [
        (Layer::Config, "400000", 1_000_000, "400,000 of 1,000,000"),
        (Layer::Config, "400000", 200_000, "136,000 of 200,000"),
        (Layer::Config, "40%", 1_000_000, "400,000 of 1,000,000"),
        (Layer::Config, "95%", 1_000_000, "850,000 of 1,000,000"),
        (Layer::Model, "400000", 200_000, "136,000 of 200,000"),
        (Layer::Session, "95%", 1_000_000, "850,000 of 1,000,000"),
    ];
    for (layer, text, window, want) in cases {
        let mut rows = defaults();
        let saved_path = match layer {
            Layer::Config => Some(SitePath::global(find(m).unwrap())),
            Layer::Model => Some(own.clone()),
            _ => None,
        };
        let from = saved_path
            .as_ref()
            .map_or_else(|| "session".to_owned(), ToString::to_string);
        let saved = saved_path
            .into_iter()
            .map(|path| SavedValue::of(path, find(m).unwrap().parse(text).unwrap(), Some(1)))
            .collect();
        put(&mut rows, m, layer, text, &from);
        let screen = with_window(rows, saved, ok(), m, Some(window));
        assert_eq!(value_of(&screen, "Compact at"), want, "{text} on {window}");
        let detail = screen.detail(&Quote::default()).unwrap();
        if text == "400000" && window == 200_000 {
            assert_eq!(detail.line, "Saved: 400,000.");
        }
    }
    for (text, window, want, saved_line) in [
        ("150000", 272_000, "150,000 of 272,000", ""),
        ("400000", 470_589, "400,000 of 470,589", ""),
        ("900000", 872_000, "741,200 of 872,000", "Saved: 900,000."),
    ] {
        let spec = find(m).unwrap();
        let saved = vec![SavedValue::of(
            SitePath::global(spec),
            spec.parse(text).unwrap(),
            Some(1),
        )];
        let mut rows = defaults();
        put(&mut rows, m, Layer::Config, text, m);
        let mut shot = snapshot(rows, saved, ok());
        shot.context.window = Some(window);
        shot.context.room = Some(WindowRoom {
            base: 272_000,
            max: 872_000,
        });
        let mut screen = Screen::opening(RequestId(1), m);
        assert!(screen.answered(&shot, &Display::default()));
        assert_eq!(value_of(&screen, "Compact at"), want);
        let detail = screen.detail(&Quote::default()).unwrap();
        assert_eq!(detail.line, saved_line);
        assert_eq!(detail.default, "208,000");
        assert_eq!(detail.default_origin, "of this model's 272,000 tokens");
    }
    let mut rows = defaults();
    put(&mut rows, m, Layer::Config, "300000", m);
    put(&mut rows, m, Layer::Model, "400000", &own.to_string());
    let (all, spec) = (SitePath::global(find(m).unwrap()), find(m).unwrap());
    let saved = vec![
        SavedValue::of(all, spec.parse("300000").unwrap(), Some(1)),
        SavedValue::of(own, spec.parse("400000").unwrap(), Some(3)),
    ];
    let pane = loaded_with(rows, saved, ok(), m)
        .detail(&Quote::default())
        .unwrap();
    let origins: Vec<&str> = pane.layers.iter().map(|l| l.origin.as_str()).collect();
    let words = [
        "this model · config.toml line 3",
        "all models · config.toml line 1",
    ];
    assert_eq!(origins, words);
    assert_eq!(pane.layers[1].note, None);
}

#[test]
fn the_first_screen_has_seven_settings_and_two_doors_with_a_mark_when_rows_are_hidden() {
    let mut shot = snapshot(defaults(), Vec::new(), ok());
    shot.plugins = vec![search::search_plugin(true, true, None)];
    let mut screen = loaded("");
    screen.ask(RequestId(1));
    assert!(screen.answered(&shot, &Display::default()));
    let (entries, _) = screen.entries();
    let labels: Vec<&str> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Row(row) => Some(row.label.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        [
            "Model",
            "Effort",
            "Account",
            "Web search",
            "Tools",
            "Compact at",
            "Theme",
            "Plugins",
            "More settings…"
        ]
    );
    screen.spaced(16, 8);
    screen.fit(8);
    assert_eq!(screen.top(), 0);
    assert!(screen.entries().0.len() > 8);
    screen.spaced(17, 10);
    assert!(screen.entries().0.iter().any(|e| matches!(e, Entry::Gap)));
}

#[test]
fn more_opens_groups_without_first_screen_rows_and_back_returns_to_the_leading_row() {
    let mut screen = loaded("");
    assert_eq!(press(&mut screen, KeyCode::Left), Act::Stay);
    assert_eq!(screen.current, Page::Home);
    press(&mut screen, KeyCode::End);
    assert_eq!(press(&mut screen, KeyCode::Right), Act::Stay);
    assert_eq!(screen.current, Page::More);
    assert_eq!(press(&mut screen, KeyCode::Down), Act::Stay);
    assert_eq!(
        screen.selected_spot(),
        Some(&Spot::Open(Page::Group(Section::Compaction)))
    );
    assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
    assert_eq!(screen.current, Page::Group(Section::Compaction));
    assert!(!screen.shown().contains(&Spot::Setting(names::COMPACT_AT)));
    let draft = screen.draft();
    let mut restored = Screen::restored(RequestId(1), draft);
    assert!(restored.answered(&snapshot(defaults(), Vec::new(), ok()), &Display::default()));
    assert_eq!(restored.current, screen.current);
    assert_eq!(press(&mut screen, KeyCode::Left), Act::Stay);
    assert_eq!(
        screen.selected_spot(),
        Some(&Spot::Open(Page::Group(Section::Compaction)))
    );
    assert_eq!(press(&mut screen, KeyCode::Esc), Act::Stay);
    assert_eq!(screen.selected_spot(), Some(&Spot::Open(Page::More)));
    assert_eq!(press(&mut screen, KeyCode::Esc), Act::Close);
}

#[test]
fn a_filter_finds_a_row_behind_more_and_enter_edits_it_without_losing_the_matches() {
    let mut screen = loaded("");
    press(&mut screen, KeyCode::Char('/'));
    typed(&mut screen, "background");
    let (entries, map) = screen.entries();
    assert!(
        entries
            .iter()
            .any(|e| matches!(e, Entry::Heading(h) if h == "Compaction"))
    );
    assert!(map.iter().flatten().all(|s| !matches!(s, Spot::Open(_))));
    press(&mut screen, KeyCode::Enter);
    press(&mut screen, KeyCode::Down);
    let before = screen.shown();
    assert!(before.len() > 1);
    assert_eq!(
        screen.selected().unwrap().spec.name,
        names::CONTEXT_BACKGROUND_TOKENS
    );
    assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
    assert_eq!(screen.current, Page::Home);
    assert!(screen.edit().is_some());
    assert_eq!(screen.filter(), "background");
    assert_eq!(screen.shown(), before);
    press(&mut screen, KeyCode::Esc);
    assert_eq!(screen.shown(), before);
}

#[test]
fn right_on_a_plugin_lists_its_sections_and_hooks_and_left_returns_to_it() {
    let mut plugins = sectioned(None);
    plugins[0].hooks.push(hook_entry(
        "runner/fill",
        "before-turn",
        0,
        Some(names::CONTEXT_CONTEXT_NOTE),
        None,
    ));
    let mut screen = section_screen(plugins, &Display::default());
    screen.open_page(Page::Plugins);
    assert_eq!(press(&mut screen, KeyCode::Right), Act::Stay);
    assert_eq!(screen.current, Page::Plugin("runner".to_owned()));
    assert_eq!(
        screen.shown(),
        [
            Spot::Section("runner/usage".to_owned()),
            Spot::Hook("runner/fill".to_owned())
        ]
    );
    let labels: Vec<_> = screen
        .entries()
        .0
        .into_iter()
        .filter_map(|e| match e {
            Entry::Row(row) => Some(row.label),
            _ => None,
        })
        .collect();
    assert_eq!(labels, ["usage", "fill"]);
    let detail = screen.detail(&Quote::default()).unwrap();
    assert_eq!(detail.lead, "usage");
    assert_eq!(detail.title, "the sidebar section \"Usage\"");
    assert_eq!(hints_text(&detail.keys), "enter turn off");
    press(&mut screen, KeyCode::Char('/'));
    typed(&mut screen, "usage");
    assert!(
        screen
            .entries()
            .0
            .iter()
            .any(|e| matches!(e, Entry::Row(row) if row.label == "runner/usage"))
    );
    press(&mut screen, KeyCode::Esc);
    press(&mut screen, KeyCode::Down);
    assert_eq!(press(&mut screen, KeyCode::Enter), Act::Stay);
    assert_eq!(screen.current, Page::Group(Section::Chat));
    assert_eq!(
        screen.selected_spot(),
        Some(&Spot::Setting(names::CONTEXT_CONTEXT_NOTE))
    );
    screen.open_page(Page::Plugin("runner".to_owned()));
    assert_eq!(press(&mut screen, KeyCode::Left), Act::Stay);
    assert_eq!(
        screen.selected_spot(),
        Some(&Spot::Plugin("runner".to_owned()))
    );
}

#[test]
fn chat_lines_and_marks_distinguish_chat_values_from_other_overrides() {
    for (layer, now, saved, marked, line) in [
        (
            Layer::Flag,
            "high",
            Some("medium"),
            true,
            "This chat: high. New chats: medium.",
        ),
        (
            Layer::Session,
            "high",
            Some("medium"),
            true,
            "This chat: high. New chats: medium.",
        ),
        (
            Layer::Mode,
            "high",
            Some("medium"),
            false,
            "Set by the mode. Saved: medium.",
        ),
        (
            Layer::Agent,
            "high",
            Some("medium"),
            false,
            "Set by the preset. Saved: medium.",
        ),
        (
            Layer::Env,
            "high",
            Some("medium"),
            false,
            "Set outside Wisp. Saved: medium.",
        ),
        (
            Layer::Forced,
            "high",
            Some("medium"),
            false,
            "Set outside Wisp. Saved: medium.",
        ),
        (
            Layer::Env,
            "high",
            None,
            false,
            "Set outside Wisp. Wisp's own: medium.",
        ),
        (
            Layer::Forced,
            "high",
            None,
            false,
            "Set outside Wisp. Wisp's own: medium.",
        ),
        (
            Layer::Mode,
            "high",
            None,
            false,
            "Set by the mode. Wisp's own: medium.",
        ),
        (
            Layer::Agent,
            "high",
            None,
            false,
            "Set by the preset. Wisp's own: medium.",
        ),
        (Layer::Config, "high", None, false, ""),
        (Layer::Session, "medium", Some("medium"), false, ""),
        (Layer::Default, "medium", None, false, ""),
    ] {
        let mut settings = defaults();
        if let Some(saved) = saved {
            put(&mut settings, "effort", Layer::Config, saved, "effort");
        }
        put(&mut settings, "effort", layer.clone(), now, "an origin");
        let screen = loaded_with(settings, vec![], ok(), "effort");
        let row = screen.selected().unwrap();
        assert_eq!(chat_only(row), marked, "{layer:?}");
        assert_eq!(chat_line(row), line, "{layer:?}");
        if matches!(
            layer,
            Layer::Env | Layer::Forced | Layer::Mode | Layer::Agent
        ) {
            assert!(reset_key(row, &Quote::default()).is_none());
        }
    }
    let mut settings = defaults();
    put(&mut settings, "effort", Layer::Config, "low", "effort");
    put(&mut settings, "effort", Layer::Env, "medium", "WISP_EFFORT");
    put(&mut settings, "effort", Layer::Flag, "high", "--effort");
    let mut screen = loaded_with(settings, vec![], ok(), "effort");
    assert_eq!(
        chat_line(screen.selected().unwrap()),
        "This chat: high. New chats: medium."
    );
    let save = saved(press(&mut screen, KeyCode::Char('d')));
    assert!(!save.save);
    assert_eq!(
        save.change.value,
        Some(SettingValue::Text("medium".to_owned()))
    );
    let mouse = loaded("display.mouse");
    assert!(reset_key(mouse.selected().unwrap(), &Quote::default()).is_none());
}

#[test]
fn tools_and_plugins_count_the_same_three_states() {
    for (on, total, want) in [
        (0, 0, "none on"),
        (0, 12, "none on"),
        (12, 12, "all on"),
        (11, 12, "11 of 12"),
        (15, 17, "15 of 17"),
    ] {
        assert_eq!(rows::count(on, total), want);
    }
    for (saved, want) in [
        (None, "all on"),
        (Some("ask"), "1 of 5"),
        (Some("none"), "none on"),
    ] {
        let screen = loaded_with(with_tools(TOOLS, saved), vec![], ok(), "tools");
        assert_eq!(value_of(&screen, "Tools"), want);
    }
}

#[test]
fn often_used_is_absent_when_empty_and_lists_only_three_rows_from_groups_or_plugins() {
    let mut screen = loaded("");
    let heading = Entry::Heading(wisp_render::chrome::config_screen::OFTEN_USED.to_owned());
    assert!(!screen.entries().0.contains(&heading));
    let mut snap = snapshot(defaults(), Vec::new(), ok());
    snap.plugins = vec![plugins::declared_plugin("often-used")];
    let spec = find("plugins.often-used.minutes").unwrap();
    snap.settings.push(setting(
        spec.name,
        spec.default_value(),
        Layer::Default,
        "default",
    ));
    snap.often = [
        names::MODEL,
        names::DISPLAY_THEME,
        names::CONTEXT_VERBATIM_USER_TOKENS,
        names::DISPLAY_MOUSE,
        spec.name,
        names::DEBUG_CACHE,
    ]
    .map(str::to_owned)
    .to_vec();
    screen = Screen::opening(RequestId(1), "");
    assert!(screen.answered(&snap, &Display::default()));
    let shown = screen.shown();
    assert_eq!(
        shown
            .iter()
            .filter(|spot| **spot == Spot::Setting(names::MODEL))
            .count(),
        1
    );
    let often: Vec<_> = shown
        .iter()
        .filter_map(Spot::setting)
        .filter(|name| !EVERYDAY.contains(name))
        .collect();
    assert_eq!(
        often,
        [
            names::CONTEXT_VERBATIM_USER_TOKENS,
            names::DISPLAY_MOUSE,
            spec.name
        ]
    );
    assert!(screen.entries().0.contains(&heading));
    screen.sent(RequestId(2), &[names::TIME], true);
    let event = changed(RequestId(2), names::TIME, When::Now);
    assert_eq!(screen.used(&event), [names::TIME]);
    assert!(screen.saved(&event, false));
    assert!(screen.used(&event).is_empty());
    screen.ask(RequestId(3));
    snap.answers = RequestId(3);
    snap.often.reverse();
    assert!(screen.answered(&snap, &Display::default()));
    assert_eq!(screen.shown(), shown);
    snap.answers = RequestId(1);
    snap.often = vec![names::TIME.into()];
    let mut reopened = Screen::opening(RequestId(1), "");
    reopened.answered(&snap, &Display::default());
    assert_eq!(
        reopened.often_spots().collect::<Vec<_>>(),
        [Spot::Setting(names::TIME)]
    );
}

#[test]
fn only_an_applied_persisted_save_of_a_group_or_plugin_setting_counts_as_used() {
    let plugin = plugins::declared_plugin("counted-row");
    let spec = find("plugins.counted-row.minutes").unwrap();
    let mut screen = plugins::opened_with(plugin, Vec::new(), defaults(), Vec::new());
    screen.open_page(Page::setting(spec.name));
    for (name, save, applied, counted) in [
        (spec.name, true, true, true),
        (spec.name, true, false, false),
        (spec.name, false, true, false),
        (names::DISPLAY_MOUSE, true, true, true),
        (names::DISPLAY_MOUSE, true, false, false),
        (names::DISPLAY_MOUSE, false, true, false),
        (names::DISPLAY_THEME, true, true, false),
    ] {
        screen.sent(RequestId(2), &[name], save);
        let event = if applied {
            changed(RequestId(2), name, When::Now)
        } else {
            AgentEvent::refused(RequestId(2), "locked")
        };
        assert_eq!(
            !screen.used(&event).is_empty(),
            counted,
            "{name} {save} {applied}"
        );
        assert!(screen.saved(&event, false));
    }
    screen.sent_spot(RequestId(3), Spot::Plugin("example".into()));
    assert!(
        screen
            .used(&changed(RequestId(3), names::DISPLAY_MOUSE, When::Now))
            .is_empty()
    );
}

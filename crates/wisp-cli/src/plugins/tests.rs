use wisp_protocol::settings::SettingValue;

use super::*;

fn removed_time() -> SavedSettings {
    let mut saved = SavedSettings::default();
    let path = SitePath::plugin_switch("time", INSTALLED_KEY).unwrap();
    saved.insert(path, SettingValue::Bool(false), 1);
    let path = SitePath::plugin_switch("context", ENABLED_KEY).unwrap();
    saved.insert(path, SettingValue::Bool(false), 2);
    saved
}

#[test]
fn the_list_names_each_plugin_with_its_state_tools_and_settings() {
    let (registry, _) = crate::tools::registry();
    let catalog = crate::tools::catalog(&registry);
    let shown = listing(&catalog.plugins, &removed_time());
    let time = shown
        .split_once("time ")
        .map(|(_, rest)| rest.lines().take(3).collect::<Vec<_>>().join("\n"))
        .unwrap();
    assert!(time.starts_with("0.1.0 removed\n"), "{shown}");
    assert!(time.contains("\n  settings: time"), "{shown}");
    assert!(shown.contains("\ncontext "), "{shown}");
    let context = shown.lines().find(|l| l.starts_with("context ")).unwrap();
    assert!(context.ends_with(" off"), "{context}");
    let shell = shown.lines().find(|l| l.starts_with("shell ")).unwrap();
    assert!(shell.ends_with(" on"), "{shell}");
    assert!(shown.contains("  tools: bash, process\n"), "{shown}");
}

#[test]
fn the_bundled_agent_viz_declares_the_section_agents_opens() {
    let (registry, _) = crate::tools::registry();
    let catalog = crate::tools::catalog(&registry);
    let viz = catalog
        .plugins
        .iter()
        .find(|p| p.id == "agent-viz")
        .unwrap();
    let sections: Vec<&str> = viz.slots.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(sections, [wisp_tui::app::sidebar::AGENTS_SECTION]);
    assert!(viz.slots.iter().all(|s| s.default_on));
}

#[test]
fn adding_from_files_without_wasm_is_refused_with_the_usage_code() {
    assert_eq!(files_need_wasm(), ExitCode::from(USAGE_EXIT));
    assert_eq!(
        FROM_FILES_NEEDS_WASM,
        "adding a plugin from files needs a wisp built with --features wasm"
    );
}

#[test]
fn hostile_plugin_strings_are_sanitised_in_the_list() {
    let entry = PluginEntry {
        id: "evil".into(),
        version: "1.0\u{1b}[2J".into(),
        summary: "line\u{1b}]0;title\u{7}".into(),
        source: wisp_protocol::PluginSource::Bundled,
        installed: true,
        enabled: true,
        tools: vec![wisp_protocol::plugin::ToolEntry {
            name: "t\u{1b}[31m".into(),
            on: true,
            held: false,
            default_on: true,
            native: false,
            follows: None,
            unavailable: None,
            waits: false,
        }],
        commands: Vec::new(),
        hooks: Vec::new(),
        slots: Vec::new(),
        prompt_sections: Vec::new(),
        settings: vec![wisp_protocol::plugin::Setting::Name("s\u{1b}[1m".into())],
        problems: vec!["bad\u{1b}[0m\u{7}".into()],
    };
    let shown = listing(&[entry], &SavedSettings::default());
    assert!(!shown.contains('\u{1b}'), "{shown:?}");
    assert!(!shown.contains('\u{7}'), "{shown:?}");
    assert!(shown.starts_with("evil 1.0"), "{shown:?}");
    assert!(shown.contains("  problem: bad"), "{shown:?}");
}

fn testkit_manifest(id: &str, hooks: &str) -> wisp_plugin_api::manifest::Manifest {
    wisp_plugin_api::manifest::Manifest::parse(
        &wisp_plugin_testkit::hooks::manifest(id, hooks),
        wisp_plugin_api::manifest::Origin::Bundled,
    )
    .unwrap()
}

fn hook(name: &str, point: &str, extra: &str) -> String {
    format!("\n[[hooks]]\nname = \"{name}\"\npoint = \"{point}\"\n{extra}")
}

#[test]
fn the_hook_list_prints_each_points_order_with_cycles_requires_and_switches() {
    let manifests = [
        testkit_manifest(
            "aa",
            &[
                hook("plain", "before-turn", ""),
                hook("loop", "after-turn", "after = [\"bb/loop\"]\n"),
                hook(
                    "needs",
                    "before-turn",
                    "requires = [\"zz/gone\", \"zz/\\u001B[31mx\"]\n",
                ),
                hook("quiet", "after-turn", ""),
            ]
            .concat(),
        ),
        testkit_manifest("bb", &hook("loop", "after-turn", "after = [\"aa/loop\"]\n")),
        testkit_manifest("cc", &hook("x", "session-start", "")),
    ];
    let table = Table::build(&manifests);
    let mut saved = SavedSettings::default();
    saved.insert(
        SitePath::plugin_hook("aa", "quiet").unwrap(),
        SettingValue::Bool(false),
        1,
    );
    saved.insert(
        SitePath::plugin_switch("cc", ENABLED_KEY).unwrap(),
        SettingValue::Bool(false),
        2,
    );
    let switches = start_switches(&[], &saved);
    let shown = hooks_listing(&table, &switches, &saved);
    assert_eq!(
        shown,
        "after-turn\n\
         \x20 aa/quiet \u{b7} off\n\
         \x20 aa/loop \u{b7} off (in a cycle: aa/loop \u{2192} bb/loop \u{2192} aa/loop)\n\
         \x20 bb/loop \u{b7} off (in a cycle: aa/loop \u{2192} bb/loop \u{2192} aa/loop)\n\
         before-turn\n\
         \x20 core/tools\n\
         \x20 aa/needs \u{b7} off (requires zz/gone, zz/^[[31mx)\n\
         \x20 aa/plain \u{b7} on\n\
         \x20 core/typed\n\
         session-start\n\
         \x20 cc/x \u{b7} off (plugin off)\n"
    );
}

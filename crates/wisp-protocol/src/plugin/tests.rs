use super::*;

fn tool(name: &str, follows: Option<&str>, default_on: bool) -> ToolEntry {
    ToolEntry {
        name: name.into(),
        on: default_on,
        default_on,
        native: false,
        follows: follows.map(Into::into),
        unavailable: None,
        waits: false,
    }
}

fn plugin(id: &str, tools: Vec<ToolEntry>) -> PluginEntry {
    PluginEntry {
        prompt_sections: Vec::new(),
        id: id.into(),
        version: "0.1.0".into(),
        summary: id.into(),
        source: PluginSource::Bundled,
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

#[test]
fn a_record_round_trips_and_an_unknown_source_reads_as_unknown() {
    let record = PluginRecord {
        id: "show-diff".into(),
        version: "0.1.0".into(),
        source: PluginSource::Bundled,
    };
    let text = serde_json::to_string(&record).unwrap();
    assert_eq!(
        text,
        r#"{"id":"show-diff","version":"0.1.0","source":{"type":"bundled"}}"#
    );
    assert_eq!(serde_json::from_str::<PluginRecord>(&text).unwrap(), record);
    let wasm: PluginSource = serde_json::from_str(r#"{"type":"wasm","sha256":"ab"}"#).unwrap();
    assert_eq!(
        wasm,
        PluginSource::Wasm {
            sha256: "ab".into()
        }
    );
    let later: PluginSource = serde_json::from_str(r#"{"type":"git","url":"x"}"#).unwrap();
    assert_eq!(later, PluginSource::Unknown);
}

#[test]
fn an_unknown_tool_is_named_only_when_it_cant_be_a_pasted_key() {
    let catalog = Catalog {
        plugins: vec![plugin(
            "shell",
            vec![
                tool("bash", None, true),
                tool("process", Some("bash"), true),
            ],
        )],
    };
    for pasted in [
        "ghp_CANARY0123456789abcdef",
        &"ghp_canary012345CANARYcdef".to_ascii_lowercase(),
        "canary0123456789abcdefgh",
        "github_pat_11CANARY0123456789",
        "sk-ant-CANARY0123456789abcdef",
    ] {
        let error = catalog.checked_names(&[pasted.to_owned()]).unwrap_err();
        assert_eq!(error, "that is no tool; tools: bash, or none", "{pasted}");
    }
    for name in ["nope", "ask_secret", "canary_token_value"] {
        assert_eq!(
            catalog.checked_names(&[name.to_owned()]),
            Err(format!("{name} is no tool; tools: bash, or none"))
        );
    }
    assert_eq!(
        catalog.checked_names(&["process".to_owned()]),
        Err("process comes with bash; name bash instead".to_owned())
    );
}

#[test]
fn a_follower_beside_its_leader_is_dropped_and_alone_names_its_leader_or_its_children() {
    let catalog = Catalog {
        plugins: vec![plugin(
            "agents",
            vec![
                tool("list_agents", Some("spawn_agent"), true),
                tool("quiet", Some("spawn_agent"), false),
                tool("report", Some("spawn_agent"), false),
                tool("spawn_agent", None, true),
            ],
        )],
    };
    let names = |n: &[&str]| -> Vec<String> { n.iter().map(|s| (*s).to_owned()).collect() };
    let all = names(&["list_agents", "report", "spawn_agent"]);
    assert_eq!(catalog.chosen(&all), names(&["spawn_agent"]));
    assert_eq!(catalog.checked_names(&all), Ok(()));
    assert_eq!(
        catalog.checked_names(&names(&["list_agents"])),
        Err("list_agents comes with spawn_agent; name spawn_agent instead".to_owned())
    );
    assert_eq!(
        catalog.checked_names(&names(&["report"])),
        Err("report is for child agents only; spawn_agent gives it to every child".to_owned())
    );
    assert_eq!(
        catalog.checked_names(&names(&["quiet"])),
        Err("quiet follows spawn_agent but doesn't come with it, so it can't be named".to_owned())
    );
    assert_eq!(catalog.follower_note("spawn_agent"), None);
    assert_eq!(
        crate::agents::tools_off_followers(&catalog),
        [("spawn_agent".to_owned(), "list_agents".to_owned())]
    );
}

#[test]
fn a_list_is_the_default_after_followers_and_left_out_plugins_drop_out() {
    let catalog = Catalog {
        plugins: vec![
            plugin(
                "agents",
                vec![
                    tool("list_agents", Some("spawn_agent"), true),
                    tool("report", Some("spawn_agent"), false),
                    tool("spawn_agent", None, true),
                ],
            ),
            plugin("board", vec![tool("board", None, true)]),
            plugin("page", vec![tool("show_page", None, false)]),
            plugin(
                "view",
                vec![ToolEntry {
                    unavailable: Some("not available on this platform".into()),
                    ..tool("view_file", None, true)
                }],
            ),
        ],
    };
    let names = |n: &[&str]| -> Vec<String> { n.iter().map(|s| (*s).to_owned()).collect() };
    let none = |_: &str| false;
    let board_off = |id: &str| id == "board";
    assert!(catalog.is_default(&names(&["board", "spawn_agent"]), none));
    assert!(catalog.is_default(
        &names(&["board", "list_agents", "report", "spawn_agent"]),
        none
    ));
    assert!(catalog.is_default(&names(&["board", "spawn_agent", "view_file"]), none));
    assert!(catalog.is_default(&names(&["spawn_agent"]), board_off));
    assert!(catalog.is_default(&names(&["board", "spawn_agent"]), board_off));
    assert!(!catalog.is_default(&names(&["spawn_agent"]), none));
    assert!(!catalog.is_default(&names(&["board", "show_page", "spawn_agent"]), none));
    assert!(!catalog.is_default(&names(&["board", "spawn_agent", "telepathy"]), none));
    assert!(!catalog.is_default(&names(&["board", "list_agents"]), none));
}

#[test]
fn a_catalog_setting_is_a_name_or_a_row() {
    let mut entry = plugin("catalog-rows", Vec::new());
    entry.settings = serde_json::from_str(r#"["time",{"name":"minutes","label":"Minutes","help":"Minutes before away.","kind":{"type":"int","min":1,"max":60,"default":10}},{"name":"future","label":"Future","help":"A future row.","kind":{"type":"future","default":"unknown"}}]"#).unwrap();
    assert!(matches!(&entry.settings[0], Setting::Name(name) if name == "time"));
    assert!(matches!(
        &entry.settings[1],
        Setting::Row(SettingRow {
            kind: RowKind::Int { default: 10, .. },
            ..
        })
    ));
    assert!(matches!(
        &entry.settings[2],
        Setting::Row(SettingRow {
            kind: RowKind::Unknown,
            ..
        })
    ));
    let catalog = Catalog {
        plugins: vec![entry],
    };
    let encoded = serde_json::to_string(&catalog).unwrap();
    assert_eq!(serde_json::from_str::<Catalog>(&encoded).unwrap(), catalog);
    assert_eq!(
        catalog.plugins[0].setting_names().collect::<Vec<_>>(),
        [
            "time",
            "plugins.catalog-rows.minutes",
            "plugins.catalog-rows.future"
        ]
    );
    catalog.declare();
    assert!(crate::settings::find("plugins.catalog-rows.minutes").is_some());
    assert!(crate::settings::find("plugins.catalog-rows.future").is_none());
}

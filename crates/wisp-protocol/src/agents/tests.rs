use super::*;

#[test]
fn preset_names_follow_the_agent_name_rule() {
    for good in ["reviewer", "a", "plan-2", "deep_research", &"a".repeat(32)] {
        assert!(valid_preset_name(good), "{good}");
    }
    for bad in [
        "",
        "Reviewer",
        "2fast",
        "-x",
        "_x",
        "has space",
        "a.b",
        &"a".repeat(33),
    ] {
        assert!(!valid_preset_name(bad), "{bad}");
    }
}

#[test]
fn a_preset_name_is_shown_only_when_it_is_valid_and_not_key_shaped() {
    assert_eq!(shown_preset_name("reviewer"), Some("reviewer"));
    assert_eq!(shown_preset_name("Reviewer"), None);
    assert_eq!(shown_preset_name("abcdefghij0123456789xyz"), None);
    assert_eq!(shown_preset_name("ab-cdefghij0123456789"), None);
    assert_eq!(preset_table(Some("reviewer")), "[agents.presets.reviewer]");
    assert_eq!(preset_table(None), "[agents.presets.NAME]");
}

#[test]
fn a_key_shaped_description_is_not_shown_but_a_dated_model_is() {
    for plain in [
        "Reviews a change it did not write",
        "Runs claude-sonnet-4-5-20250929 for speed",
        "Plans in 3 steps",
    ] {
        assert_eq!(crate::shown_text(plain), plain);
    }
    for keyed in [
        "Uses sk-ant-api03-CANARY0123456789abcdef",
        "token hf_CANARYGhIjKlMnOpQrStUv",
    ] {
        assert_eq!(crate::shown_text(keyed), crate::NOT_SHOWN, "{keyed}");
    }
}

fn catalog(tools: &[(&str, Option<&str>)]) -> Catalog {
    Catalog {
        plugins: vec![crate::plugin::PluginEntry {
            prompt_sections: Vec::new(),
            id: "p".into(),
            version: "0.1.0".into(),
            summary: "P".into(),
            source: crate::PluginSource::Bundled,
            installed: true,
            enabled: true,
            tools: tools
                .iter()
                .map(|(name, follows)| crate::plugin::ToolEntry {
                    name: (*name).into(),
                    on: true,
                    default_on: true,
                    native: false,
                    follows: follows.map(str::to_owned),
                    unavailable: None,
                    waits: false,
                })
                .collect(),
            commands: Vec::new(),
            hooks: Vec::new(),
            slots: Vec::new(),
            settings: Vec::new(),
            problems: Vec::new(),
        }],
    }
}

#[test]
fn tools_off_takes_the_catalog_and_the_later_tools_but_not_the_kept_ones() {
    let catalog = catalog(&[
        ("bash", None),
        ("process", Some("bash")),
        ("write_todos", None),
        ("web_search", None),
        ("list_agents", None),
        ("report", None),
        ("spawn_agent", None),
    ]);
    assert_eq!(
        tools_off_names(&catalog),
        ["bash", "stop_agent", "web_search", "write_todos"]
    );
    for kept in KEPT_TOOLS.into_iter().chain([SPAWN_AGENT]) {
        assert!(
            !tools_off_names(&catalog).iter().any(|t| t == kept),
            "{kept}"
        );
        assert!(AGENT_TOOLS.contains(&kept), "{kept}");
    }
    assert!(SPAWN_TOOLS.iter().all(|t| AGENT_TOOLS.contains(t)));
}

#[test]
fn a_preset_serialises_only_what_it_sets() {
    let preset = Preset {
        name: "reviewer".into(),
        description: "Reviews".into(),
        model: None,
        effort: Some(Effort::High),
        preload: None,
        tools_off: vec!["show_diff".into()],
        spawn: None,
        instructions: Some("roles/reviewer".into()),
        endpoint: None,
        fallback: Vec::new(),
        worker: Some(false),
    };
    let text = serde_json::to_string(&preset).unwrap();
    assert_eq!(
        text,
        r#"{"name":"reviewer","description":"Reviews","effort":"high","tools_off":["show_diff"],"instructions":"roles/reviewer","worker":false}"#
    );
    assert_eq!(serde_json::from_str::<Preset>(&text).unwrap(), preset);
    let old = r#"{"name":"reviewer","description":"Reviews","report":"review"}"#;
    let read: Preset = serde_json::from_str(old).unwrap();
    assert_eq!(read.instructions, None);
    assert_eq!(read.endpoint, None);
    assert_eq!(read.description, "Reviews");
    assert!(read.fallback.is_empty());
    let mut with_fallback = preset;
    with_fallback.fallback.push(super::Choice {
        endpoint: "other".into(),
        model: Some("gpt-6-astra".into()),
        effort: None,
    });
    let text = serde_json::to_string(&with_fallback).unwrap();
    assert_eq!(
        serde_json::from_str::<Preset>(&text).unwrap(),
        with_fallback
    );
}

#[test]
fn the_preset_fields_are_sorted_and_name_every_tool_tools_off_takes() {
    let keys = preset_keys();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted);
    assert_eq!(
        keys,
        [
            "description",
            "effort",
            "endpoint",
            "fallback",
            "instructions",
            "model",
            "preload",
            "spawn",
            "tools_off",
            "worker"
        ]
    );
    let tools_off = PRESET_FIELDS.iter().find(|f| f.key == "tools_off").unwrap();
    assert_eq!(
        tools_off.help_text(),
        "Tools the child doesn't get: any `--tools` value from the plugin catalog, or stop_agent."
    );
    let description = PRESET_FIELDS
        .iter()
        .find(|f| f.key == "description")
        .unwrap();
    assert_eq!(description.help_text(), description.help);
    let effort = PRESET_FIELDS.iter().find(|f| f.key == "effort").unwrap();
    for level in Effort::ALL {
        assert!(effort.values.contains(level.as_str()), "{level:?}");
    }
}

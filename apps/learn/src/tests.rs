use super::*;

fn flow(name: &str) -> String {
    std::fs::read_to_string(Path::new(FLOW).join(name)).unwrap()
}

#[test]
fn the_flow_names_the_hand_off_labels_and_ignores_its_private_files() {
    use wisp_tool_text::voice::{ASKED, CUT_OFF, EARLIER, HEARD, SAID, TAPPED, finished};
    let brain = flow("BRAIN.md");
    for label in [HEARD, SAID, TAPPED, ASKED, CUT_OFF] {
        assert!(brain.contains(&format!("`{label}`")), "{label}: {brain}");
    }
    let changed = finished(wisp_plugin_page::NAME);
    for quoted in [changed.as_str(), EARLIER] {
        assert!(
            flow("VOICE.md").contains(&format!("`{quoted}`")),
            "{quoted}"
        );
    }
    use wisp_core::secrets::{CREDENTIALS, LOCK_FILE, OAUTH};
    let files = ["config.toml", CREDENTIALS, LOCK_FILE].map(|file| format!("/{file}"));
    let folders = [OAUTH, FOLDER].map(|folder| format!("/{folder}/"));
    let ignored: Vec<String> = files.into_iter().chain(folders).collect();
    assert_eq!(flow(".gitignore").lines().collect::<Vec<_>>(), ignored);
}

#[test]
fn the_flows_settings_are_given_from_its_folder_and_never_name_an_account() {
    let flow = std::fs::canonicalize(FLOW).unwrap();
    let (flags, _) = flow_settings(&flow).unwrap();
    let voice = flags
        .iter()
        .find(|given| matches!(&given.target, Target::Name(spec) if spec.name == names::VOICE_INSTRUCTIONS))
        .unwrap();
    let file = flow.join("VOICE.md").display().to_string();
    assert_eq!(voice.value, SettingValue::Text(file));
    assert!(flags.iter().all(|given| given.origin == START_ORIGIN));
    let scratch = tempfile::tempdir().unwrap();
    for accounts in [
        "[voice]\nendpoint = \"talk\"\n",
        "default = \"gw\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_TOKEN\"\nurl = \"http://127.0.0.1:9\"\n",
        "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_TOKEN\"\nurl = \"http://127.0.0.1:9\"\n",
    ] {
        std::fs::write(scratch.path().join(SETTINGS), accounts).unwrap();
        assert_eq!(
            flow_settings(scratch.path()).err().as_deref(),
            Some(ACCOUNTS),
            "{accounts}"
        );
    }
}

#[test]
fn a_withdrawn_switch_writes_the_lessons_pair_back_over_its_pick() {
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join(SETTINGS);
    let changed = |withdrawn: Option<&str>, model: &str| -> AgentEvent {
        let mut event = serde_json::json!({"type": "settings_changed", "answers": 7,
            "endpoint": "gw", "model": model, "effort": "low", "cache": {"type": "kept"},
            "changes": [{"target": {"name": "model"}, "value": {"kind": "text", "text": model},
                "layer": "session", "when": "next_turn"}]});
        if let Some(why) = withdrawn {
            event["withdrawn"] = serde_json::json!(why);
            event.as_object_mut().unwrap().remove("answers");
        }
        serde_json::from_value(event).unwrap()
    };
    remember(&changed(None, "claude-sonnet-5-5"), &file).unwrap();
    let picked = "account = \"gw\"\nmodel = \"claude-sonnet-5-5\"\n";
    assert_eq!(std::fs::read_to_string(&file).unwrap(), picked);
    let back = changed(Some("switch_compaction_failed"), "claude-opus-5-5");
    remember(&back, &file).unwrap();
    let restored = "account = \"gw\"\nmodel = \"claude-opus-5-5\"\n";
    assert_eq!(std::fs::read_to_string(&file).unwrap(), restored);
}

use super::*;

fn sets(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn the_resolved_model_takes_the_flag_then_the_saved_value_then_the_default() {
    assert_eq!(resolved_model(&[], None), DEFAULT_MODEL);
    let flags = flag_layer(Vec::new(), &sets(&["model=claude-sonnet-5-5"])).unwrap();
    assert_eq!(resolved_model(&flags, None), "claude-sonnet-5-5");
    let typed = vec![Flag::new(
        "--model",
        "model",
        SettingValue::Text("claude-haiku-5".into()),
    )];
    let flags = flag_layer(typed, &[]).unwrap();
    assert_eq!(resolved_model(&flags, None), "claude-haiku-5");
}

#[test]
fn a_flag_and_a_set_of_one_setting_are_refused_without_their_values() {
    let typed = vec![Flag::new(
        "--effort",
        "effort",
        SettingValue::Text("high".into()),
    )];
    let error = flag_layer(typed, &sets(&["effort=low"])).unwrap_err();
    assert_eq!(error, "--effort and --set effort both set effort; give one");
    let error = flag_layer(Vec::new(), &sets(&["effort=sk-ant-CANARY00000000001"])).unwrap_err();
    assert!(!error.contains("CANARY"), "{error}");
    let error = flag_layer(Vec::new(), &sets(&["effort"])).unwrap_err();
    assert_eq!(error, "--set needs NAME=VALUE (what was given isn't shown)");
    let error = flag_layer(
        Vec::new(),
        &sets(&["endpoints.gw.key=sk-ant-CANARY00000000001"]),
    )
    .unwrap_err();
    assert!(!error.contains("CANARY"), "{error}");
    assert!(error.starts_with("endpoints.gw.key is a secret"), "{error}");
    let key = "sk-ant-api03-CANARY0000000000000000000001";
    let typed = vec![Flag::new(
        "--model",
        "model",
        SettingValue::Text(key.into()),
    )];
    let error = flag_layer(typed, &[]).unwrap_err();
    assert_eq!(error, format!("--model: {KEY_SHAPED_MODEL}"));
    for (name, origin) in [
        ("model", "--set model"),
        (
            r#"models."claude-opus-5-5".aliases"#,
            r#"--set models."claude-opus-5-5".aliases"#,
        ),
    ] {
        let error = flag_layer(Vec::new(), &[format!("{name}={key}")]).unwrap_err();
        assert_eq!(error, format!("{origin}: {KEY_SHAPED_MODEL}"));
    }
    let error = flag_layer(Vec::new(), &[format!("agents.coder.model={key}")]).unwrap_err();
    assert!(!error.contains("CANARY"), "{error}");
}

#[test]
fn a_hook_switch_flag_is_checked_against_the_registry() {
    let error = flag_layer(Vec::new(), &sets(&["plugins.aa.hooks.x=off"])).unwrap_err();
    assert_eq!(
        error,
        "--set plugins.aa.hooks.x: aa/x is no hook; hooks: agent-viz/mail, idle/done, idle/note, idle/seen, mcp/compacted, mcp/list, mcp/refresh, mcp/rewound, shell/processes, skills/compacted, skills/nudge, skills/rewound, skills/scan"
    );
    assert!(flag_layer(Vec::new(), &sets(&["plugins.A.hooks.x=off"])).is_err());
}

#[test]
fn a_section_switch_flag_is_checked_against_the_registry() {
    for bad in [
        "plugins.aa.slots.notes=off",
        "plugins.A.slots.notes=off",
        "plugins.aa.slots.n=off",
        "plugins.aa.slots.notes=maybe",
    ] {
        assert!(flag_layer(Vec::new(), &sets(&[bad])).is_err(), "{bad}");
    }
}

#[test]
fn a_bool_flag_matches_set_the_other_way() {
    let flags = flag_layer(
        Vec::new(),
        &sets(&["debug_cache=off", "refusal_fallback=on"]),
    )
    .unwrap();
    assert_eq!(
        flag_value(&flags, "debug_cache"),
        Some(&SettingValue::Bool(false))
    );
    assert_eq!(
        flag_value(&flags, "refusal_fallback"),
        Some(&SettingValue::Bool(true))
    );
}

#[test]
fn a_fold_or_pin_list_set_on_the_command_line_keeps_any_section_name_and_refuses_bad_grammar() {
    let flags = flag_layer(
        Vec::new(),
        &sets(&[
            "display.sidebar_folded=testkit/busy,gone/section",
            "display.sidebar_pinned=none",
        ]),
    )
    .unwrap();
    assert_eq!(
        flag_value(&flags, "display.sidebar_folded"),
        Some(&SettingValue::List(vec![
            "gone/section".into(),
            "testkit/busy".into()
        ]))
    );
    assert_eq!(
        flag_value(&flags, "display.sidebar_pinned"),
        Some(&SettingValue::List(vec![]))
    );
    let error = flag_layer(Vec::new(), &sets(&["display.sidebar_folded=busy"])).unwrap_err();
    assert_eq!(
        error,
        "--set display.sidebar_folded must be none or a list of sidebar sections, each \
<plugin>/<name> (what was given isn't shown)"
    );
}

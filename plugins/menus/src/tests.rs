use futures::executor::block_on;
use wisp_plugin_api::config::{AccountList, AccountRow, SettingRow};
use wisp_plugin_api::manifest::{Manifest, Origin, Switch};
use wisp_plugin_testkit::host::{FakeCall, FakeHost};

use super::*;

fn plugin(host: &FakeHost) -> Arc<dyn Plugin> {
    instantiate(Arc::new(host.clone()))
}

fn run_tool(host: &FakeHost, input: &Value) -> ToolRun {
    run_named(host, NAME, input)
}

fn run_named(host: &FakeHost, tool: &str, input: &Value) -> ToolRun {
    let plugin = plugin(host);
    let prepared = block_on(plugin.prepare(tool, &input.to_string())).unwrap();
    block_on(plugin.run(
        prepared,
        &FakeCall {
            id: "toolu_1".to_owned(),
            ..FakeCall::default()
        },
    ))
}

fn completed(is_error: bool) -> Outcome {
    Outcome::Completed(Completed {
        is_error,
        exit: None,
    })
}

fn theme() -> SettingRow {
    SettingRow {
        name: "display.theme".to_owned(),
        value: "dark".to_owned(),
        layer: "env".to_owned(),
        help: "The colour theme.".to_owned(),
        rule: "must be one of auto, dark or light".to_owned(),
    }
}

#[test]
fn the_input_is_an_optional_filter_read_strictly() {
    let cases: Vec<(Value, Result<Option<&str>, &str>)> = vec![
        (json!({}), Ok(None)),
        (json!({"filter": null}), Ok(None)),
        (json!({"filter": "  theme "}), Ok(Some("theme"))),
        (json!({"filter": "   "}), Ok(None)),
        (json!({"filter": 3}), Err("`filter` must be a string")),
        (json!({"all": true}), Err("unknown field `all`")),
        (json!(["theme"]), Err("input must be an object")),
    ];
    for (input, expected) in cases {
        assert_eq!(
            parse(NAME, &input.to_string()),
            expected
                .map(|f| f.map(str::to_owned))
                .map_err(str::to_owned),
            "{input}"
        );
    }
    assert!(
        parse(NAME, "{")
            .unwrap_err()
            .starts_with("input is not JSON")
    );
    assert_eq!(parse(ACCOUNTS, "{}"), Ok(None));
    for input in [json!({"filter": "x"}), json!({"all": true})] {
        let field = input.as_object().unwrap().keys().next().unwrap().clone();
        assert_eq!(
            parse(ACCOUNTS, &input.to_string()),
            Err(format!("unknown field `{field}`"))
        );
    }
    assert_eq!(
        block_on(plugin(&FakeHost::default()).prepare("list_agents", "{}"))
            .err()
            .as_deref(),
        Some("the menus plugin has no tool `list_agents`")
    );
}

#[test]
fn the_row_reads_listing_settings_with_the_filter_as_its_target() {
    let shown = |input: Value| {
        block_on(plugin(&FakeHost::default()).prepare(NAME, &input.to_string()))
            .unwrap()
            .display()
    };
    let filtered = shown(json!({"filter": "display"}));
    assert_eq!(filtered.action, Action::Other(NAME.to_owned()));
    assert_eq!(filtered.target, "display");
    assert_eq!(
        filtered.verbs,
        Some(Verbs {
            doing: "Listing settings".to_owned(),
            done: "Listed settings".to_owned(),
            failed: "couldn't list settings".to_owned(),
        })
    );
    assert_eq!(shown(json!({})).target, "");
}

#[test]
fn the_hosts_rows_are_listed_and_the_filter_adds_help_and_rule() {
    let host = FakeHost {
        config: vec![theme()],
        ..FakeHost::default()
    };
    let all = run_tool(&host, &json!({}));
    assert_eq!(all.outcome, completed(false));
    assert_eq!(
        all.result.as_deref(),
        Some("1 setting, with where each value comes from:\n- display.theme: dark (env)")
    );
    let filtered = run_tool(&host, &json!({"filter": "theme"}));
    assert_eq!(
        filtered.result.as_deref(),
        Some(
            "1 setting, with where each value comes from:\n- display.theme: dark (env)\n  The colour theme.\n  must be one of auto, dark or light"
        )
    );
    let none = run_tool(&FakeHost::default(), &json!({"filter": "xyz"}));
    assert_eq!(none.outcome, completed(false));
    assert_eq!(
        none.result.as_deref(),
        Some("No setting matches `xyz`; leave filter out to list them all.")
    );
}

#[test]
fn a_refusal_is_a_tool_error_carrying_the_hosts_reason() {
    let host = FakeHost {
        config: vec![theme()],
        refuse_config: Some(
            "no menus tool call toolu_1 is running, so nothing was done".to_owned(),
        ),
        ..FakeHost::default()
    };
    let run = run_tool(&host, &json!({}));
    assert_eq!(run.outcome, completed(true));
    assert_eq!(
        run.result.as_deref(),
        Some("no menus tool call toolu_1 is running, so nothing was done")
    );
}

#[test]
fn the_row_reads_listing_accounts_with_no_target_and_the_result_is_the_hosts_list() {
    let shown = block_on(plugin(&FakeHost::default()).prepare(ACCOUNTS, "{}"))
        .unwrap()
        .display();
    assert_eq!(shown.action, Action::Other(ACCOUNTS.to_owned()));
    assert_eq!(shown.target, "");
    assert_eq!(
        shown.verbs,
        Some(Verbs {
            doing: "Listing accounts".to_owned(),
            done: "Listed accounts".to_owned(),
            failed: "couldn't list accounts".to_owned(),
        })
    );
    let list = AccountList {
        model: "opus-5-5".to_owned(),
        effort: "high".to_owned(),
        rows: vec![AccountRow {
            name: "claude-api".to_owned(),
            yours: true,
            ..AccountRow::default()
        }],
    };
    let host = FakeHost {
        accounts: list.clone(),
        ..FakeHost::default()
    };
    let run = run_named(&host, ACCOUNTS, &json!({}));
    assert_eq!(run.outcome, completed(false));
    assert_eq!(
        run.result.as_deref(),
        Some(wisp_tool_text::config::accounts(&list, 0).as_str())
    );
}

#[test]
fn two_static_read_only_tools_of_five_and_ten_seconds_with_no_system_line() {
    let one = block_on(plugin(&FakeHost::default()).describe(&BuildContext {
        platform: "unix".to_owned(),
        capabilities: vec!["shell".to_owned()],
    }));
    let two = block_on(plugin(&FakeHost::default()).describe(&BuildContext {
        platform: "windows".to_owned(),
        capabilities: Vec::new(),
    }));
    assert_eq!(one, two);
    assert_eq!(one, defs());
    let names: Vec<&str> = one.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, [NAME, ACCOUNTS]);
    for (def, limit) in one.iter().zip([5_000, 10_000]) {
        assert_eq!(def.system_line, None);
        assert_eq!(def.context_line, None);
        let ToolKind::Client(client) = &def.kind else {
            panic!("{:?}", def.kind);
        };
        assert_eq!(client.meta.effect, Effect::ReadOnly);
        assert_eq!(
            client.meta.timeouts,
            Timeouts {
                default_ms: limit,
                max_ms: Some(limit)
            }
        );
        assert!(!client.meta.needs_workspace && !client.meta.asks_user && !client.meta.ends_reply);
        assert_eq!(
            block_on(plugin(&FakeHost::default()).time_limit_ms(&def.name, "{}")),
            Some(limit)
        );
        let schema: Value = serde_json::from_str(&client.input_schema).unwrap();
        assert_eq!(schema["additionalProperties"], false);
        assert!(schema.get("required").is_none());
    }
    let ToolKind::Client(settings) = &one[0].kind else {
        panic!("{:?}", one[0].kind);
    };
    let schema: Value = serde_json::from_str(&settings.input_schema).unwrap();
    assert_eq!(schema, self::schema());
    assert_eq!(schema["properties"]["filter"]["type"], "string");
    assert!(settings.description.contains("point them to /config"));
    let manifest = Manifest::parse(MANIFEST, Origin::Bundled).unwrap();
    assert_eq!(manifest.id, ID);
    assert_eq!(manifest.world, "0.2.0");
    assert_eq!(
        manifest.summary,
        "Lets the model read what /config, /account and /usage show"
    );
    let tools: Vec<(&str, Option<&str>, Switch)> = manifest
        .tools
        .iter()
        .map(|t| (t.name.as_str(), t.follows.as_deref(), t.default))
        .collect();
    assert_eq!(
        tools,
        [(NAME, None, Switch::On), (ACCOUNTS, None, Switch::On)]
    );
    assert!(manifest.hooks.is_empty());
    assert!(manifest.system.is_none());
    assert!(manifest.settings.is_empty());
}

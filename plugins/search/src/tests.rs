mod skills;

use futures::executor::block_on;
use wisp_plugin_testkit::host::{FakeCall, FakeHost};

use super::*;
use wisp_plugin_api::index::Entry;

fn host() -> FakeHost {
    let entries = [
        (Kind::Tool, "create_issue"),
        (Kind::Skill, "issue-style"),
        (Kind::Document, "issue-guide"),
    ]
    .into_iter()
    .map(|(kind, name)| Entry {
        kind,
        name: name.to_owned(),
        summary: "Issue guidance".to_owned(),
        text: format!("{name} issue guidance"),
    })
    .collect();
    FakeHost {
        secret_check: Some(Arc::new(|_| Verdict::Safe)),
        entries,
        opened: vec![
            (
                "issue-guide".to_owned(),
                Ok("Placeholder document text.".to_owned()),
            ),
            (
                "issue-style".to_owned(),
                Ok("Placeholder skill text.".to_owned()),
            ),
            (
                "create_issue".to_owned(),
                Ok(json!({"type":"object","properties":{"title":{"type":"string"}}}).to_string()),
            ),
        ],
        ..Default::default()
    }
}

fn run(host: FakeHost, tool: &str, input: Value) -> ToolRun {
    let plugin = Search {
        host: Arc::new(host),
    };
    let prepared = block_on(plugin.prepare(tool, &input.to_string())).unwrap();
    block_on(plugin.run(prepared, &FakeCall::default()))
}

#[test]
fn hits_are_listed_with_their_kinds_and_next_action() {
    let run = run(host(), "search", json!({"query":"issue"}));
    let text = run.result.unwrap();
    for kind in [
        "tool create_issue",
        "skill issue-style",
        "document issue-guide",
    ] {
        assert!(text.contains(kind), "{text}");
    }
    assert!(text.contains("read it with use"));
}

#[test]
fn the_kind_filter_and_limit_are_applied_and_input_is_strict() {
    let result = run(
        host(),
        "search",
        json!({"query":"issue","kind":"skill","limit":1}),
    );
    assert_eq!(
        result.facts,
        [Fact::Hits(vec![Hit {
            kind: Kind::Skill,
            name: "issue-style".to_owned()
        }])]
    );
    assert_eq!(
        run(host(), "search", json!({"query":"issue","limit":0}))
            .result
            .as_deref(),
        Some("nothing found")
    );
    let plugin = Search {
        host: Arc::new(host()),
    };
    for input in [
        json!({"query":"x","kind":"other"}),
        json!({"query":"x","limit":-1}),
        json!({"query":"x","extra":true}),
        json!({}),
    ] {
        assert!(block_on(plugin.prepare("search", &input.to_string())).is_err());
    }
}

#[test]
fn a_search_with_no_matches_returns_nothing_found() {
    let result = run(host(), "search", json!({"query":"qzxv"}));
    assert_eq!(result.result.as_deref(), Some("nothing found"));
    assert_eq!(result.facts, [Fact::Hits(Vec::new())]);
    assert_eq!(result.outcome, ToolRun::completed().outcome);
}

#[test]
fn use_opens_a_skill_and_shows_its_kind_and_token_estimate() {
    for (name, network) in [("search", false), ("use", true)] {
        let definition = defs().into_iter().find(|def| def.name == name).unwrap();
        let ToolKind::Client(client) = definition.kind else {
            panic!("expected client tool");
        };
        assert_eq!(
            client.meta.tags,
            Tags {
                reads_untrusted: true,
                network,
                ..Tags::default()
            },
            "{name}"
        );
    }

    let plugin = Search {
        host: Arc::new(host()),
    };
    for (name, noun, text) in [
        ("issue-style", "skill", "Placeholder skill text."),
        ("issue-guide", "document", "Placeholder document text."),
    ] {
        let prepared = block_on(plugin.prepare("use", &json!({"name":name}).to_string())).unwrap();
        let verbs = prepared.display().verbs.unwrap();
        assert_eq!(verbs.doing, format!("Reading {noun}"));
        assert_eq!(verbs.done, format!("Read {noun}"));
        assert_eq!(verbs.failed, format!("couldn't read {noun}"));
        let run = block_on(plugin.run(prepared, &FakeCall::default()));
        assert_eq!(run.result.as_deref(), Some(text));
        assert!(matches!(run.facts.as_slice(), [Fact::Tokens(_)]));
    }
}

#[test]
fn use_of_an_unknown_name_reports_the_refusal() {
    let run = run(host(), "use", json!({"name":"missing"}));
    assert_eq!(
        run.result.as_deref(),
        Some("nothing named `missing` was found")
    );
    assert!(matches!(
        run.outcome,
        Outcome::Completed(Completed { is_error: true, .. })
    ));
}

#[test]
fn a_tool_hit_tells_the_model_how_to_run_it_and_its_parameters() {
    let result = run(
        host(),
        "search",
        json!({"query":"create_issue","kind":"tool"}),
    );
    let text = result.result.unwrap();
    assert!(text.contains("run it with use"));
    assert!(text.contains("parameters: {"));
    assert!(text.contains("\"title\""));
    let manifest = wisp_plugin_api::manifest::Manifest::parse(
        MANIFEST,
        wisp_plugin_api::manifest::Origin::Bundled,
    )
    .unwrap();
    use wisp_plugin_api::manifest::Switch;
    assert_eq!(
        manifest
            .tools
            .iter()
            .map(|tool| (tool.name.as_str(), tool.default, tool.follows.as_deref()))
            .collect::<Vec<_>>(),
        [
            ("search", Switch::On, None),
            ("use", Switch::On, Some("search")),
            ("manage", Switch::On, None)
        ]
    );
    assert_eq!(
        manifest
            .tools
            .iter()
            .filter(|tool| tool.held)
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["manage"]
    );
    assert!(
        manifest
            .tools
            .iter()
            .find(|tool| tool.name == "use")
            .unwrap()
            .forwards
    );
    assert_eq!(defs()[0].system_line.as_deref(), Some(SYSTEM_LINE));
    let mut unavailable = host();
    unavailable.opened = vec![(
        "create_issue".to_owned(),
        Err("source is unavailable".to_owned()),
    )];
    let result = run(
        unavailable,
        "search",
        json!({"query":"create_issue", "kind":"tool"}),
    );
    let text = result.result.unwrap();
    assert!(text.contains("source is unavailable"), "{text}");
    assert!(!text.contains("parameters:"), "{text}");
}

#[test]
fn the_fact_names_exactly_the_returned_hits() {
    let run = run(host(), "search", json!({"query":"issue","limit":2}));
    let [Fact::Hits(hits)] = run.facts.as_slice() else {
        panic!("{:?}", run.facts);
    };
    assert_eq!(hits.len(), 2);
    for hit in hits {
        assert!(run.result.as_ref().unwrap().contains(&format!(
            "{} {}",
            hit.kind.name(),
            hit.name
        )));
    }
}

fn manage_input(action: &str, kind: &str, name: &str) -> String {
    json!({"action": action, "kind": kind, "name": name}).to_string()
}

#[test]
fn manage_asks_the_host_and_its_row_reads_the_switch() {
    use wisp_plugin_api::manage::{Action as Switched, Change, Kind as Named};
    let missing = manage::nothing_named("tool", "show_diff");
    for (reply, outcome) in [
        (
            Ok("show_diff is off in this chat from the next message".to_owned()),
            ToolRun::completed().with_result("show_diff is off in this chat from the next message"),
        ),
        (
            Err(missing.clone()),
            ToolRun::new(Outcome::Failed(Failure {
                message: missing,
                before_start: false,
            })),
        ),
    ] {
        let host = FakeHost {
            manage_reply: Some(reply),
            ..FakeHost::default()
        };
        let managed = host.managed.clone();
        let plugin = Search {
            host: Arc::new(host),
        };
        let prepared =
            block_on(plugin.prepare("manage", &manage_input("off", "tool", "show_diff"))).unwrap();
        let display = prepared.display();
        assert_eq!(display.target, "show_diff");
        let verbs = display.verbs.unwrap();
        assert_eq!(
            [verbs.doing, verbs.done],
            ["Turn off tool"; 2].map(str::to_owned)
        );
        assert_eq!(verbs.failed, "couldn't turn off tool");
        let run = block_on(plugin.run(prepared, &FakeCall::default()));
        assert_eq!((run.outcome, run.result), (outcome.outcome, outcome.result));
        assert_eq!(
            *managed.lock().unwrap(),
            [Change {
                action: Switched::Off,
                kind: Named::Tool,
                name: "show_diff".to_owned(),
                from: None,
                keys: Vec::new(),
            }]
        );
    }
}

#[test]
fn tool_add_and_remove_explain_plugin_installation_without_a_host_call() {
    let host = FakeHost::default();
    let managed = host.managed.clone();
    let plugin = Search {
        host: Arc::new(host),
    };
    for (action, word, expected) in [
        ("add", "Add", wisp_tool_text::manage::ADD_TOOL),
        ("remove", "Remove", wisp_tool_text::manage::REMOVE_TOOL),
    ] {
        let prepared =
            block_on(plugin.prepare("manage", &manage_input(action, "tool", "placeholder")))
                .unwrap();
        assert_eq!(
            prepared.display().verbs.unwrap().done,
            format!("{word} tool")
        );
        let run = block_on(plugin.run(prepared, &FakeCall::default()));
        assert_eq!(
            run.outcome,
            Outcome::Failed(Failure {
                message: expected.to_owned(),
                before_start: false
            })
        );
    }
    assert!(managed.lock().unwrap().is_empty());
}

#[test]
fn a_call_stopped_while_asking_ends_cancelled_and_the_input_is_strict() {
    assert_eq!(
        block_on(
            Search {
                host: Arc::new(FakeHost::default())
            }
            .time_limit_ms("manage", "{}")
        ),
        Some(wisp_tool_text::manage::TIME_LIMIT_MS)
    );
    let plugin = Search {
        host: Arc::new(FakeHost {
            manage_reply: Some(Ok("nothing changed: the user didn't answer".to_owned())),
            ..FakeHost::default()
        }),
    };
    let stopped = FakeCall::default();
    stopped.cancel.set();
    let prepared =
        block_on(plugin.prepare("manage", &manage_input("on", "skill", "rust-code"))).unwrap();
    assert_eq!(prepared.display().verbs.unwrap().doing, "Turn on skill");
    assert_eq!(
        block_on(plugin.run(prepared, &stopped)).outcome,
        Outcome::Cancelled
    );
    for input in [
        json!({"action": "toggle", "kind": "tool", "name": "bash"}),
        json!({"action": "on", "kind": "unknown", "name": "github"}),
        json!({"action": "on", "kind": "tool"}),
        json!({"action": "on", "kind": "tool", "name": "bash", "extra": true}),
    ] {
        assert!(
            block_on(plugin.prepare("manage", &input.to_string())).is_err(),
            "{input}"
        );
    }
}

#[test]
fn server_actions_keep_from_off_the_row_and_reach_the_host() {
    use wisp_plugin_api::manage::{Action, Kind};
    let host = FakeHost::default();
    let managed = host.managed.clone();
    let plugin = Search {
        host: Arc::new(host),
    };
    for (action, expected, words) in [
        ("add", Action::Add, "Add server"),
        ("remove", Action::Remove, "Remove server"),
        ("on", Action::On, "Turn on server"),
        ("off", Action::Off, "Turn off server"),
    ] {
        let input = json!({"action":action,"kind":"server","name":"github","from":"npx server"})
            .to_string();
        let prepared = block_on(plugin.prepare("manage", &input)).unwrap();
        let verbs = prepared.display().verbs.unwrap();
        assert_eq!([verbs.doing, verbs.done], [words; 2]);
        assert_eq!(verbs.failed, format!("couldn't {}", words.to_lowercase()));
        assert_eq!(prepared.display().target, "github");
        block_on(plugin.run(prepared, &FakeCall::default()));
        assert_eq!(
            managed.lock().unwrap().last(),
            Some(&wisp_plugin_api::manage::Change {
                action: expected,
                kind: Kind::Server,
                name: "github".into(),
                from: Some("npx server".into()),
                keys: Vec::new(),
            })
        );
    }
    let prepared = block_on(plugin.prepare(
        "manage",
        r#"{"action":"add","kind":"server","name":"linear"}"#,
    ))
    .unwrap();
    block_on(plugin.run(prepared, &FakeCall::default()));
    assert_eq!(managed.lock().unwrap().last().unwrap().from, None);
}

#[test]
fn a_key_like_server_or_skill_name_is_refused_before_the_row_and_is_never_repeated() {
    let plugin = Search {
        host: Arc::new(FakeHost::default()),
    };
    let name = format!("sk-ant-api03-CANARY{}", "x".repeat(64));
    for kind in ["server", "skill"] {
        for action in ["add", "remove"] {
            let input =
                json!({"action":action,"kind":kind,"name":name,"from":"placeholder"}).to_string();
            let Err(error) = block_on(plugin.prepare("manage", &input)) else {
                panic!("key accepted");
            };
            assert_eq!(error, wisp_tool_text::manage::KEY_NAME);
            let shown = block_on(plugin.rejected_display("manage", &input)).unwrap();
            assert_eq!(shown.target, "");
            assert_eq!(
                shown.verbs.unwrap().failed,
                format!("couldn't {action} {kind}")
            );
        }
    }
}

#[test]
fn manage_passes_key_names_and_refuses_key_values_before_drawing_the_row() {
    let host = FakeHost::default();
    let managed = host.managed.clone();
    let plugin = Search {
        host: Arc::new(host),
    };
    let input = json!({"action":"add","kind":"server","name":"sentry","from":"npx sentry","keys":["SENTRY_TOKEN"]}).to_string();
    let prepared = block_on(plugin.prepare("manage", &input)).unwrap();
    assert_eq!(prepared.display().target, "sentry");
    block_on(plugin.run(prepared, &FakeCall::default()));
    assert_eq!(managed.lock().unwrap()[0].keys, ["SENTRY_TOKEN"]);
    for key in [
        "lin_api_CANARYa12345678901234567890",
        "sk-ant-api03-CANARY0000000000000000000000000000000000000000000000000000000000000000",
    ] {
        let input =
            json!({"action":"add","kind":"server","name":"sentry","keys":[key]}).to_string();
        let refusal = block_on(plugin.prepare("manage", &input)).err().unwrap();
        assert_eq!(refusal, wisp_tool_text::mcp::KEY_VARIABLE);
        let display = block_on(plugin.rejected_display("manage", &input)).unwrap();
        assert_eq!(display.target, "sentry");
        assert!(!format!("{display:?}").contains("CANARY"));
    }
    let canary = "lin_api_CANARYa12345678901234567890";
    for input in [
        json!({"action":"add","kind":"server","name":"sentry","keys":canary}),
        json!({"action":"add","kind":"server","name":"sentry","keys":[{"value":canary}]}),
        json!({"action":"add","kind":"server","name":"sentry","from":[canary]}),
        json!({"action":canary,"kind":"server","name":"sentry"}),
        json!({"action":"add","kind":canary,"name":"sentry"}),
        json!({"action":"add","kind":"server","name":[canary]}),
        json!({(canary): "unknown field"}),
    ] {
        let refusal = block_on(plugin.prepare("manage", &input.to_string()))
            .err()
            .unwrap();
        assert!(!refusal.contains("CANARY"));
        assert_eq!(refusal, wisp_tool_text::index::INVALID_INPUT);
    }
    for (tool, input) in [
        ("search", json!({"query":[canary]})),
        ("search", json!({"query":"a tool", "kind":canary})),
        ("use", json!({"name":[canary]})),
    ] {
        let refusal = block_on(plugin.prepare(tool, &input.to_string()))
            .err()
            .unwrap();
        assert!(!refusal.contains("CANARY"));
        assert_eq!(refusal, wisp_tool_text::index::INVALID_INPUT);
    }
    assert_eq!(managed.lock().unwrap().len(), 1);
}

#[test]
fn skill_add_and_remove_reuse_the_rows_without_sending_paths_to_the_host() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source/guide");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("SKILL.md"),
        "---\nname: guide\ndescription: Guide\n---\nPlaceholder.\n",
    )
    .unwrap();
    let host = FakeHost {
        root: temp.path().join("source").to_string_lossy().into_owned(),
        config_dir: Some(temp.path().join("config").to_string_lossy().into_owned()),
        ..FakeHost::default()
    };
    let managed = host.managed.clone();
    for (action, word) in [("add", "Add"), ("remove", "Remove")] {
        let ctx = FakeCall::default();
        let cancel = ctx.cancel.clone();
        let plugin = Search {
            host: Arc::new(FakeHost {
                index_changed: Some(Arc::new(move || cancel.set())),
                ..host.clone()
            }),
        };
        let input =
            json!({"action":action,"kind":"skill","name":"guide","from":"guide"}).to_string();
        let prepared = block_on(plugin.prepare("manage", &input)).unwrap();
        let shown = prepared.display();
        assert_eq!(shown.target, "guide");
        let verbs = shown.verbs.unwrap();
        assert_eq!(
            [verbs.doing, verbs.done],
            [format!("{word} skill"), format!("{word} skill")]
        );
        assert_eq!(verbs.failed, format!("couldn't {action} skill"));
        let run = block_on(plugin.run(prepared, &ctx));
        assert!(ctx.cancelled());
        assert_eq!(
            (run.outcome, run.result),
            (
                ToolRun::completed().outcome,
                Some(format!(
                    "guide {}",
                    if action == "add" { "added" } else { "removed" }
                ))
            )
        );
    }
    assert!(source.join("SKILL.md").exists());
    assert!(managed.lock().unwrap().is_empty());
}

#[test]
fn an_unsafe_lookup_never_builds_a_display_and_changed_verdicts_never_return_results() {
    use std::sync::atomic::{AtomicBool, Ordering};
    const HELD: &str = "CANARY-late-search-lookup";
    let held = Arc::new(AtomicBool::new(false));
    let mut fixture = host();
    fixture.entries.push(Entry {
        kind: Kind::Document,
        name: HELD.into(),
        summary: "Sample.".into(),
        text: "Test word.".into(),
    });
    fixture.secret_check = Some(Arc::new({
        let held = held.clone();
        move |text| {
            if held.load(Ordering::Relaxed) && text.contains(HELD) {
                Verdict::Refused
            } else {
                Verdict::Safe
            }
        }
    }));
    let plugin = Search {
        host: Arc::new(fixture),
    };
    for (tool, key) in [("use", "name"), ("search", "query")] {
        let input = json!({(key): HELD}).to_string();
        held.store(false, Ordering::Relaxed);
        let prepared = block_on(plugin.prepare(tool, &input)).unwrap();
        assert!(prepared.display().target.contains(HELD));
        held.store(true, Ordering::Relaxed);
        let reply = block_on(plugin.run(prepared, &FakeCall::default()));
        assert!(
            matches!(reply.outcome, Outcome::Failed(Failure { message, .. }) if message == INVALID_TARGET)
        );
        assert!(reply.result.is_none() && reply.facts.is_empty());
        assert_eq!(
            block_on(plugin.prepare(tool, &input)).err().unwrap(),
            INVALID_TARGET
        );
        for rejected in [&input, &json!({(HELD):true}).to_string()] {
            assert!(
                !block_on(plugin.rejected_display(tool, rejected))
                    .unwrap()
                    .target
                    .contains(HELD)
            );
        }
    }
    let prepared = block_on(plugin.prepare("use", r#"{"name":"missing"}"#)).unwrap();
    assert_eq!(prepared.display().target, "entry");
}

use futures::executor::block_on;
use wisp_plugin_api::host::{AgentRow, Instant, Relation, WarmRow};
use wisp_plugin_api::manifest::{Manifest, Origin, Switch, is_static_sentence};
use wisp_plugin_api::tree::HoldReason;
use wisp_plugin_testkit::host::{FakeCall, FakeHost, agent_node as node, identity};

use super::*;

fn plugin(host: &FakeHost) -> Arc<dyn Plugin> {
    instantiate(Arc::new(host.clone()))
}

fn call(id: &str) -> FakeCall {
    FakeCall {
        id: id.to_owned(),
        ..FakeCall::default()
    }
}

fn run_tool(host: &FakeHost, tool: &str, input: &Value) -> ToolRun {
    let plugin = plugin(host);
    let prepared = block_on(plugin.prepare(tool, &input.to_string())).unwrap();
    block_on(plugin.run(prepared, &call("toolu_1")))
}

fn completed(is_error: bool) -> Outcome {
    Outcome::Completed(Completed {
        is_error,
        exit: None,
    })
}

#[test]
fn spawn_input_is_read_strictly_and_completely() {
    let cases: Vec<(Value, Option<&str>)> = vec![
        (json!({"name": "a", "task": "t"}), None),
        (
            json!({"name": "a", "task": "t", "todos": ["x"], "cwd": "~/w", "preset": "p", "endpoint": "b", "model": "m", "effort": "low", "tools_off": ["bash"], "spawn": false, "keep_warm": true}),
            None,
        ),
        (
            json!({"name": "a", "task": "t", "report": "code"}),
            Some("unknown field `report`"),
        ),
        (json!({"task": "t"}), Some("`name` is required")),
        (json!({"name": "a"}), Some("`task` is required")),
        (
            json!({"name": 1, "task": "t"}),
            Some("`name` must be a string"),
        ),
        (
            json!({"name": "a", "task": "t", "todos": "x"}),
            Some("`todos` must be an array of strings"),
        ),
        (
            json!({"name": "a", "task": "t", "tools_off": [1]}),
            Some("every item of `tools_off` must be a string"),
        ),
        (
            json!({"name": "a", "task": "t", "endpoint": 1}),
            Some("`endpoint` must be a string"),
        ),
        (
            json!({"name": "a", "task": "t", "spawn": "no"}),
            Some("`spawn` must be true or false"),
        ),
        (
            json!({"name": "a", "task": "t", "keep_warm": "yes"}),
            Some("`keep_warm` must be true or false"),
        ),
        (
            json!({"name": "a", "task": "t", "keep_warm": 1}),
            Some("`keep_warm` must be true or false"),
        ),
        (
            json!({"name": "a", "task": "t", "wait": true}),
            Some("unknown field `wait`"),
        ),
        (json!(["a"]), Some("input must be an object")),
    ];
    for (input, expected) in cases {
        let got =
            block_on(plugin(&FakeHost::default()).prepare(SPAWN_AGENT, &input.to_string())).err();
        assert_eq!(got.as_deref(), expected, "{input}");
    }
    let full = parse_spawn(
        &json!({"name": "a", "task": "t", "todos": ["x", "y"], "cwd": "~/w", "preset": "p", "endpoint": "b", "model": "m", "effort": "low", "tools_off": ["bash"], "spawn": false, "keep_warm": true})
            .to_string(),
    )
    .unwrap();
    assert_eq!(
        full,
        SpawnInput {
            name: "a".to_owned(),
            task: "t".to_owned(),
            todos: vec!["x".to_owned(), "y".to_owned()],
            cwd: Some("~/w".to_owned()),
            preset: Some("p".to_owned()),
            endpoint: Some("b".to_owned()),
            model: Some("m".to_owned()),
            effort: Some("low".to_owned()),
            tools_off: vec!["bash".to_owned()],
            spawn: Some(false),
            keep_warm: Some(true),
        }
    );
    let fields: Vec<String> = spawn_schema()["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(fields, SPAWN_FIELDS);
}

#[test]
fn a_spawn_hands_its_input_to_the_host_and_returns_its_line_at_once() {
    let started = "Started /planner on b (anthropic) with claude-test, low, in /w. Its report will arrive as a message; don't poll it, and end your turn unless you have other independent work.";
    let input = json!({"name": "planner", "task": "Plan it.", "effort": "low", "endpoint": "b"});
    for (state, tail) in [
        (
            AgentState::Queued(QueueReason::Concurrency),
            format!(" {QUEUED_SENTENCE}"),
        ),
        (AgentState::Queued(QueueReason::Stagger), String::new()),
        (AgentState::Running, String::new()),
    ] {
        let host = FakeHost {
            root: "/w".to_owned(),
            tree: vec![
                node("/other", AgentState::Queued(QueueReason::Concurrency)),
                node("/planner", state),
            ],
            ..FakeHost::default()
        };
        let run = run_tool(&host, SPAWN_AGENT, &input);
        assert_eq!(run.result, Some(format!("{started}{tail}")), "{state:?}");
    }
    let host = FakeHost {
        root: "/w".to_owned(),
        ..FakeHost::default()
    };
    let run = run_tool(&host, SPAWN_AGENT, &input);
    assert_eq!(run.outcome, completed(false));
    assert_eq!(run.result.as_deref(), Some(started));
    let spawned = host.spawned.lock().unwrap().clone();
    assert_eq!(spawned.len(), 1);
    assert_eq!(spawned[0].0, "toolu_1");
    assert_eq!(spawned[0].1.task, "Plan it.");
    assert_eq!(spawned[0].1.endpoint.as_deref(), Some("b"));
}

#[test]
fn a_kept_warm_spawn_passes_the_mark_and_words_the_hosts_limit() {
    for (limit, tail) in [
        (
            8,
            " While it idles, Wisp refreshes its prompt cache up to 8 times per idle spell.",
        ),
        (
            0,
            " keep_warm is off in this session (agents.warm_refreshes is 0).",
        ),
    ] {
        let host = FakeHost {
            root: "/w".to_owned(),
            warm_refreshes: limit,
            ..FakeHost::default()
        };
        let run = run_tool(
            &host,
            SPAWN_AGENT,
            &json!({"name": "planner", "task": "Plan it.", "keep_warm": true}),
        );
        assert_eq!(
            run.result,
            Some(format!(
                "Started /planner on env (anthropic) with claude-test, high, in /w. Its report will arrive as a message; don't poll it, and end your turn unless you have other independent work.{tail}"
            ))
        );
        assert_eq!(host.spawned.lock().unwrap()[0].1.keep_warm, Some(true));
    }
}

#[test]
fn a_refusal_is_a_tool_error_carrying_the_hosts_reason() {
    let host = FakeHost {
        refuse_agents: Some("the depth limit (2) is reached".to_owned()),
        ..FakeHost::default()
    };
    for (tool, input) in [
        (SPAWN_AGENT, json!({"name": "a", "task": "t"})),
        (LIST_AGENTS, json!({})),
        (REPORT, json!({"text": "Done."})),
        (SEND_MESSAGE, json!({"to": "..", "text": "hi"})),
        (STOP_AGENT, json!({"name": "a"})),
    ] {
        let run = run_tool(&host, tool, &input);
        assert_eq!(run.outcome, completed(true), "{tool}");
        assert_eq!(
            run.result.as_deref(),
            Some("the depth limit (2) is reached"),
            "{tool}"
        );
    }
    assert!(host.spawned.lock().unwrap().is_empty());
    assert!(host.reported.lock().unwrap().is_empty());
}

#[test]
fn list_agents_takes_no_input_and_words_the_hosts_rows() {
    assert_eq!(
        block_on(
            plugin(&FakeHost::default()).prepare(LIST_AGENTS, &json!({"all": true}).to_string())
        )
        .err()
        .as_deref(),
        Some("unknown field `all`")
    );
    let empty = run_tool(&FakeHost::default(), LIST_AGENTS, &json!({}));
    assert_eq!(empty.result.as_deref(), Some("No other agents yet."));
    let host = FakeHost {
        agents: vec![AgentRow {
            path: "/a".to_owned(),
            session: "s-a".to_owned(),
            relation: Relation::Child,
            preset: None,
            model: "claude-test".to_owned(),
            effort: "high".to_owned(),
            endpoint: Some("b".to_owned()),
            provider: Some("anthropic".to_owned()),
            state: Some("idle".to_owned()),
            detail: Some("reported".to_owned()),
            billing_note: None,
            plan_micros: None,
            spent_micros: None,
            lower_bound: false,
            priced: true,
            checklist: None,
            last_report: None,
            cwd: None,
            running_ms: None,
            context_used: None,
            context_window: None,
            context_limit: None,
            context_estimated: false,
            keep_warm: None,
        }],
        ..FakeHost::default()
    };
    let run = run_tool(&host, LIST_AGENTS, &json!({}));
    assert_eq!(run.outcome, completed(false));
    assert_eq!(
        run.result.as_deref(),
        Some(
            "1 agent you can reach:\n- /a (your child) · session s-a · claude-test, high, on b (anthropic) · idle: reported"
        )
    );
}

#[test]
fn report_passes_its_input_through_for_the_kernel_to_check() {
    let host = FakeHost::default();
    let input = json!({"text": "\nDone: the plan is written\nDetails follow.", "extra": 1});
    let run = run_tool(&host, REPORT, &input);
    assert_eq!(run.outcome, completed(false));
    assert_eq!(run.result.as_deref(), Some("Reported."));
    let reported = host.reported.lock().unwrap().clone();
    assert_eq!(reported.len(), 1);
    let seen: Value = serde_json::from_str(&reported[0].1).unwrap();
    assert_eq!(seen, input);
    assert_eq!(
        block_on(plugin(&host).prepare(REPORT, "\"done\""))
            .err()
            .as_deref(),
        Some("input must be an object")
    );
    let shown = block_on(plugin(&host).prepare(REPORT, &input.to_string()))
        .unwrap()
        .display();
    assert_eq!(shown.target, "Done: the plan is written");
    assert_eq!(shown.action, Action::Other(REPORT.to_owned()));
}

#[test]
fn every_agent_tool_row_has_its_own_verbs() {
    let host = FakeHost::default();
    let inputs = [
        (
            SPAWN_AGENT,
            json!({"name": "review", "task": "Look"}),
            "Starting",
            "Started",
            "couldn't start",
        ),
        (
            SEND_MESSAGE,
            json!({"to": "..", "text": "x"}),
            "Messaging",
            "Messaged",
            "couldn't message",
        ),
        (
            STOP_AGENT,
            json!({"name": "coder"}),
            "Stopping",
            "Stopped",
            "couldn't stop",
        ),
        (
            STOP_AGENT,
            json!({"name": "coder", "end": true}),
            "Ending",
            "Ended",
            "couldn't end",
        ),
        (
            RENAME_AGENT,
            json!({"name": "coder", "new_name": "fixer"}),
            "Renaming",
            "Renamed",
            "couldn't rename",
        ),
        (
            LIST_AGENTS,
            json!({}),
            "Listing agents",
            "Listed agents",
            "couldn't list agents",
        ),
        (
            REPORT,
            json!({"text": "Done"}),
            "Reporting",
            "Reported",
            "couldn't report",
        ),
    ];
    for (tool, input, doing, done, failed) in inputs {
        let shown = block_on(plugin(&host).prepare(tool, &input.to_string()))
            .unwrap()
            .display();
        let verbs = shown.verbs.unwrap();
        assert_eq!(
            (
                verbs.doing.as_str(),
                verbs.done.as_str(),
                verbs.failed.as_str()
            ),
            (doing, done, failed),
            "{tool}"
        );
    }
}

#[test]
fn the_definitions_are_static_and_spawn_agent_leads_the_rest() {
    let context = BuildContext {
        platform: "unix".to_owned(),
        capabilities: vec!["shell".to_owned()],
    };
    let bare = BuildContext {
        platform: "windows".to_owned(),
        capabilities: Vec::new(),
    };
    let one = block_on(plugin(&FakeHost::default()).describe(&context));
    let two = block_on(
        plugin(&FakeHost {
            root: "/elsewhere".to_owned(),
            ..FakeHost::default()
        })
        .describe(&bare),
    );
    assert_eq!(one, two);
    assert_eq!(one, defs());
    let names: Vec<&str> = one.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, NAMES);
    for def in &one {
        let ToolKind::Client(client) = &def.kind else {
            panic!("{:?}", def.kind);
        };
        assert!(!client.meta.needs_workspace, "{}", def.name);
        assert!(!client.meta.asks_user, "{}", def.name);
        assert!(!client.meta.ends_reply, "{}", def.name);
        assert!(!client.description.contains('~'), "{}", def.name);
        let schema: Value = serde_json::from_str(&client.input_schema).unwrap();
        assert_eq!(schema["type"], "object", "{}", def.name);
        assert_eq!(schema["additionalProperties"], false, "{}", def.name);
    }
    for def in &one {
        let want = match def.name.as_str() {
            REPORT => REPORT_SENTENCE,
            SPAWN_AGENT => SPAWN_LINE,
            _ => MESSAGES_SENTENCE,
        };
        assert_eq!(def.system_line.as_deref(), Some(want), "{}", def.name);
        assert!(is_static_sentence(want), "{}", def.name);
    }
    let manifest = Manifest::parse(MANIFEST, Origin::Bundled).unwrap();
    assert_eq!(manifest.id, ID);
    let declared: Vec<&str> = manifest.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(declared, NAMES);
    let grouping: Vec<(&str, Option<&str>, Switch)> = manifest
        .tools
        .iter()
        .map(|t| (t.name.as_str(), t.follows.as_deref(), t.default))
        .collect();
    assert_eq!(
        grouping,
        [
            ("list_agents", Some("spawn_agent"), Switch::On),
            ("rename_agent", Some("spawn_agent"), Switch::On),
            ("report", Some("spawn_agent"), Switch::Off),
            ("send_message", Some("spawn_agent"), Switch::On),
            ("spawn_agent", None, Switch::On),
            ("stop_agent", Some("spawn_agent"), Switch::On),
        ]
    );
    assert!(manifest.hooks.is_empty());
    assert!(manifest.system.is_none());
}

#[test]
fn send_message_reads_its_input_strictly_and_words_the_delivery() {
    let host = FakeHost::default();
    let run = run_tool(
        &host,
        SEND_MESSAGE,
        &json!({"to": "../coder", "text": "Ready for review."}),
    );
    assert_eq!(run.outcome, completed(false));
    assert_eq!(
        run.result.as_deref(),
        Some("Sent to ../coder (idle: woken).")
    );
    let quiet = run_tool(
        &host,
        SEND_MESSAGE,
        &json!({"to": "..", "text": "FYI", "wake": false}),
    );
    assert_eq!(
        quiet.result.as_deref(),
        Some("Sent to .. (idle: it reads this when it next runs).")
    );
    let tree = vec![node("/coord/rev-a", AgentState::Idle(None))];
    let coord = FakeHost {
        identity: Some(identity("/coord")),
        tree: tree.clone(),
        ..FakeHost::default()
    };
    let woke_child = run_tool(
        &coord,
        SEND_MESSAGE,
        &json!({"to": "/coord/rev-a", "text": "Fix it."}),
    );
    assert_eq!(
        woke_child.result.as_deref(),
        Some(
            "Sent to /coord/rev-a (idle: woken). Its report will arrive as a message; don't poll it, and end your turn unless you have other independent work."
        )
    );
    let idle_child = run_tool(
        &coord,
        SEND_MESSAGE,
        &json!({"to": "/coord/rev-a", "text": "FYI", "wake": false}),
    );
    assert_eq!(
        idle_child.result.as_deref(),
        Some("Sent to /coord/rev-a (idle: it reads this when it next runs).")
    );
    let root = FakeHost {
        identity: Some(identity("/")),
        tree,
        ..FakeHost::default()
    };
    let woke_descendant = run_tool(
        &root,
        SEND_MESSAGE,
        &json!({"to": "/coord/rev-a", "text": "Check in."}),
    );
    assert_eq!(
        woke_descendant.result.as_deref(),
        Some("Sent to /coord/rev-a (idle: woken).")
    );
    let messaged = host.messaged.lock().unwrap().clone();
    assert_eq!(
        messaged,
        [
            (
                "toolu_1".to_owned(),
                "../coder".to_owned(),
                "Ready for review.".to_owned(),
                true
            ),
            (
                "toolu_1".to_owned(),
                "..".to_owned(),
                "FYI".to_owned(),
                false
            ),
        ]
    );
    for (input, error) in [
        (json!({"text": "x"}), "`to` is required"),
        (json!({"to": ".."}), "`text` is required"),
        (
            json!({"to": "..", "text": "x", "wake": "yes"}),
            "`wake` must be true or false",
        ),
        (
            json!({"to": "..", "text": "x", "cc": "/a"}),
            "unknown field `cc`",
        ),
    ] {
        assert_eq!(
            block_on(plugin(&host).prepare(SEND_MESSAGE, &input.to_string()))
                .err()
                .as_deref(),
            Some(error),
            "{input}"
        );
    }
    let shown = block_on(
        plugin(&host).prepare(SEND_MESSAGE, &json!({"to": "..", "text": "x"}).to_string()),
    )
    .unwrap()
    .display();
    assert_eq!(shown.target, "..");
    let fields: Vec<String> = send_schema()["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(fields, ["text", "to", WAKE]);
}

#[test]
fn stop_agent_defaults_to_stopping_and_end_retires() {
    let host = FakeHost::default();
    let run = run_tool(&host, STOP_AGENT, &json!({"name": "coder"}));
    assert_eq!(run.outcome, completed(false));
    assert_eq!(
        run.result.as_deref(),
        Some("Stopped /coder. It stays idle and keeps its context; message it to go on.")
    );
    let ended = run_tool(
        &host,
        STOP_AGENT,
        &json!({"name": "/review/x", "end": true}),
    );
    assert_eq!(
        ended.result.as_deref(),
        Some(
            "Ended /review/x. Ended agents take no more messages and no longer count toward the agent limit."
        )
    );
    let halted = host.halted.lock().unwrap().clone();
    assert_eq!(
        halted,
        [
            ("toolu_1".to_owned(), "coder".to_owned(), false),
            ("toolu_1".to_owned(), "/review/x".to_owned(), true),
        ]
    );
    for (input, error) in [
        (json!({}), "`name` is required"),
        (
            json!({"name": "a", "end": 1}),
            "`end` must be true or false",
        ),
        (json!({"name": "a", "force": true}), "unknown field `force`"),
    ] {
        assert_eq!(
            block_on(plugin(&host).prepare(STOP_AGENT, &input.to_string()))
                .err()
                .as_deref(),
            Some(error),
            "{input}"
        );
    }
    let fields: Vec<String> = stop_schema()["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(fields, ["end", "name"]);
}

#[test]
fn rename_agent_reads_its_input_strictly_and_words_the_answer() {
    let host = FakeHost::default();
    let run = run_tool(
        &host,
        RENAME_AGENT,
        &json!({"name": "/task-a/coder", "new_name": "fixer"}),
    );
    assert_eq!(run.outcome, completed(false));
    assert_eq!(
        run.result.as_deref(),
        Some("Renamed /task-a/coder to /task-a/fixer; its old name still reaches it.")
    );
    assert_eq!(
        *host.renamed.lock().unwrap(),
        [(
            "toolu_1".to_owned(),
            "/task-a/coder".to_owned(),
            "fixer".to_owned()
        )]
    );
    let refused = FakeHost {
        refuse_agents: Some("task-a already has an agent named helper".to_owned()),
        ..FakeHost::default()
    };
    let run = run_tool(
        &refused,
        RENAME_AGENT,
        &json!({"name": "coder", "new_name": "helper"}),
    );
    assert_eq!(run.outcome, completed(true));
    assert_eq!(
        run.result.as_deref(),
        Some("task-a already has an agent named helper")
    );
    for (input, error) in [
        (json!({"new_name": "x"}), "`name` is required"),
        (json!({"name": "a"}), "`new_name` is required"),
        (
            json!({"name": "a", "new_name": 1}),
            "`new_name` must be a string",
        ),
        (
            json!({"name": "a", "new_name": "b", "end": true}),
            "unknown field `end`",
        ),
    ] {
        assert_eq!(
            block_on(plugin(&host).prepare(RENAME_AGENT, &input.to_string()))
                .err()
                .as_deref(),
            Some(error),
            "{input}"
        );
    }
    let shown = block_on(plugin(&host).prepare(
        RENAME_AGENT,
        &json!({"name": "/task-a/coder", "new_name": "fixer"}).to_string(),
    ))
    .unwrap()
    .display();
    assert_eq!(shown.target, "/task-a/coder to fixer");
    let schema = rename_schema();
    assert_eq!(
        schema["properties"]["name"],
        stop_schema()["properties"]["name"]
    );
    assert_eq!(schema["required"], json!(["name", "new_name"]));
    let ToolKind::Client(rename) = &defs()[1].kind else {
        panic!("{:?}", defs()[1].kind);
    };
    let ToolKind::Client(stop) = &defs()[5].kind else {
        panic!("{:?}", defs()[5].kind);
    };
    assert!(rename.description.contains(REACH) && stop.description.contains(REACH));
    assert_eq!(meta(RENAME_AGENT), meta(STOP_AGENT));
}

#[test]
fn the_report_and_spawn_texts_ask_for_free_text_reports() {
    let report = report_schema();
    assert_eq!(
        report,
        json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The whole report, in the form your task or instructions ask for."
                }
            },
            "required": ["text"],
            "additionalProperties": false
        })
    );
    assert!(REPORT_DESCRIPTION.contains("so lead with the outcome"));
    assert!(REPORT_DESCRIPTION.contains("never pointing to \"above\""));
    assert!(spawn_schema()["properties"].get("report").is_none());
    assert!(SPAWN_DESCRIPTION.contains("which may include instructions for the child's role"));
    assert!(SPAWN_DESCRIPTION.contains("what to hand back"));
}

const NOW_S: i64 = 10_000;

type Warm = Option<(u32, u32, bool)>;

fn lead_tree(children: &[(&str, AgentState, u64, Warm)]) -> FakeHost {
    let tree = children.iter().map(|(path, state, idle_s, _)| AgentNode {
        since_ms: (u64::try_from(NOW_S).unwrap() - idle_s) * 1000,
        ..node(path, *state)
    });
    let states = children.iter().map(|(path, _, _, warm)| ShownState {
        path: (*path).to_owned(),
        state: wisp_plugin_api::slots::NodeState::Done,
        keep_warm: warm.map(|(refreshes, limit, paused)| WarmRow {
            refreshes,
            limit,
            paused,
        }),
    });
    FakeHost {
        identity: Some(identity("/lead")),
        tree: tree.collect(),
        states: states.collect(),
        now: Some(Arc::new(|| Instant {
            seconds: NOW_S,
            nanos: 0,
        })),
        ..FakeHost::default()
    }
}

fn kept_warm_text(host: &FakeHost, at: At, previous: Prior, target: Option<&str>) -> Rendered {
    block_on(plugin(host).render_section(KEPT_WARM, at, previous, target))
}

const REPORTED: AgentState = AgentState::Idle(Some(IdleOutcome::Reported));

const TWO_NAMED: &str = "Children idle and still kept warm: /lead/planner for 1h12m, /lead/review-a for 58m40s. Each costs another cache refresh while it idles; end any you won't message again with stop_agent and end: true.\n";

#[test]
fn only_own_idle_children_refreshed_and_still_to_be_refreshed_are_named_with_their_times() {
    let asked = AgentState::Idle(Some(IdleOutcome::Asked));
    let waiting = AgentState::Idle(Some(IdleOutcome::Waiting));
    let running = AgentState::Running;
    let host = lead_tree(&[
        ("/lead/planner", REPORTED, 4320, Some((1, 2, false))),
        ("/lead/planner/deep", REPORTED, 4000, Some((1, 8, false))),
        ("/lead/review-a", REPORTED, 3520, Some((3, 8, false))),
        ("/lead/fresh", REPORTED, 60, Some((0, 8, false))),
        ("/lead/asked", asked, 4000, Some((1, 8, false))),
        ("/lead/waiting", waiting, 4000, Some((1, 8, false))),
        ("/lead/running", running, 4000, Some((1, 8, false))),
        ("/lead/paused", REPORTED, 4000, Some((1, 8, true))),
        ("/lead/full", REPORTED, 9000, Some((2, 2, false))),
        ("/lead/plain", REPORTED, 4000, None),
        ("/lead/ended", AgentState::Ended, 4000, Some((1, 8, false))),
        ("/other", REPORTED, 4000, Some((1, 8, false))),
    ]);
    let rendered = kept_warm_text(&host, At::Turn, Prior::None, None);
    assert_eq!(rendered.text.as_deref(), Some(TWO_NAMED));
    assert_eq!(
        rendered.snapshot,
        "/lead/planner\t5680000\t1\n/lead/review-a\t6480000\t3"
    );
    let outside = FakeHost {
        identity: None,
        ..host.clone()
    };
    assert_eq!(
        kept_warm_text(&outside, At::Turn, Prior::None, None),
        Rendered::default()
    );
    let other = block_on(plugin(&host).render_section("other", At::Turn, Prior::None, None));
    assert_eq!(other, Rendered::default());
}

#[test]
fn a_child_is_named_again_only_after_another_refresh_and_a_head_names_every_listed_child() {
    let children = |planner_refreshes| {
        lead_tree(&[
            (
                "/lead/planner",
                REPORTED,
                4320,
                Some((planner_refreshes, 8, false)),
            ),
            ("/lead/review-a", REPORTED, 3520, Some((3, 8, false))),
        ])
    };
    let host = children(1);
    let first = kept_warm_text(&host, At::Turn, Prior::None, None);
    assert_eq!(first.text.as_deref(), Some(TWO_NAMED));
    for seen in [
        Prior::Snapshot(first.snapshot.clone()),
        Prior::Legacy(Some(first.snapshot.clone())),
    ] {
        let again = kept_warm_text(&host, At::Turn, seen, None);
        assert_eq!(again.text, None);
        assert_eq!(again.snapshot, first.snapshot);
    }
    let refreshed = kept_warm_text(
        &children(2),
        At::Turn,
        Prior::Snapshot(first.snapshot.clone()),
        None,
    );
    assert_eq!(
        refreshed.text.as_deref(),
        Some(
            "Children idle and still kept warm: /lead/planner for 1h12m. Each costs another cache refresh while it idles; end any you won't message again with stop_agent and end: true.\n"
        )
    );
    let head = kept_warm_text(&host, At::Head, Prior::None, Some(&first.snapshot));
    assert_eq!(head.text.as_deref(), Some(TWO_NAMED));
    assert!(TWO_NAMED.ends_with(".\n"));
}

const RUNNING_AND_NO_REPORT: &str = "Child agents at the start of this context window, except those ended or idle after a report, each with its state and the time since it last started or stopped working or waiting (waiting for an endpoint counts as working): /lead/planner (running, 2m05s), /lead/coder (idle: no_report, 41s).\nTheir reports arrive as messages.\n";

fn children_text(host: &FakeHost, at: At, previous: Prior, target: Option<&str>) -> Rendered {
    block_on(plugin(host).render_section(CHILDREN, at, previous, target))
}

#[test]
fn a_head_lists_own_children_not_idle_after_a_report_and_nothing_else_does() {
    let host = lead_tree(&[
        ("/lead/planner", AgentState::Running, 125, None),
        ("/lead/planner/deep", AgentState::Running, 60, None),
        ("/lead/done", REPORTED, 4000, None),
        (
            "/lead/coder",
            AgentState::Idle(Some(IdleOutcome::NoReport)),
            41,
            None,
        ),
        ("/lead/ended", AgentState::Ended, 9, None),
        ("/other", AgentState::Running, 9, None),
    ]);
    let listed = Rendered {
        text: Some(RUNNING_AND_NO_REPORT.to_owned()),
        snapshot: String::new(),
    };
    assert_eq!(children_text(&host, At::Head, Prior::None, None), listed);
    let seen = Prior::Snapshot(RUNNING_AND_NO_REPORT.to_owned());
    assert_eq!(children_text(&host, At::Head, seen, Some("x")), listed);
    for at in [At::Start, At::Turn] {
        assert_eq!(
            children_text(&host, at, Prior::None, None),
            Rendered::default()
        );
    }
    let outside = FakeHost {
        identity: None,
        ..host.clone()
    };
    let settled = lead_tree(&[
        ("/lead/done", REPORTED, 4000, None),
        ("/lead/ended", AgentState::Ended, 9, None),
        ("/lead/planner/deep", AgentState::Running, 60, None),
    ]);
    for quiet in [outside, settled, lead_tree(&[])] {
        assert_eq!(
            children_text(&quiet, At::Head, Prior::None, None),
            Rendered::default()
        );
    }
}

#[test]
fn every_state_is_awaited_except_ended_and_idle_after_a_report() {
    let queued = [
        QueueReason::Concurrency,
        QueueReason::Stagger,
        QueueReason::Endpoint,
        QueueReason::Retry,
        QueueReason::Unknown,
    ];
    let held = [
        HoldReason::Stopped,
        HoldReason::HeldMessages,
        HoldReason::RetryOffer,
        HoldReason::Concurrency,
        HoldReason::Unknown,
    ];
    let outcomes = [
        IdleOutcome::Reported,
        IdleOutcome::Asked,
        IdleOutcome::Waiting,
        IdleOutcome::NoReport,
        IdleOutcome::InvalidReport,
        IdleOutcome::Failed,
        IdleOutcome::Stopped,
        IdleOutcome::Unknown,
    ];
    let states: Vec<AgentState> = [
        AgentState::Running,
        AgentState::Stopping,
        AgentState::Ended,
        AgentState::Unknown,
        AgentState::Idle(None),
    ]
    .into_iter()
    .chain(queued.map(AgentState::Queued))
    .chain(held.map(AgentState::Held))
    .chain(outcomes.map(|o| AgentState::Idle(Some(o))))
    .collect();
    let left_out: Vec<AgentState> = states.iter().copied().filter(|s| !awaited(*s)).collect();
    assert_eq!(left_out, [AgentState::Ended, REPORTED]);
    assert_eq!(states.len(), 23);
}

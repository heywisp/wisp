use std::sync::atomic::{AtomicUsize, Ordering};

use wisp_plugin_api::hooks::Reply;
use wisp_plugin_testkit::hooks::{self as kit, Calls, Script as HookScript};
use wisp_protocol::{HookChange, RewindPoint};

use super::*;

fn counting() -> HookScript {
    let n = Arc::new(AtomicUsize::new(0));
    Arc::new(move |_, _, _| {
        let k = n.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move { Reply::Append(vec![format!("call {k}\n")]) })
    })
}

fn hooked(plan: Vec<Step>, calls: &Calls) -> Setup {
    let body = "\n[[hooks]]\nname = \"x\"\npoint = \"before-turn\"\n";
    let make: crate::plugin::Make = kit::instantiate(counting(), calls.clone());
    let mut setup = Setup::new(plan);
    setup.hooks = vec![(kit::manifest("aa", body), make)];
    setup
}

fn leads(records: &[LogRecord]) -> Vec<Vec<(String, String)>> {
    turn_started_records(records)
        .into_iter()
        .map(|e| match e {
            LogEvent::TurnStarted { lead, .. } => lead
                .entries
                .iter()
                .map(|l| (l.by.clone(), l.text.clone()))
                .collect(),
            _ => unreachable!(),
        })
        .collect()
}

fn hook_entry(text: &str) -> (String, String) {
    ("aa/x".to_owned(), text.to_owned())
}

#[tokio::test]
async fn a_retry_of_a_failed_first_request_calls_the_hook_again_and_logs_the_new_lead() {
    let calls = Calls::default();
    let mut rig = hooked(vec![failed(ErrorKind::Server), done("ok")], &calls).rig();
    rig.run(vec![ClientCommand::send("tui-1", "hello")], 1)
        .await;
    rig.run(vec![retry_turn(1, None, 2)], 1).await;
    assert_eq!(calls.count("x"), 2);
    let leads = leads(&records(&rig.store));
    assert!(leads[0].contains(&hook_entry("call 1\n")), "{leads:?}");
    assert!(leads[1].contains(&hook_entry("call 2\n")), "{leads:?}");
    let bodies = rig.a.texts();
    assert!(bodies[0].contains("call 1\\n") && bodies[1].contains("call 2\\n"));
    rig.assert_rebuilds();
}

#[tokio::test]
async fn a_retry_that_continues_a_committed_message_calls_no_hook() {
    let calls = Calls::default();
    let mut rig = hooked(
        vec![
            done("first answer").then(ClientCommand::send("tui-2", "one more thing")),
            failed(ErrorKind::Overloaded),
            done("second answer"),
        ],
        &calls,
    )
    .rig();
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    rig.run(vec![retry_turn(1, None, 4)], 1).await;
    assert_eq!(calls.count("x"), 1);
    let bodies = rig.a.texts();
    assert_eq!(bodies[2], bodies[1]);
}

#[tokio::test]
async fn rewind_and_resume_replay_the_lead_without_calling_the_hook() {
    let calls = Calls::default();
    let mut rig = hooked(vec![done("one"), done("two"), done("again")], &calls).rig();
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    rig.run(vec![ClientCommand::send("tui-2", "more")], 1).await;
    rig.idle(vec![ClientCommand::Rewind {
        to: RewindPoint::Turn { turn: 2 },
        request: RequestId(1),
    }])
    .await;
    assert_eq!(calls.count("x"), 2);
    let records = records(&rig.store);
    let to = records
        .iter()
        .find_map(|r| match r.event {
            LogEvent::Rewound { to, .. } => Some(to),
            _ => None,
        })
        .unwrap();
    let runs: Vec<u64> = records
        .iter()
        .filter(|r| matches!(&r.event, LogEvent::HookRan { hook, .. } if hook == "aa/x"))
        .map(|r| r.seq)
        .collect();
    assert_eq!(runs.len(), 2);
    assert!(runs[0] < to && to < runs[1], "{runs:?} {to}");
    rig.run(vec![ClientCommand::send("tui-3", "more")], 1).await;
    assert_eq!(calls.count("x"), 3);
    let bodies = rig.a.texts();
    let first = |body: &str| {
        let value: serde_json::Value = serde_json::from_str(body).unwrap();
        value["messages"][0].to_string()
    };
    assert_eq!(first(&bodies[2]), first(&bodies[0]));
    assert!(first(&bodies[0]).contains("call 1\\n"));
    assert!(bodies[2].contains("call 3\\n"));
    let resumed_calls = Calls::default();
    let mut again = hooked(vec![done("again")], &resumed_calls);
    again.from = Some(records);
    let mut again = again.rig();
    assert_eq!(resumed_calls.count("x"), 0);
    again
        .run(vec![ClientCommand::send("tui-3", "more")], 1)
        .await;
    assert_eq!(resumed_calls.count("x"), 1);
    let resumed = &again.a.texts()[0];
    assert_eq!(first(resumed), first(&bodies[0]));
    let kept = |body: &str| {
        let value: serde_json::Value = serde_json::from_str(body).unwrap();
        value["messages"].as_array().unwrap()[..2].to_vec()
    };
    assert_eq!(kept(resumed), kept(&bodies[2]));
    assert!(resumed.contains("call 1\\n"));
}

#[tokio::test]
async fn esc_during_after_response_gives_each_call_one_result_and_one_cancelled_end() {
    let calls = Calls::default();
    let started = Arc::new(tokio::sync::Notify::new());
    let held = started.clone();
    let script: HookScript = Arc::new(move |_, _, _| {
        let held = held.clone();
        Box::pin(async move {
            held.notify_one();
            std::future::pending::<()>().await;
            Reply::None
        })
    });
    let body = "\n[[hooks]]\nname = \"heard\"\npoint = \"after-response\"\n";
    let make: crate::plugin::Make = kit::instantiate(script, calls.clone());
    let mut setup = Setup::new(vec![uses(&[tool_call()])]);
    setup.hooks = vec![(kit::manifest("aa", body), make)];
    let mut rig = setup.rig();
    let tx = rig.tx.clone();
    let canceller = tokio::spawn(async move {
        within("the hook's start", started.notified()).await;
        tx.send(ClientCommand::Cancel).await.unwrap();
    });
    let events = rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    canceller.await.unwrap();
    assert!(
        matches!(
            outcomes(&events).as_slice(),
            [TurnOutcome::Cancelled { .. }]
        ),
        "{:?}",
        outcomes(&events)
    );
    let history = rig.agent.history();
    let uses: Vec<&str> = history
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    let results: Vec<&str> = history
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(uses, results);
    assert_eq!(uses.len(), 1);
    rig.assert_rebuilds();
}

#[tokio::test]
async fn a_point_deferred_during_a_call_fires_before_the_steps_next_call() {
    let calls = Calls::default();
    let body = "\n[[hooks]]\nname = \"noted\"\npoint = \"settings-changed\"\n";
    let make: crate::plugin::Make = kit::instantiate(kit::none(), calls.clone());
    let change = ClientCommand::ChangeSettings {
        request: RequestId(1),
        changes: vec![
            wisp_protocol::settings::SettingChange::named(
                "context.count_tokens",
                wisp_protocol::settings::SettingValue::Bool(false),
            )
            .unwrap(),
        ],
        save: false,
    };
    let two = [
        call("c1", "sleep", serde_json::json!({"ms": 50})),
        call("c2", "sleep", serde_json::json!({"ms": 0})),
    ];
    let mut setup = Setup::new(vec![uses(&two).then(change), done("ok")]);
    setup.hooks = vec![(kit::manifest("aa", body), make)];
    let mut rig = setup.rig();
    let events = rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    let at = |want: &dyn Fn(&AgentEvent) -> bool| position(&events, want);
    let noted =
        at(&|e| matches!(e, AgentEvent::HookRan { point, .. } if point == "settings-changed"));
    let first =
        at(&|e| matches!(e, AgentEvent::ToolCallStarted { call_id, .. } if call_id == "c1"));
    let second =
        at(&|e| matches!(e, AgentEvent::ToolCallStarted { call_id, .. } if call_id == "c2"));
    assert!(first < noted && noted < second, "{first} {noted} {second}");
    assert_eq!(calls.count("noted"), 1);
}

#[tokio::test]
async fn esc_during_the_second_before_request_keeps_the_committed_tool_results() {
    let calls = Calls::default();
    let started = Arc::new(tokio::sync::Notify::new());
    let held = started.clone();
    let n = Arc::new(AtomicUsize::new(0));
    let script: HookScript = Arc::new(move |_, _, _| {
        let held = held.clone();
        let second = n.fetch_add(1, Ordering::SeqCst) == 1;
        Box::pin(async move {
            if second {
                held.notify_one();
                std::future::pending::<()>().await;
            }
            Reply::None
        })
    });
    let body = "\n[[hooks]]\nname = \"asked\"\npoint = \"before-request\"\n";
    let make: crate::plugin::Make = kit::instantiate(script, calls.clone());
    let mut setup = Setup::new(vec![uses(&[tool_call()]), done("next")]);
    setup.hooks = vec![(kit::manifest("aa", body), make)];
    let mut rig = setup.rig();
    let tx = rig.tx.clone();
    let canceller = tokio::spawn(async move {
        within("the hook's start", started.notified()).await;
        tx.send(ClientCommand::Cancel).await.unwrap();
    });
    let events = rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    canceller.await.unwrap();
    assert!(
        matches!(
            outcomes(&events).as_slice(),
            [TurnOutcome::Cancelled { .. }]
        ),
        "{:?}",
        outcomes(&events)
    );
    assert_eq!(rig.a.texts().len(), 1);
    let ids = |pick: fn(&ContentBlock) -> Option<&str>| -> Vec<String> {
        rig.agent
            .history()
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| pick(b).map(str::to_owned))
            .collect()
    };
    let uses = ids(|b| match b {
        ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
        _ => None,
    });
    let results = ids(|b| match b {
        ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
        _ => None,
    });
    assert_eq!(uses, ["c1"]);
    assert_eq!(results, uses);
    rig.assert_rebuilds();
    rig.run(vec![ClientCommand::send("tui-2", "again")], 1)
        .await;
    let bodies = rig.a.texts();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[1].contains("\"tool_result\""), "{}", bodies[1]);
    assert!(
        bodies[1].contains("\"tool_use_id\":\"c1\""),
        "{}",
        bodies[1]
    );
}

#[tokio::test]
async fn a_rewind_of_a_hooked_turn_returns_its_pasted_image() {
    let calls = Calls::default();
    let mut rig = hooked(vec![done("one")], &calls).rig();
    let pasted = ClientCommand::Send {
        id: InputId("tui-1".into()),
        text: "look".into(),
        images: vec![ImageData { data: RED.into() }],
        mode: DeliveryMode::Steer,
        turn: None,
        replaces: None,
    };
    rig.run(vec![pasted], 1).await;
    let back = rig
        .idle(vec![ClientCommand::Rewind {
            to: RewindPoint::Turn { turn: 1 },
            request: RequestId(1),
        }])
        .await;
    let returned = back
        .iter()
        .find_map(|e| match e {
            AgentEvent::Rewound { returned, .. } => Some(returned.clone()),
            _ => None,
        })
        .expect("rewound");
    assert_eq!(returned.len(), 1, "{returned:?}");
    assert_eq!(returned[0].text, "look");
    assert_eq!(returned[0].images.len(), 1, "{returned:?}");
    assert!(returned[0].missing.is_empty(), "{returned:?}");
    assert_eq!(calls.count("x"), 1);
    assert!(rig.agent.history().is_empty());
}

fn sent_suffix() -> HookScript {
    Arc::new(|_, _, event| {
        let wisp_plugin_api::hooks::Event::UserInput(input) = event else {
            panic!("{event:?}");
        };
        Box::pin(async move { Reply::Rewrite(format!("{} [sent]", input.text)) })
    })
}

fn rewriting(plan: Vec<Step>, calls: &Calls) -> Setup {
    let body = "\n[[hooks]]\nname = \"u\"\npoint = \"user-input\"\n";
    let make: crate::plugin::Make = kit::instantiate(sent_suffix(), calls.clone());
    let mut setup = Setup::new(plan);
    setup.hooks = vec![(kit::manifest("aa", body), make)];
    setup
}

#[tokio::test]
async fn a_retry_of_a_rewritten_turn_resends_the_text_sent_and_calls_no_user_input_hook() {
    let calls = Calls::default();
    let mut rig = rewriting(vec![failed(ErrorKind::Server), done("ok")], &calls).rig();
    rig.run(vec![ClientCommand::send("tui-1", "hello")], 1)
        .await;
    rig.run(vec![retry_turn(1, None, 2)], 1).await;
    assert_eq!(calls.count("u"), 1);
    let bodies = rig.a.texts();
    for body in &bodies {
        assert!(body.contains("\"text\":\"hello [sent]\""), "{body}");
    }
    let applied: Vec<(Option<InputId>, HookChange)> = pick!(
        log_events(&rig.store),
        LogEvent::HookApplied {
            input_id, change, ..
        } => (input_id, change)
    );
    assert_eq!(
        applied,
        [(
            Some(InputId("tui-1".into())),
            HookChange::Text {
                before: "hello".into(),
                after: "hello [sent]".into(),
            }
        )]
    );
    rig.assert_rebuilds();
}

#[tokio::test]
async fn a_rewritten_steer_resumed_and_retried_resends_the_text_sent_and_calls_no_hook() {
    let calls = Calls::default();
    let mut live = rewriting(
        vec![
            uses(&[tool_call()]).then(ClientCommand::send("tui-2", "and also this")),
            failed(ErrorKind::Server),
            done("finished"),
        ],
        &calls,
    )
    .rig();
    live.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    assert_eq!(calls.count("u"), 2);
    let records = records(&live.store);
    live.run(vec![retry_turn(1, None, 3)], 1).await;
    assert!(live.a.texts()[2].contains("and also this [sent]"));

    let again_calls = Calls::default();
    let mut setup = rewriting(vec![done("finished")], &again_calls);
    setup.from = Some(records.clone());
    let mut again = setup.rig();
    again.run(vec![retry_turn(1, None, 3)], 1).await;
    assert_eq!(again.a.texts()[0], live.a.texts()[2]);
    assert_eq!(again_calls.count("u"), 0);
    let typed = records.iter().any(|r| {
        matches!(&r.event, LogEvent::InputAccepted { id, text, .. } if id.0 == "tui-2" && text == "and also this")
    });
    assert!(typed);
    let cut_at = records
        .iter()
        .position(|r| {
            matches!(&r.event, LogEvent::HookApplied { input_id: Some(id), .. } if id.0 == "tui-2")
        })
        .unwrap();
    let cut_calls = Calls::default();
    let mut setup = rewriting(Vec::new(), &cut_calls);
    setup.from = Some(records[..=cut_at].to_vec());
    let cut = setup.rig();
    let returned: Vec<(String, String)> = cut
        .agent
        .returned_inputs()
        .iter()
        .map(|r| (r.id.0.clone(), r.text.clone()))
        .collect();
    assert_eq!(returned, [("tui-2".to_owned(), "and also this".to_owned())]);
}

fn rewound_texts(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Rewound { returned, .. } => {
                Some(returned.iter().map(|r| r.text.clone()).collect())
            }
            _ => None,
        })
        .expect("rewound")
}

#[tokio::test]
async fn a_rewind_of_a_rewritten_turn_returns_the_text_sent_live_and_after_a_resume() {
    let calls = Calls::default();
    let mut rig = rewriting(vec![done("ok")], &calls).rig();
    rig.run(vec![ClientCommand::send("tui-1", "hello")], 1)
        .await;
    let records = records(&rig.store);
    let back = rig
        .idle(vec![ClientCommand::Rewind {
            to: RewindPoint::Turn { turn: 1 },
            request: RequestId(1),
        }])
        .await;
    assert_eq!(rewound_texts(&back), ["hello [sent]"]);

    let again_calls = Calls::default();
    let mut setup = rewriting(Vec::new(), &again_calls);
    setup.from = Some(records);
    let mut again = setup.rig();
    let back = again
        .idle(vec![ClientCommand::Rewind {
            to: RewindPoint::Turn { turn: 1 },
            request: RequestId(1),
        }])
        .await;
    assert_eq!(rewound_texts(&back), ["hello [sent]"]);
    assert_eq!(calls.count("u"), 1);
    assert_eq!(again_calls.count("u"), 0);
}

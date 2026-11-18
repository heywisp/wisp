use wisp_protocol::Dropped;

use super::*;

fn rewound(events: &[AgentEvent]) -> Option<&Dropped> {
    events.iter().find_map(|e| match e {
        AgentEvent::Rewound { dropped, .. } => Some(dropped),
        _ => None,
    })
}

fn cwds(events: &[AgentEvent]) -> Vec<String> {
    pick!(events, AgentEvent::ToolCallStarted { cwd, .. } => cwd.clone())
}

fn sleep_in(id: &str, cd: Option<&str>) -> B {
    match cd {
        Some(dir) => call(id, "sleep", serde_json::json!({"ms": 0, "cd": dir})),
        None => call(id, "sleep", serde_json::json!({"ms": 0})),
    }
}

#[tokio::test]
async fn a_rewind_to_turn_one_undoes_a_cd_and_sends_the_environment_again() {
    let mut rig = Setup::new(vec![
        uses(&[sleep_in("a", Some("sub"))]),
        done("one"),
        uses(&[sleep_in("b", None)]),
        done("two"),
        uses(&[sleep_in("c", Some("sub"))]),
        done("again"),
    ])
    .rig();
    std::fs::create_dir(rig._env.work.join("sub")).unwrap();
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    let second = rig.run(vec![ClientCommand::send("tui-2", "more")], 1).await;
    assert_eq!(cwds(&second), ["~/work/sub"]);
    let back = rig.idle(vec![rewind(1, 1)]).await;
    assert_eq!(rewound(&back).map(|d| d.turns.clone()), Some(vec![1, 2]));
    assert!(rig.agent.history().is_empty());
    let again = rig.run(vec![ClientCommand::send("tui-3", "go")], 1).await;
    assert_eq!(cwds(&again), ["~/work"]);
    let bodies = rig.a.texts();
    assert_eq!(bodies[4], bodies[0]);
    rig.assert_rebuilds_log();
}

#[tokio::test]
async fn a_rewind_to_turn_two_keeps_the_cd_made_before_it() {
    let mut rig = Setup::new(vec![
        uses(&[sleep_in("a", Some("sub"))]),
        done("one"),
        uses(&[sleep_in("b", Some(".."))]),
        done("two"),
        uses(&[sleep_in("c", None)]),
        done("again"),
    ])
    .rig();
    std::fs::create_dir(rig._env.work.join("sub")).unwrap();
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    rig.run(vec![ClientCommand::send("tui-2", "up")], 1).await;
    rig.idle(vec![rewind(1, 2)]).await;
    let again = rig.run(vec![ClientCommand::send("tui-3", "up")], 1).await;
    assert_eq!(cwds(&again), ["~/work/sub"]);
    let bodies = rig.a.texts();
    assert_eq!(without_stamps(&bodies[4]), without_stamps(&bodies[2]));
    rig.assert_rebuilds_log();
}

#[tokio::test]
async fn a_rewind_lapses_the_retry_offer() {
    let mut rig = Setup::new(vec![done("one"), failed(ErrorKind::Server), done("again")]).rig();
    rig.run(vec![ClientCommand::send("tui-1", "first")], 1)
        .await;
    rig.run(vec![ClientCommand::send("tui-2", "second")], 1)
        .await;
    assert!(rig.agent.retry_offer().is_some());
    let back = rig.idle(vec![rewind(1, 1)]).await;
    assert!(rewound(&back).is_some(), "{back:#?}");
    assert!(rig.agent.retry_offer().is_none());
    let refused = rig.idle(vec![retry_turn(2, None, 2)]).await;
    assert_eq!(refusals(&refused).len(), 1, "{refused:#?}");
}

#[tokio::test]
async fn a_session_without_a_log_rewinds_from_its_marks() {
    let a = planned(vec![done("one"), done("two"), done("again")]);
    let env = Env::new("rewind-no-log");
    let (tx, mut rx) = mpsc::channel(16);
    let mut agent = Agent::new(a.clone(), default_session())
        .with_clock(Arc::new(still_clock()))
        .with_tools(test_toolbox(), env.workspace())
        .with_platform("Linux")
        .with_resources(None);
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    for command in [
        ClientCommand::send("tui-1", "first"),
        ClientCommand::send("tui-2", "second"),
    ] {
        tx.send(command).await.unwrap();
        let serve = agent.serve(&mut rx, &ev_tx);
        tokio::pin!(serve);
        loop {
            tokio::select! {
                () = &mut serve => break,
                event = ev_rx.recv() => {
                    if matches!(event, Some(AgentEvent::TurnEnded { .. }) | None) {
                        break;
                    }
                }
            }
        }
    }
    assert_eq!(agent.history().len(), 4);
    tx.send(rewind(1, 2)).await.unwrap();
    let mut seen = Vec::new();
    {
        let serve = agent.serve(&mut rx, &ev_tx);
        tokio::pin!(serve);
        loop {
            tokio::select! {
                () = &mut serve => break,
                event = ev_rx.recv() => {
                    let Some(event) = event else { break };
                    let done = matches!(event, AgentEvent::Rewound { .. });
                    seen.push(event);
                    if done {
                        break;
                    }
                }
            }
        }
    }
    assert_eq!(rewound(&seen).map(|d| d.messages), Some(2));
    assert_eq!(agent.history().len(), 2);
}

impl Rig {
    fn assert_rebuilds_log(&self) {
        let records = records(&self.store);
        let rebuilt = crate::session::rebuild_checked(&self.store, &records, None)
            .unwrap()
            .unwrap()
            .messages;
        assert_eq!(rebuilt, &*self.agent.history());
    }
}

fn merged_plan() -> Vec<Step> {
    vec![
        uses(&[sleep_in("a", None)]),
        hanging(&[B::Text("thinking")]).then(ClientCommand::Cancel),
        done("two"),
        done("three"),
        done("again"),
    ]
}

#[tokio::test]
async fn a_point_inside_a_merged_message_cuts_it_and_resends_its_prefix() {
    let mut rig = Setup::new(merged_plan()).rig();
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    let open = rig.agent.history().len();
    let tail_blocks = rig.agent.history()[open - 1].content.len();
    rig.run(vec![ClientCommand::send("tui-2", "two")], 1).await;
    assert_eq!(rig.agent.history().len(), open + 1, "the message merged");
    rig.run(vec![ClientCommand::send("tui-3", "three")], 1)
        .await;
    let back = rig.idle(vec![rewind(1, 2)]).await;
    let records = records(&rig.store);
    assert_eq!(rewound(&back).map(|d| d.turns.clone()), Some(vec![2, 3]));
    let history = rig.agent.history();
    assert_eq!(history.len(), open);
    assert_eq!(history[open - 1].content.len(), tail_blocks);
    rig.run(vec![ClientCommand::send("tui-4", "two b")], 1)
        .await;
    let bodies = rig.a.texts();
    assert_eq!(
        without_stamps(&bodies[4]).replace("two b", "two"),
        without_stamps(&bodies[2])
    );
    rig.assert_rebuilds_log();

    let mut again = Setup::new(vec![done("again")]);
    again.from = Some(records);
    let mut again = again.rig();
    again
        .run(vec![ClientCommand::send("tui-4", "two b")], 1)
        .await;
    assert_eq!(
        without_stamps(&again.a.texts()[0]),
        without_stamps(&bodies[4])
    );
}

#[tokio::test]
async fn a_resumed_rewind_to_turn_one_sends_what_live_sent() {
    let mut rig = Setup::new(vec![
        uses(&[sleep_in("a", Some("sub"))]),
        done("one"),
        done("two"),
        done("again"),
    ])
    .rig();
    std::fs::create_dir(rig._env.work.join("sub")).unwrap();
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    rig.run(vec![ClientCommand::send("tui-2", "more")], 1).await;
    rig.idle(vec![rewind(1, 1)]).await;
    let records = records(&rig.store);
    rig.run(vec![ClientCommand::send("tui-3", "go")], 1).await;
    let bodies = rig.a.texts();
    assert_eq!(bodies[3], bodies[0]);
    let mut again = Setup::new(vec![done("again")]);
    again.from = Some(records);
    let mut again = again.rig();
    again.run(vec![ClientCommand::send("tui-3", "go")], 1).await;
    assert_eq!(
        without_stamps(&again.a.texts()[0]),
        without_stamps(&bodies[3])
    );
}

#[tokio::test]
async fn a_resumed_session_rewinds_inside_a_merged_message_like_live() {
    let mut rig = Setup::new(vec![
        uses(&[sleep_in("a", None)]),
        hanging(&[B::Text("thinking")]).then(ClientCommand::Cancel),
        done("two"),
        done("three"),
        done("two b"),
        done("four"),
    ])
    .rig();
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    rig.run(vec![ClientCommand::send("tui-2", "two")], 1).await;
    rig.run(vec![ClientCommand::send("tui-3", "three")], 1)
        .await;
    rig.idle(vec![rewind(1, 3)]).await;
    rig.run(vec![ClientCommand::send("tui-4", "three b")], 1)
        .await;
    let records = records(&rig.store);
    rig.idle(vec![rewind(2, 2)]).await;
    rig.run(vec![ClientCommand::send("tui-5", "two c")], 1)
        .await;
    rig.assert_rebuilds_log();
    let bodies = rig.a.texts();
    let last = bodies.len() - 1;
    let mut again = Setup::new(vec![done("four")]);
    again.from = Some(records);
    let mut again = again.rig();
    let back = again.idle(vec![rewind(2, 2)]).await;
    assert!(rewound(&back).is_some(), "{back:#?}");
    again
        .run(vec![ClientCommand::send("tui-5", "two c")], 1)
        .await;
    assert_eq!(
        without_stamps(&again.a.texts()[0]),
        without_stamps(&bodies[last])
    );
}

fn file_refused() -> Step {
    let error = classified(
        ErrorKind::InvalidRequest,
        "Could not process file.",
        FailurePhase::Response,
        RetryClass::Never,
    )
    .refusing(crate::provider::Refused::Attachment);
    Step::from(vec![Ok(ProviderEvent::Sending), Err(error)])
}

fn images_in(body: &str) -> usize {
    body.matches("\"type\":\"image\"").count()
}

#[tokio::test]
async fn the_merged_exception_keeps_the_tails_notes_and_drops_the_paste() {
    let snap = call("s", "snap", serde_json::json!({"name": "shots/a.png"}));
    let mut rig = Setup::new(vec![
        uses(&[snap]),
        failed(ErrorKind::InvalidRequest),
        file_refused(),
        done("two"),
        done("three"),
        done("again"),
    ])
    .with_toolbox(crate::tool::Toolbox::new([crate::tool::snap::tool()]))
    .rig();
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    let open = rig.agent.history().len();
    let tail_blocks = rig.agent.history()[open - 1].content.len();
    let image = ClientCommand::Send {
        id: InputId("tui-2".into()),
        text: "two".into(),
        images: vec![ImageData { data: RED.into() }],
        mode: DeliveryMode::Steer,
        turn: None,
        replaces: None,
    };
    let second = rig.run(vec![image], 1).await;
    assert!(
        second
            .iter()
            .any(|e| matches!(e, AgentEvent::AttachmentsWithdrawn { .. })),
        "{second:#?}"
    );
    assert_eq!(rig.agent.history().len(), open + 1, "the message merged");
    rig.run(vec![ClientCommand::send("tui-3", "three")], 1)
        .await;
    let back = rig.idle(vec![rewind(1, 2)]).await;
    let records = records(&rig.store);
    let returned = back.iter().find_map(|e| match e {
        AgentEvent::Rewound { returned, .. } => Some(returned.clone()),
        _ => None,
    });
    let returned = returned.expect("rewound");
    assert_eq!(returned[0].text, "two");
    assert!(returned[0].images.is_empty(), "{returned:?}");
    assert_eq!(returned[0].missing.len(), 1, "{returned:?}");
    assert_eq!(rig.agent.history().len(), open);
    assert_eq!(rig.agent.history()[open - 1].content.len(), tail_blocks);
    rig.run(vec![ClientCommand::send("tui-4", "two b")], 1)
        .await;
    rig.assert_rebuilds_log();
    let bodies = rig.a.texts();
    let last = bodies.len() - 1;
    assert_eq!(images_in(&bodies[last]), 0, "{}", bodies[last]);
    let mut again = Setup::new(vec![done("again")])
        .with_toolbox(crate::tool::Toolbox::new([crate::tool::snap::tool()]));
    again.from = Some(records);
    let png = crate::tool::snap::png();
    again.blobs = vec![(
        crate::session::sha256_bytes_hex(&png),
        crate::tool::snap::mime("image/png"),
        png,
    )];
    let mut again = again.rig();
    again
        .run(vec![ClientCommand::send("tui-4", "two b")], 1)
        .await;
    assert_eq!(
        without_stamps(&again.a.texts()[0]),
        without_stamps(&bodies[last])
    );
}

#[tokio::test]
async fn a_rewind_restores_the_ledger_the_cap_evicted_after_the_point_and_resume_agrees() {
    let many: Vec<B> = (0..=crate::agent::compact::LEDGER_CALLS)
        .map(|n| sleep_in(&format!("b{n}"), None))
        .collect();
    let mut rig = Setup::new(vec![
        uses(&[sleep_in("a0", None), sleep_in("a1", None)]),
        done("one"),
        uses(&many),
        done("two"),
    ])
    .rig();
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    let at_point = rig.agent.compaction.calls_now();
    assert_eq!(at_point.len(), 2);
    rig.run(vec![ClientCommand::send("tui-2", "more")], 1).await;
    let before = records(&rig.store);
    rig.idle(vec![rewind(1, 2)]).await;
    assert_eq!(rig.agent.compaction.calls_now(), at_point);

    let mut after = Setup::new(Vec::new());
    after.from = Some(records(&rig.store));
    assert_eq!(after.rig().agent.compaction.calls_now(), at_point);

    let mut resumed = Setup::new(Vec::new());
    resumed.from = Some(before);
    let mut resumed = resumed.rig();
    resumed.idle(vec![rewind(1, 2)]).await;
    assert_eq!(resumed.agent.compaction.calls_now(), at_point);
}

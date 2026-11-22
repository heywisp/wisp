use super::*;

fn diff() -> Value {
    json!({"files": [{"path": "notes.txt", "before": "one\ntwo\n", "after": "one\n2\n"}]})
}

#[tokio::test]
async fn the_default_tools_through_one_turn() {
    let setup = Setup {
        tools: Tools::Default,
        ..Setup::default()
    };
    let script = vec![
        calls(
            "msg_01",
            400,
            vec![
                Part::Think,
                Part::Text("Running a command."),
                call("toolu_01", "bash", json!({"command": "echo hi"})),
            ],
        )
        .then(ClientCommand::send("s1", "Also show me the change.")),
        calls("msg_02", 520, vec![call("toolu_02", "show_diff", diff())]),
        calls(
            "msg_03",
            640,
            vec![call("toolu_03", "view_file", json!({"path": "red.png"}))],
        ),
        calls(
            "msg_04",
            760,
            vec![call("toolu_04", "ask_user", question())],
        ),
        calls("msg_05", 880, vec![call("toolu_05", "bash", json!({}))]),
        says("msg_06", 900, "Done."),
        says("msg_07", 960, "Got your answer."),
        says("msg_08", 1020, "You're welcome."),
    ];
    let rig = Rig::new("tools_default", setup, script).await;
    fs::write(rig.env.work().join("red.png"), crate::tool::snap::png()).unwrap();
    let mut live = rig.start();
    live.send(ClientCommand::send("u1", "Look around.")).await;
    let asked = live
        .until(|e| matches!(e, AgentEvent::QuestionAsked { .. }))
        .await;
    let AgentEvent::QuestionAsked { call_id, .. } = asked else {
        unreachable!()
    };
    let outcome = live.ended().await;
    assert!(completed(&outcome), "{outcome:?}");
    live.send(answer_first(&call_id)).await;
    let outcome = live.ended().await;
    assert!(completed(&outcome), "{outcome:?}");
    live.completed("u2", "Thanks.").await;
    live.counted_report(1).await;
    live.close().await;
    rig.check();
}

#[tokio::test]
async fn a_session_without_the_workspace_plugin_sends_no_environment() {
    let setup = Setup {
        tools: Tools::Default,
        workspace: false,
        ..Setup::default()
    };
    let script = vec![says("msg_01", 400, "Hi."), says("msg_02", 420, "Bye.")];
    let rig = Rig::new("environment_removed", setup, script).await;
    let mut live = rig.start();
    live.completed("u1", "Hello.").await;
    live.completed("u2", "Again.").await;
    live.close().await;
    rig.check();
    let seen = rig.server.seen();
    assert!(!seen.bodies.is_empty());
    for body in &seen.bodies {
        assert!(!body.contains("<environment>"), "{body}");
    }
}

#[tokio::test]
async fn a_context_report_after_a_resume_counts_the_whole_conversation() {
    let setup = Setup {
        tools: Tools::Default,
        models: Some("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n"),
        ..Setup::default()
    };
    let script = vec![
        says("msg_01", 40_000, "Nothing to see."),
        summary("msg_02", 40_100),
        says("msg_03", 900, "Still nothing."),
    ];
    let rig = Rig::new("context_after_resume", setup, script).await;
    let mut live = rig.start();
    live.completed("u1", "Look around.").await;
    live.send(ClientCommand::Compact {
        focus: None,
        request: request(1),
    })
    .await;
    let done = live
        .until(|e| match e {
            AgentEvent::Compacted { answers, .. }
            | AgentEvent::CompactionFailed { answers, .. } => *answers == Some(request(1)),
            AgentEvent::Rejected { request: r, .. } => *r == Some(request(1)),
            _ => false,
        })
        .await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    live.close().await;
    let mut back = rig.resume();
    back.counted_report(2).await;
    back.completed("u2", "Look again.").await;
    back.close().await;
    rig.check();
}

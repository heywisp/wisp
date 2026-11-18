use wisp_protocol::RequestId;

use super::*;
use crate::agent::tests::completed_last;
use crate::agent::tool_tests::retry_turn;

pub(super) const OTHER: &str = "other";

impl Rig {
    pub(super) async fn serve_for(&mut self, command: ClientCommand) -> Vec<AgentEvent> {
        self.provider.commands().try_send(command).unwrap();
        let (ev_tx, mut ev_rx) = mpsc::channel(4096);
        let mut seen = Vec::new();
        {
            let serve = self.agent.serve(&mut self.rx, &ev_tx);
            tokio::pin!(serve);
            loop {
                tokio::select! {
                    () = &mut serve => break,
                    event = ev_rx.recv() => {
                        let Some(event) = event else { break };
                        let done = matches!(
                            event,
                            AgentEvent::TurnEnded { .. } | AgentEvent::Rejected { .. }
                        );
                        seen.push(event);
                        if done {
                            break;
                        }
                    }
                }
            }
        }
        while let Ok(event) = ev_rx.try_recv() {
            seen.push(event);
        }
        seen
    }

    pub(super) fn with_other(&mut self, steps: Vec<Step>) -> Arc<Fake> {
        let other = searching(steps, self.provider.commands());
        let ready = vec![
            connection("env", self.provider.clone()),
            connection(OTHER, other.clone()),
        ];
        let placeholder = Agent::new(self.provider.clone(), default_session());
        let agent = std::mem::replace(&mut self.agent, placeholder);
        self.agent = agent.with_connections(ready, Vec::new(), "env");
        other
    }
}

pub(super) fn retry(endpoint: Option<&str>) -> ClientCommand {
    ClientCommand::Retry {
        turn: 1,
        endpoint: endpoint.map(str::to_owned),
        request: RequestId(1),
    }
}

#[tokio::test]
async fn retry_after_an_error_sends_the_open_search_again_and_it_ends_once() {
    let mut rig = rig(vec![
        uses(&[B::Search("s1", "rust"), echo("e1")]),
        failing(
            &[],
            mid_stream(ErrorKind::InvalidResponse, RetryClass::Never),
        ),
        done(&[B::Found("s1"), B::Text("found")]),
    ]);
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Error { offer: Some(_), .. }),
        "{outcome:?}"
    );
    assert!(ended(&events).is_empty());
    let events = rig.serve_for(retry(None)).await;
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 3);
    assert_eq!(
        bodies[2], bodies[1],
        "a Retry closes nothing ahead of its request"
    );
    assert!(!rig.kinds().contains(&"tail_closed".to_owned()));
    let ends = ended(&events);
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert!(matches!(ends[0].1, SearchEnd::Done(_)), "{ends:?}");
    assert!(matches!(
        events.last(),
        Some(AgentEvent::TurnEnded {
            outcome: TurnOutcome::Completed { .. },
            ..
        })
    ));
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn a_retry_that_fails_again_keeps_the_search_open() {
    let mut rig = rig(vec![
        uses(&[B::Search("s1", "rust"), echo("e1")]),
        failing(&[], mid_stream(ErrorKind::Server, RetryClass::Never)),
        failing(&[], mid_stream(ErrorKind::Server, RetryClass::Never)),
        done(&[B::Text("ok")]),
    ]);
    rig.turn("go").await;
    let events = rig.serve_for(retry(None)).await;
    assert!(ended(&events).is_empty());
    assert!(rig.agent.tail_live());
    let (_, events) = rig.turn("next").await;
    assert_eq!(
        ended(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Failed, true))]
    );
    assert_eq!(position(&rig.kinds(), "tail_closed").len(), 1);
}

#[tokio::test]
async fn a_retry_on_another_endpoint_closes_the_open_search_first() {
    let mut rig = rig(vec![
        uses(&[B::Search("s1", "rust"), echo("e1")]),
        failing(
            &[],
            mid_stream(ErrorKind::InvalidResponse, RetryClass::Never),
        ),
    ]);
    let other = rig.with_other(vec![done(&[B::Text("over here")])]);
    rig.turn("go").await;
    let events = rig.serve_for(retry(Some(OTHER))).await;
    assert_eq!(
        ended(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Failed, true))]
    );
    let kinds = rig.kinds();
    let changed = position(&kinds, "settings_changed");
    let closed = position(&kinds, "tail_closed");
    let started = position(&kinds, "turn_started");
    assert_eq!(closed.len(), 1);
    assert!(
        changed[0] < closed[0] && closed[0] < started[1],
        "{kinds:?}"
    );
    let sent = other.bodies();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["messages"][1]["content"][0], closed_text("rust"));
    assert_resumes_byte_for_byte(&rig);
}

async fn failed_paused_continuation(rig: &mut Rig) {
    let (outcome, _) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Error { offer: Some(_), .. }),
        "{outcome:?}"
    );
    assert!(rig.agent.paused_tail());
}

#[tokio::test]
async fn a_retry_on_another_endpoint_after_a_failed_paused_continuation_ends_in_a_user_note() {
    let mut rig = rig(vec![
        paused(&[B::Search("s1", "rust")]),
        failing(&[], mid_stream(ErrorKind::Server, RetryClass::Never)),
    ]);
    let other = rig.with_other(vec![done(&[B::Text("over here")])]);
    failed_paused_continuation(&mut rig).await;
    let events = rig.serve_for(retry(Some(OTHER))).await;
    assert!(completed_last(&events), "{events:#?}");
    let sent = other.bodies();
    assert_eq!(sent.len(), 1);
    let messages = sent[0]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3, "{messages:#?}");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"][0], closed_text("rust"));
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(
        messages[2]["content"],
        json!([{"type": "text", "text": CLOSED_PAUSE_NOTE}])
    );
    let request = &rig.agent.history()[..3];
    assert!(
        check_layout(
            request,
            &rig.agent.paused_flags(request.len()),
            &rig.agent.search.closed.calls,
            rig.agent.provider().as_ref(),
        )
        .is_ok()
    );
    let without_note = &rig.agent.history()[..2];
    let refused = crate::agent::check_request_layout(
        without_note,
        (
            &rig.agent.paused_flags(without_note.len()),
            &rig.agent.closed_tail_flags(without_note.len()),
        ),
        &rig.agent.search.closed.calls,
        rig.agent.provider().as_ref(),
    );
    assert!(refused.is_err(), "{refused:?}");
    assert_resumes_byte_for_byte(&rig);
}

fn ends_in_switch_note(body: &serde_json::Value) {
    let messages = body["messages"].as_array().unwrap();
    let last = messages.last().unwrap();
    assert_eq!(last["role"], "user", "{messages:#?}");
    assert_eq!(
        last["content"],
        json!([{"type": "text", "text": CLOSED_PAUSE_NOTE}])
    );
    assert_eq!(body.to_string().matches(CLOSED_PAUSE_NOTE).count(), 1);
}

#[tokio::test]
async fn a_second_retry_after_the_switched_retry_fails_still_ends_in_the_note() {
    for back in [false, true] {
        let mut steps = vec![
            paused(&[B::Search("s1", "rust")]),
            failing(&[], mid_stream(ErrorKind::Server, RetryClass::Never)),
        ];
        if back {
            steps.push(done(&[B::Text("back here")]));
        }
        let mut rig = rig(steps);
        let mut on_other = vec![failing(
            &[],
            mid_stream(ErrorKind::Server, RetryClass::Never),
        )];
        if !back {
            on_other.push(done(&[B::Text("over here")]));
        }
        let other = rig.with_other(on_other);
        failed_paused_continuation(&mut rig).await;
        let events = rig.serve_for(retry(Some(OTHER))).await;
        assert!(
            matches!(
                events.last(),
                Some(AgentEvent::TurnEnded {
                    outcome: TurnOutcome::Error { offer: Some(_), .. },
                    ..
                })
            ),
            "{back}: {events:#?}"
        );
        let endpoint = back.then_some("env");
        let events = rig.serve_for(retry_turn(2, endpoint, 2)).await;
        assert!(completed_last(&events), "{back}: {events:#?}");
        let last = if back {
            rig.provider.bodies().pop().unwrap()
        } else {
            other.bodies().pop().unwrap()
        };
        ends_in_switch_note(&last);
        assert_resumes_byte_for_byte(&rig);
    }
}

#[tokio::test]
async fn a_retry_on_the_same_endpoint_after_a_failed_paused_continuation_resends_the_pause() {
    let mut rig = rig(vec![
        paused(&[B::Search("s1", "rust")]),
        failing(&[], mid_stream(ErrorKind::Server, RetryClass::Never)),
        done(&[B::Found("s1"), B::Text("found")]),
    ]);
    failed_paused_continuation(&mut rig).await;
    let events = rig.serve_for(retry(None)).await;
    assert!(completed_last(&events), "{events:#?}");
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[2], bodies[1]);
    assert!(!bodies[2].to_string().contains(CLOSED_PAUSE_NOTE));
    assert_resumes_byte_for_byte(&rig);
}

pub(super) async fn serve_until_rewound(rig: &mut Rig, command: ClientCommand) -> Vec<AgentEvent> {
    rig.provider.commands().try_send(command).unwrap();
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    let mut seen = Vec::new();
    {
        let serve = rig.agent.serve(&mut rig.rx, &ev_tx);
        tokio::pin!(serve);
        loop {
            tokio::select! {
                () = &mut serve => break,
                event = ev_rx.recv() => {
                    let Some(event) = event else { break };
                    let done = matches!(event, AgentEvent::Rewound { .. } | AgentEvent::Rejected { .. });
                    seen.push(event);
                    if done { break; }
                }
            }
        }
    }
    seen
}

fn resumed_rig(steps: Vec<Step>, records: &[wisp_protocol::log::LogRecord]) -> Rig {
    let mut rig = rig(steps);
    let (store, log) = continued(records);
    let tools: Vec<crate::tool::ToolEntry> = samples().into_iter().map(|s| s.tool).collect();
    rig.agent = Agent::new(rig.provider.clone(), default_session())
        .with_clock(TickingClock::new())
        .with_tools(Toolbox::new(tools), rig._env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_log(log, log_context())
        .resume(records, 0)
        .unwrap();
    rig.store = store;
    rig
}

async fn rewound_then_again(rig: &mut Rig) -> String {
    let back = serve_until_rewound(
        rig,
        ClientCommand::Rewind {
            to: wisp_protocol::RewindPoint::Turn { turn: 2 },
            request: RequestId(7),
        },
    )
    .await;
    assert!(
        back.iter().any(|e| matches!(e, AgentEvent::Rewound { .. })),
        "{back:#?}"
    );
    let (outcome, _) = rig.turn("again").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let kinds = rig.kinds();
    let after = kinds.iter().rposition(|k| k == "rewound").unwrap();
    assert!(
        kinds[after..].contains(&"tail_closed".to_owned()),
        "{kinds:?}"
    );
    rig.provider.bodies().last().unwrap().to_string()
}

#[tokio::test]
async fn a_rewind_to_a_retry_point_closes_the_failed_pause_like_a_new_message() {
    let mut b = rig(vec![
        paused(&[B::Search("s1", "rust")]),
        failing(&[], mid_stream(ErrorKind::Server, RetryClass::Never)),
        done(&[B::Text("again")]),
    ]);
    failed_paused_continuation(&mut b).await;
    b.turn("again").await;
    let b_kinds = b.kinds();

    let mut a = rig(vec![
        paused(&[B::Search("s1", "rust")]),
        failing(&[], mid_stream(ErrorKind::Server, RetryClass::Never)),
        done(&[B::Found("s1"), B::Text("found")]),
        done(&[B::Text("again")]),
    ]);
    failed_paused_continuation(&mut a).await;
    let events = a.serve_for(retry(None)).await;
    assert!(completed_last(&events), "{events:#?}");
    let records = records(&a.store);
    let sent = rewound_then_again(&mut a).await;
    assert!(b_kinds.contains(&"tail_closed".to_owned()));
    let closed = closed_text("rust").to_string();
    assert!(sent.contains(&closed), "{sent}");
    assert!(
        b.provider
            .bodies()
            .last()
            .unwrap()
            .to_string()
            .contains(&closed)
    );

    let mut again = resumed_rig(vec![done(&[B::Text("again")])], &records);
    let resumed = rewound_then_again(&mut again).await;
    assert!(resumed.contains(&closed), "{resumed}");
}

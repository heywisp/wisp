use wisp_protocol::log::{LogRecord, LoggedInput};
use wisp_protocol::{
    DeliveryMode, ImageData, ImageType, InputId, RejectKind, RequestId, WithdrawnAttachment,
    WithdrawnBy,
};

use super::*;
use crate::provider::anthropic::capabilities;
use crate::session::{rebuild, rebuild_checked};
use crate::test_guard::{BLUE_PIXEL as BLUE, GREEN_PIXEL as GREEN, RED_PIXEL as RED, text};

struct Harness {
    agent: Agent,
    provider: Arc<Fake>,
    tx: mpsc::Sender<ClientCommand>,
    rx: mpsc::Receiver<ClientCommand>,
    store: MemoryStore,
    _env: Env,
}

fn staged(plan: Vec<Step>) -> (Arc<Fake>, Arc<Fake>) {
    let provider = scripted(plan);
    let row = provider.view(|k| k.caps = Some(Caps::Fn(capabilities::row)));
    (row, provider)
}

fn harness(plan: Vec<Step>) -> Harness {
    let env = Env::new("steer");
    let (tx, rx) = mpsc::channel(128);
    let (staged, provider) = staged(plan);
    provider.send_to(tx.clone());
    let store = MemoryStore::default();
    let agent = Agent::new(staged, default_session())
        .with_clock(TickingClock::new())
        .with_tools(
            ScriptTool::toolbox(tx.clone(), Duration::from_secs(10), Ops::default()),
            env.workspace(),
        )
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300))
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        );
    Harness {
        agent,
        provider,
        tx,
        rx,
        store,
        _env: env,
    }
}

fn turns(n: usize) -> impl Fn(&[AgentEvent]) -> bool {
    move |events| terminal(events) >= n
}

fn never(_: &[AgentEvent]) -> bool {
    false
}

fn nothing(_: &AgentEvent) -> Vec<ClientCommand> {
    Vec::new()
}

fn declining(e: &AgentEvent) -> Vec<ClientCommand> {
    match e {
        AgentEvent::AttachRequested { call_id, .. } => vec![ClientCommand::DeclineAttach {
            call_id: call_id.clone(),
        }],
        _ => Vec::new(),
    }
}

impl Harness {
    async fn drive(
        &mut self,
        sends: Vec<ClientCommand>,
        mut react: impl FnMut(&AgentEvent) -> Vec<ClientCommand>,
        until: impl Fn(&[AgentEvent]) -> bool,
        quiet: Duration,
    ) -> Vec<AgentEvent> {
        for command in sends {
            self.tx.try_send(command).unwrap();
        }
        let (ev_tx, mut ev_rx) = mpsc::channel(4096);
        let mut seen = Vec::new();
        {
            let tx = &self.tx;
            let serve = self.agent.serve(&mut self.rx, &ev_tx);
            tokio::pin!(serve);
            loop {
                tokio::select! {
                    () = &mut serve => break,
                    event = ev_rx.recv() => {
                        let Some(event) = event else { break };
                        for command in react(&event) {
                            tx.try_send(command).unwrap();
                        }
                        seen.push(event);
                        if until(&seen) {
                            break;
                        }
                    }
                    () = tokio::time::sleep(quiet) => break,
                }
            }
        }
        while let Ok(event) = ev_rx.try_recv() {
            seen.push(event);
        }
        seen
    }

    async fn run(&mut self, sends: Vec<ClientCommand>, n: usize) -> Vec<AgentEvent> {
        let events = self.drive(sends, nothing, turns(n), PATIENT).await;
        assert_eq!(terminal(&events), n, "{events:#?}");
        events
    }

    async fn idle(&mut self, sends: Vec<ClientCommand>) -> Vec<AgentEvent> {
        self.drive(sends, nothing, never, QUIET).await
    }

    fn assert_rebuilds(&self) {
        let events = log_events(&self.store);
        let rebuilt = rebuild(&self.store, events.iter()).unwrap().messages;
        assert_eq!(rebuilt, &*self.agent.history());
    }

    fn requests(&self) -> Vec<Vec<Message>> {
        self.provider
            .sent()
            .into_iter()
            .map(|r| r.messages)
            .collect()
    }
}

fn id(s: &str) -> InputId {
    InputId(s.into())
}

fn ids(list: &[&str]) -> Vec<InputId> {
    list.iter().map(|s| id(s)).collect()
}

fn steer(name: &str) -> ClientCommand {
    ClientCommand::send(name, name)
}

fn queue(name: &str) -> ClientCommand {
    ClientCommand::queue(name, name)
}

fn send(
    name: &str,
    mode: DeliveryMode,
    turn: Option<u64>,
    replaces: Option<&str>,
) -> ClientCommand {
    ClientCommand::Send {
        id: id(name),
        text: name.into(),
        images: Vec::new(),
        mode,
        turn,
        replaces: replaces.map(id),
    }
}

fn edit(name: &str, text: &str, mode: DeliveryMode, replaces: &str) -> ClientCommand {
    ClientCommand::Send {
        id: id(name),
        text: text.into(),
        images: Vec::new(),
        mode,
        turn: None,
        replaces: Some(id(replaces)),
    }
}

fn withdrawn(events: &[AgentEvent]) -> Vec<(Vec<InputId>, WithdrawnBy)> {
    pick!(events, AgentEvent::InputWithdrawn { ids, by } => (ids.clone(), by.clone()))
}

fn held(events: &[AgentEvent]) -> Vec<(Vec<InputId>, WithdrawnBy)> {
    pick!(events, AgentEvent::InputHeld { ids, by } => (ids.clone(), by.clone()))
}

fn started(events: &[AgentEvent]) -> Vec<u64> {
    pick!(events, AgentEvent::TurnStarted { turn, .. } => *turn)
}

fn outcomes(events: &[AgentEvent]) -> Vec<&TurnOutcome> {
    pick!(events, AgentEvent::TurnEnded { outcome, .. } => outcome)
}

fn before_end(events: &[AgentEvent], at: impl Fn(&AgentEvent) -> bool) {
    let found = events.iter().position(at).expect("the event was emitted");
    let end = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .unwrap();
    assert!(found < end, "{events:#?}");
}

fn extends(before: &[Message], after: &[Message]) {
    let (Some((last_before, head)), true) = (before.split_last(), after.len() >= before.len())
    else {
        panic!("{before:#?}\n{after:#?}")
    };
    assert_eq!(&after[..head.len()], head, "{before:#?}\n{after:#?}");
    let same = &after[head.len()];
    assert_eq!(same.role, last_before.role);
    assert!(
        same.content.starts_with(&last_before.content),
        "{before:#?}\n{after:#?}"
    );
}

fn assert_each_request_extends_the_last(requests: &[Vec<Message>]) {
    for pair in requests.windows(2) {
        extends(&pair[0], &pair[1]);
    }
}

fn assert_bodies_extend(provider: &Fake, pairs: &[(usize, usize)]) {
    let bodies = provider.texts();
    for (a, b) in pairs {
        let (head_a, msgs_a) = head_and_messages(&bodies[*a]);
        let (head_b, msgs_b) = head_and_messages(&bodies[*b]);
        assert_eq!(head_a, head_b);
        let open = msgs_a.strip_suffix(']').unwrap();
        assert!(
            msgs_b.starts_with(open)
                || msgs_a
                    .strip_suffix("]}]")
                    .is_some_and(|t| msgs_b.starts_with(t)),
            "\n{msgs_a}\n{msgs_b}"
        );
    }
}

fn two_calls() -> Step {
    uses(&[steps("a", &["write:one\n"]), steps("b", &["write:two\n"])])
}

#[tokio::test]
async fn a_steer_during_a_tool_reply_lands_after_both_results_with_no_stamp() {
    let mut h = harness(vec![two_calls(), done("ok")]);
    let events = h.run(vec![steer("go"), steer("also this")], 1).await;
    check_events(&events);
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Completed { steps: 2, .. }]
    ));
    assert_eq!(
        delivered(&events),
        [(ids(&["go"]), 1), (ids(&["also this"]), 2)]
    );
    let requests = h.requests();
    let history = h.agent.history();
    let mut expected = requests[0].clone();
    expected.push(history[1].clone());
    let mut tail = history[2].clone();
    assert_eq!(
        results(&tail).iter().map(|r| r.0).collect::<Vec<_>>(),
        ["a", "b"]
    );
    let steered = [text("also this"), text(STEER_NOTE)];
    assert_eq!(tail.content.split_off(tail.content.len() - 2), steered);
    expected.push(tail);
    let mut sent = requests[1].clone();
    let last = sent.last_mut().unwrap();
    assert_eq!(last.content.split_off(last.content.len() - 2), steered);
    assert_eq!(sent, expected);
    assert_bodies_extend(&h.provider, &[(0, 1)]);
    h.assert_rebuilds();
    let logged = log_events(&h.store);
    let committed: Vec<&LogEvent> = logged
        .iter()
        .filter(|e| {
            matches!(
                e,
                LogEvent::InputCommitted { .. } | LogEvent::MessageCommitted { .. }
            )
        })
        .collect();
    assert!(matches!(
        committed[2],
        LogEvent::InputCommitted { step: 2, inputs, .. } if inputs == &[LoggedInput { id: id("also this"), text: "also this".into(), images: Vec::new(), note: Some(STEER_NOTE.into()) }]
    ));
    assert!(matches!(committed[3], LogEvent::MessageCommitted { .. }));
}

#[tokio::test]
async fn a_steer_with_an_image_sends_the_image_before_its_text() {
    let mut h = harness(vec![uses(&[steps("a", &[])]), done("ok")]);
    let with_image = ClientCommand::Send {
        id: id("look"),
        text: "look at this".into(),
        images: vec![ImageData { data: RED.into() }],
        mode: DeliveryMode::Steer,
        turn: Some(1),
        replaces: None,
    };
    let broken = ClientCommand::Send {
        id: id("broken"),
        text: "and this".into(),
        images: vec![ImageData {
            data: "not an image".into(),
        }],
        mode: DeliveryMode::Steer,
        turn: Some(1),
        replaces: None,
    };
    let events = h.run(vec![steer("go"), with_image, broken], 1).await;
    assert_eq!(
        rejected_kinds(&events),
        [(Some(id("broken")), Some(RejectKind::Image))]
    );
    let accepted = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::InputAccepted { id, images, .. } if id.as_str() == "look" => {
                Some(images.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(accepted.len(), 1);
    let requests = h.requests();
    let last = requests[1].last().unwrap();
    let n = last.content.len();
    assert!(matches!(last.content[n - 3], ContentBlock::Image { .. }));
    assert_eq!(
        last.content[n - 2..],
        [text("look at this"), text(STEER_NOTE)]
    );
    h.assert_rebuilds();
}

#[tokio::test]
async fn a_steer_during_a_text_reply_takes_one_more_step_in_the_same_turn() {
    let mut h = harness(vec![done("first"), done("second")]);
    let events = h.run(vec![steer("go"), steer("and this")], 1).await;
    check_events(&events);
    assert_eq!(terminal(&events), 1);
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Completed {
            steps: 2,
            committed: true,
            ..
        }]
    ));
    let history = h.agent.history();
    assert_eq!(history.len(), 4);
    let steered = Message {
        role: Role::User,
        content: vec![text("and this"), text(STEER_NOTE)],
    };
    assert_eq!(history[2], steered);
    let requests = h.requests();
    assert_eq!(requests[1][..2], history[..2]);
    assert_eq!(requests[1][2], steered);
    assert_bodies_extend(&h.provider, &[(0, 1)]);
    h.assert_rebuilds();
}

#[tokio::test]
async fn a_steer_after_an_uncommittable_reply_goes_with_the_turns_own_message() {
    let mut h = harness(vec![
        reply(&[B::Text("   ")], StopReason::MaxTokens),
        done("both"),
    ]);
    let events = h.run(vec![steer("go"), steer("and this")], 1).await;
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Completed {
            steps: 2,
            committed: true,
            ..
        }]
    ));
    let requests = h.requests();
    assert_eq!(requests[1].len(), 1);
    let sent = texts(&requests[1][0]);
    assert_eq!(&sent[sent.len() - 3..], ["go", "and this", STEER_NOTE]);
    assert_eq!(texts(&requests[0][0]), sent[..sent.len() - 2]);
    let history = h.agent.history();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0], requests[1][0]);
    h.assert_rebuilds();
}

#[tokio::test]
async fn three_steers_at_one_boundary_go_together_in_order() {
    let mut h = harness(vec![uses(&[steps("a", &[])]), done("ok")]);
    let events = h
        .run(vec![steer("go"), steer("s1"), steer("s2"), steer("s3")], 1)
        .await;
    assert_eq!(
        delivered(&events),
        [(ids(&["go"]), 1), (ids(&["s1", "s2", "s3"]), 2)]
    );
    let requests = h.requests();
    let last = requests[1].last().unwrap();
    assert_eq!(texts(last), ["s1", "s2", "s3", STEER_NOTE]);
    assert_eq!(h.requests().len(), 2);
    h.assert_rebuilds();
}

#[tokio::test]
async fn queued_messages_start_one_next_turn_in_the_order_typed() {
    let mut h = harness(vec![done("one"), done("two")]);
    let events = h.run(vec![steer("go"), queue("q1"), queue("q2")], 2).await;
    assert_eq!(started(&events), [1, 2]);
    assert_eq!(
        delivered(&events),
        [(ids(&["go"]), 1), (ids(&["q1", "q2"]), 1)]
    );
    let requests = h.requests();
    assert_eq!(requests.len(), 2);
    let second = requests[1].last().unwrap();
    let sent = texts(second);
    assert_eq!(sent.len(), 5);
    assert!(sent[0].starts_with("Sent: "), "{sent:?}");
    assert!(sent[1].starts_with("Context: "), "{sent:?}");
    assert_eq!(&sent[2..], ["q1", "q2", STEER_NOTE]);
    assert!(texts(&requests[0][0])[0].starts_with("<environment>"));
    let turn_two = log_events(&h.store)
        .into_iter()
        .find_map(|e| match e {
            LogEvent::TurnStarted {
                turn: 2,
                text,
                input_id,
                inputs,
                ..
            } => Some((text, input_id, inputs)),
            _ => None,
        })
        .unwrap();
    assert_eq!(turn_two.0, "");
    assert_eq!(turn_two.1, None);
    assert_eq!(
        turn_two
            .2
            .iter()
            .map(|i| (i.id.clone(), i.note.as_deref()))
            .collect::<Vec<_>>(),
        [(id("q1"), None), (id("q2"), Some(STEER_NOTE))]
    );
    assert_each_request_extends_the_last(&requests);
    assert_bodies_extend(&h.provider, &[(0, 1)]);
    h.assert_rebuilds();
}

#[tokio::test]
async fn a_steer_waiting_when_a_cancel_ends_the_turn_is_returned_and_a_queued_one_held() {
    let mut h = harness(vec![hanging(&[B::Text("partial")])]);
    let events = h
        .run(
            vec![steer("go"), steer("s"), queue("q"), ClientCommand::Cancel],
            1,
        )
        .await;
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Cancelled { .. }]
    ));
    assert_eq!(
        withdrawn(&events),
        [(ids(&["go", "s"]), WithdrawnBy::TurnStopped)]
    );
    assert_eq!(held(&events), [(ids(&["q"]), WithdrawnBy::TurnStopped)]);
    before_end(&events, |e| matches!(e, AgentEvent::InputHeld { .. }));
    let logged: Vec<LogEvent> = log_events(&h.store)
        .into_iter()
        .filter(|e| {
            matches!(
                e,
                LogEvent::InputWithdrawn { .. } | LogEvent::InputHeld { .. }
            )
        })
        .collect();
    assert_eq!(
        logged,
        [
            LogEvent::InputWithdrawn {
                turn: Some(1),
                ids: ids(&["go", "s"]),
                by: WithdrawnBy::TurnStopped,
            },
            LogEvent::InputHeld {
                turn: Some(1),
                ids: ids(&["q"]),
                by: WithdrawnBy::TurnStopped,
            },
        ]
    );
    let later = h.idle(Vec::new()).await;
    assert!(started(&later).is_empty(), "{later:#?}");
    assert_eq!(h.requests().len(), 1);
    assert!(h.agent.history().is_empty());
}

#[tokio::test]
async fn a_failed_turn_returns_its_steers_and_holds_its_queue() {
    let failing = Step::from(vec![
        Ok(ProviderEvent::Sending),
        Err(ProviderError::new(ErrorKind::Overloaded, "Overloaded")),
    ]);
    let mut h = harness(vec![failing]);
    let events = h.run(vec![steer("go"), steer("s"), queue("q")], 1).await;
    assert!(matches!(outcomes(&events)[..], [TurnOutcome::Error { .. }]));
    assert_eq!(
        withdrawn(&events),
        [(ids(&["go", "s"]), WithdrawnBy::TurnFailed)]
    );
    assert_eq!(held(&events), [(ids(&["q"]), WithdrawnBy::TurnFailed)]);
    let later = h.idle(Vec::new()).await;
    assert!(started(&later).is_empty());
}

#[tokio::test]
async fn a_cancel_found_by_the_final_drain_holds_the_queue_and_starts_no_turn() {
    let mut h = harness(vec![done("one").then(ClientCommand::Cancel)]);
    let events = h.run(vec![steer("go"), queue("q")], 1).await;
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Completed {
            committed: true,
            ..
        }]
    ));
    assert_eq!(held(&events), [(ids(&["q"]), WithdrawnBy::TurnStopped)]);
    let later = h
        .idle(vec![
            send("late-steer", DeliveryMode::Steer, Some(1), None),
            send("late-queue", DeliveryMode::Queue, Some(1), None),
        ])
        .await;
    assert!(started(&later).is_empty(), "{later:#?}");
    assert_eq!(
        withdrawn(&later),
        [(ids(&["late-steer"]), WithdrawnBy::TurnStopped)]
    );
    assert_eq!(
        held(&later),
        [(ids(&["late-queue"]), WithdrawnBy::TurnStopped)]
    );
    assert_eq!(h.requests().len(), 1);
}

#[tokio::test]
async fn a_send_for_a_stopped_turn_never_starts_one_and_one_for_a_completed_turn_does() {
    let mut h = harness(vec![hanging(&[B::Text("partial")]), done("two")]);
    h.run(vec![steer("go"), ClientCommand::Cancel], 1).await;
    let later = h
        .idle(vec![
            send("s", DeliveryMode::Steer, Some(1), None),
            send("q", DeliveryMode::Queue, Some(1), None),
        ])
        .await;
    assert!(started(&later).is_empty());
    assert_eq!(withdrawn(&later), [(ids(&["s"]), WithdrawnBy::TurnStopped)]);
    assert_eq!(held(&later), [(ids(&["q"]), WithdrawnBy::TurnStopped)]);
    let mut h = harness(vec![done("one"), done("two")]);
    h.run(vec![steer("go")], 1).await;
    let events = h
        .run(vec![send("next", DeliveryMode::Steer, Some(1), None)], 1)
        .await;
    assert_eq!(started(&events), [2]);
    assert_eq!(delivered(&events), [(ids(&["next"]), 1)]);
}

#[tokio::test]
async fn a_stale_send_for_a_stopped_turn_is_not_steered_into_a_later_one() {
    let mut h = harness(vec![hanging(&[B::Text("partial")]), done("two")]);
    h.run(vec![steer("go"), ClientCommand::Cancel], 1).await;
    let events = h
        .run(
            vec![
                steer("again"),
                send("stale", DeliveryMode::Steer, Some(1), None),
                send("stale-q", DeliveryMode::Queue, Some(1), None),
            ],
            1,
        )
        .await;
    assert_eq!(started(&events), [2]);
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Completed { steps: 1, .. }]
    ));
    assert_eq!(
        withdrawn(&events),
        [(ids(&["stale"]), WithdrawnBy::TurnStopped)]
    );
    assert_eq!(
        held(&events),
        [(ids(&["stale-q"]), WithdrawnBy::TurnStopped)]
    );
    assert_eq!(h.requests().len(), 2);
    let later = h.idle(Vec::new()).await;
    assert!(started(&later).is_empty());
}

#[tokio::test]
async fn a_closed_channel_withdraws_everything_with_session_ended() {
    let env = Env::new("closed");
    let failing = Step::from(vec![
        Ok(ProviderEvent::Sending),
        Err(ProviderError::new(ErrorKind::Overloaded, "Overloaded")),
    ]);
    for (reply, by) in [
        (hanging(&[B::Text("partial")]), WithdrawnBy::TurnStopped),
        (failing, WithdrawnBy::TurnFailed),
    ] {
        let (first, provider) = staged(vec![reply]);
        let store = MemoryStore::default();
        let mut agent = Agent::new(first, default_session())
            .with_clock(TickingClock::new())
            .with_tools(test_toolbox(), env.workspace())
            .with_platform("Linux")
            .with_resources(None)
            .with_log(
                SessionLog::new(Box::new(store.clone()), "s1"),
                log_context(),
            );
        let (tx, mut rx) = mpsc::channel(16);
        for command in [steer("go"), queue("q")] {
            tx.try_send(command).unwrap();
        }
        if by == WithdrawnBy::TurnStopped {
            tx.try_send(ClientCommand::Cancel).unwrap();
        }
        let (ev_tx, mut ev_rx) = mpsc::channel(4096);
        let serving = async {
            agent.serve(&mut rx, &ev_tx).await;
        };
        let mut seen = Vec::new();
        let mut tx = Some(tx);
        tokio::pin!(serving);
        loop {
            tokio::select! {
                () = &mut serving => break,
                Some(event) = ev_rx.recv() => {
                    if matches!(event, AgentEvent::TurnEnded { .. }) {
                        tx = None;
                    }
                    seen.push(event);
                }
            }
        }
        drop(tx);
        while let Ok(event) = ev_rx.try_recv() {
            seen.push(event);
        }
        assert_eq!(held(&seen), [(ids(&["q"]), by)]);
        assert_eq!(
            withdrawn(&seen).last(),
            Some(&(ids(&["q"]), WithdrawnBy::SessionEnded))
        );
        assert_eq!(provider.sent().len(), 1);
        let records = records(&store);
        assert!(matches!(
            records.last().unwrap().event,
            LogEvent::SessionEnded { .. }
        ));
    }

    let (second, _) = staged(vec![hanging(&[B::Text("partial")])]);
    let mut agent = Agent::new(second, default_session())
        .with_clock(TickingClock::new())
        .with_tools(test_toolbox(), env.workspace())
        .with_platform("Linux")
        .with_resources(None);
    let (tx, mut rx) = mpsc::channel(16);
    for command in [steer("go"), steer("s"), queue("q")] {
        tx.try_send(command).unwrap();
    }
    drop(tx);
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    agent.serve(&mut rx, &ev_tx).await;
    drop(ev_tx);
    let mut seen = Vec::new();
    while let Some(event) = ev_rx.recv().await {
        seen.push(event);
    }
    assert_eq!(
        withdrawn(&seen),
        [(ids(&["go", "s", "q"]), WithdrawnBy::SessionEnded)]
    );
    assert!(held(&seen).is_empty());
    assert_eq!(terminal(&seen), 1);
}

#[tokio::test]
async fn queued_messages_can_be_edited_switched_and_removed_in_place() {
    let mut h = harness(vec![uses(&[steps("a", &[])]), done("one"), done("two")]);
    let events = h
        .run(
            vec![
                steer("go"),
                queue("q1"),
                queue("q2"),
                queue("q3"),
                queue("q4"),
                edit("q2b", "q2 edited", DeliveryMode::Queue, "q2"),
                edit("q3s", "q3", DeliveryMode::Steer, "q3"),
                ClientCommand::Withdraw { ids: ids(&["q1"]) },
            ],
            2,
        )
        .await;
    assert_eq!(
        withdrawn(&events)[..3],
        [
            (ids(&["q2"]), WithdrawnBy::Replaced),
            (ids(&["q3"]), WithdrawnBy::Replaced),
            (ids(&["q1"]), WithdrawnBy::User),
        ]
    );
    assert_eq!(
        delivered(&events),
        [
            (ids(&["go"]), 1),
            (ids(&["q3s"]), 2),
            (ids(&["q2b", "q4"]), 1)
        ]
    );
    let requests = h.requests();
    assert_eq!(texts(requests[1].last().unwrap()), ["q3", STEER_NOTE]);
    let sent = texts(requests[2].last().unwrap());
    assert!(sent[1].starts_with("Context: "), "{sent:?}");
    assert_eq!(&sent[2..], ["q2 edited", "q4", STEER_NOTE]);
    assert_each_request_extends_the_last(&requests);
    h.assert_rebuilds();
}

#[tokio::test]
async fn held_messages_survive_another_stopped_turn_and_go_first_with_the_next_send() {
    let mut h = harness(vec![
        hanging(&[B::Text("partial")]),
        hanging(&[B::Text("partial")]),
        done("three"),
    ]);
    let first = h
        .run(vec![steer("go"), queue("q1"), ClientCommand::Cancel], 1)
        .await;
    assert_eq!(held(&first), [(ids(&["q1"]), WithdrawnBy::TurnStopped)]);
    let second = h
        .run(
            vec![
                steer("ask"),
                send("q2", DeliveryMode::Queue, Some(1), None),
                ClientCommand::Cancel,
            ],
            1,
        )
        .await;
    assert_eq!(delivered(&second), [(ids(&["q1", "ask"]), 1)]);
    assert_eq!(held(&second), [(ids(&["q2"]), WithdrawnBy::TurnStopped)]);
    assert_eq!(
        withdrawn(&second),
        [(ids(&["q1", "ask"]), WithdrawnBy::TurnStopped)]
    );
    let third = h.run(vec![queue("new")], 1).await;
    assert_eq!(delivered(&third), [(ids(&["q2", "new"]), 1)]);
    let requests = h.requests();
    let sent = texts(requests[2].last().unwrap());
    assert_eq!(&sent[sent.len() - 2..], ["q2", "new"]);
    h.assert_rebuilds();
}

#[tokio::test]
async fn held_messages_are_sent_with_the_next_message_or_alone_by_release() {
    let mut h = harness(vec![
        hanging(&[B::Text("partial")]),
        done("two"),
        done("three"),
    ]);
    h.agent.gone_released = true;
    let first = h
        .run(
            vec![steer("go"), queue("q1"), queue("q2"), ClientCommand::Cancel],
            1,
        )
        .await;
    assert_eq!(
        held(&first),
        [(ids(&["q1", "q2"]), WithdrawnBy::TurnStopped)]
    );
    let idle = h
        .idle(vec![
            edit("q1b", "q1 edited", DeliveryMode::Queue, "q1"),
            edit("q2s", "q2", DeliveryMode::Steer, "q2"),
        ])
        .await;
    assert!(started(&idle).is_empty());
    assert!(events_have_held_accept(&idle, "q1b"));
    assert_eq!(
        rejected_kinds(&idle),
        [(Some(id("q2s")), Some(RejectKind::NothingRunning))]
    );
    let events = h.run(vec![steer("new")], 1).await;
    assert_eq!(delivered(&events), [(ids(&["q1b", "q2", "new"]), 1)]);
    let requests = h.requests();
    let sent = texts(requests[1].last().unwrap());
    assert_eq!(&sent[sent.len() - 3..], ["q1 edited", "q2", "new"]);
    assert!(!sent.contains(&STEER_NOTE), "{sent:?}");
    let none = h.idle(vec![ClientCommand::Release]).await;
    assert_eq!(
        rejected_kinds(&none),
        [(None, Some(RejectKind::NothingHeld))]
    );
    assert!(started(&none).is_empty());
    h.assert_rebuilds();
}

fn events_have_held_accept(events: &[AgentEvent], name: &str) -> bool {
    events.iter().any(
        |e| matches!(e, AgentEvent::InputAccepted { id, held: true, .. } if id.as_str() == name),
    )
}

#[tokio::test]
async fn release_sends_held_messages_alone_and_held_ones_can_be_removed() {
    let mut h = harness(vec![hanging(&[B::Text("partial")]), done("two")]);
    h.run(
        vec![
            steer("go"),
            queue("q1"),
            queue("q2"),
            queue("q3"),
            ClientCommand::Cancel,
        ],
        1,
    )
    .await;
    let removed = h
        .idle(vec![ClientCommand::Withdraw { ids: ids(&["q2"]) }])
        .await;
    assert_eq!(withdrawn(&removed), [(ids(&["q2"]), WithdrawnBy::User)]);
    let again = h
        .idle(vec![
            ClientCommand::Withdraw { ids: ids(&["q2"]) },
            edit("q2c", "x", DeliveryMode::Queue, "q2"),
            ClientCommand::Withdraw {
                ids: ids(&["nobody"]),
            },
        ])
        .await;
    assert_eq!(
        rejected_kinds(&again),
        [
            (Some(id("q2")), Some(RejectKind::Withdrawn)),
            (Some(id("q2c")), Some(RejectKind::Withdrawn)),
            (Some(id("nobody")), Some(RejectKind::UnknownId)),
        ]
    );
    let events = h.run(vec![ClientCommand::Release], 1).await;
    assert_eq!(delivered(&events), [(ids(&["q1", "q3"]), 1)]);
    let requests = h.requests();
    let sent = texts(requests[1].last().unwrap());
    assert_eq!(&sent[sent.len() - 2..], ["q1", "q3"]);
    h.assert_rebuilds();
}

#[tokio::test]
async fn release_during_a_turn_queues_held_messages_for_its_end() {
    let mut h = harness(vec![
        hanging(&[B::Text("partial")]),
        done("two"),
        done("three"),
    ]);
    h.run(vec![steer("go"), ClientCommand::Cancel], 1).await;
    let events = h
        .run(
            vec![
                steer("ask"),
                send("q", DeliveryMode::Queue, Some(1), None),
                ClientCommand::Release,
                ClientCommand::Release,
            ],
            2,
        )
        .await;
    assert_eq!(held(&events), [(ids(&["q"]), WithdrawnBy::TurnStopped)]);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::InputUnheld { ids, .. } if ids == &[id("q")]))
    );
    assert_eq!(
        rejected_kinds(&events),
        [(None, Some(RejectKind::NothingHeld))]
    );
    assert_eq!(started(&events), [2, 3]);
    assert_eq!(delivered(&events), [(ids(&["ask"]), 1), (ids(&["q"]), 1)]);
    h.assert_rebuilds();
}

#[tokio::test]
async fn cancel_withdraws_waiting_and_delivered_but_uncommitted_steers() {
    let mut h = harness(vec![
        uses(&[steps("a", &["steer:s1"])]),
        hanging(&[B::Text("partial")]),
    ]);
    let events = h
        .drive(
            vec![steer("go")],
            |e| {
                if matches!(e, AgentEvent::InputDelivered { step: 2, .. }) {
                    vec![steer("s2"), ClientCommand::Cancel]
                } else {
                    Vec::new()
                }
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(delivered(&events)[1], (ids(&["s1"]), 2));
    assert_eq!(
        withdrawn(&events),
        [(ids(&["s1", "s2"]), WithdrawnBy::TurnStopped)]
    );
    before_end(&events, |e| matches!(e, AgentEvent::InputWithdrawn { .. }));
    let history = h.agent.history();
    assert_eq!(history.len(), 3);
    assert!(history.iter().all(|m| !texts(m).contains(&"s1")));
    h.assert_rebuilds();
}

#[tokio::test]
async fn withdraw_works_before_delivery_and_is_refused_after() {
    let mut h = harness(vec![uses(&[steps("a", &[])]), done("ok")]);
    let events = h
        .drive(
            vec![
                steer("go"),
                steer("s1"),
                ClientCommand::Withdraw { ids: ids(&["s1"]) },
                steer("s2"),
            ],
            |e| {
                if matches!(e, AgentEvent::InputDelivered { step: 2, .. }) {
                    vec![ClientCommand::Withdraw { ids: ids(&["s2"]) }]
                } else {
                    Vec::new()
                }
            },
            turns(1),
            PATIENT,
        )
        .await;
    let events = [events, h.idle(Vec::new()).await].concat();
    assert_eq!(withdrawn(&events), [(ids(&["s1"]), WithdrawnBy::User)]);
    assert_eq!(
        rejected_kinds(&events),
        [(Some(id("s2")), Some(RejectKind::AlreadyDelivered))]
    );
    let requests = h.requests();
    assert_eq!(texts(requests[1].last().unwrap()), ["s2", STEER_NOTE]);
}

#[tokio::test]
async fn a_repeated_id_is_refused_and_delivered_once() {
    let mut h = harness(vec![uses(&[steps("a", &[])]), done("ok")]);
    let events = h
        .run(vec![steer("go"), steer("s"), steer("s"), steer("go")], 1)
        .await;
    assert_eq!(
        rejected_kinds(&events),
        [
            (Some(id("s")), Some(RejectKind::Duplicate)),
            (Some(id("go")), Some(RejectKind::Duplicate)),
        ]
    );
    let requests = h.requests();
    assert_eq!(texts(requests[1].last().unwrap()), ["s", STEER_NOTE]);
    let later = h.idle(vec![steer("s")]).await;
    assert_eq!(
        rejected_kinds(&later),
        [(Some(id("s")), Some(RejectKind::Duplicate))]
    );
    assert!(started(&later).is_empty());
}

#[tokio::test]
async fn the_thirty_third_waiting_message_is_refused() {
    let mut h = harness(vec![done("one"), done("two")]);
    let names: Vec<String> = (1..=MAX_WAITING + 1).map(|n| format!("q{n}")).collect();
    let mut sends = vec![steer("go")];
    sends.extend(names.iter().map(|n| queue(n)));
    let events = h.run(sends, 2).await;
    let last = names.last().unwrap();
    assert_eq!(
        rejected_kinds(&events),
        [(Some(id(last)), Some(RejectKind::TooMany))]
    );
    assert_eq!(delivered(&events)[1].0.len(), MAX_WAITING);
}

#[tokio::test]
async fn a_send_racing_the_last_reply_keeps_the_turn_going() {
    let mut h = harness(vec![done("one").then(steer("late")), done("two")]);
    let events = h.run(vec![steer("go")], 1).await;
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Completed { steps: 2, .. }]
    ));
    assert_eq!(delivered(&events)[1], (ids(&["late"]), 2));
    h.assert_rebuilds();
}

fn withdraw(name: &str) -> ClientCommand {
    ClientCommand::Withdraw { ids: ids(&[name]) }
}

fn steps_started(events: &[AgentEvent]) -> Vec<u32> {
    pick!(events, AgentEvent::StepStarted { step } => *step)
}

#[tokio::test]
async fn a_steer_withdrawn_before_its_extra_step_ends_the_turn_completed_with_no_request() {
    let mut h = harness(vec![done("one").then(steer("late")).then(withdraw("late"))]);
    let events = h.run(vec![steer("go")], 1).await;
    let events = [events, h.idle(Vec::new()).await].concat();
    check_events(&events);
    assert_eq!(terminal(&events), 1);
    assert!(
        matches!(
            outcomes(&events)[..],
            [TurnOutcome::Completed {
                steps: 1,
                committed: true,
                stop_reason: StopReason::EndTurn,
                ..
            }]
        ),
        "{events:#?}"
    );
    assert_eq!(steps_started(&events), [1]);
    assert_eq!(h.requests().len(), 1);
    assert_eq!(delivered(&events), [(ids(&["go"]), 1)]);
    assert_eq!(withdrawn(&events), [(ids(&["late"]), WithdrawnBy::User)]);
    before_end(&events, |e| matches!(e, AgentEvent::InputWithdrawn { .. }));
    assert!(rejected_kinds(&events).is_empty(), "{events:#?}");
    assert_eq!(started(&events), [1]);
    let history = h.agent.history();
    assert_eq!(history.len(), 2);
    assert_eq!(texts(&history[1]), ["one"]);
    assert!(history.iter().all(|m| !texts(m).contains(&"late")));
    assert!(!log_events(&h.store).iter().any(|e| matches!(
        e,
        LogEvent::StepEnded { step: 2, .. } | LogEvent::InputCommitted { .. }
    )));
    h.assert_rebuilds();
}

#[tokio::test]
async fn one_of_two_steers_withdrawn_before_the_extra_step_leaves_the_other_delivered() {
    let mut h = harness(vec![
        done("one")
            .then(steer("s1"))
            .then(steer("s2"))
            .then(withdraw("s1")),
        done("two"),
    ]);
    let events = h.run(vec![steer("go")], 1).await;
    check_events(&events);
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Completed {
            steps: 2,
            committed: true,
            ..
        }]
    ));
    assert_eq!(withdrawn(&events), [(ids(&["s1"]), WithdrawnBy::User)]);
    assert_eq!(delivered(&events), [(ids(&["go"]), 1), (ids(&["s2"]), 2)]);
    let requests = h.requests();
    assert_eq!(requests.len(), 2);
    let steered = Message {
        role: Role::User,
        content: vec![text("s2"), text(STEER_NOTE)],
    };
    assert_eq!(requests[1].last(), Some(&steered));
    assert_layout(&h.provider);
    assert_bodies_extend(&h.provider, &[(0, 1)]);
    let history = h.agent.history();
    assert_eq!(history.len(), 4);
    assert!(history.iter().all(|m| !texts(m).contains(&"s1")));
    h.assert_rebuilds();
}

#[tokio::test]
async fn a_steer_withdrawn_after_an_uncommittable_reply_completes_without_resending() {
    let mut h = harness(vec![blank().then(steer("late")).then(withdraw("late"))]);
    let events = h.run(vec![steer("go")], 1).await;
    check_events(&events);
    assert!(
        matches!(
            outcomes(&events)[..],
            [TurnOutcome::Completed {
                steps: 1,
                committed: false,
                ..
            }]
        ),
        "{events:#?}"
    );
    assert_eq!(steps_started(&events), [1]);
    assert_eq!(h.requests().len(), 1);
    assert_eq!(
        withdrawn(&events),
        [
            (ids(&["late"]), WithdrawnBy::User),
            (ids(&["go"]), WithdrawnBy::NotKept)
        ]
    );
    assert!(h.agent.history().is_empty());
    h.assert_rebuilds();
}

#[tokio::test]
async fn a_steer_withdrawn_after_a_stopped_tool_reply_completes_on_the_results() {
    let mut h = harness(vec![
        reply(&[steps("a", &[])], StopReason::MaxTokens)
            .then(steer("late"))
            .then(withdraw("late")),
    ]);
    let events = h.run(vec![steer("go")], 1).await;
    check_events(&events);
    assert!(
        matches!(
            outcomes(&events)[..],
            [TurnOutcome::Completed {
                steps: 1,
                committed: true,
                stop_reason: StopReason::MaxTokens,
                ..
            }]
        ),
        "{events:#?}"
    );
    assert_eq!(steps_started(&events), [1]);
    assert_eq!(h.requests().len(), 1);
    assert_eq!(withdrawn(&events), [(ids(&["late"]), WithdrawnBy::User)]);
    let history = h.agent.history();
    assert_eq!(history.len(), 3);
    assert!(history.iter().all(|m| !texts(m).contains(&"late")));
    h.assert_rebuilds();
}

#[tokio::test]
async fn withdrawn_and_returned_inputs_leave_no_trace_in_later_requests() {
    let mut h = harness(vec![
        uses(&[steps("a", &["steer:returned"])]),
        hanging(&[B::Text("partial")]),
        done("next"),
    ]);
    h.drive(
        vec![
            steer("go"),
            steer("removed"),
            ClientCommand::Withdraw {
                ids: ids(&["removed"]),
            },
        ],
        |e| {
            if matches!(e, AgentEvent::InputDelivered { step: 2, .. }) {
                vec![ClientCommand::Cancel]
            } else {
                Vec::new()
            }
        },
        turns(1),
        PATIENT,
    )
    .await;
    h.run(vec![steer("then")], 1).await;
    let requests = h.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert!(request.iter().all(|m| !texts(m).contains(&"removed")));
    }
    assert!(requests[2].iter().all(|m| !texts(m).contains(&"returned")));
    let mut without = requests[1].clone();
    let last = without.last_mut().unwrap();
    assert_eq!(
        last.content.split_off(last.content.len() - 2),
        [text("returned"), text(STEER_NOTE)]
    );
    extends(&without, &requests[2]);
    assert_eq!(texts(requests[2].last().unwrap()).last(), Some(&"then"));
    h.assert_rebuilds();
}

#[tokio::test]
async fn held_inputs_released_with_a_new_message_append_after_the_stopped_tail() {
    let mut h = harness(vec![
        uses(&[steps("a", &[])]),
        hanging(&[B::Text("partial")]),
        done("next"),
    ]);
    h.drive(
        vec![steer("go"), queue("held")],
        |e| {
            if matches!(e, AgentEvent::StepStarted { step: 2 }) {
                vec![ClientCommand::Cancel]
            } else {
                Vec::new()
            }
        },
        turns(1),
        PATIENT,
    )
    .await;
    let committed = h.agent.history().to_vec();
    assert_eq!(committed.len(), 3);
    h.run(vec![steer("new")], 1).await;
    let requests = h.requests();
    extends(&requests[1], &requests[2]);
    let sent = texts(requests[2].last().unwrap());
    assert_eq!(&sent[sent.len() - 2..], ["held", "new"]);
    assert_eq!(requests[2][..2], committed[..2]);
    assert_bodies_extend(&h.provider, &[(0, 1), (1, 2)]);
    h.assert_rebuilds();
}

fn red(name: &str, mode: DeliveryMode, replaces: Option<&str>) -> ClientCommand {
    ClientCommand::Send {
        id: id(name),
        text: name.into(),
        images: vec![ImageData { data: RED.into() }],
        mode,
        turn: None,
        replaces: replaces.map(id),
    }
}

fn blank() -> Step {
    reply(&[B::Text("   ")], StopReason::MaxTokens)
}

#[tokio::test]
async fn image_limits_count_waiting_and_uncommitted_images_with_history() {
    let per_request = capabilities::row(DEFAULT_MODEL)
        .and_then(|r| r.images)
        .unwrap()
        .per_request;
    let mut h = harness(vec![uses(&[steps("a", &[])]), done("ok"), done("two")]);
    let old: Vec<ContentBlock> = (0..per_request - 3)
        .map(|_| ContentBlock::image(ImageType::Png, RED))
        .chain([text("old")])
        .collect();
    h.agent.history = vec![
        Message {
            role: Role::User,
            content: old,
        },
        Message {
            role: Role::Assistant,
            content: vec![text("seen")],
        },
    ];
    let events = h
        .run(
            vec![
                red("go", DeliveryMode::Steer, None),
                red("s1", DeliveryMode::Steer, None),
                red("q1", DeliveryMode::Queue, None),
                red("s2", DeliveryMode::Steer, None),
                red("s1b", DeliveryMode::Steer, Some("s1")),
            ],
            2,
        )
        .await;
    assert_eq!(
        rejected_kinds(&events),
        [(Some(id("s2")), Some(RejectKind::Image))]
    );
    assert_eq!(
        reasons(&events),
        [format!(
            "{DEFAULT_MODEL} takes at most {per_request} images in a request, and this conversation would send {}.",
            per_request + 1
        )]
    );
    assert_eq!(
        delivered(&events),
        [(ids(&["go"]), 1), (ids(&["s1b"]), 2), (ids(&["q1"]), 1)]
    );
    assert_eq!(withdrawn(&events), [(ids(&["s1"]), WithdrawnBy::Replaced)]);
    let requests = h.requests();
    let sent = requests[2]
        .iter()
        .flat_map(|m| m.content.iter())
        .filter(|b| matches!(b, ContentBlock::Image { .. }))
        .count();
    assert_eq!(sent, per_request);
}

#[tokio::test]
async fn a_queued_image_is_prepared_for_the_model_the_next_turn_runs_on() {
    let mut h = harness(vec![uses(&[steps("a", &[])]), done("ok")]);
    let switch = ClientCommand::shortcut(RequestId(1), None, Some("claude-unknown-9".into()), None);
    let events = h
        .run(
            vec![
                steer("go"),
                switch,
                red("later", DeliveryMode::Queue, None),
                red("now", DeliveryMode::Steer, None),
            ],
            1,
        )
        .await;
    assert_eq!(
        rejected_kinds(&events),
        [(Some(id("later")), Some(RejectKind::Image))]
    );
    assert_eq!(reasons(&events), [no_image_sentence("claude-unknown-9")]);
    assert_eq!(delivered(&events), [(ids(&["go"]), 1), (ids(&["now"]), 2)]);
    assert_eq!(started(&events), [1]);
    h.assert_rebuilds();
}

#[tokio::test]
async fn a_turn_that_keeps_nothing_withdraws_its_inputs_as_not_kept() {
    let mut h = harness(vec![blank(), blank()]);
    let events = h.run(vec![steer("go"), steer("and this")], 1).await;
    check_events(&events);
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Completed {
            steps: 2,
            committed: false,
            ..
        }]
    ));
    assert_eq!(
        withdrawn(&events),
        [(ids(&["go", "and this"]), WithdrawnBy::NotKept)]
    );
    before_end(&events, |e| matches!(e, AgentEvent::InputWithdrawn { .. }));
    assert!(h.agent.history().is_empty());
    assert!(log_events(&h.store).iter().any(|e| matches!(
        e,
        LogEvent::InputWithdrawn {
            by: WithdrawnBy::NotKept,
            ..
        }
    )));
    h.assert_rebuilds();
    let events = h
        .idle(vec![ClientCommand::Withdraw { ids: ids(&["go"]) }])
        .await;
    assert_eq!(
        rejected_kinds(&events),
        [(Some(id("go")), Some(RejectKind::Withdrawn))]
    );
}

#[tokio::test]
async fn a_steer_after_an_uncommittable_reply_lands_after_the_effort_marker() {
    let mut h = harness(vec![done("one"), blank(), done("two"), done("three")]);
    h.run(vec![steer("go")], 1).await;
    h.idle(vec![ClientCommand::shortcut(
        RequestId(2),
        None,
        None,
        Some(Effort::Low),
    )])
    .await;
    let events = h.run(vec![steer("next"), steer("and this")], 1).await;
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Completed {
            steps: 2,
            committed: true,
            ..
        }]
    ));
    let requests = h.requests();
    assert_eq!(requests[1].len(), 4);
    assert!(requests[1][3].effort_marker().is_some());
    assert_eq!(requests[2][..4], requests[1][..]);
    assert_eq!(requests[2][4].content, [text("and this"), text(STEER_NOTE)]);
    assert_eq!(requests[2].len(), 5);
    assert_bodies_extend(&h.provider, &[(1, 2)]);
    let history = h.agent.history();
    assert_eq!(history[..5], requests[2][..]);
    assert_eq!(history.len(), 6);
    h.assert_rebuilds();
    h.idle(vec![ClientCommand::shortcut(
        RequestId(3),
        None,
        Some("claude-opus-4-8".into()),
        None,
    )])
    .await;
    h.run(vec![steer("on a model without markers")], 1).await;
    assert!(h.requests()[3][3].effort_marker().unwrap().rendered);
    let body = h.provider.body(3);
    let messages = body["messages"].as_array().unwrap();
    assert!(messages.iter().all(|m| m.get("output_config").is_none()));
}

#[tokio::test]
async fn a_steer_after_an_unrendered_marker_sends_two_user_messages_that_extend_the_last_body() {
    let mut h = harness(vec![done("one"), blank(), done("two")]);
    h.run(vec![steer("go")], 1).await;
    h.idle(vec![ClientCommand::shortcut(
        RequestId(3),
        None,
        Some("claude-opus-4-8".into()),
        Some(Effort::Low),
    )])
    .await;
    let events = h.run(vec![steer("next"), steer("and this")], 1).await;
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Completed {
            steps: 2,
            committed: true,
            ..
        }]
    ));
    let requests = h.requests();
    assert_eq!(
        requests[2][3].effort_marker(),
        Some(EffortMarker {
            level: Effort::Low,
            rendered: false
        })
    );
    let bodies = h.provider.texts();
    let wire: Value = serde_json::from_str(&bodies[2]).unwrap();
    let roles: Vec<&str> = wire["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["user", "assistant", "user", "user"]);
    assert_eq!(wire["messages"][3]["content"][0]["text"], "and this");
    assert_bodies_extend(&h.provider, &[(1, 2)]);
    h.assert_rebuilds();
}
mod attachments;
mod jobs;
mod processes;
mod retries;
pub(in crate::agent) mod transcript;

fn attachment_refused() -> Step {
    let error = classified(
        ErrorKind::InvalidRequest,
        "Could not process image",
        FailurePhase::Response,
        RetryClass::Never,
    )
    .refusing(Refused::Attachment);
    Step::from(vec![Ok(ProviderEvent::Sending), Err(error)])
}

fn image_count(messages: &[Message]) -> usize {
    messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter(|b| matches!(b, ContentBlock::Image { .. }))
        .count()
}

#[tokio::test]
async fn a_steer_after_an_uncommittable_reply_withdraws_only_the_new_image() {
    let mut h = harness(vec![
        blank().then(red("late", DeliveryMode::Steer, None)),
        attachment_refused(),
        done("ok"),
    ]);
    let events = h.run(vec![red("go", DeliveryMode::Steer, None)], 1).await;
    check_events(&events);
    assert!(
        matches!(
            outcomes(&events)[..],
            [TurnOutcome::Completed {
                committed: true,
                ..
            }]
        ),
        "{events:#?}"
    );
    let items: Vec<&WithdrawnAttachment> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AttachmentsWithdrawn { items, .. } => Some(items),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(items.len(), 1, "{items:#?}");
    assert_eq!(items[0].label, "#1");
    let requests = h.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(image_count(&requests[0]), 1);
    assert_eq!(image_count(&requests[1]), 2);
    assert_eq!(image_count(&requests[2]), 1);
    let resent = texts(requests[2].last().unwrap());
    assert!(resent.contains(&items[0].note.as_str()), "{resent:?}");
    h.assert_rebuilds();
}

#[tokio::test]
async fn a_steered_image_refused_after_a_tool_step_is_withdrawn_and_rebuilds() {
    let mut h = harness(vec![
        uses(&[steps("a", &[])]).then(red("late", DeliveryMode::Steer, None)),
        attachment_refused(),
        done("ok"),
    ]);
    let events = h.run(vec![steer("go")], 1).await;
    check_events(&events);
    assert!(
        matches!(
            outcomes(&events)[..],
            [TurnOutcome::Completed {
                steps: 2,
                committed: true,
                ..
            }]
        ),
        "{events:#?}"
    );
    let items: Vec<&WithdrawnAttachment> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AttachmentsWithdrawn { items, .. } => Some(items),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(items.len(), 1, "{items:#?}");
    assert_eq!(items[0].call_id, None);
    assert_eq!(items[0].label, "#1");
    let requests = h.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(image_count(&requests[1]), 1);
    assert_eq!(image_count(&requests[2]), 0);
    let resent = texts(requests[2].last().unwrap());
    assert_eq!(
        &resent[resent.len() - 3..],
        [items[0].note.as_str(), "late", STEER_NOTE]
    );
    assert_eq!(image_count(&h.agent.history()), 0);
    h.assert_rebuilds();
    let records = records(&h.store);
    let rebuilt = rebuild_checked(&h.store, &records, None).unwrap().unwrap();
    assert_eq!(rebuilt.messages, &*h.agent.history());
    assert_eq!(rebuilt.provisional, None);
}

fn painted(name: &str, data: &str) -> ClientCommand {
    ClientCommand::Send {
        id: id(name),
        text: name.into(),
        images: vec![ImageData { data: data.into() }],
        mode: DeliveryMode::Steer,
        turn: None,
        replaces: None,
    }
}

#[tokio::test]
async fn two_uncommittable_steps_then_a_refused_steer_rebuilds() {
    let mut h = harness(vec![
        blank().then(painted("a", BLUE)),
        blank().then(painted("b", GREEN)),
        attachment_refused(),
        done("ok"),
    ]);
    let events = h.run(vec![red("go", DeliveryMode::Steer, None)], 1).await;
    check_events(&events);
    assert!(
        matches!(
            outcomes(&events)[..],
            [TurnOutcome::Completed {
                committed: true,
                ..
            }]
        ),
        "{events:#?}"
    );
    let items: Vec<&WithdrawnAttachment> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AttachmentsWithdrawn { items, .. } => Some(items),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(items.len(), 1, "{items:#?}");
    assert_eq!(items[0].ordinal, 2);
    let requests = h.requests();
    let counts: Vec<usize> = requests.iter().map(|r| image_count(r)).collect();
    assert_eq!(counts, [1, 2, 3, 2]);
    let user = &h.agent.history()[0].content;
    let b = user.iter().position(|b| *b == text("b")).unwrap();
    assert_eq!(user[b - 1], text(&items[0].note));
    assert_eq!(user[b - 2], text(STEER_NOTE));
    assert_eq!(user[b - 3], text("a"));
    assert!(matches!(user[b - 4], ContentBlock::Image { .. }));
    assert_eq!(user[b + 1], text(STEER_NOTE));
    h.assert_rebuilds();
    let records = records(&h.store);
    let rebuilt = rebuild_checked(&h.store, &records, None).unwrap().unwrap();
    assert_eq!(rebuilt.messages, &*h.agent.history());
}

#[tokio::test]
async fn a_second_withdrawal_in_the_same_turn_counts_the_image_already_withdrawn() {
    let mut h = harness(vec![
        attachment_refused(),
        blank().then(painted("late", GREEN)),
        attachment_refused(),
        done("ok"),
    ]);
    let events = h.run(vec![painted("go", BLUE)], 1).await;
    check_events(&events);
    assert!(
        matches!(
            outcomes(&events)[..],
            [TurnOutcome::Completed {
                committed: true,
                ..
            }]
        ),
        "{events:#?}"
    );
    let ordinals: Vec<Vec<usize>> = pick!(
        &events,
        AgentEvent::AttachmentsWithdrawn { items, .. } => items.iter().map(|i| i.ordinal).collect()
    );
    assert_eq!(ordinals, [vec![0], vec![1]]);
    let requests = h.requests();
    let counts: Vec<usize> = requests.iter().map(|r| image_count(r)).collect();
    assert_eq!(counts, [1, 0, 1, 0]);
    assert_eq!(image_count(&h.agent.history()), 0);
    h.assert_rebuilds();
    let records = records(&h.store);
    let rebuilt = rebuild_checked(&h.store, &records, None).unwrap().unwrap();
    assert_eq!(rebuilt.messages, &*h.agent.history());
}

#[test]
fn images_that_dont_match_the_logged_ones_are_never_withdrawn() {
    let mut h = harness(Vec::new());
    let user = vec![ContentBlock::image(ImageType::Png, RED), text("look")];
    let mut pending = Pending {
        user: Some(user.clone()),
        ..Pending::default()
    };
    assert!(!h.agent.withdraw(&mut pending, "Could not process image", 1));
    let logged = ImageRef {
        sha256: crate::session::base64_sha256(RED),
        media_type: ImageType::Png,
        bytes: 69,
        width: 1,
        height: 1,
    };
    h.agent.track().pending_images = vec![logged.clone(), logged];
    assert!(!h.agent.withdraw(&mut pending, "Could not process image", 1));
    assert_eq!(pending.user, Some(user));
    assert!(h.agent.withdrawal.is_none());
}

#[tokio::test]
async fn a_refused_steer_after_the_effort_marker_withdraws_only_its_own_image() {
    let mut h = harness(vec![
        done("one"),
        blank().then(painted("late", BLUE)),
        attachment_refused(),
        done("two"),
    ]);
    h.run(vec![steer("go")], 1).await;
    h.idle(vec![ClientCommand::shortcut(
        RequestId(2),
        None,
        None,
        Some(Effort::Low),
    )])
    .await;
    let events = h.run(vec![red("next", DeliveryMode::Steer, None)], 1).await;
    check_events(&events);
    assert!(
        matches!(
            outcomes(&events)[..],
            [TurnOutcome::Completed {
                committed: true,
                ..
            }]
        ),
        "{events:#?}"
    );
    let items: Vec<&WithdrawnAttachment> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AttachmentsWithdrawn { items, .. } => Some(items),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(items.len(), 1, "{items:#?}");
    assert_eq!(items[0].ordinal, 1);
    assert_eq!(items[0].label, "#1");
    let requests = h.requests();
    let counts: Vec<usize> = requests.iter().map(|r| image_count(r)).collect();
    assert_eq!(counts, [0, 1, 2, 1]);
    assert!(requests[1][3].effort_marker().is_some());
    let history = h.agent.history();
    assert!(history[3].effort_marker().is_some());
    assert_eq!(
        history[4].content,
        [text(&items[0].note), text("late"), text(STEER_NOTE)],
        "the steer lands after the marker with its image withdrawn"
    );
    assert_eq!(image_count(&history[2..3]), 1);
    h.assert_rebuilds();
    let records = records(&h.store);
    let rebuilt = rebuild_checked(&h.store, &records, None).unwrap().unwrap();
    assert_eq!(rebuilt.messages, *history);
}

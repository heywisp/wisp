use wisp_protocol::log::{LEAD_ENVIRONMENT, LEAD_PROCESSES, LogRecord};
use wisp_protocol::{
    DeliveryMode, ImageData, InputId, RejectKind, RequestId, RetryFocus, RetryOffer, WithdrawnBy,
};

use super::*;
use crate::agent::testing::{failure, without_stamps};
use crate::provider::anthropic::capabilities;
use crate::session::rebuild;
use crate::test_guard::RED_PIXEL as RED;
use crate::tool::Toolbox;

mod hooks;
mod queue;
mod resume;
mod rewind;
mod switch;

const A: &str = "first";
const B: &str = "second";

fn failed(kind: ErrorKind) -> Step {
    let error = failure(kind, RetryClass::Never, false);
    Step::from(vec![Ok(ProviderEvent::Sending), Err(error)])
}

fn planned(plan: Vec<Step>) -> Arc<Fake> {
    Fake::with(
        Knobs {
            caps: Some(Caps::Fn(capabilities::row)),
            media: true,
            ..Knobs::default()
        },
        plan,
    )
}

fn context(name: &str) -> LogContext {
    LogContext {
        endpoint: format!("https://{name}.example.com"),
        endpoint_name: name.into(),
        ..log_context()
    }
}

struct Rig {
    agent: Agent,
    a: Arc<Fake>,
    b: Arc<Fake>,
    tx: mpsc::Sender<ClientCommand>,
    rx: mpsc::Receiver<ClientCommand>,
    store: MemoryStore,
    _env: Env,
}

struct Setup {
    a: Vec<Step>,
    b: Option<Vec<Step>>,
    clock: Arc<dyn Clock>,
    policy: retry::RetryPolicy,
    session: SessionConfig,
    host: Option<Arc<dyn crate::settings::host::SettingsHost>>,
    toolbox: Toolbox,
    from: Option<Vec<LogRecord>>,
    blobs: Vec<(String, wisp_protocol::Mime, Vec<u8>)>,
    hooks: Vec<(String, crate::plugin::Make)>,
}

impl Setup {
    fn new(a: Vec<Step>) -> Self {
        Self {
            a,
            b: None,
            clock: Arc::new(still_clock()),
            policy: retry::quick(),
            session: default_session(),
            host: None,
            toolbox: test_toolbox(),
            from: None,
            blobs: Vec::new(),
            hooks: Vec::new(),
        }
    }

    fn with_toolbox(mut self, toolbox: Toolbox) -> Self {
        self.toolbox = toolbox;
        self
    }

    fn with_b(mut self, b: Vec<Step>) -> Self {
        self.b = Some(b);
        self
    }

    fn ticking(mut self) -> Self {
        self.clock = TickingClock::new();
        self
    }

    fn rig(self) -> Rig {
        let env = Env::new("retry-choice");
        let (tx, rx) = mpsc::channel(128);
        let a = planned(self.a);
        let two = self.b.is_some();
        let b = planned(self.b.unwrap_or_default());
        for fake in [&a, &b] {
            fake.send_to(tx.clone());
        }
        let store = MemoryStore::default();
        let mut agent = Agent::new(a.clone(), self.session)
            .with_clock(self.clock)
            .with_retry_policy(self.policy)
            .with_tools(self.toolbox, env.workspace())
            .with_platform("Linux")
            .with_resources(None)
            .with_cancel_grace(Duration::from_millis(300));
        if two {
            let ready = [(A, &a), (B, &b)]
                .into_iter()
                .map(|(name, fake)| Connection {
                    name: name.into(),
                    provider: fake.clone(),
                    context: context(name),
                    priced: true,
                })
                .collect();
            agent = agent.with_connections(ready, Vec::new(), A);
        }
        if let Some(host) = self.host {
            agent = agent.with_settings_host(host);
        }
        if !self.hooks.is_empty() {
            let mut sources = crate::plugin::testing::turn_line_sources();
            sources.extend(self.hooks);
            let (registry, problems) = crate::plugin::Registry::from_sources(
                sources,
                &crate::plugin::PluginConfig::default(),
            );
            assert!(problems.is_empty(), "{problems:?}");
            let started = futures::FutureExt::now_or_never(registry.start(
                &std::collections::BTreeSet::new(),
                Arc::new(crate::plugin::KernelHost::new()),
            ))
            .unwrap();
            agent = agent.with_hooks(started.hooks, started.host);
        }
        let agent = match self.from {
            None => agent.with_log(SessionLog::new(Box::new(store.clone()), "s1"), context(A)),
            Some(records) => {
                let mut writer = store.clone();
                for (sha, mime, bytes) in &self.blobs {
                    writer.put_blob(sha, mime, bytes).unwrap();
                }
                for record in &records {
                    writer.append("s1", record).unwrap();
                }
                let seq = u64::try_from(records.len()).unwrap();
                agent
                    .with_log(
                        SessionLog::continuing(Box::new(store.clone()), "s1", seq),
                        context(A),
                    )
                    .resume(&records, 0)
                    .unwrap()
            }
        };
        Rig {
            agent,
            a,
            b,
            tx,
            rx,
            store,
            _env: env,
        }
    }
}

impl Rig {
    async fn drive(
        &mut self,
        sends: Vec<ClientCommand>,
        until: impl Fn(&[AgentEvent]) -> bool,
        quiet: Duration,
    ) -> Vec<AgentEvent> {
        for command in sends {
            self.tx.try_send(command).unwrap();
        }
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
        let events = self
            .drive(sends, move |seen| terminal(seen) >= n, PATIENT)
            .await;
        assert_eq!(terminal(&events), n, "{events:#?}");
        events
    }

    async fn idle(&mut self, sends: Vec<ClientCommand>) -> Vec<AgentEvent> {
        self.drive(sends, |_| false, QUIET).await
    }

    fn assert_rebuilds(&self) {
        let events = log_events(&self.store);
        let rebuilt = rebuild(&self.store, events.iter()).unwrap().messages;
        assert_eq!(rebuilt, &*self.agent.history());
    }
}

fn outcomes(events: &[AgentEvent]) -> Vec<TurnOutcome> {
    pick!(events, AgentEvent::TurnEnded { outcome, .. } => outcome.clone())
}

fn offer_of(events: &[AgentEvent]) -> Option<RetryOffer> {
    match outcomes(events).last() {
        Some(TurnOutcome::Error { offer, .. }) => offer.clone(),
        other => panic!("{other:?}"),
    }
}

fn started(events: &[AgentEvent]) -> Vec<(u64, Option<u64>, Option<RequestId>)> {
    pick!(
        events,
        AgentEvent::TurnStarted {
            turn,
            retry_of,
            answers,
            ..
        } => (*turn, *retry_of, *answers)
    )
}

fn refusals(events: &[AgentEvent]) -> Vec<(RequestId, String)> {
    pick!(
        events,
        AgentEvent::Rejected {
            kind: Some(RejectKind::NotRetryable),
            request: Some(request),
            reason,
            id: None,
        } => (*request, reason.clone())
    )
}

fn accepted(events: &[AgentEvent]) -> Vec<(String, DeliveryMode, String)> {
    pick!(
        events,
        AgentEvent::InputAccepted { id, mode, text, .. } =>
            (id.as_str().to_owned(), *mode, text.clone())
    )
}

fn ids(list: &[&str]) -> Vec<InputId> {
    list.iter().map(|s| InputId((*s).into())).collect()
}

fn position(events: &[AgentEvent], f: impl Fn(&AgentEvent) -> bool) -> usize {
    events.iter().position(f).unwrap_or(usize::MAX)
}

fn turn_started_records(records: &[LogRecord]) -> Vec<&LogEvent> {
    records
        .iter()
        .map(|r| &r.event)
        .filter(|e| matches!(e, LogEvent::TurnStarted { .. }))
        .collect()
}

fn tool_call() -> B {
    let sample = quick_samples().remove(0);
    call("c1", &sample.name, sample.input)
}

fn retry_offer(retry: bool, switch: bool, focus: RetryFocus) -> RetryOffer {
    RetryOffer {
        retry,
        switch,
        focus,
        suggest: None,
        retries_at: None,
    }
}

#[tokio::test]
async fn retry_after_a_failed_tool_turn_resends_the_failed_request_byte_for_byte() {
    let mut rig = Setup::new(vec![
        uses(&[tool_call()]),
        failed(ErrorKind::Server),
        uses(&[tool_call()]).then(ClientCommand::send("tui-2", "while retrying")),
        done("finished"),
    ])
    .rig();
    let first = rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    assert_eq!(
        offer_of(&first),
        Some(retry_offer(true, false, RetryFocus::Retry))
    );
    let kept = rig.agent.history().to_vec();
    assert_eq!(kept.len(), 3);

    let second = rig.run(vec![retry_turn(1, None, 7)], 1).await;
    assert_eq!(started(&second), [(2, Some(1), Some(RequestId(7)))]);
    let bodies = rig.a.texts();
    assert_eq!(bodies.len(), 4);
    assert_eq!(bodies[2], bodies[1], "Retry sends the failed request again");
    assert!(!bodies[2].contains("while retrying"));
    assert!(bodies[3].contains("while retrying"));
    assert_eq!(
        accepted(&second),
        [(
            "tui-2".to_owned(),
            DeliveryMode::Steer,
            "while retrying".to_owned()
        )]
    );
    assert_eq!(delivered(&second), [(ids(&["tui-2"]), 2)]);
    assert!(matches!(
        outcomes(&second).as_slice(),
        [TurnOutcome::Completed { .. }]
    ));
    let history = rig.agent.history();
    assert_eq!(&history[..3], kept.as_slice());
    assert_eq!(history[3].role, Role::Assistant);
    rig.assert_rebuilds();

    let records = records(&rig.store);
    let started = turn_started_records(&records);
    let LogEvent::TurnStarted {
        retry_of,
        text,
        inputs,
        lead,
        effort_marker,
        ..
    } = started[1]
    else {
        panic!("{started:?}");
    };
    assert_eq!(*retry_of, Some(1));
    assert!(text.is_empty() && inputs.is_empty());
    assert!(lead.is_empty());
    assert_eq!(*effort_marker, None);
    let offered = records.iter().find_map(|r| match &r.event {
        LogEvent::TurnEnded {
            outcome: TurnOutcome::Error { offer, .. },
            ..
        } => Some(offer.clone()),
        _ => None,
    });
    assert_eq!(
        offered,
        Some(Some(retry_offer(true, false, RetryFocus::Retry)))
    );
}

#[tokio::test]
async fn retry_resends_a_tool_s_file_byte_for_byte_and_the_log_rebuilds_it() {
    let mut rig = Setup::new(vec![
        uses(&[call("c1", "snap", json!({"pdf": true}))]),
        failed(ErrorKind::Server),
        done("finished"),
    ])
    .with_toolbox(Toolbox::new([crate::tool::snap::tool()]))
    .rig();
    let first = rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    assert_eq!(
        offer_of(&first),
        Some(retry_offer(true, false, RetryFocus::Retry))
    );
    let kept = rig.agent.history().to_vec();
    rig.assert_rebuilds();

    let second = rig.run(vec![retry_turn(1, None, 7)], 1).await;
    assert!(matches!(
        outcomes(&second).as_slice(),
        [TurnOutcome::Completed { .. }]
    ));
    let bodies = rig.a.texts();
    assert_eq!(bodies.len(), 3);
    let pdf = wisp_media::encode_base64(crate::tool::snap::PDF);
    assert!(bodies[1].contains(&pdf), "{}", bodies[1]);
    assert_eq!(bodies[2], bodies[1], "Retry sends the file again unchanged");
    assert_eq!(&rig.agent.history()[..kept.len()], kept.as_slice());
    assert_eq!(rig.store.blob_names().unwrap().len(), 1);
    rig.assert_rebuilds();
}

#[tokio::test]
async fn a_steer_delivered_into_the_failed_step_is_sent_again_after_the_tool_results() {
    let mut rig = Setup::new(vec![
        uses(&[tool_call()]).then(ClientCommand::send("tui-2", "and also this")),
        failed(ErrorKind::Server),
        done("finished"),
    ])
    .rig();
    let first = rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    assert_eq!(delivered(&first)[1], (ids(&["tui-2"]), 2));
    assert!(first.iter().any(|e| matches!(
        e,
        AgentEvent::InputWithdrawn { ids: gone, by: WithdrawnBy::TurnFailed } if gone == &ids(&["tui-2"])
    )));

    let second = rig.run(vec![retry_turn(1, None, 3)], 1).await;
    assert_eq!(
        accepted(&second),
        [(
            "wisp-retry-2-1".to_owned(),
            DeliveryMode::Steer,
            "and also this".to_owned()
        )]
    );
    assert_eq!(delivered(&second), [(ids(&["wisp-retry-2-1"]), 1)]);
    let accepted_at = position(&second, |e| matches!(e, AgentEvent::InputAccepted { .. }));
    let started_at = position(&second, |e| matches!(e, AgentEvent::TurnStarted { .. }));
    assert!(accepted_at < started_at);
    let bodies = rig.a.texts();
    assert_eq!(bodies[2], bodies[1]);
    let history = rig.agent.history();
    assert_eq!(history.len(), 4);
    assert_eq!(texts(&history[2]), ["and also this", STEER_NOTE]);
    rig.assert_rebuilds();
}

#[tokio::test]
async fn a_steer_after_a_committed_final_reply_is_sent_again_as_its_own_message() {
    let mut rig = Setup::new(vec![
        done("first answer").then(ClientCommand::send("tui-2", "one more thing")),
        failed(ErrorKind::Overloaded),
        done("second answer"),
    ])
    .rig();
    let first = rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    assert!(matches!(
        outcomes(&first).as_slice(),
        [TurnOutcome::Error {
            committed_steps: 1,
            offer: Some(_),
            ..
        }]
    ));
    assert_eq!(rig.agent.history().len(), 2);
    rig.run(vec![retry_turn(1, None, 4)], 1).await;
    let bodies = rig.a.texts();
    assert_eq!(bodies[2], bodies[1]);
    let history = rig.agent.history();
    assert_eq!(history.len(), 4);
    assert_eq!(history[2].role, Role::User);
    assert_eq!(texts(&history[2]), ["one more thing", STEER_NOTE]);
    assert!(history.iter().all(|m| !m.content.is_empty()));
    rig.assert_rebuilds();
}

#[tokio::test]
async fn an_uncommittable_first_reply_then_a_failed_step_resends_the_message_and_the_steer() {
    let mut setup = Setup::new(vec![
        reply(&[B::Think], StopReason::MaxTokens)
            .then(ClientCommand::send("tui-2", "are you there")),
        Step::from(vec![
            Ok(ProviderEvent::Sending),
            Err(crate::agent::testing::mid_stream(RetryClass::Transient)),
        ]),
        done("yes"),
    ]);
    setup.policy = retry::RetryPolicy::default();
    let mut rig = setup.rig();
    rig.idle(vec![ClientCommand::shortcut(
        RequestId(1),
        None,
        None,
        Some(Effort::High),
    )])
    .await;
    let commands = rig.tx.clone();
    let first = rig
        .drive(
            vec![ClientCommand::send("tui-1", "hello")],
            |seen| {
                if matches!(seen.last(), Some(AgentEvent::Retrying { .. })) {
                    commands.try_send(ClientCommand::Cancel).unwrap();
                }
                terminal(seen) == 1
            },
            PATIENT,
        )
        .await;
    assert_eq!(terminal(&first), 1);
    assert_eq!(rig.a.sent().len(), 2, "Esc sends nothing more");
    assert!(rig.agent.retry_offer().is_some());
    assert!(
        rig.idle(Vec::new())
            .await
            .iter()
            .all(|event| !matches!(event, AgentEvent::TurnStarted { .. }))
    );
    assert_eq!(rig.a.sent().len(), 2, "nothing resumes before Retry");
    assert!(matches!(
        outcomes(&first).as_slice(),
        [TurnOutcome::Error {
            committed_steps: 0,
            gave_up: None,
            offer: Some(_),
            ..
        }]
    ));
    let second = rig.run(vec![retry_turn(1, None, 5)], 1).await;
    assert_eq!(
        accepted(&second),
        [
            (
                "wisp-retry-2-1".to_owned(),
                DeliveryMode::Steer,
                "hello".to_owned()
            ),
            (
                "wisp-retry-2-2".to_owned(),
                DeliveryMode::Steer,
                "are you there".to_owned()
            ),
        ]
    );
    assert_eq!(
        delivered(&second),
        [(ids(&["wisp-retry-2-1", "wisp-retry-2-2"]), 1)]
    );
    let bodies = rig.a.texts();
    assert_eq!(bodies.len(), 3);
    assert_eq!(
        bodies[2], bodies[1],
        "the opening, the marker and the steer"
    );
    assert!(bodies[1].contains("are you there"));
    let history = rig.agent.history();
    let marker = history
        .iter()
        .position(|m| m.effort_marker().is_some())
        .unwrap();
    assert!(texts(&history[marker - 1]).contains(&"hello"));
    assert_eq!(texts(&history[marker + 1]), ["are you there", STEER_NOTE]);
    rig.assert_rebuilds();
}

#[tokio::test]
async fn a_failed_first_step_resends_the_texts_and_images_with_the_same_context_and_marker() {
    let image = ClientCommand::Send {
        id: InputId("tui-1".into()),
        text: "what is this".into(),
        images: vec![ImageData { data: RED.into() }],
        mode: DeliveryMode::Steer,
        turn: None,
        replaces: None,
    };
    let mut rig = Setup::new(vec![failed(ErrorKind::Server), done("a pixel")]).rig();
    rig.idle(vec![ClientCommand::shortcut(
        RequestId(1),
        None,
        None,
        Some(Effort::High),
    )])
    .await;
    rig.run(vec![image], 1).await;
    let records = records(&rig.store);
    let LogEvent::TurnStarted { images: refs, .. } = turn_started_records(&records)[0].clone()
    else {
        unreachable!();
    };
    assert_eq!(refs.len(), 1);
    let second = rig.run(vec![retry_turn(1, None, 2)], 1).await;
    let again = second
        .iter()
        .find_map(|e| match e {
            AgentEvent::InputAccepted { id, images, .. } if id.as_str() == "wisp-retry-2-1" => {
                Some(images.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(again, refs);
    let bodies = rig.a.texts();
    assert_eq!(bodies[1], bodies[0]);
    assert!(bodies[1].contains("<environment>"));
    assert!(bodies[1].contains(r#""type":"image""#));
    assert_eq!(rig.agent.history().len(), 3);
    rig.assert_rebuilds();
    let records = test_guard::records(&rig.store);
    let started = turn_started_records(&records);
    let LogEvent::TurnStarted {
        retry_of: Some(1),
        text,
        images,
        effort_marker: Some(_),
        lead,
        ..
    } = started[1]
    else {
        panic!("{started:?}");
    };
    assert!(lead.has(LEAD_ENVIRONMENT));
    assert_eq!(text, "what is this");
    assert_eq!(images, &refs);
}

#[tokio::test]
async fn an_image_only_message_is_rebuilt_with_its_images_after_a_retry() {
    let image = ClientCommand::Send {
        id: InputId("tui-1".into()),
        text: String::new(),
        images: vec![ImageData { data: RED.into() }],
        mode: DeliveryMode::Steer,
        turn: None,
        replaces: None,
    };
    let mut rig = Setup::new(vec![failed(ErrorKind::Server), done("a pixel")]).rig();
    rig.run(vec![image], 1).await;
    rig.run(vec![retry_turn(1, None, 2)], 1).await;
    let bodies = rig.a.texts();
    assert_eq!(bodies[1], bodies[0]);
    assert_eq!(rig.agent.history().len(), 2);
    rig.assert_rebuilds();
}

#[tokio::test]
async fn when_the_clock_moved_only_the_stamp_and_the_line_counting_it_differ() {
    let mut rig = Setup::new(vec![failed(ErrorKind::Server), done("ok")])
        .ticking()
        .rig();
    rig.run(vec![ClientCommand::send("tui-1", "hello")], 1)
        .await;
    rig.run(vec![retry_turn(1, None, 2)], 1).await;
    let bodies = rig.a.texts();
    assert_ne!(bodies[1], bodies[0]);
    assert_eq!(without_stamps(&bodies[1]), without_stamps(&bodies[0]));
    rig.assert_rebuilds();
}

#[tokio::test]
async fn retry_is_refused_after_a_completed_or_cancelled_turn_and_for_an_earlier_turn() {
    let mut rig = Setup::new(vec![
        done("fine"),
        hanging(&[B::Text("never")]),
        failed(ErrorKind::Server),
        done("moved on"),
    ])
    .rig();
    rig.run(vec![ClientCommand::send("tui-1", "one")], 1).await;
    let refused = rig.idle(vec![retry_turn(1, None, 1)]).await;
    assert_eq!(
        refusals(&refused),
        [(RequestId(1), "nothing to retry".to_owned())]
    );
    assert!(started(&refused).is_empty());

    let tx = rig.tx.clone();
    let cancelled = rig
        .drive(
            vec![ClientCommand::send("tui-2", "two")],
            move |seen| {
                if seen
                    .iter()
                    .any(|e| matches!(e, AgentEvent::StepStarted { .. }))
                    && terminal(seen) == 0
                {
                    drop(tx.try_send(ClientCommand::Cancel));
                }
                terminal(seen) >= 1
            },
            PATIENT,
        )
        .await;
    assert!(matches!(
        outcomes(&cancelled).as_slice(),
        [TurnOutcome::Cancelled { .. }]
    ));
    let refused = rig.idle(vec![retry_turn(2, None, 2)]).await;
    assert_eq!(
        refusals(&refused),
        [(RequestId(2), "nothing to retry".to_owned())]
    );

    rig.run(vec![ClientCommand::send("tui-3", "three")], 1)
        .await;
    rig.run(vec![ClientCommand::send("tui-4", "four")], 1).await;
    let sent = rig.a.texts().len();
    let refused = rig.idle(vec![retry_turn(3, None, 3)]).await;
    assert_eq!(
        refusals(&refused),
        [(RequestId(3), "turn 3 is no longer the last turn".to_owned())]
    );
    assert_eq!(rig.a.texts().len(), sent, "a refused Retry sends nothing");
}

#[tokio::test]
async fn retry_is_refused_while_a_turn_runs_and_after_a_message_starts_one() {
    let mut rig = Setup::new(vec![
        failed(ErrorKind::Server),
        uses(&[tool_call()]).then(retry_turn(1, None, 8)),
        done("done"),
    ])
    .rig();
    rig.run(vec![ClientCommand::send("tui-1", "one")], 1).await;
    let second = rig.run(vec![ClientCommand::send("tui-2", "two")], 1).await;
    assert_eq!(
        refusals(&second),
        [(RequestId(8), "turn 1 is no longer the last turn".to_owned())]
    );
    assert_eq!(started(&second), [(2, None, None)]);
}

#[tokio::test]
async fn nothing_is_offered_after_overflow_refusal_or_a_malformed_history() {
    for kind in [ErrorKind::ContextOverflow, ErrorKind::Refusal] {
        let mut rig = Setup::new(vec![failed(kind)]).with_b(Vec::new()).rig();
        let first = rig.run(vec![ClientCommand::send("tui-1", "one")], 1).await;
        assert_eq!(offer_of(&first), None, "{kind:?}");
        let refused = rig.idle(vec![retry_turn(1, Some(B), 1)]).await;
        assert_eq!(
            refusals(&refused),
            [(RequestId(1), "nothing to retry".to_owned())]
        );
        assert!(rig.b.texts().is_empty());
    }
}

#[tokio::test]
async fn one_endpoint_offers_only_retry_and_an_account_added_later_takes_the_retry() {
    let mut rig = Setup::new(vec![failed(ErrorKind::Auth)]).rig();
    let first = rig.run(vec![ClientCommand::send("tui-1", "one")], 1).await;
    assert_eq!(
        offer_of(&first),
        Some(retry_offer(true, false, RetryFocus::Retry))
    );
    let mut rig = Setup::new(vec![failed(ErrorKind::Billing)])
        .with_b(vec![done("on the new account")])
        .rig();
    rig.agent
        .gone
        .push((B.to_owned(), "not signed in yet".to_owned()));
    let first = rig.run(vec![ClientCommand::send("tui-1", "one")], 1).await;
    assert_eq!(
        offer_of(&first),
        Some(retry_offer(true, false, RetryFocus::Retry))
    );
    rig.agent.gone.clear();
    let events = rig.run(vec![retry_turn(1, Some(B), 2)], 1).await;
    assert_eq!(started(&events), [(2, Some(1), Some(RequestId(2)))]);
    assert_eq!(rig.b.texts().len(), 1);
}

#[tokio::test]
async fn a_queued_message_held_by_the_failure_goes_after_the_retry_completes() {
    let mut rig = Setup::new(vec![
        uses(&[tool_call()]).then(ClientCommand::queue("tui-2", "afterwards")),
        failed(ErrorKind::Server),
        done("retried"),
        done("the follow-up"),
    ])
    .rig();
    let first = rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    assert!(first.iter().any(|e| matches!(
        e,
        AgentEvent::InputHeld { ids: held, by: WithdrawnBy::TurnFailed } if held == &ids(&["tui-2"])
    )));
    let events = rig.run(vec![retry_turn(1, None, 6)], 2).await;
    let unheld = position(
        &events,
        |e| matches!(e, AgentEvent::InputUnheld { ids: u, front: false } if u == &ids(&["tui-2"])),
    );
    let retried = position(&events, |e| {
        matches!(
            e,
            AgentEvent::TurnStarted {
                retry_of: Some(1),
                ..
            }
        )
    });
    assert!(unheld < retried, "{events:#?}");
    assert_eq!(
        started(&events),
        [(2, Some(1), Some(RequestId(6))), (3, None, None)]
    );
    assert_eq!(delivered(&events), [(ids(&["tui-2"]), 1)]);
    let bodies = rig.a.texts();
    assert_eq!(bodies[2], bodies[1]);
    assert!(!bodies[2].contains("afterwards"));
    assert!(bodies[3].contains("afterwards"));
}

#[tokio::test]
async fn a_held_message_is_held_again_when_the_retry_fails_too() {
    let mut rig = Setup::new(vec![
        uses(&[tool_call()]).then(ClientCommand::queue("tui-2", "afterwards")),
        failed(ErrorKind::Server),
        failed(ErrorKind::Server),
    ])
    .rig();
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    let events = rig.run(vec![retry_turn(1, None, 6)], 1).await;
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::InputHeld { ids: held, by: WithdrawnBy::TurnFailed } if held == &ids(&["tui-2"])
    )));
    assert_eq!(
        offer_of(&events),
        Some(retry_offer(true, false, RetryFocus::Retry))
    );
    let bodies = rig.a.texts();
    assert_eq!(bodies[2], bodies[1]);
}

#[tokio::test]
async fn each_retry_gets_fresh_step_and_turn_limits() {
    let mut rig = Setup::new(vec![
        turned_away(),
        turned_away(),
        turned_away(),
        done("third time"),
    ]);
    rig.policy.step_retries = Some(1);
    let mut rig = rig.rig();
    let first = rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    assert!(matches!(
        outcomes(&first).as_slice(),
        [TurnOutcome::Error {
            attempts: 2,
            gave_up: Some(wisp_protocol::GaveUp::StepLimit),
            offer: Some(_),
            ..
        }]
    ));
    let second = rig.run(vec![retry_turn(1, None, 2)], 1).await;
    assert!(matches!(
        outcomes(&second).as_slice(),
        [TurnOutcome::Completed { .. }]
    ));
    assert_eq!(rig.agent.last_turn_stats().unwrap().retries, 1);
}

#[tokio::test]
async fn a_pending_effort_change_survives_a_retry_and_applies_on_the_next_message() {
    let mut rig = Setup::new(vec![
        uses(&[tool_call()]),
        failed(ErrorKind::Server),
        done("retried"),
        done("next"),
    ])
    .rig();
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    let changed = rig
        .idle(vec![ClientCommand::shortcut(
            RequestId(1),
            None,
            None,
            Some(Effort::High),
        )])
        .await;
    assert!(changed.iter().any(|e| matches!(
        e,
        AgentEvent::SettingsChanged {
            effort: Effort::High,
            ..
        }
    )));
    let retried = rig.run(vec![retry_turn(1, None, 2)], 1).await;
    assert!(
        !retried
            .iter()
            .any(|e| matches!(e, AgentEvent::SettingsChanged { .. }))
    );
    let bodies = rig.a.texts();
    assert_eq!(bodies[2], bodies[1]);
    assert_eq!(
        rig.agent.pending_settings().map(|p| p.2),
        Some(Effort::High)
    );
    rig.run(vec![ClientCommand::send("tui-3", "continue")], 1)
        .await;
    assert_eq!(rig.agent.session().effort, Effort::High);
    let records = records(&rig.store);
    let efforts: Vec<(Option<u64>, Option<Effort>, bool)> = turn_started_records(&records)
        .into_iter()
        .map(|e| match e {
            LogEvent::TurnStarted {
                retry_of,
                effort,
                effort_marker,
                ..
            } => (*retry_of, *effort, effort_marker.is_some()),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        efforts,
        [
            (None, Some(Effort::Medium), false),
            (Some(1), Some(Effort::Medium), false),
            (None, Some(Effort::High), true),
        ]
    );
    let bodies = rig.a.texts();
    let (head, msgs) = head_and_messages(&bodies[3]);
    let (_, retried) = head_and_messages(&bodies[2]);
    assert!(!head.is_empty());
    assert!(
        msgs.starts_with(retried.strip_suffix("]}]").unwrap())
            || msgs.starts_with(retried.strip_suffix(']').unwrap())
    );
}

#[tokio::test]
async fn a_cancel_while_idle_keeps_the_offer() {
    let mut rig = Setup::new(vec![failed(ErrorKind::Server), done("ok")]).rig();
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    rig.idle(vec![ClientCommand::Cancel]).await;
    let events = rig.run(vec![retry_turn(1, None, 2)], 1).await;
    assert_eq!(started(&events), [(2, Some(1), Some(RequestId(2)))]);
}

#[tokio::test]
async fn a_second_retry_of_the_same_failure_is_refused_once_the_first_started() {
    let mut rig = Setup::new(vec![failed(ErrorKind::Server), done("ok")]).rig();
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    let events = rig
        .run(vec![retry_turn(1, None, 2), retry_turn(1, None, 3)], 1)
        .await;
    let events = [events, rig.idle(Vec::new()).await].concat();
    assert_eq!(started(&events), [(2, Some(1), Some(RequestId(2)))]);
    assert_eq!(
        refusals(&events),
        [(RequestId(3), "turn 1 is no longer the last turn".to_owned())]
    );
}

#[tokio::test]
async fn a_retry_sends_the_failed_turns_process_notice_again_and_hears_it_once() {
    let a = ExitGate::new("a.go", 2);
    if crate::jobs::exec::find_program("bash").is_none() {
        return;
    }
    let toolbox = crate::tool::testkit::shell(crate::tool::testkit::timing(300, 3_000));
    let start = call(
        "a",
        "bash",
        json!({"command": &a.command, "run_in_background": true}),
    );
    let mut rig = Setup::new(vec![
        uses(&[start]),
        done("started"),
        failed(ErrorKind::Server),
        done("a pixel"),
    ])
    .with_toolbox(toolbox)
    .rig();
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    a.open(&rig._env.work);
    let ended = rig
        .drive(
            Vec::new(),
            |seen| {
                seen.iter()
                    .any(|e| matches!(e, AgentEvent::ProcessEnded { id: 1, .. }))
            },
            PATIENT,
        )
        .await;
    assert!(
        ended
            .iter()
            .any(|e| matches!(e, AgentEvent::ProcessEnded { id: 1, .. }))
    );
    let image = ClientCommand::Send {
        id: InputId("tui-2".into()),
        text: String::new(),
        images: vec![ImageData { data: RED.into() }],
        mode: DeliveryMode::Steer,
        turn: None,
        replaces: None,
    };
    rig.run(vec![image], 1).await;
    rig.run(vec![retry_turn(2, None, 2)], 1).await;
    let bodies = rig.a.texts();
    assert_eq!(bodies.len(), 4);
    assert_eq!(bodies[3], bodies[2]);
    assert!(bodies[3].contains(&a.ended(1, 2)));
    let block = format!("<processes>\n{}\n</processes>", a.ended(1, 2));
    let events = log_events(&rig.store);
    let notices: Vec<(u64, Option<u64>, Option<String>)> = pick!(
        &events,
        LogEvent::TurnStarted {
            turn,
            retry_of,
            lead,
            ..
        } => (
            *turn,
            *retry_of,
            lead.text(LEAD_PROCESSES).map(str::to_owned),
        )
    );
    assert_eq!(
        notices,
        [
            (1, None, None),
            (2, None, Some(block.to_owned())),
            (3, Some(2), Some(block.to_owned())),
        ]
    );
    let heard: Vec<(u64, Vec<u32>)> = pick!(
        &events,
        LogEvent::ProcessHeard { turn, ids, .. } => (*turn, ids.clone())
    );
    assert_eq!(heard, [(3, vec![1])]);
    rig.assert_rebuilds();
}

#[tokio::test(start_paused = true)]
async fn the_main_session_retries_its_used_up_turn_at_the_reset_with_the_same_bytes() {
    let start: jiff::Timestamp = "2026-10-22T03:58:00Z".parse().unwrap();
    let reset = start.as_second() + 6 * 3600;
    let past_reset = Duration::from_secs(7 * 3600);
    let used_up = || {
        let mut error = failure(ErrorKind::RateLimit, RetryClass::Never, false);
        crate::provider::limit::reached(
            &mut error.detail,
            start.as_second(),
            Some(reset),
            wisp_protocol::plan_usage::UsageSource::RetryAfter,
            None,
        );
        Step::from(vec![Ok(ProviderEvent::Sending), Err(error)])
    };
    let rig_at = |a: Vec<Step>, from: Option<Vec<LogRecord>>, at: jiff::Timestamp| {
        let clock = Arc::new(ShiftedClock::paused(at, Some(TimeZone::UTC)));
        let mut setup = Setup::new(a).with_b(vec![done("on b")]);
        setup.clock = clock.clone();
        setup.from = from;
        (setup.rig(), clock)
    };
    let switch = |request, account: &str| {
        ClientCommand::shortcut(RequestId(request), Some(account.into()), None, None)
    };
    let (mut rig, clock) = rig_at(
        vec![uses(&[tool_call()]), used_up(), done("back")],
        None,
        start,
    );
    let first = rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    assert_eq!(offer_of(&first).unwrap().retries_at, Some(reset + 60));
    let failed = records(&rig.store);
    rig.idle(vec![switch(1, B), switch(2, A)]).await;
    let retried = rig
        .drive(Vec::new(), |seen| terminal(seen) >= 1, past_reset)
        .await;
    assert_eq!(started(&retried), [(2, Some(1), None)]);
    assert!((reset + 60..reset + 120).contains(&clock.now().as_second()));
    let bodies = rig.a.texts();
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[2], bodies[1], "the retry sends the failed request");
    assert!(rig.b.sent().is_empty());
    assert!(refusals(&retried).is_empty());
    rig.assert_rebuilds();

    let (mut held, _) = rig_at(vec![used_up()], None, start);
    held.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    held.idle(vec![switch(1, B)]).await;
    let waited = held.drive(Vec::new(), |_| false, past_reset).await;
    assert!(started(&waited).is_empty(), "{waited:#?}");
    held.run(vec![ClientCommand::send("tui-2", "on b then")], 1)
        .await;
    assert_eq!(held.b.sent().len(), 1);
    let (mut rig, _) = rig_at(vec![used_up(), done("answer")], None, start);
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    rig.run(vec![ClientCommand::send("tui-2", "never mind")], 1)
        .await;
    let (mut moved, _) = rig_at(vec![used_up()], None, start);
    moved.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    let by_hand = moved.run(vec![retry_turn(1, Some(B), 2)], 1).await;
    assert_eq!(started(&by_hand), [(2, Some(1), Some(RequestId(2)))]);
    for rig in [&mut held, &mut rig, &mut moved] {
        let later = rig.drive(Vec::new(), |_| false, past_reset).await;
        assert!(started(&later).is_empty(), "{later:#?}");
        assert_eq!(rig.a.sent().len() + rig.b.sent().len(), 2);
    }

    let (mut child, _) = rig_at(vec![used_up()], None, start);
    child.agent = child.agent.with_path(AgentPath::new("/child"));
    let ended = child.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    assert_eq!(offer_of(&ended).unwrap().retries_at, None);

    for (at, armed) in [
        (start, true),
        (jiff::Timestamp::from_second(reset + 1).unwrap(), false),
    ] {
        let (mut again, clock) = rig_at(vec![done("back")], Some(failed.clone()), at);
        let restored = again.agent.retry_offer().unwrap().1.retries_at;
        assert_eq!(restored, armed.then_some(reset + 60));
        let suspended = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            clock.advance(jiff::SignedDuration::from_hours(7));
        });
        let later = again
            .drive(
                Vec::new(),
                |seen| terminal(seen) >= 1,
                Duration::from_secs(120),
            )
            .await;
        suspended.await.unwrap();
        assert_eq!(started(&later).len(), usize::from(armed), "{later:#?}");
    }
}

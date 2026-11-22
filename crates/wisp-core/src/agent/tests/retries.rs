use super::*;
use crate::agent::retry::RetryPolicy;
use crate::session::MemoryStore;
use wisp_protocol::log::LogEvent;
use wisp_protocol::{RequestId, RetryOffer};

fn half_reply(words: &str, input: u64) -> Script {
    let mut script = vec![Ok(ProviderEvent::Sending), Ok(start(input, 0))];
    script.extend(text(0, words).into_iter().take(2));
    script
}

fn failing(mut script: Script, error: ProviderError) -> Step {
    script.push(Err(error));
    Step::from(script)
}

fn refused(error: ProviderError) -> Step {
    failing(vec![Ok(ProviderEvent::Sending)], error)
}

fn server_mid_stream() -> ProviderError {
    failure(ErrorKind::Server, RetryClass::Transient, true)
}

fn overloaded() -> ProviderError {
    failure(ErrorKind::Overloaded, RetryClass::Transient, false)
}

fn retrying(events: &[AgentEvent]) -> Vec<&AgentEvent> {
    events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Retrying { .. }))
        .collect()
}

fn count(events: &[AgentEvent], f: fn(&AgentEvent) -> bool) -> usize {
    events.iter().filter(|e| f(e)).count()
}

#[tokio::test(start_paused = true)]
async fn a_reply_that_breaks_mid_stream_is_retried_from_the_same_bytes() {
    let mut script: Vec<Step> = (0..12)
        .map(|_| failing(half_reply("half an ans", 600), server_mid_stream()))
        .collect();
    script.push(Step::from(reply(
        "the whole answer",
        600,
        0,
        StopReason::EndTurn,
    )));
    let provider = Fake::new(script);
    let mut agent = Agent::new(provider.clone(), default_session());
    let began = tokio::time::Instant::now();
    let (outcome, events) = turn(&mut agent, "hi").await;
    assert!(committed(&outcome), "{outcome:?}");
    assert!(began.elapsed() > Duration::from_secs(600));

    let sent = provider.texts();
    assert_eq!(sent.len(), 13);
    assert!(
        sent.iter().all(|body| *body == sent[0]),
        "every retry re-sends the same bytes"
    );

    assert_eq!(agent.history().len(), 2);
    let kept = serde_json::to_string(&*agent.history()).unwrap();
    assert!(!kept.contains("half an ans"), "{kept}");
    assert!(kept.contains("the whole answer"));
    assert_eq!(agent.last_turn_stats().unwrap().retries, 12);

    let retries = retrying(&events);
    assert_eq!(retries.len(), 12);
    assert!(retries.iter().all(|event| matches!(
        event,
        AgentEvent::Retrying {
            max_attempts: 0,
            ..
        }
    )));
    let AgentEvent::Retrying {
        attempt: 1,
        delay_ms,
        step: 1,
        kind: ErrorKind::Server,
        detail: Some(detail),
        max_attempts: 0,
        discarded_blocks: 1,
        discarded_chars: 11,
        usage: Some(usage),
        model: Some(model),
        cost_usd: Some(cost_usd),
        lower_bound: true,
        resend: None,
        ..
    } = retries[0]
    else {
        panic!("{events:#?}")
    };
    assert!((800..=1000).contains(delay_ms));
    assert_eq!(detail.phase, FailurePhase::Stream);
    assert_eq!(usage.input_tokens, 600);
    assert_eq!(model, DEFAULT_MODEL);
    assert!((cost_usd - priced(600)).abs() < 1e-12, "{cost_usd}");
    assert_eq!(
        count(&events, |e| matches!(e, AgentEvent::StepStarted { .. })),
        1
    );
    assert_eq!(
        count(&events, |e| matches!(e, AgentEvent::StepEnded { .. })),
        1
    );

    let TurnOutcome::Completed {
        usage,
        cost: Some(cost),
        steps: 1,
        ..
    } = outcome
    else {
        panic!("{outcome:?}")
    };
    assert_eq!(
        usage.input_tokens, 7800,
        "the failed attempt's usage is counted"
    );
    assert!(cost.turn_lower_bound, "a possibly billed attempt failed");
}

#[tokio::test(start_paused = true)]
async fn six_failures_in_a_step_give_up_at_the_step_limit() {
    let server = || failure(ErrorKind::Server, RetryClass::Transient, false);
    let provider = Fake::new((0..6).map(|_| refused(server())));
    let mut agent = Agent::new(provider.clone(), default_session()).with_retry_policy(policy());
    let began = tokio::time::Instant::now();
    let (outcome, events) = turn(&mut agent, "hi").await;
    assert_eq!(began.elapsed(), Duration::from_secs(1 + 2 + 4 + 8 + 16));
    let TurnOutcome::Error {
        kind: ErrorKind::Server,
        attempts: 6,
        gave_up: Some(GaveUp::StepLimit),
        detail: Some(_),
        ..
    } = outcome
    else {
        panic!("{outcome:?}")
    };
    assert_eq!(retrying(&events).len(), 5);
    assert_eq!(agent.last_turn_stats().unwrap().retries, 5);
    let sent = provider.texts();
    assert_eq!(sent.len(), 6);
    assert!(sent.iter().all(|b| *b == sent[0]));
    assert!(agent.history().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_failure_that_cannot_succeed_later_is_not_retried() {
    let provider = Fake::new(vec![
        refused(failure(ErrorKind::InvalidRequest, RetryClass::Never, false)),
        refused(ProviderError::new(ErrorKind::InvalidRequest, "local")),
    ]);
    let mut agent = Agent::new(provider.clone(), default_session());
    let (outcome, events) = turn(&mut agent, "hi").await;
    assert!(
        matches!(
            &outcome,
            TurnOutcome::Error {
                kind: ErrorKind::InvalidRequest,
                attempts: 1,
                gave_up: Some(GaveUp::NotRetryable),
                detail: Some(detail),
                ..
            } if detail.model.as_deref() == Some(DEFAULT_MODEL)
        ),
        "{outcome:?}"
    );
    assert!(retrying(&events).is_empty());
    assert_eq!(provider.sent().len(), 1);
    let (outcome, _) = turn(&mut agent, "again").await;
    assert!(
        matches!(&outcome, TurnOutcome::Error { detail: Some(d), .. } if d.model.is_none()),
        "{outcome:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn retry_after_sets_the_wait_even_when_it_is_long() {
    let limited = |ms| failure(ErrorKind::RateLimit, RetryClass::After { ms }, false);
    let provider = Fake::new(vec![
        refused(limited(2_000)),
        Step::from(reply("ok", 600, 0, StopReason::EndTurn)),
        refused(limited(86_400_000)),
        Step::from(reply("ok", 600, 0, StopReason::EndTurn)),
    ]);
    let mut agent = Agent::new(provider.clone(), default_session());
    let began = tokio::time::Instant::now();
    let (outcome, events) = turn(&mut agent, "a").await;
    assert!(committed(&outcome));
    assert_eq!(began.elapsed(), Duration::from_secs(2));
    assert!(matches!(
        retrying(&events).as_slice(),
        [AgentEvent::Retrying { delay_ms: 2000, .. }]
    ));

    let began = tokio::time::Instant::now();
    let (outcome, events) = turn(&mut agent, "b").await;
    assert!(committed(&outcome), "{outcome:?}");
    assert_eq!(began.elapsed(), Duration::from_secs(86_400));
    assert!(
        matches!(retrying(&events).as_slice(), [AgentEvent::Retrying {
        delay_ms: 86_400_000, max_attempts: 0, busy: Some(busy), ..
    }] if busy.window_ms == 0)
    );
}

fn priced(input: u64) -> f64 {
    let info = pricing::model_info(DEFAULT_MODEL).unwrap();
    let usage = Usage {
        input_tokens: input,
        ..Usage::default()
    };
    pricing::estimate_usd(info, &usage, default_session().cache_ttl)
}

#[tokio::test(start_paused = true)]
async fn an_opt_in_budget_stops_retries_that_would_cost_too_much() {
    let attempt = priced(100_000);
    let script = || failing(half_reply("x", 100_000), server_mid_stream());
    let provider = Fake::new((0..6).map(|_| script()));
    let mut agent = Agent::new(provider.clone(), default_session())
        .with_retry_policy(policy())
        .with_retry_budget(Some(attempt * 2.5));
    let (outcome, _) = turn(&mut agent, "hi").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Error {
                attempts: 2,
                gave_up: Some(GaveUp::Budget),
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(provider.sent().len(), 2);

    let provider = Fake::new((0..6).map(|_| script()));
    let mut agent = Agent::new(provider.clone(), default_session()).with_retry_policy(policy());
    let (outcome, _) = turn(&mut agent, "hi").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Error {
                attempts: 6,
                gave_up: Some(GaveUp::StepLimit),
                ..
            }
        ),
        "without a budget only the counts apply: {outcome:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn an_attempt_with_unknown_usage_counts_at_its_estimate() {
    let lost = failure(ErrorKind::Network, RetryClass::Transient, true);
    let provider = Fake::new(vec![
        Step::from(reply("ok", 600, 0, StopReason::EndTurn)),
        refused(lost),
        refused(overloaded()),
        Step::from(reply("ok", 600, 600, StopReason::EndTurn)),
    ]);
    let mut agent = Agent::new(provider.clone(), default_session())
        .with_retry_policy(policy())
        .with_retry_budget(Some(0.0));
    turn(&mut agent, "a").await;
    let (outcome, _) = turn(&mut agent, "b").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Error {
                kind: ErrorKind::Network,
                gave_up: Some(GaveUp::Budget),
                ..
            }
        ),
        "{outcome:?}"
    );
    let (outcome, events) = turn(&mut agent, "c").await;
    assert!(
        committed(&outcome),
        "a failure that can't be billed is retried: {outcome:?}"
    );
    assert_eq!(retrying(&events).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_zero_budget_binds_on_the_first_request_of_a_session() {
    let provider = Fake::new((0..6).map(|_| refused(server_mid_stream())));
    let mut agent = Agent::new(provider.clone(), default_session())
        .with_retry_policy(policy())
        .with_retry_budget(Some(0.0));
    let (outcome, events) = turn(&mut agent, "hi").await;
    let TurnOutcome::Error {
        attempts: 1,
        gave_up: Some(GaveUp::Budget),
        message,
        ..
    } = outcome
    else {
        panic!("{outcome:?}")
    };
    assert!(
        !message.contains("price") && !message.contains("couldn't be checked"),
        "a priced model never blames its price: {message}"
    );
    assert!(retrying(&events).is_empty());
    assert_eq!(provider.sent().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_first_request_is_retried_while_its_estimate_fits_the_budget() {
    let provider = Fake::new(vec![
        refused(server_mid_stream()),
        Step::from(reply("ok", 600, 0, StopReason::EndTurn)),
    ]);
    let mut agent = Agent::new(provider.clone(), default_session())
        .with_retry_policy(policy())
        .with_retry_budget(Some(1.0));
    let (outcome, events) = turn(&mut agent, "hi").await;
    assert!(committed(&outcome), "{outcome:?}");
    assert_eq!(retrying(&events).len(), 1);
    assert_eq!(provider.sent().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn an_unpriced_model_runs_on_counts_and_says_the_budget_was_unchecked() {
    for (model, named) in [
        ("model-without-a-price", "model-without-a-price"),
        ("sk-ant-api03-CANARYMODEL0000", wisp_protocol::NOT_SHOWN),
    ] {
        let mut session = default_session();
        session.model = model.into();
        let script = || refused(server_mid_stream());
        let provider = Fake::new((0..6).map(|_| script()));
        let mut agent = Agent::new(provider, session).with_retry_budget(Some(0.0));
        let (outcome, _) = turn(&mut agent, "hi").await;
        let TurnOutcome::Error {
            attempts: 6,
            gave_up: Some(GaveUp::StepLimit),
            message,
            ..
        } = outcome
        else {
            panic!("{outcome:?}")
        };
        assert!(
            message.ends_with(&format!(
                "(the retry budget couldn't be checked: Wisp has no price for {named})"
            )),
            "{message}"
        );
    }
    let text = "[models.\"model-without-a-price\"]\nprice_input = 1\nprice_output = 4\nprice_cache_read = 0.1\n";
    let config = crate::models::ConfigModels::parse(text).unwrap();
    let shipped = crate::models::ModelTable::shipped().unwrap();
    let mut session = default_session();
    session.model = "model-without-a-price".into();
    let provider = Fake::new((0..6).map(|_| refused(server_mid_stream())));
    let mut agent = Agent::new(provider.clone(), session)
        .with_model_settings(crate::context::ModelSettings::new(shipped, config))
        .with_retry_budget(Some(0.0));
    let (outcome, _) = turn(&mut agent, "hi").await;
    let TurnOutcome::Error {
        attempts: 1,
        gave_up: Some(GaveUp::Budget),
        message,
        ..
    } = outcome
    else {
        panic!("{outcome:?}")
    };
    assert!(!message.contains("couldn't be checked"), "{message}");
    assert_eq!(provider.sent().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn an_unpriced_endpoint_says_its_pricing_is_off_not_that_the_model_has_no_price() {
    let script = || refused(server_mid_stream());
    let provider = Fake::new((0..6).map(|_| script()));
    let gateway = Connection {
        name: "gw".into(),
        provider,
        context: LogContext {
            endpoint: "https://gateway.invalid".into(),
            endpoint_name: "gw".into(),
            ..log_context()
        },
        priced: false,
    };
    let mut agent = Agent::new(Fake::new([]), default_session())
        .with_connections(vec![gateway], Vec::new(), "gw")
        .with_retry_budget(Some(0.0));
    let (outcome, _) = turn(&mut agent, "hi").await;
    let TurnOutcome::Error {
        attempts: 6,
        gave_up: Some(GaveUp::StepLimit),
        message,
        ..
    } = outcome
    else {
        panic!("{outcome:?}")
    };
    assert!(
        message
            .ends_with("(the retry budget couldn't be checked: account gw has pricing turned off)"),
        "{message}"
    );
    assert!(!message.contains("no price"), "{message}");
}

#[tokio::test(start_paused = true)]
async fn cancel_during_the_backoff_sends_nothing_more() {
    for retry in [RetryClass::Transient, RetryClass::After { ms: 86_400_000 }] {
        let mut error = server_mid_stream();
        error.detail.retry = retry;
        let provider = Fake::new(vec![
            failing(half_reply("half", 600), error),
            Step::from(reply("ok", 600, 0, StopReason::EndTurn)),
        ]);
        let mut agent =
            Agent::new(provider.clone(), default_session()).with_retry_policy(RetryPolicy {
                base_delay: Duration::from_secs(60),
                ..RetryPolicy::default()
            });
        let outcome = cancel_on(&mut agent, |e| matches!(e, AgentEvent::Retrying { .. })).await;
        let TurnOutcome::Error {
            usage: Some(usage),
            cost: Some(cost),
            committed_steps: 0,
            attempts: 1,
            gave_up: None,
            offer: Some(offer),
            ..
        } = outcome
        else {
            panic!("{outcome:?}")
        };
        assert_eq!(
            usage.input_tokens, 600,
            "the failed attempt is still charged"
        );
        assert!(cost.turn_lower_bound);
        assert!(
            (cost.turn_usd - priced(600)).abs() < 1e-12,
            "the failed request is charged once"
        );
        assert!(offer.retry && agent.retry_offer().is_some());
        assert_eq!(provider.sent().len(), 1);
        assert!(agent.history().is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn a_settings_change_during_the_backoff_waits_for_the_next_turn() {
    let provider = Fake::new(vec![
        refused(overloaded()),
        Step::from(reply("ok", 600, 0, StopReason::EndTurn)),
        Step::from(reply("ok", 600, 600, StopReason::EndTurn)),
    ]);
    let mut agent = Agent::new(provider.clone(), default_session()).with_retry_policy(policy());
    let (cmd_tx, mut cmd_rx) = mpsc::channel(4);
    let (ev_tx, mut ev_rx) = mpsc::channel(256);
    let changer = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(e) = ev_rx.recv().await {
            if matches!(e, AgentEvent::Retrying { .. }) {
                let change = ClientCommand::shortcut(RequestId(1), None, None, Some(Effort::High));
                cmd_tx.send(change).await.unwrap();
            }
            seen.push(e);
        }
        seen
    });
    let outcome = agent
        .run_turn("a".into(), &mut cmd_rx, &ev_tx)
        .await
        .unwrap();
    drop(ev_tx);
    let events = changer.await.unwrap();
    assert!(committed(&outcome));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::SettingsChanged { .. }))
    );
    let sent = provider.texts();
    assert_eq!(
        sent[0], sent[1],
        "the change never reaches the retried request"
    );
    turn(&mut agent, "b").await;
    let sent = provider.texts();
    let high = |body: &str| body.contains("\"high\"");
    assert!(!high(&sent[0]));
    assert!(high(&sent[2]), "it applies from the next turn");
}

#[tokio::test(start_paused = true)]
async fn retries_are_logged_with_what_they_discarded_and_every_step_ends_once() {
    let diagnostics = Box::new(wisp_protocol::StreamDiagnostics {
        bytes: 77,
        events: 1,
        last_event_keys: vec!["candidates".into()],
        partial_event: true,
        ..Default::default()
    });
    let mut error = server_mid_stream();
    error.detail.stream = Some(diagnostics.clone());
    let store = MemoryStore::default();
    let provider = Fake::new(vec![
        failing(half_reply("half", 600), error),
        Step::from(reply("ok", 600, 0, StopReason::EndTurn)),
        refused(failure(ErrorKind::Auth, RetryClass::Never, false)),
    ]);
    let mut agent = logged(provider, &store).with_retry_policy(policy());
    let (_, emitted) = turn(&mut agent, "a").await;
    assert_eq!(
        pick!(&emitted, AgentEvent::Retrying { detail: Some(detail), .. } => detail.stream.clone()),
        [Some(diagnostics.clone())]
    );
    turn(&mut agent, "b").await;
    let events = log_events(&store);
    let retries: Vec<&LogEvent> = events
        .iter()
        .filter(|e| matches!(e, LogEvent::Retrying { .. }))
        .collect();
    let [
        LogEvent::Retrying {
            turn: Some(1),
            attempt: 1,
            delay_ms: 1000,
            step: Some(1),
            kind: Some(ErrorKind::Server),
            detail: Some(detail),
            usage: Some(_),
            partial_output: Some(partial),
            ..
        },
    ] = retries.as_slice()
    else {
        panic!("{retries:#?}")
    };
    assert_eq!(partial, "half");
    assert_eq!(detail.stream.as_ref(), Some(&diagnostics));
    let steps: Vec<(u64, u32, Option<String>, Option<u64>)> = pick!(
        &events,
        LogEvent::StepEnded {
            turn,
            step,
            response_id,
            prompt_tokens,
            ..
        } => (*turn, *step, response_id.clone(), *prompt_tokens)
    );
    assert_eq!(
        steps,
        [
            (1, 1, Some("msg_600".to_owned()), Some(600)),
            (2, 1, None, None)
        ]
    );
    let ended: Vec<&TurnOutcome> = events
        .iter()
        .filter_map(|e| match e {
            LogEvent::TurnEnded {
                outcome,
                partial_output,
                ..
            } => {
                assert_eq!(partial_output, &None, "the discarded text is in `retrying`");
                Some(outcome)
            }
            _ => None,
        })
        .collect();
    assert!(matches!(
        ended[1],
        TurnOutcome::Error {
            attempts: 1,
            gave_up: Some(GaveUp::NotRetryable),
            ..
        }
    ));
}

#[tokio::test]
async fn closing_the_channel_during_the_backoff_ends_the_turn_and_the_session() {
    let store = MemoryStore::default();
    let provider = Fake::new(vec![
        failing(half_reply("half", 600), server_mid_stream()),
        Step::from(reply("ok", 600, 0, StopReason::EndTurn)),
    ]);
    let agent = logged(provider.clone(), &store).with_retry_policy(RetryPolicy {
        base_delay: Duration::from_secs(60),
        ..policy()
    });
    let (commands, mut events, _task) = spawn(agent);
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    let mut commands = Some(commands);
    let mut seen = Vec::new();
    while let Some(event) = events.recv().await {
        if matches!(event, AgentEvent::Retrying { .. })
            && let Some(sender) = commands.take()
        {
            sender
                .send(ClientCommand::queue("q", "later"))
                .await
                .unwrap();
            drop(sender);
        }
        seen.push(event);
    }
    let ended: Vec<&TurnOutcome> = pick!(&seen, AgentEvent::TurnEnded { outcome, .. } => outcome);
    assert!(
        matches!(
            ended.as_slice(),
            [TurnOutcome::Cancelled {
                committed_steps: 0,
                ..
            }]
        ),
        "{ended:?}"
    );
    assert!(
        seen.iter().any(|e| matches!(
            e,
            AgentEvent::InputWithdrawn {
                ids,
                by: wisp_protocol::WithdrawnBy::SessionEnded,
            } if ids.iter().any(|id| id.as_str() == "q")
        )),
        "{seen:#?}"
    );
    assert_eq!(provider.sent().len(), 1, "no request after the close");
    assert_eq!(kinds(&store).last().unwrap(), "session_ended");
}

fn rate_limited() -> ProviderError {
    failure(ErrorKind::RateLimit, RetryClass::Transient, false)
}

fn delays(events: &[AgentEvent]) -> Vec<u64> {
    pick!(&retrying(events), AgentEvent::Retrying { delay_ms, .. } => *delay_ms)
}

fn all_busy_and_uncounted(events: &[AgentEvent]) -> bool {
    retrying(events).iter().all(|e| {
        matches!(
            e,
            AgentEvent::Retrying {
                busy: Some(_),
                max_attempts: 0,
                ..
            }
        )
    })
}

#[tokio::test(start_paused = true)]
async fn twelve_rate_limits_then_a_reply_commit_one_step_from_the_same_bytes() {
    let mut script: Vec<Step> = (0..12).map(|_| refused(rate_limited())).collect();
    script.push(Step::from(reply("ok", 600, 0, StopReason::EndTurn)));
    let provider = Fake::new(script);
    let mut agent =
        Agent::new(provider.clone(), default_session()).with_retry_policy(RetryPolicy {
            busy_window: None,
            ..policy()
        });
    let (outcome, events) = turn(&mut agent, "hi").await;
    assert!(committed(&outcome), "{outcome:?}");
    assert_eq!(retrying(&events).len(), 12);
    assert!(all_busy_and_uncounted(&events), "{events:#?}");
    assert_eq!(
        count(&events, |e| matches!(e, AgentEvent::StepEnded { .. })),
        1
    );
    let sent = provider.texts();
    assert_eq!(sent.len(), 13);
    assert!(sent.iter().all(|b| *b == sent[0]));
    assert_eq!(agent.history().len(), 2);
    assert_eq!(agent.last_turn_stats().unwrap().retries, 12);
}

#[tokio::test(start_paused = true)]
async fn an_overload_for_ten_minutes_gives_up_at_the_busy_window() {
    let provider = Fake::new((0..26).map(|_| refused(overloaded())));
    let mut agent = Agent::new(provider.clone(), default_session()).with_retry_policy(policy());
    let began = tokio::time::Instant::now();
    let (outcome, events) = turn(&mut agent, "hi").await;
    assert_eq!(began.elapsed(), Duration::from_secs(600));
    let expected = vec![
        1_000, 2_000, 4_000, 8_000, 16_000, 32_000, 64_000, 128_000, 256_000, 89_000,
    ];
    assert_eq!(delays(&events), expected);
    assert!(all_busy_and_uncounted(&events), "{events:#?}");
    let Some(AgentEvent::Retrying {
        attempt: 10,
        busy: Some(last),
        ..
    }) = retrying(&events).last()
    else {
        panic!("{events:#?}")
    };
    assert_eq!(
        (last.waited_ms, last.window_ms),
        (511_000, 600_000),
        "the wait so far, before this one"
    );
    let TurnOutcome::Error {
        kind: ErrorKind::Overloaded,
        message,
        attempts: 11,
        gave_up: Some(GaveUp::BusyWindow),
        detail: Some(_),
        offer: Some(offer),
        ..
    } = outcome
    else {
        panic!("{outcome:?}")
    };
    assert_eq!(message, "overloaded happened");
    assert!(
        offer.retry && offer.focus == wisp_protocol::RetryFocus::Retry,
        "{offer:?}"
    );
    assert_eq!(provider.sent().len(), 11);
    assert!(agent.history().is_empty());
}

#[tokio::test(start_paused = true)]
async fn busy_retries_in_one_step_leave_the_next_step_its_counted_retries() {
    let mut script: Vec<Step> = (0..11).map(|_| refused(rate_limited())).collect();
    script.push(Step::from(tool_call("t", "x")));
    script.push(refused(failure(
        ErrorKind::Server,
        RetryClass::Transient,
        false,
    )));
    script.push(Step::from(reply("done", 10, 0, StopReason::EndTurn)));
    let provider = Fake::new(script);
    let mut agent = Agent::new(provider, default_session()).with_retry_policy(RetryPolicy {
        busy_window: None,
        ..policy()
    });
    let (outcome, events) = turn(&mut agent, "hi").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { steps: 2, .. }),
        "{outcome:?}"
    );
    let last = retrying(&events);
    let Some(AgentEvent::Retrying {
        step: 2,
        attempt: 1,
        delay_ms: 1_000,
        max_attempts: 5,
        busy: None,
        ..
    }) = last.last()
    else {
        panic!("{events:#?}")
    };
    assert_eq!(last.len(), 12);
}

#[tokio::test(start_paused = true)]
async fn server_errors_among_busy_ones_keep_their_own_count() {
    let server = || refused(failure(ErrorKind::Server, RetryClass::Transient, false));
    let mut script = Vec::new();
    for _ in 0..3 {
        script.extend((0..3).map(|_| refused(overloaded())));
        script.push(server());
    }
    script.push(refused(overloaded()));
    script.push(Step::from(reply("ok", 600, 0, StopReason::EndTurn)));
    let provider = Fake::new(script);
    let mut agent = Agent::new(provider, default_session()).with_retry_policy(RetryPolicy {
        busy_window: None,
        ..policy()
    });
    let (outcome, events) = turn(&mut agent, "hi").await;
    assert!(committed(&outcome), "{outcome:?}");
    assert_eq!(retrying(&events).len(), 13);
    assert!(
        retrying(&events).iter().all(|e| matches!(
            e,
            AgentEvent::Retrying {
                max_attempts: 0,
                ..
            }
        )),
        "{events:#?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_busy_retry_after_is_a_floor_under_the_backoff() {
    let limited = || {
        refused(failure(
            ErrorKind::RateLimit,
            RetryClass::After { ms: 2_000 },
            false,
        ))
    };
    let provider = Fake::new(vec![
        limited(),
        limited(),
        limited(),
        Step::from(reply("ok", 600, 0, StopReason::EndTurn)),
    ]);
    let mut agent = Agent::new(provider, default_session()).with_retry_policy(policy());
    let began = tokio::time::Instant::now();
    let (outcome, events) = turn(&mut agent, "hi").await;
    assert!(committed(&outcome), "{outcome:?}");
    assert_eq!(delays(&events), [2_000, 2_000, 4_000]);
    assert_eq!(began.elapsed(), Duration::from_secs(8));
}

#[tokio::test(start_paused = true)]
async fn cancel_four_minutes_into_a_busy_wait_ends_the_turn_at_once() {
    let provider = Fake::new((0..30).map(|_| refused(overloaded())));
    let mut agent = Agent::new(provider.clone(), default_session()).with_retry_policy(policy());
    let began = tokio::time::Instant::now();
    let outcome = cancel_on(
        &mut agent,
        |e| matches!(e, AgentEvent::Retrying { busy: Some(b), .. } if b.waited_ms >= 240_000),
    )
    .await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Error {
                committed_steps: 0,
                gave_up: None,
                offer: Some(_),
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(began.elapsed(), Duration::from_secs(255));
    assert_eq!(provider.sent().len(), 9, "nothing more is sent");
}

#[tokio::test(start_paused = true)]
async fn busy_retries_are_logged_with_their_wait_and_the_window_ends_the_turn() {
    let store = MemoryStore::default();
    let provider = Fake::new((0..25).map(|_| refused(rate_limited())));
    let mut agent = logged(provider, &store).with_retry_policy(policy());
    turn(&mut agent, "a").await;
    let events = log_events(&store);
    let waits: Vec<Option<u64>> = pick!(
        &events,
        LogEvent::Retrying { busy, .. } => busy.map(|b| b.waited_ms)
    );
    assert_eq!(waits.len(), 10);
    assert_eq!(waits[..3], [Some(0), Some(1_000), Some(3_000)]);
    let ended: Vec<&TurnOutcome> = pick!(&events, LogEvent::TurnEnded { outcome, .. } => outcome);
    assert!(
        matches!(
            ended.as_slice(),
            [TurnOutcome::Error {
                kind: ErrorKind::RateLimit,
                attempts: 11,
                gave_up: Some(GaveUp::BusyWindow),
                ..
            }]
        ),
        "{ended:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_five_minute_session_busy_past_its_ttl_reads_an_expired_cache() {
    let store = MemoryStore::default();
    let mut script = vec![Step::from(reply("one", 2_000, 0, StopReason::EndTurn))];
    script.extend((0..15).map(|_| refused(rate_limited())));
    script.push(Step::from(reply("two", 2_100, 0, StopReason::EndTurn)));
    let provider = Fake::new(script);
    let mut session = default_session();
    session.cache_ttl = CacheTtl::FiveMinutes;
    let context = log_context();
    let mut agent = Agent::new(provider, session)
        .with_clock(Arc::new(crate::clock::ShiftedClock::paused(
            "2026-10-29T04:48:00Z".parse().unwrap(),
            Some(jiff::tz::TimeZone::UTC),
        )))
        .with_log(SessionLog::new(Box::new(store.clone()), "s1"), context)
        .with_retry_policy(RetryPolicy {
            busy_window: None,
            ..policy()
        });
    turn(&mut agent, "a").await;
    let (outcome, events) = turn(&mut agent, "b").await;
    assert!(committed(&outcome), "{outcome:?}");
    let causes: Vec<&Option<MissCause>> = pick!(
        &events,
        AgentEvent::CacheMiss { cause, .. } => cause
    );
    assert!(
        matches!(
            causes.as_slice(),
            [Some(MissCause::TtlExpired { gap_s })] if *gap_s > 300
        ),
        "{causes:?}"
    );
    assert!(!kinds(&store).iter().any(|k| k == "rebuild_marked"));
}

#[tokio::test(start_paused = true)]
async fn an_overload_after_the_stream_began_stays_a_counted_retry() {
    let billed = || {
        failing(
            half_reply("half", 600),
            failure(ErrorKind::Overloaded, RetryClass::Transient, true),
        )
    };
    let provider = Fake::new((0..6).map(|_| billed()));
    let mut agent = Agent::new(provider, default_session()).with_retry_policy(policy());
    let (outcome, events) = turn(&mut agent, "hi").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Error {
                kind: ErrorKind::Overloaded,
                attempts: 6,
                gave_up: Some(GaveUp::StepLimit),
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(
        retrying(&events).iter().all(|e| matches!(
            e,
            AgentEvent::Retrying {
                busy: None,
                max_attempts: 5,
                ..
            }
        )),
        "{events:#?}"
    );
}

#[tokio::test(start_paused = true)]
async fn every_step_and_retry_logs_its_estimate_and_model() {
    for case in COST_CASES {
        let store = MemoryStore::default();
        let provider = Fake::new(vec![
            failing(billed(half_reply("half", 600), case.0), server_mid_stream()),
            Step::from(billed(reply("ok", 600, 0, StopReason::EndTurn), case.0)),
        ]);
        let mut agent = priced_as(logged(provider, &store), case.1).with_retry_policy(policy());
        let (outcome, _) = turn(&mut agent, "a").await;
        assert!(committed(&outcome), "{outcome:?}");
        let events: Vec<LogEvent> = log_events(&store);
        assert_logged_costs(&events, case, &["retrying", "step_ended"]);
    }
}

#[tokio::test(start_paused = true)]
async fn invalid_response_retries_cap_on_default_policy() {
    let note = crate::agent::retry::MALFORMED_CALL_NOTE;
    let bad_with = |malformed_call: bool| {
        let mut error = failure(ErrorKind::InvalidResponse, RetryClass::Transient, true);
        error.malformed_call = malformed_call;
        failing(half_reply(crate::provider::NO_TEXT, 600), error)
    };
    let bad = || bad_with(false);
    let malformed = || bad_with(true);
    let deterministic = RetryPolicy {
        jitter: || 0.0,
        ..RetryPolicy::default()
    };
    async fn next_turn_end(
        ev_rx: &mut mpsc::Receiver<AgentEvent>,
        seen: &mut Vec<AgentEvent>,
    ) -> TurnOutcome {
        while let Some(event) = ev_rx.recv().await {
            seen.push(event.clone());
            if let AgentEvent::TurnEnded { outcome, .. } = event {
                return outcome;
            }
        }
        panic!("events ended before TurnEnded; seen {seen:#?}");
    }
    let plain_session = || SessionConfig {
        message_time: wisp_protocol::MessageTime::Off,
        context_note: false,
        ..default_session()
    };
    let logged_plain = |provider: Arc<Fake>, store: &MemoryStore| {
        Agent::new(provider, plain_session())
            .with_clock(TickingClock::new())
            .with_log(
                SessionLog::new(Box::new(store.clone()), "s1"),
                log_context(),
            )
    };
    let with_prefix = |step_two: bool, rest: Vec<Step>| {
        let mut script = if step_two {
            vec![Step::from(tool_call("t", "x"))]
        } else {
            Vec::new()
        };
        script.extend(rest);
        script
    };
    let rebuild = |store: &MemoryStore| {
        let recs = records(store);
        let agent = logged_plain(Fake::new(Vec::new()), store)
            .resume(&recs, 0)
            .unwrap();
        (recs, agent)
    };
    let check_note_delta =
        |before: &serde_json::Value, after: &serde_json::Value, msg_idx: usize| {
            let b = before["messages"].as_array().unwrap();
            let a = after["messages"].as_array().unwrap();
            assert_eq!(a[..msg_idx], b[..msg_idx]);
            assert_eq!(a[msg_idx]["content"][0], b[msg_idx]["content"][0]);
            assert_eq!(a[msg_idx]["content"][1]["text"], note);
        };
    let assert_rebuilt_without_note = |store: &MemoryStore| {
        let (_, resumed) = rebuild(store);
        assert!(
            !serde_json::to_string(&*resumed.history())
                .unwrap()
                .contains(note)
        );
    };
    let verify_log_and_providers = |store: &MemoryStore, live_history: &[Message]| {
        let (recs, resumed) = rebuild(store);
        assert_eq!(resumed.history(), live_history);
        assert!(!resumed.compaction.said_texts().contains(&note));
        let replayed = crate::resume::transcript(&recs, &Toolbox::default(), None);
        assert!(!replayed.iter().any(|event| matches!(
            event,
            AgentEvent::InputAccepted { id, text, .. }
                if crate::agent::retry::is_call_note_id(id) || text == note
        )));
        assert!(
            !crate::agent::view::input_ids(&recs)
                .iter()
                .any(|id| id.starts_with(crate::agent::retry::CALL_NOTE_ID_PREFIX))
        );
        let session = plain_session();
        let req = crate::provider::Request {
            session: &session,
            tools: &[],
            deferred: &[],
            by_value: &[],
            server_tools: &[],
            messages: live_history,
            previous_response_id: None,
            features: crate::provider::Features::default(),
            options: crate::provider::RequestOptions::default(),
            rendering: &crate::provider::UNRENDERED,
            compaction: None,
        };
        let anthropic =
            String::from_utf8(crate::provider::anthropic::build_body(&req).unwrap()).unwrap();
        assert_eq!(anthropic.matches(note).count(), 1, "{anthropic}");
        let openai_session = SessionConfig {
            model: crate::provider::openai::DEFAULT_MODEL.into(),
            ..plain_session()
        };
        let openai = crate::provider::openai::build_body(&crate::provider::Request {
            session: &openai_session,
            ..req
        })
        .unwrap()
        .to_string();
        assert_eq!(openai.matches(note).count(), 1, "{openai}");
    };

    for (step_two, is_malformed, cap, wait_secs) in [
        (false, false, 3u32, 7u64),
        (false, true, 8, 255),
        (true, true, 8, 255),
    ] {
        let (total, first, expected_step, msg_idx) = (
            cap + 1,
            usize::from(step_two),
            if step_two { 2 } else { 1 },
            if step_two { 2 } else { 0 },
        );
        let mut rest: Vec<Step> = (0..total).map(|_| bad_with(is_malformed)).collect();
        rest.push(Step::from(reply("after-retry", 10, 0, StopReason::EndTurn)));
        let store = MemoryStore::default();
        let provider = Fake::new(with_prefix(step_two, rest));
        let agent = logged_plain(provider.clone(), &store).with_retry_policy(deterministic);
        let (commands, mut ev_rx, task) = spawn(agent);
        let began = tokio::time::Instant::now();
        commands
            .send(ClientCommand::send("u1", "hi"))
            .await
            .unwrap();
        let mut events = Vec::new();
        let outcome = next_turn_end(&mut ev_rx, &mut events).await;
        assert_eq!(began.elapsed(), Duration::from_secs(wait_secs));
        assert!(matches!(
            outcome,
            TurnOutcome::Error {
                kind: ErrorKind::InvalidResponse,
                attempts,
                gave_up: Some(GaveUp::StepLimit),
                offer: Some(RetryOffer { retry: true, .. }),
                ..
            } if attempts == total
        ));
        let retries = retrying(&events);
        assert_eq!(retries.len(), cap as usize);
        assert!(retries.iter().enumerate().all(|(i, event)| matches!(
            event,
            AgentEvent::Retrying { step, attempt, max_attempts, kind: ErrorKind::InvalidResponse, discarded_chars: 9, .. }
                if *step == expected_step && *attempt == i as u32 + 1 && *max_attempts == cap
        )));
        let retry_cmd = ClientCommand::Retry {
            turn: 1,
            endpoint: None,
            request: RequestId(1),
        };
        commands.send(retry_cmd).await.unwrap();
        let outcome = next_turn_end(&mut ev_rx, &mut Vec::new()).await;
        assert!(committed(&outcome), "{outcome:?}");
        drop(commands);
        task.handle.await.unwrap();
        let (sent, bodies) = (provider.texts(), provider.bodies());
        assert_eq!(sent.len(), first + total as usize + 1);
        assert!(!sent[first].contains(note));
        if is_malformed {
            assert_eq!(sent[first + 1].matches(note).count(), 1);
            assert!(
                sent[first + 1..first + total as usize]
                    .iter()
                    .all(|b| *b == sent[first + 1])
            );
            check_note_delta(&bodies[first], &bodies[first + 1], msg_idx);
        } else {
            assert!(
                sent[first..first + total as usize]
                    .iter()
                    .all(|b| *b == sent[first])
            );
        }
        assert_eq!(
            sent[first + total as usize],
            sent[first],
            "Retry after give-up sends no note"
        );
        assert_rebuilt_without_note(&store);
    }

    for step_two in [false, true] {
        let (first, msg_idx, expected_steps) = (
            usize::from(step_two),
            if step_two { 2 } else { 0 },
            if step_two { 2 } else { 1 },
        );
        let store = MemoryStore::default();
        let provider = Fake::new(with_prefix(
            step_two,
            vec![
                malformed(),
                malformed(),
                Step::from(reply("ok", 10, 0, StopReason::EndTurn)),
                Step::from(reply("turn2-ok", 10, 0, StopReason::EndTurn)),
            ],
        ));
        let mut agent = logged_plain(provider.clone(), &store).with_retry_policy(deterministic);
        let (outcome, _) = turn(&mut agent, "hi").await;
        assert!(matches!(outcome, TurnOutcome::Completed { steps, .. } if steps == expected_steps));
        let (outcome, _) = turn(&mut agent, "next").await;
        assert!(committed(&outcome), "{outcome:?}");
        let bodies = provider.bodies();
        assert_eq!(bodies.len(), first + 4);
        assert!(!bodies[first].to_string().contains(note));
        assert_eq!(
            bodies[first + 1],
            bodies[first + 2],
            "2nd malformed finish must not stack notes"
        );
        check_note_delta(&bodies[first], &bodies[first + 1], msg_idx);
        assert_eq!(
            bodies[first + 3]["messages"].as_array().unwrap()[..=msg_idx],
            bodies[first + 1]["messages"].as_array().unwrap()[..=msg_idx]
        );
        assert!(!agent.compaction.said_texts().contains(&note));
        verify_log_and_providers(&store, &agent.history());
    }

    let store = MemoryStore::default();
    let provider = Fake::new(vec![
        malformed(),
        Step::from(reply("", 10, 0, StopReason::MaxTokens)),
        Step::from(reply("after-steer", 10, 0, StopReason::EndTurn)),
    ]);
    let agent = logged_plain(provider.clone(), &store).with_retry_policy(deterministic);
    let (commands, mut ev_rx, task) = spawn(agent);
    commands
        .send(ClientCommand::send("u1", "hi"))
        .await
        .unwrap();
    while let Some(event) = ev_rx.recv().await {
        if matches!(event, AgentEvent::Retrying { .. }) {
            commands
                .send(ClientCommand::send("s1", "steer"))
                .await
                .unwrap();
            break;
        }
    }
    let outcome = next_turn_end(&mut ev_rx, &mut Vec::new()).await;
    assert!(matches!(
        outcome,
        TurnOutcome::Completed {
            steps: 2,
            committed: true,
            ..
        }
    ));
    drop(commands);
    task.handle.await.unwrap();
    let sent = provider.texts();
    assert_eq!(sent.len(), 3);
    assert!(!sent[0].contains(note) && sent[1].matches(note).count() == 1);
    assert!(
        !sent[2].contains(note) && sent[2].contains("steer"),
        "{}",
        sent[2]
    );
    assert_rebuilt_without_note(&store);

    let provider = Fake::new(vec![
        bad(),
        malformed(),
        bad(),
        malformed(),
        failing(half_reply("half", 600), server_mid_stream()),
        bad(),
    ]);
    let mut agent = Agent::new(provider.clone(), plain_session()).with_retry_policy(deterministic);
    let (outcome, events) = turn(&mut agent, "hi").await;
    assert!(matches!(
        outcome,
        TurnOutcome::Error {
            kind: ErrorKind::InvalidResponse,
            attempts: 6,
            gave_up: Some(GaveUp::StepLimit),
            ..
        }
    ));
    let maxes: Vec<u32> = retrying(&events)
        .into_iter()
        .map(|e| match e {
            AgentEvent::Retrying { max_attempts, .. } => *max_attempts,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(maxes, [3, 8, 3, 8, 0]);
    let sent = provider.texts();
    assert!(
        !sent[0].contains(note) && !sent[1].contains(note) && sent[2].matches(note).count() == 1
    );
    assert!(sent[2..].iter().all(|body| *body == sent[2]));

    let mut script = vec![bad(), bad(), Step::from(tool_call("t", "x"))];
    script.extend((0..3).map(|_| bad()));
    script.push(Step::from(reply("done", 10, 0, StopReason::EndTurn)));
    let provider = Fake::new(script);
    let mut agent = Agent::new(provider, plain_session()).with_retry_policy(deterministic);
    let (outcome, events) = turn(&mut agent, "hi").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { steps: 2, .. }),
        "{outcome:?}"
    );
    let retries = retrying(&events);
    assert_eq!(retries.len(), 5);
    assert!(retries[..2].iter().enumerate().all(|(i, event)| matches!(
        event,
        AgentEvent::Retrying { step: 1, attempt, max_attempts: 3, .. } if *attempt == i as u32 + 1
    )));
    assert!(retries[2..].iter().enumerate().all(|(i, event)| matches!(
        event,
        AgentEvent::Retrying { step: 2, attempt, max_attempts: 3, .. } if *attempt == i as u32 + 1
    )));
    assert!(
        serde_json::to_string(&*agent.history())
            .unwrap()
            .contains("done")
    );
}

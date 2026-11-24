use wisp_protocol::{CacheEffect, RebuildReason};

use super::*;

fn switches(events: &[AgentEvent]) -> Vec<(String, Effort, CacheEffect, Option<RequestId>)> {
    pick!(
        events,
        AgentEvent::SettingsChanged {
            endpoint,
            effort,
            cache,
            answers,
            ..
        } => (endpoint.clone(), *effort, cache.clone(), *answers)
    )
}

fn rebuilt() -> CacheEffect {
    CacheEffect::Rebuilt {
        reason: RebuildReason::EndpointSwitch,
    }
}

fn messages_of(body: &str) -> Value {
    let v: Value = serde_json::from_str(body).unwrap();
    json!({"messages": v["messages"], "system": v["system"], "tools": v["tools"]})
}

#[tokio::test]
async fn retry_on_another_endpoint_switches_and_sends_the_same_messages_there() {
    let mut rig = Setup::new(vec![uses(&[tool_call()]), failed(ErrorKind::Server)])
        .with_b(vec![done("on the second"), done("still second")])
        .rig();
    let folder = tempfile::tempdir().unwrap();
    let remembered =
        crate::settings::remembered::RememberedStore::shared(folder.path().to_path_buf());
    let pair = wisp_protocol::settings::Remembered {
        model: "claude-sonnet-5-5".into(),
        effort: Effort::High,
    };
    remembered.keep(B, &pair).unwrap();
    rig.agent.settings.layers.remembered = remembered.clone();
    let first = rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    assert_eq!(
        offer_of(&first),
        Some(retry_offer(true, true, RetryFocus::Retry))
    );
    let kept = rig.agent.history().len();
    let events = rig.run(vec![retry_turn(1, Some(B), 9)], 1).await;
    assert_eq!(
        switches(&events),
        [(B.to_owned(), Effort::Medium, rebuilt(), None)]
    );
    let switched = position(&events, |e| matches!(e, AgentEvent::SettingsChanged { .. }));
    let retried = position(&events, |e| matches!(e, AgentEvent::TurnStarted { .. }));
    assert!(switched < retried);
    assert_eq!(started(&events), [(2, Some(1), Some(RequestId(9)))]);
    let a = rig.a.texts();
    let b = rig.b.texts();
    assert_eq!(a.len(), 2);
    assert_eq!(b.len(), 1);
    assert_eq!(messages_of(&b[0]), messages_of(&a[1]));
    assert_eq!(rig.agent.history().len(), kept + 1);
    let causes: Vec<wisp_protocol::MissCause> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::CacheMiss { cause, .. } => cause.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(
        causes,
        vec![wisp_protocol::MissCause::Expected {
            reason: "endpoint_switch".into()
        }]
    );
    rig.assert_rebuilds();

    rig.run(vec![ClientCommand::send("tui-2", "and now")], 1)
        .await;
    let b = rig.b.texts();
    assert_eq!(b.len(), 2, "the switch persists for the next turn");
    let (_, retried) = head_and_messages(&b[0]);
    let (_, next) = head_and_messages(&b[1]);
    assert!(
        next.starts_with(retried.strip_suffix("]}]").unwrap())
            || next.starts_with(retried.strip_suffix(']').unwrap())
    );

    let logged = log_events(&rig.store);
    let events: Vec<&LogEvent> = logged.iter().collect();
    let changed = events
        .iter()
        .position(|e| matches!(e, LogEvent::SettingsChanged { .. }))
        .unwrap();
    let started_at = events
        .iter()
        .position(|e| {
            matches!(
                e,
                LogEvent::TurnStarted {
                    retry_of: Some(1),
                    ..
                }
            )
        })
        .unwrap();
    assert!(changed < started_at);
    let LogEvent::TurnStarted { endpoint_name, .. } = events[started_at] else {
        unreachable!();
    };
    assert_eq!(endpoint_name.as_deref(), Some(B));

    assert_eq!(remembered.of(B), Some(pair));
    assert_eq!(remembered.of(A), None);

    for serves_failed in [true, false] {
        let source = "gpt-oss-gateway";
        let default = "claude-sonnet-5-5";
        let mut rig = Setup::new(vec![failed(ErrorKind::Server)])
            .with_b(vec![done("landed"), done("next")])
            .rig();
        rig.agent.session.model = source.into();
        rig.agent.models = crate::test_guard::models(&format!(
            "[models.\"{source}\"]\ncontext_window = 1000000\n"
        ));
        let ids = if serves_failed {
            vec![default, source]
        } else {
            vec![default]
        };
        rig.b.knobs(|k| {
            k.list = Some(Arc::new(
                crate::models::list::ModelList::from_gemini_ids(
                    ids.into_iter().map(|id| (id.to_owned(), None, None)),
                    Some(default),
                )
                .unwrap(),
            ))
        });
        rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
        let events = rig.run(vec![retry_turn(1, Some(B), 9)], 1).await;
        let expected = if serves_failed { source } else { default };
        assert_eq!(started(&events), [(2, Some(1), Some(RequestId(9)))]);
        assert_eq!(rig.b.body(0)["model"], expected);
        assert_eq!(rig.agent.session().model, expected);
        assert_eq!(
            messages_of(&rig.b.texts()[0]),
            messages_of(&rig.a.texts()[0])
        );
        assert!(log_events(&rig.store).iter().any(|e| matches!(e,
            LogEvent::TurnStarted { retry_of: Some(1), model: Some(model), .. } if model == expected
        )));
        rig.run(vec![ClientCommand::send("tui-2", "continue")], 1)
            .await;
        assert_eq!(rig.b.body(1)["model"], expected);
        rig.assert_rebuilds();
    }
}

#[tokio::test]
async fn a_retry_to_a_pair_lands_on_its_model_and_the_next_turn_stays_there() {
    let model = "claude-sonnet-5-5";
    for (account, reason) in [
        (A, RebuildReason::ModelSwitch),
        (B, RebuildReason::EndpointSwitch),
    ] {
        let failing = vec![uses(&[tool_call()]), failed(ErrorKind::Server)];
        let landed = vec![done("moved"), done("next")];
        let (a, b) = if account == A {
            ([failing, landed].concat(), Vec::new())
        } else {
            (failing, landed)
        };
        let mut rig = Setup::new(a).with_b(b).rig();
        rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
        let endpoint = rig.agent.find_endpoint(account).unwrap();
        let to = rig
            .agent
            .validate_pair(
                rig.agent.session_pair(),
                endpoint,
                Some(model.into()),
                Some(Effort::High),
            )
            .unwrap();
        let (tx, mut rx) = mpsc::channel(64);
        assert!(
            rig.agent
                .retry_command(None, 1, Some(account.into()), Some(to), false, &tx)
                .await
        );
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events.extend(rig.run(Vec::new(), 1).await);
        let cache = CacheEffect::Rebuilt { reason };
        assert_eq!(
            switches(&events),
            [(account.to_owned(), Effort::High, cache.clone(), None)]
        );
        let switched = position(&events, |e| matches!(e, AgentEvent::SettingsChanged { .. }));
        assert!(switched < position(&events, |e| matches!(e, AgentEvent::TurnStarted { .. })));
        assert_eq!(started(&events), [(2, Some(1), None)]);
        let landed = if account == A {
            rig.a.clone()
        } else {
            rig.b.clone()
        };
        let at = if account == A { 2 } else { 0 };
        assert_eq!(landed.body(at)["model"], model);
        assert_eq!(
            messages_of(&landed.texts()[at]),
            messages_of(&rig.a.texts()[1])
        );
        assert!(log_events(&rig.store).iter().any(|e| matches!(e,
            LogEvent::SettingsChanged { model: logged, cache: effect, endpoint_name: Some(name), .. }
                if logged == model && *effect == cache && name == account
        )));
        rig.run(vec![ClientCommand::send("tui-2", "and now")], 1)
            .await;
        assert_eq!(landed.body(at + 1)["model"], model);
        assert_eq!(rig.agent.session().model, model);
        let texts = landed.texts();
        let (_, retried) = head_and_messages(&texts[at]);
        let (_, next) = head_and_messages(&texts[at + 1]);
        assert!(next.starts_with(retried.strip_suffix(']').unwrap()));
        rig.assert_rebuilds();
    }
}

#[tokio::test]
async fn an_unknown_endpoint_is_refused_and_nothing_starts() {
    let mut rig = Setup::new(vec![failed(ErrorKind::Server)])
        .with_b(Vec::new())
        .rig();
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    let events = rig.idle(vec![retry_turn(1, Some("nowhere"), 4)]).await;
    let refused = refusals(&events);
    assert_eq!(refused.len(), 1);
    assert!(refused[0].1.contains("nowhere"), "{refused:?}");
    assert!(started(&events).is_empty());
    assert!(switches(&events).is_empty());
}

#[tokio::test]
async fn auth_offers_retry_and_a_switch_and_retries_on_the_same_endpoint() {
    let mut rig = Setup::new(vec![failed(ErrorKind::Auth), done("signed in again")])
        .with_b(Vec::new())
        .rig();
    let first = rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    assert_eq!(
        offer_of(&first),
        Some(retry_offer(true, true, RetryFocus::Retry))
    );
    assert_eq!(
        rig.a.texts().len(),
        1,
        "an auth failure is never retried by itself"
    );
    let events = rig.run(vec![retry_turn(1, None, 1)], 1).await;
    assert_eq!(started(&events), [(2, Some(1), Some(RequestId(1)))]);
    assert_eq!(rig.a.texts().len(), 2);
    assert!(rig.b.texts().is_empty());
}

#[tokio::test]
async fn a_pending_endpoint_change_applies_to_retry_without_a_second_switch() {
    let mut rig = Setup::new(vec![failed(ErrorKind::Overloaded)])
        .with_b(vec![done("over here")])
        .rig();
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    let changed = rig
        .idle(vec![ClientCommand::shortcut(
            RequestId(1),
            Some(B.into()),
            None,
            None,
        )])
        .await;
    assert_eq!(switches(&changed).len(), 1);
    let events = rig.run(vec![retry_turn(1, None, 2)], 1).await;
    assert!(switches(&events).is_empty());
    assert_eq!(rig.b.texts().len(), 1);
    assert_eq!(
        messages_of(&rig.b.texts()[0]),
        messages_of(&rig.a.texts()[0])
    );
    assert_eq!(rig.agent.endpoint_name(), B);
    assert_eq!(rig.agent.pending_settings(), None);
}

#[tokio::test]
async fn a_switching_retry_keeps_a_pending_effort_for_the_next_turn() {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Steers {
        None,
        Resent,
        Fresh,
    }

    for (model, retried_effort, per_message, after_marker, steers) in [
        (None, Effort::Xhigh, true, false, Steers::None),
        (
            Some(DEFAULT_MODEL),
            Effort::High,
            false,
            false,
            Steers::None,
        ),
        (Some(DEFAULT_MODEL), Effort::High, true, false, Steers::None),
        (
            Some("gemini-4-carbon"),
            Effort::High,
            false,
            false,
            Steers::None,
        ),
        (None, Effort::Xhigh, true, true, Steers::None),
        (Some(DEFAULT_MODEL), Effort::High, true, true, Steers::None),
        (
            Some(DEFAULT_MODEL),
            Effort::High,
            true,
            true,
            Steers::Resent,
        ),
        (Some(DEFAULT_MODEL), Effort::High, true, true, Steers::Fresh),
    ] {
        let mut tool_step = uses(&[tool_call()]);
        if steers == Steers::Resent {
            tool_step = tool_step.then(ClientCommand::send("tui-steer", "and also this"));
        }
        let mut steps = vec![tool_step, failed(ErrorKind::Server)];
        if after_marker {
            steps.insert(0, done("first"));
        }
        let mut retried_steps = vec![done("retried"), done("next")];
        let corrective = after_marker && model.is_some();
        if corrective {
            retried_steps.insert(0, uses(&[tool_call()]));
        }
        if steers == Steers::Fresh {
            retried_steps.insert(
                0,
                reply(&[B::Think], StopReason::StopSequence)
                    .then(ClientCommand::send("tui-fresh", "and also this")),
            );
        }
        let mut setup = Setup::new(steps).with_b(retried_steps);
        setup.session.effort = if after_marker {
            Effort::Low
        } else {
            Effort::Xhigh
        };
        let mut rig = setup.rig();
        if after_marker {
            rig.run(vec![ClientCommand::send("tui-before", "first")], 1)
                .await;
            rig.idle(vec![ClientCommand::shortcut(
                RequestId(1),
                None,
                None,
                Some(Effort::Xhigh),
            )])
            .await;
        }
        rig.run(vec![ClientCommand::send("tui-failed", "go")], 1)
            .await;
        rig.idle(vec![ClientCommand::shortcut(
            RequestId(1),
            None,
            None,
            Some(Effort::Low),
        )])
        .await;
        if let Some(model) = model {
            rig.b.knobs(|k| {
                k.caps = Some(Caps::Fixed(Some(Box::leak(Box::new(
                    crate::provider::ModelCapabilities {
                        efforts: &[Effort::Low, Effort::Medium, Effort::High],
                        per_message_effort: per_message,
                        ..*capabilities::row(DEFAULT_MODEL).unwrap()
                    },
                )))));
                if model != DEFAULT_MODEL {
                    k.kind = Some(crate::endpoints::ProviderKind::Gemini);
                    k.list = Some(Arc::new(
                        crate::models::list::ModelList::from_gemini_ids(
                            [(model.to_owned(), None, None)],
                            Some(model),
                        )
                        .unwrap(),
                    ));
                }
            });
        }
        let model = model.unwrap_or(DEFAULT_MODEL);
        let failed_turn = if after_marker { 2 } else { 1 };
        let kept = rig.agent.history().to_vec();
        let events = rig.run(vec![retry_turn(failed_turn, Some(B), 2)], 1).await;
        assert_eq!(
            switches(&events),
            [(B.to_owned(), Effort::Low, rebuilt(), None)]
        );
        assert_eq!(rig.agent.session().effort, retried_effort);
        assert_eq!(rig.agent.session().model, model);
        let baseline = if after_marker {
            Effort::Low
        } else {
            retried_effort
        };
        assert_eq!(rig.b.sent()[0].session.effort, baseline);
        assert_eq!(
            rig.b.sent()[0].features.per_message_effort.is_some(),
            per_message
        );
        assert_eq!(rig.agent.bare_request_parts().0.effort, baseline);
        assert_eq!(&rig.agent.history()[..kept.len()], kept.as_slice());
        if after_marker && retried_effort == Effort::Xhigh {
            assert_eq!(
                rig.b.sent()[0].messages,
                rig.a.sent().last().unwrap().messages
            );
        }
        if corrective {
            let request = &rig.b.sent()[0];
            assert_eq!(
                request.messages.len(),
                kept.len() + 1 + usize::from(steers == Steers::Resent)
            );
            assert_eq!(&request.messages[..kept.len()], kept.as_slice());
            assert_eq!(
                request.messages.get(kept.len()),
                Message::turn_system(
                    Some(wisp_protocol::EffortMarker {
                        level: retried_effort,
                        rendered: true,
                    }),
                    &[]
                )
                .as_ref()
            );
        }
        rig.assert_rebuilds();
        assert_eq!(rig.agent.pending_settings(), Some((B, model, Effort::Low)));
        let records = records(&rig.store);
        let retried = turn_started_records(&records)
            .into_iter()
            .find_map(|e| match e {
                LogEvent::TurnStarted {
                    retry_of: Some(of),
                    effort,
                    endpoint_name,
                    effort_marker,
                    ..
                } if *of == failed_turn => Some((*effort, endpoint_name.clone(), *effort_marker)),
                _ => None,
            })
            .unwrap();
        let corrective = (after_marker && retried_effort != Effort::Xhigh).then_some(
            wisp_protocol::EffortMarker {
                level: retried_effort,
                rendered: true,
            },
        );
        assert_eq!(
            retried,
            (Some(retried_effort), Some(B.to_owned()), corrective)
        );
        rig.run(vec![ClientCommand::send("tui-next", "continue")], 1)
            .await;
        assert_eq!(rig.agent.session().effort, Effort::Low);
        let sent = rig.b.sent();
        let retry_requests =
            if corrective.is_some() { 2 } else { 1 } + usize::from(steers == Steers::Fresh);
        assert_eq!(sent.len(), retry_requests + 1);
        let events = log_events(&rig.store);
        let rebuilt = rebuild(&rig.store, events.iter()).unwrap().messages;
        if let Some(marker) = corrective {
            assert_eq!(events.iter().filter(|event| matches!(event,
                LogEvent::MessageCommitted { message, .. } if message.effort_marker() == Some(marker)
            )).count(), 1);
        }
        for (index, request) in sent.iter().enumerate() {
            let expected = if index < retry_requests {
                retried_effort
            } else {
                Effort::Low
            };
            assert_eq!(
                &rebuilt[..request.messages.len()],
                request.messages.as_slice()
            );
            assert!(
                rig.b
                    .capabilities(&request.session.model)
                    .unwrap()
                    .takes_effort(request.session.effort)
            );
            let marker = request.features.per_message_effort.and_then(|_| {
                request
                    .messages
                    .iter()
                    .rev()
                    .filter_map(Message::effort_marker)
                    .find(|m| m.rendered)
            });
            assert_eq!(marker.map_or(request.session.effort, |m| m.level), expected);
        }
        rig.assert_rebuilds();
    }
}

#[tokio::test]
async fn a_binding_failure_suggests_the_endpoint_the_conversation_came_from() {
    let mut rig = Setup::new(vec![done("on the first"), done("back again")])
        .with_b(vec![failed(ErrorKind::ThinkingBinding)])
        .rig();
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    rig.idle(vec![ClientCommand::shortcut(
        RequestId(1),
        Some(B.into()),
        None,
        None,
    )])
    .await;
    let failed = rig.run(vec![ClientCommand::send("tui-2", "next")], 1).await;
    assert_eq!(
        offer_of(&failed),
        Some(RetryOffer {
            retry: false,
            switch: true,
            focus: RetryFocus::Switch,
            suggest: Some(A.to_owned()),
            retries_at: None,
        })
    );
    let refused = rig.idle(vec![retry_turn(2, None, 2)]).await;
    assert_eq!(
        refusals(&refused),
        [(
            RequestId(2),
            "a thinking-binding failure isn't retried on the same account".to_owned()
        )]
    );
    rig.run(vec![retry_turn(2, Some(A), 3)], 1).await;
    assert_eq!(rig.a.texts().len(), 2);
}

#[tokio::test]
async fn a_binding_failure_without_a_move_suggests_nothing() {
    let mut rig = Setup::new(vec![failed(ErrorKind::ThinkingBinding)])
        .with_b(Vec::new())
        .rig();
    let failed = rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    assert_eq!(
        offer_of(&failed),
        Some(retry_offer(false, true, RetryFocus::Switch))
    );
}

fn on_gemini(fake: &Fake) {
    fake.knobs(|k| {
        k.kind = Some(crate::endpoints::ProviderKind::Gemini);
        k.list = Some(Arc::new(
            crate::models::list::ModelList::from_gemini_ids([(NEON.to_owned(), None, None)], None)
                .unwrap(),
        ));
    });
}

fn report(id: &str) -> (crate::agent::Mail, String) {
    let report = crate::agent::Mail {
        id: wisp_protocol::MailId(id.into()),
        from: AgentPath::new("/child"),
        to: AgentPath::root(),
        kind: wisp_protocol::MailKind::Report,
        wake: true,
        text: format!("child finished {id}"),
    };
    let shown = crate::agent::render_mail(&report.from, report.kind, &report.text);
    (report, shown)
}

fn reported(rig: &Rig, id: &str) -> String {
    let (report, shown) = report(id);
    rig.agent.mail_sender().send(report).unwrap();
    shown
}

#[tokio::test]
async fn mail_folded_into_a_running_neon_turn_after_a_switch_to_claude_stays_framed() {
    let to_claude = ClientCommand::shortcut(
        RequestId(1),
        Some(B.into()),
        Some(DEFAULT_MODEL.into()),
        None,
    );
    let sample = quick_samples().remove(0);
    let (folded, shown) = report("m1");
    let mut setup = Setup::new(vec![
        uses(&[call("c1", &sample.name, sample.input.clone())]).then(to_claude),
        uses(&[call("c2", &sample.name, sample.input)]).mail(folded),
        done("neon"),
    ])
    .with_b(vec![done("claude")]);
    setup.session.model = NEON.into();
    let mut rig = setup.rig();
    on_gemini(&rig.a);
    rig.a.mail_to(rig.agent.mail_sender());
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    let later = reported(&rig, "m2");
    rig.run(Vec::new(), 1).await;
    let form = neon_form();
    let framed = format!("{}{shown}{}", form.mail_before, form.mail_after);
    let neon = rig.a.sent();
    assert!(texts(neon[2].messages.last().unwrap()).contains(&framed.as_str()));
    let claude = rig.b.sent();
    let opening = texts(claude[0].messages.last().unwrap());
    assert_eq!(opening.last(), Some(&later.as_str()), "{opening:#?}");
}

#[tokio::test]
async fn a_switch_to_neon_and_back_forms_only_new_turns_and_their_mail_and_requests_extend() {
    let mut rig = Setup::new(vec![done("one"), done("two")])
        .with_b(vec![done("three")])
        .rig();
    on_gemini(&rig.b);
    rig.run(vec![ClientCommand::send("tui-1", "a")], 1).await;
    rig.run(vec![ClientCommand::send("tui-2", "b")], 1).await;
    let to_neon = ClientCommand::shortcut(RequestId(1), Some(B.into()), Some(NEON.into()), None);
    rig.idle(vec![to_neon]).await;
    let first = reported(&rig, "m1");
    rig.run(Vec::new(), 1).await;
    let mut again = Setup::new(vec![done("four")]).with_b(vec![done("five")]);
    again.from = Some(records(&rig.store));
    let mut again = again.rig();
    on_gemini(&again.a);
    again.run(vec![ClientCommand::send("tui-4", "d")], 1).await;
    let back = ClientCommand::shortcut(
        RequestId(2),
        Some(B.into()),
        Some(DEFAULT_MODEL.into()),
        None,
    );
    again.idle(vec![back]).await;
    let second = reported(&again, "m2");
    again.run(Vec::new(), 1).await;
    let requests: Vec<Vec<Message>> = [&rig.a, &rig.b, &again.a, &again.b]
        .into_iter()
        .flat_map(|fake| fake.sent())
        .map(|request| request.messages)
        .collect();
    assert_eq!(requests.len(), 5);
    for pair in requests.windows(2) {
        assert!(pair[1].starts_with(&pair[0]), "{pair:#?}");
    }
    let openings: Vec<Vec<&str>> = requests[4]
        .iter()
        .filter(|m| m.role == Role::User)
        .map(texts)
        .collect();
    assert_eq!(openings.len(), 5, "{openings:#?}");
    for claude in [&openings[0], &openings[1], &openings[4]] {
        assert!(claude.iter().any(|t| bare_lines(t)), "{claude:#?}");
        assert!(!claude.iter().any(|t| is_data(t)), "{claude:#?}");
    }
    for neon in &openings[2..4] {
        assert!(!neon.iter().any(|t| bare_lines(t)), "{neon:#?}");
        assert!(is_data(neon.last().unwrap()), "{neon:#?}");
    }
    let form = neon_form();
    let framed = format!("{}{first}{}", form.mail_before, form.mail_after);
    assert_eq!(openings[2][openings[2].len() - 2], framed);
    assert_eq!(openings[4].last(), Some(&second.as_str()));
    again.assert_rebuilds();
}

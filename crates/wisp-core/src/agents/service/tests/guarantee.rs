use super::follow::subscription;
use super::*;
use crate::agents::{NUDGE, NUDGE_REPORT_ONLY};
use crate::plugin::testing::hook_flag;
use wisp_protocol::agents::SEND_MESSAGE;
use wisp_protocol::{AttachInput, AttachItems, AttachKey, AttachKeyCode};

fn last_idle(run: &Run, agent: &str) -> Option<(Option<IdleOutcome>, Activity)> {
    run.seen.iter().rev().find_map(|e| match e {
        AgentEvent::AgentStateChanged {
            agent: a,
            state: AgentState::Idle { outcome },
            activity,
            ..
        } if a.as_str() == agent => Some((*outcome, activity.clone())),
        _ => None,
    })
}

fn idle_outcome(run: &Run, agent: &str) -> Option<IdleOutcome> {
    last_idle(run, agent)?.0
}

async fn settled(run: &mut Run, agent: &str) {
    let agent = agent.to_owned();
    run.until("the child to settle", move |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent: a, state: AgentState::Idle { .. }, .. }
                if a.as_str() == agent
        )
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_child_without_send_message_is_nudged_only_to_report() {
    let mut run = start(
        "guarantee-nudge-report-only",
        Setup {
            without: vec!["send_message"],
            ..Setup::default()
        },
        vec![
            (
                "/quiet",
                vec![done("first"), uses(&[report_call("r1", "ok")])],
            ),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(spawn_request("quiet", "Do it.")).await;
    run.child_ended("/quiet").await;
    let requests = run.requests("/quiet");
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].tool_names().contains(&SEND_MESSAGE.to_owned()));
    let nudge = requests[1].last_user_texts();
    assert!(
        nudge
            .iter()
            .any(|t| t.contains(NUDGE_REPORT_ONLY) && t.contains("kind=\"notice\"")),
        "{nudge:?}"
    );
    assert!(
        nudge.iter().all(|t| !t.contains("send_message")),
        "{nudge:?}"
    );
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_child_that_never_reports_is_nudged_twice_then_reported_as_no_report() {
    let mut run = start(
        "guarantee-no-report",
        Setup::default(),
        vec![
            (
                "/quiet",
                vec![done("first"), done("second"), done("last words")],
            ),
            ("/", vec![done("noted"), done("more")]),
        ],
    )
    .await;
    run.spawn(spawn_request("quiet", "Do it.")).await;
    let outcome = run.child_ended("/quiet").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    settled(&mut run, "/quiet").await;
    let requests = run.requests("/quiet");
    assert_eq!(requests.len(), 3, "two nudges, then stop");
    let nudge = requests[1].last_user_texts();
    assert!(
        nudge
            .iter()
            .any(|t| t.contains(NUDGE) && t.contains("kind=\"notice\"")),
        "{nudge:?}"
    );
    assert!(requests[0].tool_names().contains(&SEND_MESSAGE.to_owned()));
    assert_eq!(idle_outcome(&run, "/quiet"), Some(IdleOutcome::NoReport));
    let reported = run
        .until("the report", |e| {
            matches!(e, AgentEvent::AgentReported { .. })
        })
        .await;
    assert!(matches!(
        reported,
        AgentEvent::AgentReported {
            outcome: ReportOutcome::NoReport,
            report: None,
            ..
        }
    ));
    let run = run.quit().await;
    let mail = run.mail_to("/");
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert_eq!((mail[0].0, mail[0].1), (MailKind::Report, true));
    assert!(
        mail[0].2.contains("ended without a report (no_report"),
        "{}",
        mail[0].2
    );
    assert!(
        mail[0].2.ends_with("last words\n</wisp:message>"),
        "{}",
        mail[0].2
    );
}

#[tokio::test(start_paused = true)]
async fn two_invalid_reports_end_the_turn_in_an_error_and_report_invalid_report() {
    let mut run = start(
        "guarantee-invalid",
        Setup::default(),
        vec![
            (
                "/sloppy",
                vec![
                    uses(&[call("r1", REPORT, json!({"text": "  "}))]),
                    uses(&[call(
                        "r2",
                        REPORT,
                        json!({"kind": "summary", "text": "Done."}),
                    )]),
                    done("never"),
                ],
            ),
            ("/", vec![done("noted"), done("more")]),
        ],
    )
    .await;
    run.spawn(spawn_request("sloppy", "Do it.")).await;
    let outcome = run.child_ended("/sloppy").await;
    let TurnOutcome::Error { kind, message, .. } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(kind, ErrorKind::Other);
    assert!(
        message.starts_with("the report was refused twice"),
        "{message}"
    );
    assert_eq!(run.requests("/sloppy").len(), 2);
    settled(&mut run, "/sloppy").await;
    assert_eq!(
        idle_outcome(&run, "/sloppy"),
        Some(IdleOutcome::InvalidReport)
    );
    let run = run.quit().await;
    let mail = run.mail_to("/");
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert_eq!(mail[0].0, MailKind::Report);
    assert!(
        mail[0].2.contains("two invalid reports (invalid_report)"),
        "{}",
        mail[0].2
    );
    assert!(mail[0].2.contains("Last input (invalid):"), "{}", mail[0].2);
    assert!(
        mail[0]
            .2
            .contains("Last refusal: report takes one field, text, holding the whole report"),
        "{}",
        mail[0].2
    );
    let errors: Vec<String> = run
        .requests("/sloppy")
        .iter()
        .skip(1)
        .map(|r| result_of(r, "r1").1)
        .collect();
    assert_eq!(errors, [crate::agents::report::ONE_FIELD]);
    let refusals = run
        .records("/sloppy")
        .into_iter()
        .filter(|e| {
            matches!(
                e,
                LogEvent::AgentRefused {
                    action: AgentAction::Report,
                    ..
                }
            )
        })
        .count();
    assert_eq!(refusals, 2);
}

fn used_up(now: i64, reset: Option<i64>) -> ProviderError {
    let mut error = classified(
        ErrorKind::RateLimit,
        "PRIVATE_CANARY",
        FailurePhase::Response,
        reset.map_or(RetryClass::Never, |at| RetryClass::After {
            ms: u64::try_from(at - now).unwrap() * 1000,
        }),
    );
    error.detail.status = Some(429);
    crate::provider::limit::reached(
        &mut error.detail,
        now,
        reset,
        wisp_protocol::plan_usage::UsageSource::RetryAfter,
        None,
    );
    error
}

pub(super) fn guarded(now: i64, model: &str) -> ProviderError {
    use wisp_protocol::plan_usage::{RETRY_AFTER_KEY, UsageScope};
    let mut error = used_up(now, Some(now + 508_200));
    let window = error.detail.limit_reached.as_mut().unwrap();
    window.key = RETRY_AFTER_KEY.into();
    window.label = "Retry after".into();
    window.scope = UsageScope::Model(model.into());
    error
}

pub(super) fn failing(gate: &Arc<Semaphore>, now: i64, reset: i64) -> Vec<Step> {
    let error = used_up(now, Some(reset));
    vec![gated(
        Step::from(vec![Ok(ProviderEvent::Sending), Err(error)]),
        gate,
    )]
}

pub(super) const MOVING: &str = "[agents.presets.worker]\ndescription = 'Works'\nendpoint = 'first'\nmodel = 'claude-opus-5-5'\neffort = 'high'\nfallback = [{endpoint = 'second', model = 'claude-opus-5-5', effort = 'high'}]\n";

pub(super) fn hold_limit(
    run: &Run,
    cache: &crate::plan_usage::PlanUsageCache,
    name: &str,
    (now, reset): (i64, i64),
) {
    let generation = cache
        .bind(name, run.providers.get(&format!("{name}:/")))
        .unwrap();
    let error = used_up(now, Some(reset));
    cache.reached(
        name,
        generation,
        error.detail.limit_reached.as_deref().unwrap(),
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_turn_sends_a_failed_notice() {
    let clock = Arc::new(crate::clock::StillClock);
    let now = clock.now().as_second();
    for (limited, reset, guard) in [
        (false, None, false),
        (true, None, false),
        (true, Some(now + 360), false),
        (true, Some(now + 508_200), false),
        (true, Some(now + 508_200), true),
    ] {
        let gate = Arc::new(Semaphore::new(0));
        let refusal = if guard {
            Step::from(vec![Ok(ProviderEvent::Sending), Err(guarded(now, MODEL))])
        } else if limited {
            Step::from(vec![Ok(ProviderEvent::Sending), Err(used_up(now, reset))])
        } else {
            refused_request()
        };
        let mut run = start(
            "guarantee-failed",
            Setup {
                endpoints: vec![endpoint("example"), endpoint("example2")],
                clock: Some(clock.clone()),
                ..Setup::default()
            },
            vec![
                ("example:/broken", vec![gated(refusal, &gate)]),
                ("example:/", vec![done("noted"), done("more")]),
            ],
        )
        .await;
        run.spawn(spawn_request("broken", "Do it.")).await;
        run.until("the child to start its step", |e| {
            matches!(inner(e, "/broken"), Some(AgentEvent::StepStarted { .. }))
        })
        .await;
        let request = run.request();
        run.send(ClientCommand::Child {
            agent: path("/broken"),
            command: Box::new(ClientCommand::shortcut(
                request,
                Some("example2".into()),
                None,
                None,
            )),
        })
        .await;
        run.until("a pending account switch", |e| matches!(inner(e, "/broken"), Some(AgentEvent::SettingsChanged { answers: Some(r), .. }) if *r == request)).await;
        gate.add_permits(1);
        let outcome = run.child_ended("/broken").await;
        let kind = if limited {
            ErrorKind::RateLimit
        } else {
            ErrorKind::InvalidRequest
        };
        assert!(
            matches!(&outcome, TurnOutcome::Error { kind: actual, .. } if *actual == kind),
            "{outcome:?}"
        );
        if reset.is_some() {
            assert!(matches!(
                outcome,
                TurnOutcome::Error {
                    gave_up: Some(wisp_protocol::GaveUp::RetryAfterTooLong),
                    ..
                }
            ));
        }
        settled(&mut run, "/broken").await;
        assert_eq!(idle_outcome(&run, "/broken"), Some(IdleOutcome::Failed));
        run.until("the root to wake", |e| {
            matches!(e, AgentEvent::TurnStarted { .. })
        })
        .await;
        let run = run.quit().await;
        let mail = run.mail_to("/");
        assert_eq!(mail.len(), 1, "{mail:?}");
        assert_eq!((mail[0].0, mail[0].1), (MailKind::Notice, true));
        let expected = match (limited, reset, guard) {
            (_, _, true) => {
                "/broken failed (rate_limit): example refused the request, retry in 5d21h."
            }
            (true, Some(at), false) if at == now + 360 => {
                "/broken failed (rate_limit): example reached its usage limit, resets in 6m00s."
            }
            (true, Some(_), false) => {
                "/broken failed (rate_limit): example reached its usage limit, resets in 5d21h."
            }
            (true, None, false) => "/broken failed (rate_limit).",
            _ => "/broken failed (invalid_request).",
        };
        assert_eq!(wisp_protocol::mail_body(&mail[0].2), expected);
        for private in ["something is wrong", "org-7f3a9c", "CANARY", "example2"] {
            assert!(!mail[0].2.contains(private), "{}", mail[0].2);
        }
        let requests = run.requests("example:/");
        let sent = format!("{:?}", requests.last().unwrap().messages);
        assert!(sent.contains(expected), "{sent}");
        assert!(!sent.contains("org-7f3a9c"), "{sent}");
        assert_eq!(run.requests("example:/broken").len(), 1);
        let records = run.records("/broken");
        assert_eq!(
            records
                .iter()
                .filter(|e| matches!(e, LogEvent::TurnEnded { .. }))
                .count(),
            1
        );
        assert!(!records.iter().any(|e| matches!(
            e,
            LogEvent::Retrying { .. } | LogEvent::AgentReported { .. }
        )));
    }
}

#[tokio::test(start_paused = true)]
async fn a_used_up_child_moves_to_its_first_choice_with_room_and_tells_its_parent_once() {
    let now = 1_800_000_000;
    let clock = Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None));
    let gates = [
        Arc::new(Semaphore::new(0)),
        Arc::new(Semaphore::new(0)),
        Arc::new(Semaphore::new(0)),
    ];
    let cache = crate::plan_usage::PlanUsageCache::default();
    let mut run = start(
        "guarantee-moved",
        Setup {
            endpoints: vec![
                subscription("first"),
                openai("spare", wisp_protocol::Billing::ChatGptPlan),
                subscription("second"),
            ],
            clock: Some(clock),
            plan_usage: cache.clone(),
            preset_config: Some(
                "[agents.presets.worker]\ndescription = 'Works'\nendpoint = 'first'\nmodel = 'claude-opus-5-5'\neffort = 'high'\nfallback = [{endpoint = 'spare', model = 'gpt-6-astra', effort = 'high'}, {endpoint = 'second', model = 'claude-opus-5-5', effort = 'high'}]\n",
            ),
            ..Setup::default()
        },
        vec![
            ("first:/worker", failing(&gates[0], now, now + 3600)),
            ("second:/worker", failing(&gates[1], now, now + 7200)),
            ("first:/unheld", failing(&gates[2], now, now + 3600)),
            ("first:/", vec![done("noted"); 2]),
        ],
    )
    .await;
    for name in ["unheld", "worker"] {
        run.spawn(SpawnRequest {
            preset: Some("worker".into()),
            ..spawn_request(name, "Do it.")
        })
        .await;
    }
    gates[2].add_permits(1);
    run.child_ended("/unheld").await;
    run.until("the root to wake", |e| {
        matches!(e, AgentEvent::TurnStarted { .. })
    })
    .await;
    for ((name, reset), gate) in [("first", now + 3600), ("second", now + 7200)]
        .into_iter()
        .zip(&gates)
    {
        run.until("the child's step", |e| {
            matches!(inner(e, "/worker"), Some(AgentEvent::StepStarted { .. }))
        })
        .await;
        hold_limit(&run, &cache, name, (now, reset));
        gate.add_permits(1);
        run.child_ended("/worker").await;
    }
    settled(&mut run, "/worker").await;
    run.until("the root to wake", |e| {
        matches!(e, AgentEvent::TurnStarted { .. })
    })
    .await;
    let run = run.quit().await;
    let moved: Vec<&AgentEvent> = run
        .seen
        .iter()
        .filter_map(|e| inner(e, "/worker"))
        .filter(|e| {
            matches!(
                e,
                AgentEvent::TurnEnded { .. }
                    | AgentEvent::SettingsChanged { .. }
                    | AgentEvent::TurnStarted { .. }
            )
        })
        .collect();
    assert!(
        matches!(
            moved[..],
            [
                AgentEvent::TurnStarted { retry_of: None, .. },
                AgentEvent::TurnEnded { .. },
                AgentEvent::SettingsChanged { endpoint, answers: None, .. },
                AgentEvent::TurnStarted { retry_of: Some(1), answers: None, .. },
                AgentEvent::TurnEnded { .. },
            ] if endpoint == "second"
        ),
        "{moved:#?}"
    );
    let failed = run.requests("first:/worker");
    let retried = run.requests("second:/worker");
    assert_eq!((failed.len(), retried.len()), (1, 1));
    assert!(run.requests("spare:/worker").is_empty());
    assert_eq!(
        format!("{:?}", retried[0].messages),
        format!("{:?}", failed[0].messages)
    );
    assert!(run.requests("second:/unheld").is_empty());
    let mut mail = run.mail_to("/");
    assert_eq!(mail.len(), 3, "{mail:?}");
    assert_eq!(
        wisp_protocol::mail_body(&mail.remove(0).2),
        "/unheld failed (rate_limit): first reached its usage limit, resets in 1h00m."
    );
    assert_eq!((mail[0].0, mail[0].1), (MailKind::Notice, false));
    let notice = wisp_protocol::mail_body(&mail[0].2);
    assert!(
        notice.starts_with("moved to second with claude-opus-5-5 high (cache rebuild ~$"),
        "{notice}"
    );
    assert!(
        notice.ends_with("): first reached its usage limit, resets in 1h00m."),
        "{notice}"
    );
    assert_eq!((mail[1].0, mail[1].1), (MailKind::Notice, true));
    assert_eq!(
        wisp_protocol::mail_body(&mail[1].2),
        "/worker failed (rate_limit): second reached its usage limit, resets in 2h00m."
    );

    use crate::agent::testing::{NEW_GEMINI, gemini_list};
    use crate::models::list::tests::{claude_entry, claude_list};
    for account_only in [false, true] {
        let gate = Arc::new(Semaphore::new(0));
        let first = scripted(failing(&gate, now, now + 3600));
        let second = scripted([uses(&[report_call("r1", "ok")])]);
        let (model, effort, old, expected, landed_effort, preset) = if account_only {
            let model = "claude-opus-7";
            let old = "claude-opus-5-5";
            let entry = |id| claude_entry(id, "opus", 1_000_000);
            let mut short = entry(model);
            for level in ["xhigh", "max"] {
                short["capabilities"]["effort"][level] = serde_json::json!({ "supported": false });
            }
            for provider in [&first, &second] {
                provider.knobs(|k| {
                    k.billing = Some(wisp_protocol::Billing::AnthropicPlan);
                    k.caps = Some(Caps::Fixed(capabilities::row(old)));
                });
            }
            first.knobs(|k| k.list = Some(Arc::new(claude_list(&[entry(model)]))));
            second.knobs(|k| {
                k.list = Some(Arc::new(claude_list(&[entry(old)])));
                k.fresh = Some(Arc::new(claude_list(&[short])));
            });
            (
                model,
                Effort::Max,
                old,
                model,
                Some(Effort::High),
                "[agents.presets.worker]\ndescription = 'Works'\nfallback = [{endpoint = 'backup'}]\n",
            )
        } else {
            let model = "gemini-4-carbon-high";
            for provider in [&first, &second] {
                table(provider, Table::Gemini);
                provider.knobs(|k| {
                    k.billing = Some(wisp_protocol::Billing::AntigravityPlan);
                    k.list = Some(gemini_list(&[model]));
                });
            }
            second.knobs(|k| k.fresh = Some(gemini_list(&[NEW_GEMINI])));
            (
                model,
                Effort::High,
                model,
                NEW_GEMINI,
                None,
                "[agents.presets.worker]\ndescription = 'Works'\nfallback = [{endpoint = 'backup', model = 'gemini-nova-new'}]\n",
            )
        };
        let folder = tempfile::tempdir().unwrap();
        let mut host = crate::endpoints::testing::AuthHost::new();
        let catalog = host.catalog.clone().with_lists(Some(folder.path().into()));
        Arc::get_mut(&mut host).unwrap().catalog = catalog;
        *host.next_provider.lock().unwrap() = Some(first.clone());
        host.load("env");
        *host.next_provider.lock().unwrap() = Some(second.clone());
        host.load("backup");
        host.catalog.check().await;
        let env = host
            .catalog
            .view()
            .ready
            .into_iter()
            .find(|c| c.name == "env")
            .unwrap();
        let plan_usage = crate::plan_usage::PlanUsageCache::default();
        let mut run = start(
            "guarantee-moved-unlisted",
            Setup {
                endpoint_host: Some(host),
                root_model: model,
                root_effort: Some(effort),
                clock: Some(Arc::new(ShiftedClock::at(
                    Timestamp::from_second(now).unwrap(),
                    None,
                ))),
                plan_usage: plan_usage.clone(),
                preset_config: Some(preset),
                ..Setup::default()
            },
            Vec::new(),
        )
        .await;
        run.spawn(SpawnRequest {
            preset: Some("worker".into()),
            ..spawn_request("worker", "Do it.")
        })
        .await;
        run.until("the child's step", |e| {
            matches!(inner(e, "/worker"), Some(AgentEvent::StepStarted { .. }))
        })
        .await;
        assert_eq!(second.refreshes(), 0);
        let generation = plan_usage.bind("env", env.provider).unwrap();
        let error = used_up(now, Some(now + 3600));
        plan_usage.reached(
            "env",
            generation,
            error.detail.limit_reached.as_deref().unwrap(),
        );
        gate.add_permits(1);
        run.child_ended("/worker").await;
        run.child_ended("/worker").await;
        assert_eq!(second.refreshes(), 1);
        let sent = second.sent();
        assert_eq!(sent[0].session.model, expected);
        if let Some(effort) = landed_effort {
            assert_eq!(sent[0].session.effort, effort);
        }
        assert!(second.model_list().unwrap().find(old).is_some());
        run.quit().await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_move_refused_after_its_choice_sends_the_failed_notice() {
    let now = 1_800_000_000;
    let clock = Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None));
    let gate = Arc::new(Semaphore::new(0));
    let kernels = Kernels::default();
    let cache = crate::plan_usage::PlanUsageCache::default();
    let mut run = start(
        "guarantee-move-refused",
        Setup {
            kernels: kernels.clone(),
            endpoints: vec![subscription("first"), subscription("second")],
            clock: Some(clock),
            plan_usage: cache.clone(),
            preset_config: Some(MOVING),
            ..Setup::default()
        },
        vec![
            ("first:/worker", failing(&gate, now, now + 3600)),
            ("first:/", vec![done("noted")]),
        ],
    )
    .await;
    let moving = pause_at("/worker", "moving");
    run.spawn(SpawnRequest {
        preset: Some("worker".into()),
        ..spawn_request("worker", "Do it.")
    })
    .await;
    run.until("the child's step", |e| {
        matches!(inner(e, "/worker"), Some(AgentEvent::StepStarted { .. }))
    })
    .await;
    hold_limit(&run, &cache, "first", (now, now + 3600));
    gate.add_permits(1);
    within("the moving pause", moving.wait()).await;
    service_of(&kernels)
        .handle(AgentPath::root())
        .forget_endpoint("second", None);
    moving.wait().await;
    run.until("the root to wake", |e| {
        matches!(e, AgentEvent::TurnStarted { .. })
    })
    .await;
    let run = run.quit().await;
    let mail = run.mail_to("/");
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert_eq!((mail[0].0, mail[0].1), (MailKind::Notice, true));
    assert_eq!(
        wisp_protocol::mail_body(&mail[0].2),
        "/worker failed (rate_limit): first reached its usage limit, resets in 1h00m."
    );
    assert!(run.requests("second:/worker").is_empty());
    assert!(!run.seen.iter().any(|e| matches!(
        inner(e, "/worker"),
        Some(AgentEvent::SettingsChanged { .. })
    )));
}

#[tokio::test(start_paused = true)]
async fn a_stop_by_you_tells_the_parent_without_waking_it_and_a_cascade_says_with_its_parent() {
    let mut run = start(
        "guarantee-stopped",
        Setup::default(),
        vec![
            (
                "/lead",
                vec![
                    uses(&[spawn_call("s1", "helper", "Help.")]),
                    hanging("thinking"),
                    done("woken by the notice"),
                ],
            ),
            ("/lead/helper", vec![hanging("helping")]),
        ],
    )
    .await;
    run.spawn(spawn_request("lead", "Lead.")).await;
    run.until("the helper to stream", |e| {
        matches!(inner(e, "/lead/helper"), Some(AgentEvent::TextDelta { .. }))
    })
    .await;
    run.until("the lead to stream again", |e| {
        matches!(inner(e, "/lead"), Some(AgentEvent::TextDelta { text, .. }) if text == "thinking")
    })
    .await;
    run.send(ClientCommand::Child {
        agent: path("/lead"),
        command: Box::new(ClientCommand::Cancel),
    })
    .await;
    let lead = run.child_ended("/lead").await;
    assert!(matches!(lead, TurnOutcome::Cancelled { .. }), "{lead:?}");
    let helper = run.child_ended("/lead/helper").await;
    assert!(
        matches!(helper, TurnOutcome::Cancelled { .. }),
        "{helper:?}"
    );
    settled(&mut run, "/lead").await;
    tokio::time::sleep(Duration::from_secs(10)).await;
    let run = run.quit().await;
    let to_root = run.mail_to("/");
    assert_eq!(to_root.len(), 1, "{to_root:?}");
    assert_eq!((to_root[0].0, to_root[0].1), (MailKind::Notice, false));
    assert!(to_root[0].2.contains("/lead was stopped by the user."));
    let to_lead = run.mail_to("/lead");
    let notices: Vec<&(MailKind, bool, String)> = to_lead
        .iter()
        .filter(|(k, _, _)| *k == MailKind::Notice)
        .collect();
    assert_eq!(notices.len(), 1, "{to_lead:?}");
    assert!(!notices[0].1);
    assert!(
        notices[0]
            .2
            .contains("/lead/helper was stopped with its parent.")
    );
    assert!(
        !run.seen
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnStarted { .. })),
        "a stop wakes no one"
    );
    assert_eq!(
        run.requests("/lead").len(),
        2,
        "the lead's stop notice didn't wake it"
    );
}

#[tokio::test(start_paused = true)]
async fn a_child_that_only_ends_quietly_still_tells_its_parent() {
    let quiet = || Step::from(script(&[], StopReason::EndTurn));
    let typed = || Step::from(script(&[B::Text("[no text]")], StopReason::EndTurn));
    let line = "Building it in the background.";
    let wrote = uses(&[B::Text(line), call("c1", "nope", json!({}))]);
    let mut run = start(
        "guarantee-not-kept",
        Setup::default(),
        vec![
            ("/blank", vec![quiet(), quiet(), quiet()]),
            ("/w", vec![wrote, quiet(), typed(), quiet()]),
            ("/", vec![done("noted"), done("more"), done("again")]),
        ],
    )
    .await;
    for name in ["blank", "w"] {
        run.spawn(spawn_request(name, "Do it.")).await;
        run.child_ended(&format!("/{name}")).await;
        settled(&mut run, &format!("/{name}")).await;
    }
    let live = super::resume::list(&mut run).await;
    let run = run.quit().await;
    let mail: Vec<String> = run.mail_to("/").into_iter().map(|m| m.2).collect();
    assert_eq!(mail.len(), 2, "{mail:?}");
    assert!(mail[0].contains(crate::provider::NO_TEXT), "{}", mail[0]);
    assert!(mail[1].contains(line), "{}", mail[1]);
    let w = live.iter().find(|e| e.path.as_str() == "/w").unwrap();
    assert_eq!(w.last_report.as_deref(), Some(line));
    let records = run.store.read(&session_name("/w")).unwrap();
    let folded = crate::agents::fold::fold(&AgentPath::new("/w"), &records, &[]);
    assert_eq!(folded.last_report.as_deref(), Some(line));
}

#[tokio::test(start_paused = true)]
async fn a_valid_report_ends_the_turn_after_the_step_and_wakes_the_parent_with_it() {
    let mut run = start(
        "guarantee-reported",
        Setup::default(),
        vec![
            (
                "/planner",
                vec![
                    uses(&[
                        call(
                            "r1",
                            REPORT,
                            json!({"text": "\nPlanned it\nin detail: docs/plans/x.md"}),
                        ),
                        call(
                            "m1",
                            SEND_MESSAGE,
                            json!({"to": "..", "text": "fyi", "wake": false}),
                        ),
                    ]),
                    done("never sent"),
                ],
            ),
            ("/", vec![done("noted"), done("more")]),
        ],
    )
    .await;
    run.spawn(spawn_request("planner", "Plan it.")).await;
    let outcome = run.child_ended("/planner").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { steps: 1, .. }),
        "{outcome:?}"
    );
    assert_eq!(
        run.requests("/planner").len(),
        1,
        "no request after the report"
    );
    settled(&mut run, "/planner").await;
    assert_eq!(idle_outcome(&run, "/planner"), Some(IdleOutcome::Reported));
    run.until("the root to wake", |e| {
        matches!(
            e,
            AgentEvent::TurnStarted {
                origin: Some(wisp_protocol::TurnOrigin::Mail),
                ..
            }
        )
    })
    .await;
    run.root_ended().await;
    let request = run.request();
    run.send(ClientCommand::ListAgents { request }).await;
    let snapshot = run
        .until("the snapshot", |e| {
            matches!(e, AgentEvent::AgentsSnapshot { .. })
        })
        .await;
    let AgentEvent::AgentsSnapshot { agents, .. } = snapshot else {
        unreachable!()
    };
    assert_eq!(agents[0].last_report.as_deref(), Some("Planned it"));
    let woken = run.requests("/");
    let text = woken.last().unwrap().all_text();
    assert!(
        text.contains("<wisp:message from=\"/planner\" kind=\"report\">\n/planner reported · "),
        "{text}"
    );
    assert!(
        text.contains(" API\n\nPlanned it\nin detail: docs/plans/x.md\n</wisp:message>"),
        "{text}"
    );
    let reported = run.records("/planner");
    let logged = reported.iter().find_map(|e| match e {
        LogEvent::AgentReported { report, .. } => report.clone(),
        _ => None,
    });
    assert_eq!(
        logged,
        Some(json!("\nPlanned it\nin detail: docs/plans/x.md"))
    );
    let _ = run.quit().await;
}

#[test]
fn only_holds_that_lift_by_themselves_keep_a_parent_waiting() {
    use wisp_protocol::HoldReason;
    let held = |reason| AgentState::Held { reason };
    assert!(still_working(held(HoldReason::Concurrency)));
    for reason in [
        HoldReason::Stopped,
        HoldReason::RetryOffer,
        HoldReason::HeldMessages,
        HoldReason::Unknown,
    ] {
        assert!(!still_working(held(reason)), "{reason:?}");
    }
    assert!(still_working(AgentState::Running));
    assert!(still_working(AgentState::Idle {
        outcome: Some(IdleOutcome::Waiting)
    }));
    for outcome in [
        IdleOutcome::Failed,
        IdleOutcome::Asked,
        IdleOutcome::Stopped,
    ] {
        assert!(!still_working(AgentState::Idle {
            outcome: Some(outcome)
        }));
    }
}

#[tokio::test(start_paused = true)]
async fn a_coordinator_whose_child_failed_is_nudged_and_reports_no_report() {
    let mut run = start(
        "guarantee-child-failed",
        Setup::default(),
        vec![
            (
                "/lead",
                vec![
                    uses(&[spawn_call("s1", "worker", "Work.")]),
                    done("on it"),
                    done("it failed"),
                    done("still failed"),
                    done("really failed"),
                ],
            ),
            ("/lead/worker", vec![refused_request()]),
            ("/", vec![done("noted"), done("more")]),
        ],
    )
    .await;
    run.spawn(spawn_request("lead", "Lead.")).await;
    run.until("the lead's no_report", |e| {
        matches!(
            e,
            AgentEvent::AgentReported { agent, outcome: ReportOutcome::NoReport, .. }
                if agent.as_str() == "/lead"
        )
    })
    .await;
    let run = run.quit().await;
    assert!(
        run.mail_to("/lead")
            .iter()
            .any(|(kind, _, text)| *kind == MailKind::Notice
                && text.contains("/lead/worker failed (invalid_request).")),
        "{:?}",
        run.mail_to("/lead")
    );
    assert!(
        run.mail_to("/")
            .iter()
            .any(|(kind, _, text)| *kind == MailKind::Report
                && text.contains("/lead ended without a report")),
        "{:?}",
        run.mail_to("/")
    );
}

fn nudged(run: &Run, agent: &str) -> bool {
    run.requests(agent)
        .iter()
        .any(|r| r.last_user_texts().iter().any(|t| t.contains(NUDGE)))
}

fn reports_to(run: &Run, agent: &str) -> Vec<String> {
    run.mail_to(agent)
        .into_iter()
        .filter(|(kind, _, _)| *kind == MailKind::Report)
        .map(|(_, _, text)| text)
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_lead_whose_turn_ends_while_its_child_sends_its_report_waits_without_a_nudge() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "guarantee-report-window",
        Setup::default(),
        vec![
            (
                "/lead",
                vec![
                    uses(&[spawn_call("s1", "w", "Work.")]),
                    gated(done("spawned"), &gate),
                    uses(&[report_call("r1", "lead done")]),
                    done("extra"),
                ],
            ),
            ("/lead/w", vec![uses(&[report_call("r1", "w done")])]),
            (
                "/",
                vec![
                    uses(&[call("l1", "list_agents", json!({}))]),
                    done("listed"),
                    done("noted"),
                    done("noted"),
                ],
            ),
        ],
    )
    .await;
    let sending = pause_at("/lead/w", "mailing");
    run.spawn(spawn_request("lead", "Lead.")).await;
    within("the mailing pause", sending.wait()).await;
    gate.add_permits(1);
    run.child_ended("/lead").await;
    assert!(!nudged(&run, "/lead"), "{:#?}", run.requests("/lead"));
    settled(&mut run, "/lead").await;
    let waiting = Some((Some(IdleOutcome::Waiting), Activity::Idle));
    assert_eq!(last_idle(&run, "/lead"), waiting);
    run.say("List.").await;
    run.root_ended().await;
    within("the mailing pause", sending.wait()).await;
    run.child_ended("/lead").await;
    let run = run.quit().await;
    let (_, listed) = result_of(&run.requests("/")[1], "l1");
    assert!(listed.contains(" · idle: waiting · "), "{listed}");
    let reports = reports_to(&run, "/lead");
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert!(reports[0].contains("w done"), "{reports:?}");
    assert!(!nudged(&run, "/lead"));
}

#[tokio::test(start_paused = true)]
async fn a_report_that_lands_as_the_lead_checks_its_children_is_delivered_without_a_nudge() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "guarantee-nudge-window",
        Setup::default(),
        vec![
            (
                "/lead",
                vec![
                    uses(&[spawn_call("s1", "w", "Work.")]),
                    done("spawned"),
                    uses(&[report_call("r1", "lead done")]),
                    done("extra"),
                ],
            ),
            (
                "/lead/w",
                vec![gated(uses(&[report_call("r1", "w done")]), &gate)],
            ),
            ("/", vec![done("noted"); 2]),
        ],
    )
    .await;
    let checking = pause_at("/lead", "nudge");
    run.spawn(spawn_request("lead", "Lead.")).await;
    within("the nudge pause", checking.wait()).await;
    gate.add_permits(1);
    run.child_ended("/lead/w").await;
    settled(&mut run, "/lead/w").await;
    within("the nudge pause", checking.wait()).await;
    run.child_ended("/lead").await;
    run.child_ended("/lead").await;
    let requests = run.requests("/lead");
    assert!(!nudged(&run, "/lead"), "{requests:#?}");
    assert_eq!(requests.len(), 3, "{requests:#?}");
    assert!(
        requests[2]
            .last_user_texts()
            .iter()
            .any(|t| t.contains("w done")),
        "{requests:#?}"
    );
    let run = run.quit().await;
    assert_eq!(reports_to(&run, "/lead").len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_report_whose_lead_ended_while_it_was_sent_is_neither_mailed_nor_announced() {
    let mut run = start(
        "guarantee-report-refused",
        Setup::default(),
        vec![
            (
                "/lead",
                vec![uses(&[spawn_call("s1", "w", "Work.")]), hanging("waiting")],
            ),
            ("/lead/w", vec![uses(&[report_call("r1", "w done")])]),
            ("/", vec![done("noted"); 2]),
        ],
    )
    .await;
    let sending = pause_at("/lead/w", "mailing");
    run.spawn(spawn_request("lead", "Lead.")).await;
    within("the mailing pause", sending.wait()).await;
    run.send(ClientCommand::EndAgent {
        agent: path("/lead"),
    })
    .await;
    run.until("the worker to end", |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent, state: AgentState::Ended, .. }
                if agent.as_str() == "/lead/w"
        )
    })
    .await;
    within("the mailing pause", sending.wait()).await;
    let run = run.quit().await;
    assert!(
        !run.seen.iter().any(|e| matches!(
            e,
            AgentEvent::AgentReported { agent, .. } if agent.as_str() == "/lead/w"
        )),
        "{:#?}",
        run.seen
    );
    assert!(reports_to(&run, "/lead").is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_report_that_arrives_during_the_leads_last_step_wakes_it_without_a_nudge() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "guarantee-last-step",
        Setup::default(),
        vec![
            (
                "/lead",
                vec![
                    uses(&[spawn_call("s1", "w", "Work.")]),
                    gated(done("spawned"), &gate),
                    uses(&[report_call("r1", "lead done")]),
                    done("extra"),
                ],
            ),
            ("/lead/w", vec![uses(&[report_call("r1", "w done")])]),
            ("/", vec![done("noted"); 2]),
        ],
    )
    .await;
    run.spawn(spawn_request("lead", "Lead.")).await;
    run.child_ended("/lead/w").await;
    gate.add_permits(1);
    run.child_ended("/lead").await;
    settled(&mut run, "/lead").await;
    assert_eq!(idle_outcome(&run, "/lead"), Some(IdleOutcome::Waiting));
    run.child_ended("/lead").await;
    let requests = run.requests("/lead");
    assert!(!nudged(&run, "/lead"), "{requests:#?}");
    assert_eq!(requests.len(), 3, "{requests:#?}");
    assert!(
        requests[2]
            .last_user_texts()
            .iter()
            .any(|t| t.contains("w done")),
        "{requests:#?}"
    );
    let run = run.quit().await;
    assert_eq!(reports_to(&run, "/lead").len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_report_that_lands_while_the_leads_turn_closes_keeps_it_waiting() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "guarantee-ending-window",
        Setup::default(),
        vec![
            (
                "/lead",
                vec![
                    uses(&[spawn_call("s1", "w", "Work.")]),
                    done("spawned"),
                    uses(&[report_call("r1", "lead done")]),
                    done("extra"),
                ],
            ),
            (
                "/lead/w",
                vec![gated(uses(&[report_call("r1", "w done")]), &gate)],
            ),
            ("/", vec![done("noted"); 2]),
        ],
    )
    .await;
    let ending = pause_at("/lead", "ending");
    run.spawn(spawn_request("lead", "Lead.")).await;
    within("the ending pause", ending.wait()).await;
    gate.add_permits(1);
    run.child_ended("/lead/w").await;
    settled(&mut run, "/lead/w").await;
    within("the ending pause", ending.wait()).await;
    settled(&mut run, "/lead").await;
    assert_eq!(idle_outcome(&run, "/lead"), Some(IdleOutcome::Waiting));
    run.child_ended("/lead").await;
    run.child_ended("/lead").await;
    let run = run.quit().await;
    let to_root = reports_to(&run, "/");
    assert_eq!(to_root.len(), 1, "{to_root:?}");
    assert!(to_root[0].contains("lead done"), "{to_root:?}");
    assert_eq!(reports_to(&run, "/lead").len(), 1);
    assert!(!nudged(&run, "/lead"));
}

pub(super) fn background(id: &str, command: &str) -> B {
    call(
        id,
        "bash",
        json!({"command": command, "run_in_background": true}),
    )
}

pub(super) fn idle_as(agent: &'static str, outcome: IdleOutcome) -> impl Fn(&AgentEvent) -> bool {
    move |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged {
                agent: a,
                state: AgentState::Idle { outcome: o },
                ..
            } if a.as_str() == agent && *o == Some(outcome)
        )
    }
}

async fn taken(run: &mut Run, from: &'static str, kind: MailKind) {
    run.until("the root to take the mail", move |e| {
        matches!(
            e,
            AgentEvent::InputAccepted { from: Some(f), .. }
                if f.agent.as_str() == from && f.kind == kind
        )
    })
    .await;
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn only_a_process_started_for_the_current_task_defers_the_report() {
    let send = |id, text| call(id, SEND_MESSAGE, json!({"to": "quiet", "text": text}));
    let mut run = start(
        "guarantee-old-process",
        Setup {
            shell: true,
            ..Setup::default()
        },
        vec![
            (
                "/quiet",
                vec![
                    uses(&[background("b1", "sleep 30")]),
                    uses(&[report_call("r1", "server up")]),
                    done("first"),
                    done("second"),
                    done("last words"),
                    uses(&[background("b2", "sleep 30")]),
                    done("building"),
                    done("still building"),
                ],
            ),
            (
                "/",
                vec![
                    uses(&[send("m1", "Again.")]),
                    done("sent"),
                    uses(&[send("m2", "Build.")]),
                    done("sent too"),
                    uses(&[send("m3", "A new rule.")]),
                    done("sent the rule"),
                    done("noted"),
                ],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("quiet", "Serve.")).await;
    run.until("the no_report", idle_as("/quiet", IdleOutcome::NoReport))
        .await;
    assert_eq!(run.requests("/quiet").len(), 5, "two nudges, then stop");
    run.until("the build to wait", idle_as("/quiet", IdleOutcome::Waiting))
        .await;
    assert_eq!(run.requests("/quiet").len(), 7, "no nudge");
    run.until("the rule's wait", idle_as("/quiet", IdleOutcome::Waiting))
        .await;
    assert_eq!(run.requests("/quiet").len(), 8, "still no nudge");
    taken(&mut run, "/quiet", MailKind::Notice).await;
    taken(&mut run, "/quiet", MailKind::Notice).await;
    run.send(child("/quiet", ClientCommand::StopProcess { id: 1 }))
        .await;
    run.until("the old process's end", |e| {
        matches!(
            inner(e, "/quiet"),
            Some(AgentEvent::ProcessEnded { id: 1, .. })
        )
    })
    .await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(run.requests("/quiet").len(), 8, "an old process's stop");
    let run = run.quit().await;
    let mail = run.mail_to("/");
    assert_eq!(mail.len(), 4, "{mail:?}");
    for (notice, last) in [(&mail[2], "building"), (&mail[3], "still building")] {
        assert_eq!((notice.0, notice.1), (MailKind::Notice, true));
        assert!(
            notice.2.contains("/quiet waits for its process 2 to end")
                && notice
                    .2
                    .ends_with(&format!("follows.\n{last}\n</wisp:message>")),
            "{}",
            notice.2
        );
    }
    assert!(
        mail[1].2.contains("ended without a report (no_report"),
        "{}",
        mail[1].2
    );
    assert!(
        mail[1].2.ends_with("last words\n</wisp:message>"),
        "{}",
        mail[1].2
    );
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_child_with_the_process_notice_off_is_nudged_then_reports_no_report() {
    let mut run = start(
        "guarantee-notice-off",
        Setup {
            shell: true,
            flags: vec![hook_flag("shell", "processes", false)],
            ..Setup::default()
        },
        vec![
            (
                "/quiet",
                vec![
                    uses(&[background("b1", "sleep 30")]),
                    done("first"),
                    done("second"),
                    done("last words"),
                ],
            ),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(spawn_request("quiet", "Serve.")).await;
    run.until("the no_report", idle_as("/quiet", IdleOutcome::NoReport))
        .await;
    assert_eq!(run.requests("/quiet").len(), 4, "two nudges, then stop");
    let environment = &run.requests("/quiet")[0].first_user_texts()[0];
    assert!(
        !environment.contains("Background processes:"),
        "{environment}"
    );
    let run = run.quit().await;
    let mail = run.mail_to("/");
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert_eq!((mail[0].0, mail[0].1), (MailKind::Report, true));
    assert!(
        mail[0].2.contains("ended without a report (no_report"),
        "{}",
        mail[0].2
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_a_childs_settling_wake_tells_its_parent_it_stopped() {
    let mut run = start(
        "guarantee-settling-cancel",
        Setup {
            shell: true,
            ..Setup::default()
        },
        vec![
            (
                "/quiet",
                vec![
                    uses(&[background("b1", &wait_for("go"))]),
                    done("started"),
                    done("woke"),
                ],
            ),
            ("/", vec![done("noted"), done("noted too")]),
        ],
    )
    .await;
    run.spawn(spawn_request("quiet", "Serve.")).await;
    run.until("the wait", idle_as("/quiet", IdleOutcome::Waiting))
        .await;
    std::fs::write(run.env.dir("work").join("go"), "").unwrap();
    run.until("the process end", |e| {
        matches!(inner(e, "/quiet"), Some(AgentEvent::ProcessEnded { .. }))
    })
    .await;
    run.send(ClientCommand::Child {
        agent: path("/quiet"),
        command: Box::new(ClientCommand::Cancel),
    })
    .await;
    run.until("the stop", idle_as("/quiet", IdleOutcome::Stopped))
        .await;
    tokio::time::sleep(crate::agent::WAKE_SETTLE + Duration::from_millis(500)).await;
    let run = run.quit().await;
    assert_eq!(run.requests("/quiet").len(), 2, "no wake turn");
    let mail = run.mail_to("/");
    assert_eq!(mail.len(), 2, "{mail:?}");
    assert_eq!((mail[0].0, mail[0].1), (MailKind::Notice, true));
    assert!(mail[0].2.contains("/quiet waits for its process 1"));
    assert_eq!((mail[1].0, mail[1].1), (MailKind::Notice, false));
    assert!(
        mail[1].2.contains("/quiet was stopped by the user."),
        "{}",
        mail[1].2
    );
}

fn mail_from(run: &Run, to: &str, from: &str) -> Vec<(MailKind, bool, String)> {
    let from = format!("from=\"{from}\"");
    run.mail_to(to)
        .into_iter()
        .filter(|(_, _, text)| text.contains(&from))
        .collect()
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_wait_wakes_the_parent_once_per_task_naming_each_process_by_its_title() {
    let titled = json!({
        "command": hold(),
        "run_in_background": true,
        "description": "Run the placeholder tests",
    });
    let mut run = start(
        "guarantee-wait-notice",
        Setup {
            shell: true,
            ..Setup::default()
        },
        vec![
            (
                "/quiet",
                vec![
                    uses(&[
                        call("b1", "bash", titled),
                        background("b2", &format!("{} # placeholder-marker", hold())),
                    ]),
                    done("Placeholder last text"),
                    done("Placeholder user reply"),
                    done("Placeholder still waiting"),
                ],
            ),
            (
                "/",
                vec![
                    done("noted"),
                    uses(&[call("l1", "list_agents", json!({}))]),
                    uses(&[send_call("m1", "quiet", "Report now.")]),
                    done("sent"),
                    done("noted again"),
                ],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("quiet", "Test it.")).await;
    let waits = |e: &AgentEvent| {
        matches!(
            e,
            AgentEvent::AgentStateChanged {
                agent,
                activity: Activity::Waiting { processes },
                ..
            } if agent.as_str() == "/quiet" && **processes == [1, 2]
        )
    };
    taken(&mut run, "/quiet", MailKind::Notice).await;
    run.until("the waiting activity", waits).await;
    run.root_ended().await;
    run.send(ClientCommand::Child {
        agent: path("/quiet"),
        command: Box::new(ClientCommand::send("u1", "Placeholder user line")),
    })
    .await;
    run.child_ended("/quiet").await;
    run.child_ended("/quiet").await;
    run.until("the waiting activity again", waits).await;
    run.say("Ask quiet again.").await;
    taken(&mut run, "/quiet", MailKind::Notice).await;
    assert!(!nudged(&run, "/quiet"));
    let environment = &run.requests("/quiet")[0].first_user_texts()[0];
    assert!(
        environment.contains(wisp_tool_text::agents::BACKGROUND_LINE),
        "{environment}"
    );
    let run = run.quit().await;
    let (_, listed) = result_of(&run.requests("/")[2], "l1");
    assert!(
        listed.contains(" · idle: waiting on its processes 1, 2 · "),
        "{listed}"
    );
    let mail = run.mail_to("/");
    assert_eq!(mail.len(), 2, "{mail:?}");
    let named = "/quiet waits for its processes 1 (Run the placeholder tests) and 2 to end and will report then. If one of them won't end by itself, message /quiet to report now. Its last text follows.";
    for ((kind, wake, text), last) in mail
        .iter()
        .zip(["Placeholder last text", "Placeholder still waiting"])
    {
        assert_eq!((*kind, *wake), (MailKind::Notice, true));
        let body = format!("{named}\n{last}");
        assert_eq!(
            *text,
            crate::agent::render_mail(&path("/quiet"), MailKind::Notice, &body)
        );
        assert!(!text.contains("sleep") && !text.contains("placeholder-marker"));
    }
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_message_to_the_parent_during_a_wait_is_the_note_and_never_makes_the_child_asked() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "guarantee-own-note",
        Setup {
            shell: true,
            ..Setup::default()
        },
        vec![
            (
                "/quiet",
                vec![
                    uses(&[background("b1", &hold())]),
                    uses(&[quiet_call("q1", "..", "Placeholder quiet note")]),
                    uses(&[call("l1", "list_agents", json!({}))]),
                    done("Placeholder quiet end"),
                    uses(&[send_call("q2", "..", "Placeholder waking note")]),
                    done("Placeholder loud end"),
                    uses(&[send_call("q3", "../peer", "Placeholder question?")]),
                    done("Placeholder asked end"),
                ],
            ),
            (
                "/peer",
                vec![
                    uses(&[report_call("r1", "peer done")]),
                    gated(done("Placeholder no answer"), &gate),
                ],
            ),
            (
                "/",
                vec![
                    done("noted"),
                    uses(&[send_call("m1", "quiet", "Placeholder follow-up")]),
                    done("sent"),
                    done("noted"),
                    done("noted"),
                ],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("peer", "Placeholder.")).await;
    run.root_ended().await;
    run.spawn(spawn_request("quiet", "Test it.")).await;
    run.until("the quiet wait", idle_as("/quiet", IdleOutcome::Waiting))
        .await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(run.requests("/").len(), 1, "a quiet note wakes no one");
    run.say("Follow up.").await;
    run.until("the loud wait", idle_as("/quiet", IdleOutcome::Waiting))
        .await;
    taken(&mut run, "/quiet", MailKind::Message).await;
    taken(&mut run, "/quiet", MailKind::Message).await;
    run.send(ClientCommand::Child {
        agent: path("/quiet"),
        command: Box::new(ClientCommand::send("u1", "Ask the peer.")),
    })
    .await;
    run.until("the asking wait", idle_as("/quiet", IdleOutcome::Waiting))
        .await;
    gate.add_permits(1);
    run.child_ended("/peer").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(run.requests("/quiet").len(), 8, "no unanswered turn");
    let run = run.quit().await;
    let mail: Vec<(MailKind, bool)> = mail_from(&run, "/", "/quiet")
        .into_iter()
        .map(|(kind, wake, _)| (kind, wake))
        .collect();
    assert_eq!(
        mail,
        [(MailKind::Message, false), (MailKind::Message, true)]
    );
    assert!(notices(&run, "/quiet").is_empty());
    assert!(
        !run.seen
            .iter()
            .any(|e| idle_as("/quiet", IdleOutcome::Asked)(e))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_process_that_ends_after_the_notice_wakes_the_child_to_report() {
    let mut run = start(
        "guarantee-wait-ends",
        Setup {
            shell: true,
            ..Setup::default()
        },
        vec![
            (
                "/quiet",
                vec![
                    uses(&[background("b1", &wait_for("go"))]),
                    done("started"),
                    uses(&[report_call("r1", "Placeholder result")]),
                ],
            ),
            ("/", vec![done("noted"), done("noted the report")]),
        ],
    )
    .await;
    run.spawn(spawn_request("quiet", "Test it.")).await;
    taken(&mut run, "/quiet", MailKind::Notice).await;
    std::fs::write(run.env.dir("work").join("go"), "").unwrap();
    taken(&mut run, "/quiet", MailKind::Report).await;
    let run = run.quit().await;
    let mail = run.mail_to("/");
    let kinds: Vec<(MailKind, bool)> = mail.iter().map(|(k, w, _)| (*k, *w)).collect();
    assert_eq!(kinds, [(MailKind::Notice, true), (MailKind::Report, true)]);
    assert!(mail[0].2.contains("/quiet waits for its process 1 to end"));
    assert!(mail[1].2.contains("Placeholder result"), "{}", mail[1].2);
}

#[cfg(unix)]
#[tokio::test]
async fn a_process_end_in_the_reporting_step_wakes_the_child_after_the_turn() {
    let command = wait_for("go");
    let mut run = start(
        "guarantee-report-carries-end",
        Setup {
            shell: true,
            ..Setup::default()
        },
        vec![
            (
                "/quiet",
                vec![
                    uses(&[background("b1", &command)]),
                    uses(&[
                        report_call("r1", "Placeholder report"),
                        call("p1", "process", json!({"id": 1, "wait_ms": 10000})),
                    ]),
                    done("Placeholder wake reply"),
                ],
            ),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(spawn_request("quiet", "Test it.")).await;
    run.until("the process wait to start", |e| {
        matches!(inner(e, "/quiet"), Some(AgentEvent::ToolCallStarted { call_id, .. }) if call_id == "p1")
    })
    .await;
    std::fs::write(run.env.dir("work").join("go"), "").unwrap();
    let outcome = run.child_ended("/quiet").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { steps: 2, .. }),
        "{outcome:?}"
    );
    assert_eq!(
        run.requests("/quiet").len(),
        2,
        "no request after the report"
    );
    let ended = std::time::Instant::now();
    taken(&mut run, "/quiet", MailKind::Report).await;
    run.until("the child to wake on the end", |e| {
        matches!(
            inner(e, "/quiet"),
            Some(AgentEvent::TurnStarted {
                origin: Some(wisp_protocol::TurnOrigin::ProcessExit),
                ..
            })
        )
    })
    .await;
    assert!(ended.elapsed() >= agent::WAKE_SETTLE - Duration::from_millis(100));
    run.child_ended("/quiet").await;
    let run = run.quit().await;
    assert_eq!(run.mail_to("/").len(), 1);
    let requests = run.requests("/quiet");
    assert_eq!(requests.len(), 3);
    let woken = requests[2].last_user_texts();
    let process = wisp_tool_text::jobs::numbered(1, Some(&command));
    assert_eq!(
        woken.last().unwrap(),
        &format!(
            "<processes>\n{}\nProcess {process} exited with code 0.\n</processes>",
            wisp_tool_text::jobs::WAKE_LINE
        )
    );
    assert!(
        result_of(&requests[2], "p1")
            .1
            .starts_with("Process 1 exited with code 0.")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_users_stop_of_an_awaited_process_wakes_the_child_to_report() {
    let command = hold();
    let mut run = start(
        "guarantee-wait-stopped",
        Setup {
            shell: true,
            ..Setup::default()
        },
        vec![
            (
                "/quiet",
                vec![
                    uses(&[background("b1", &command)]),
                    done("started"),
                    uses(&[report_call("r1", "Placeholder result")]),
                ],
            ),
            ("/", vec![done("noted"), done("noted the report")]),
        ],
    )
    .await;
    run.spawn(spawn_request("quiet", "Test it.")).await;
    taken(&mut run, "/quiet", MailKind::Notice).await;
    run.send(child("/quiet", ClientCommand::StopProcess { id: 1 }))
        .await;
    taken(&mut run, "/quiet", MailKind::Report).await;
    let run = run.quit().await;
    let woken = run.requests("/quiet").last().unwrap().last_user_texts();
    let woken = woken.join("\n");
    let wake = woken.find(wisp_tool_text::jobs::WAKE_LINE).unwrap();
    let stopped = woken.find(&format!("Process 1 ({command}) was stopped by the user."));
    assert!(stopped.is_some_and(|stopped| wake < stopped), "{woken}");
    assert!(run.mail_to("/")[1].2.contains("Placeholder result"));
}

#[cfg(unix)]
#[tokio::test]
async fn a_driven_end_of_an_awaited_terminal_wakes_the_child_to_report() {
    if !crate::test_guard::has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let kernels = Kernels::default();
    let terminal = call(
        "b1",
        "bash",
        json!({"command": "exec cat", "tty": true, "run_in_background": true, "timeout_ms": 60000}),
    );
    let mut run = start(
        "guarantee-wait-driven",
        Setup {
            shell: true,
            kernels: kernels.clone(),
            ..Setup::default()
        },
        vec![
            (
                "/quiet",
                vec![
                    uses(&[terminal]),
                    done("started"),
                    uses(&[report_call("r1", "Placeholder result")]),
                ],
            ),
            ("/", vec![done("noted"), done("noted the report")]),
        ],
    )
    .await;
    run.spawn(spawn_request("quiet", "Test it.")).await;
    taken(&mut run, "/quiet", MailKind::Notice).await;
    let handle = service_of(&kernels).tree().entries[&path("/quiet")]
        .processes
        .handle(1)
        .unwrap();
    assert!(handle.state.live().deadline.is_some());
    let model_size = handle.state.terminal().unwrap().size();
    let attach = ClientCommand::AttachProcess {
        id: 1,
        size: Some(wisp_protocol::TermSize {
            cols: 100,
            rows: 30,
        }),
    };
    run.send(child("/quiet", attach.clone())).await;
    run.until("the child's attach", |event| {
        matches!(
            inner(event, "/quiet"),
            Some(AgentEvent::ProcessAttached { .. })
        )
    })
    .await;
    assert!(handle.state.live().deadline.is_none());
    assert!(
        handle
            .state
            .model_apply(
                handle.state.terminal().unwrap(),
                crate::jobs::state::ModelChange::default()
            )
            .is_none()
    );
    run.send(child(
        "/quiet",
        ClientCommand::ProcessInput {
            id: 1,
            items: AttachItems(vec![AttachInput::Text {
                text: "placeholder\n".into(),
            }]),
        },
    ))
    .await;
    run.send(ClientCommand::WindowGone).await;
    run.send(ClientCommand::WindowGone).await;
    run.until("the child's window detach", |event| matches!(inner(event, "/quiet"), Some(AgentEvent::ProcessDetached { id: 1, reason: wisp_protocol::DetachReason::Disconnected, typed: true, size }) if *size == model_size)).await;
    assert!(handle.state.is_running());
    assert!(handle.state.live().deadline.is_some());
    assert!(
        handle
            .state
            .model_apply(
                handle.state.terminal().unwrap(),
                crate::jobs::state::ModelChange::default()
            )
            .is_some()
    );
    run.send(child("/quiet", attach)).await;
    let key = AttachKey::new(AttachKeyCode::Char { char: 'd' }, false, false, true).unwrap();
    let items = AttachItems(vec![AttachInput::Key { key }]);
    run.send(child(
        "/quiet",
        ClientCommand::ProcessInput { id: 1, items },
    ))
    .await;
    taken(&mut run, "/quiet", MailKind::Report).await;
    let run = run.quit().await;
    assert!(run.mail_to("/")[1].2.contains("Placeholder result"));
    assert_eq!(
        run.records("/quiet")
            .iter()
            .filter(|event| matches!(
                event,
                LogEvent::ProcessDetached {
                    reason: wisp_protocol::DetachReason::Disconnected,
                    typed: true,
                    ..
                }
            ))
            .count(),
        1
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_session_describes_programs_from_earlier_turns_and_children() {
    let mut run = start(
        "describe-programs",
        Setup {
            shell: true,
            ..Setup::default()
        },
        vec![
            (
                "/",
                vec![
                    uses(&[background("b1", &hold())]),
                    done("up"),
                    done("noted"),
                ],
            ),
            (
                "/worker",
                vec![
                    uses(&[background("b2", &hold())]),
                    uses(&[report_call("r1", "up")]),
                ],
            ),
        ],
    )
    .await;
    run.say("start").await;
    let main = run
        .until("the main program", |e| {
            matches!(e, AgentEvent::ProcessStarted { id: 1, .. })
        })
        .await;
    run.root_ended().await;
    assert!(run.describe().await.1.contains(&main));
    run.spawn(spawn_request("worker", "serve")).await;
    let child = run
        .until("the child's program", |e| {
            matches!(inner(e, "/worker"), Some(AgentEvent::ProcessStarted { .. }))
        })
        .await;
    run.child_ended("/worker").await;
    run.root_ended().await;
    assert!(run.describe().await.1.contains(&child));
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_guard_moves_a_child_to_another_model_on_the_same_account() {
    let now = 1_800_000_000;
    let clock = Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None));
    let gate = Arc::new(Semaphore::new(0));
    let cache = crate::plan_usage::PlanUsageCache::default();
    let error = guarded(now, "claude-fable-5-2");
    let mut run = start("example-model-guard", Setup {
        endpoints: vec![subscription("example")], clock: Some(clock), plan_usage: cache.clone(),
        preset_config: Some("[agents.presets.work]\ndescription = 'Works'\nendpoint = 'example'\nmodel = 'claude-fable-5-2'\neffort = 'high'\nfallback = [{endpoint = 'example', model = 'claude-opus-5-5', effort = 'high'}]\n"),
        ..Setup::default()
    }, vec![("example:/x", vec![gated(Step::from(vec![Ok(ProviderEvent::Sending), Err(error.clone())]), &gate), uses(&[report_call("r1", "example report")])]), ("example:/", vec![done("example note")])]).await;
    run.spawn(SpawnRequest {
        preset: Some("work".into()),
        ..spawn_request("x", "Do it.")
    })
    .await;
    run.until("the child's step", |e| {
        matches!(inner(e, "/x"), Some(AgentEvent::StepStarted { .. }))
    })
    .await;
    let generation = cache
        .bind("example", run.providers.get("example:/"))
        .unwrap();
    cache.reached(
        "example",
        generation,
        error.detail.limit_reached.as_deref().unwrap(),
    );
    gate.add_permits(1);
    run.child_ended("/x").await;
    run.until("retry on the same account", |e| {
        matches!(
            inner(e, "/x"),
            Some(AgentEvent::TurnStarted {
                retry_of: Some(1),
                ..
            })
        )
    })
    .await;
    assert!(matches!(
        run.child_ended("/x").await,
        TurnOutcome::Completed { .. }
    ));
    let run = run.quit().await;
    let sent = run.requests("example:/x");
    assert_eq!(
        (
            sent[0].session.model.as_str(),
            sent[1].session.model.as_str()
        ),
        ("claude-fable-5-2", "claude-opus-5-5")
    );
    let notes: Vec<_> = run
        .mail_to("/")
        .into_iter()
        .filter(|n| n.0 == MailKind::Notice)
        .collect();
    assert_eq!(notes.len(), 1);
    let note = wisp_protocol::mail_body(&notes[0].2);
    assert!(
        note.starts_with("moved to example with claude-opus-5-5 high"),
        "{note}"
    );
    assert!(
        note.ends_with(": example refused a request for claude-fable-5-2, retry in 5d21h."),
        "{note}"
    );
}

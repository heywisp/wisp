use std::sync::Arc;

use serde_json::json;
use tokio::sync::Semaphore;
use wisp_protocol::log::{LogEvent, LogRecord};
use wisp_protocol::plan_usage::{PlanUsage, UsageAction};
use wisp_protocol::settings::{SettingChange, SettingValue};
use wisp_protocol::{
    AgentEvent, Billing, CacheEffect, ClientCommand, Dropped, Effort, ErrorKind, Message,
    RequestId, RetryClass, RewindPoint, RewindReason, StopReason, Usage,
};

use super::{INSTRUCTION, NAME_LIMIT, Naming, cleaned};
use crate::agent::keep_warm::tests::{
    Closed, Rig, answer, resumed, resumed_from, shifted, usage, warm,
};
use crate::agent::testing::{Caps, Step, Wait, ended, failure, started, text};
use crate::agent::{DEFAULT_MODEL, IMPLICIT_ENDPOINT};
use crate::plan_usage::{Display, PlanUsageCache};
use crate::provider::{ModelCapabilities, ProviderEvent};
use crate::session::MemoryStore;
use crate::test_guard::{log_events, records};

static HIGH_ONLY: ModelCapabilities = ModelCapabilities {
    efforts: &[Effort::High],
    default_effort: None,
    per_message_effort: false,
    effort_in_id: None,
    inline_tools: false,
    tools_after_results: false,
    deferred_tools: false,
    adaptive_thinking: false,
    thinking_budgets: None,
    max_output: None,
    images: None,
};

fn name_reply(name: &str) -> Step {
    let used = Usage {
        billing: Billing::AnthropicPlan,
        ..usage(30, 0, 0, 5)
    };
    let mut script = vec![
        Ok(ProviderEvent::Sending),
        Ok(started(Some("msg_name"), DEFAULT_MODEL, used)),
    ];
    script.extend(text(0, name));
    script.extend(ended(StopReason::EndTurn, used, Vec::new()));
    Step::from(script)
}

fn refused() -> Step {
    Step::from(vec![
        Ok(ProviderEvent::Sending),
        Err(failure(ErrorKind::InvalidRequest, RetryClass::Never, false)),
    ])
}

fn naming(steps: Vec<Step>) -> Rig {
    Rig::with(steps, |agent| agent.with_naming(true))
}

async fn named(rig: &mut Rig) -> Option<String> {
    let event = rig
        .until(|e| matches!(e, AgentEvent::SessionNamed { .. }))
        .await;
    let AgentEvent::SessionNamed { name, .. } = event else {
        unreachable!()
    };
    name
}

fn names(closed: &Closed) -> Vec<(Option<String>, Option<String>, bool)> {
    let named = |event| match event {
        LogEvent::SessionNamed {
            name,
            detail,
            lower_bound,
            ..
        } => Some((name, detail, lower_bound)),
        _ => None,
    };
    log_events(&closed.store)
        .into_iter()
        .filter_map(named)
        .collect()
}

fn back(closed: &Closed, steps: Vec<Step>) -> Rig {
    let provider = warm(steps);
    let clock = shifted();
    let agent = resumed(closed, &provider, &clock).with_naming(true);
    Rig::serve(agent, provider, closed.store.clone(), clock)
}

#[tokio::test(start_paused = true)]
async fn a_chat_is_named_after_its_third_sent_turn_from_those_texts_alone() {
    let (gate, landing) = (Arc::new(Semaphore::new(0)), Arc::new(Semaphore::new(0)));
    let steps = vec![
        answer("msg_1"),
        answer("msg_2").held(Wait::Gate(gate)),
        answer("msg_3"),
        answer("msg_4"),
        name_reply("\"Cache **fix**.\"").gated(&landing),
    ];
    let mut rig = Rig::with(steps, |agent| {
        agent
            .with_naming(true)
            .with_plan_usage(PlanUsageCache::default(), Display::Enabled)
    });
    let read = PlanUsage::new(None, Vec::new()).unwrap();
    rig.provider.knobs(|k| k.plan_usage = Some(read));
    rig.send(ClientCommand::PlanUsage {
        request: RequestId(100),
        endpoint: IMPLICIT_ENDPOINT.into(),
        action: UsageAction::Subscribe,
    })
    .await;
    rig.completed("first").await;
    rig.say("stopped").await;
    rig.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    rig.send(ClientCommand::Cancel).await;
    rig.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    rig.completed("second").await;
    rig.idle(60).await;
    assert_eq!(rig.provider.sent().len(), 3, "two sent turns name nothing");
    let third = format!("third {}", "x".repeat(4_000));
    rig.completed(&third).await;
    rig.idle(11).await;
    let looked_up = rig.provider.plan_usage_calls();
    landing.add_permits(1);
    assert_eq!(named(&mut rig).await.as_deref(), Some("Cache fix"));
    rig.idle(1).await;
    assert_eq!(rig.provider.plan_usage_calls(), looked_up + 1);
    let closed = rig.close().await;
    let sent = closed.provider.sent();
    let ask = &sent[4];
    assert_eq!(ask.session.system.as_deref(), Some(INSTRUCTION));
    assert_eq!(
        (ask.session.effort, ask.session.max_tokens),
        (Effort::Low, 1_024)
    );
    assert!(ask.tools.is_empty() && ask.server_tools.is_empty());
    assert!(ask.previous_response_id.is_none() && !ask.options.refusal_fallback);
    let opening = format!("first\n\nsecond\n\n{third}");
    assert_eq!(ask.messages, [Message::user_text(&opening[..4_000])]);
    assert!(!ask.text().contains("cache_control"), "{}", ask.text());
    assert_eq!(names(&closed), [(Some("Cache fix".into()), None, false)]);
}

#[tokio::test(start_paused = true)]
async fn a_failed_name_is_tried_again_on_the_next_resume_and_a_made_one_never() {
    let mut rig = naming(vec![
        answer("msg_1"),
        answer("msg_2"),
        answer("msg_3"),
        refused(),
        answer("msg_4"),
    ]);
    for said in ["one", "two", "three"] {
        rig.completed(said).await;
    }
    assert_eq!(named(&mut rig).await, None);
    rig.completed("four").await;
    rig.idle(60).await;
    let closed = rig.close().await;
    assert_eq!(closed.provider.sent().len(), 5);
    assert_eq!(
        names(&closed),
        [(None, Some("invalid_request".into()), false)]
    );

    let mut again = back(&closed, vec![name_reply("Four steps")]);
    assert_eq!(named(&mut again).await.as_deref(), Some("Four steps"));
    let closed = again.close().await;
    let done = back(&closed, Vec::new());
    done.idle(60).await;
    assert!(done.close().await.provider.sent().is_empty());

    let records = records(&closed.store);
    let unnamed: Vec<LogRecord> = records
        .iter()
        .filter(|r| !matches!(r.event, LogEvent::SessionNamed { .. }))
        .zip(0..)
        .map(|(record, seq)| LogRecord {
            seq,
            ..record.clone()
        })
        .collect();
    let mut asked = Vec::new();
    for logged in [&records, &unnamed] {
        let provider = warm(vec![answer("msg_5")]);
        let (store, clock) = (MemoryStore::default(), shifted());
        let agent = resumed_from(logged, &store, &provider, &clock);
        let mut rig = Rig::serve(agent, provider, store, clock);
        rig.completed("five").await;
        asked.push(rig.close().await.provider.sent()[0].text());
    }
    assert_eq!(asked[0], asked[1]);
}

#[tokio::test(start_paused = true)]
async fn a_pending_switch_or_an_unsent_try_waits_for_the_next_turn() {
    let gate = Arc::new(Semaphore::new(0));
    let unsent = Step::from(vec![Err(failure(
        ErrorKind::Network,
        RetryClass::Transient,
        false,
    ))]);
    let mut rig = naming(vec![
        answer("msg_1"),
        answer("msg_2"),
        answer("msg_3").held(Wait::Gate(gate.clone())),
        answer("msg_4"),
        unsent,
        answer("msg_5"),
        name_reply("Late name"),
    ]);
    rig.completed("one").await;
    rig.completed("two").await;
    rig.say("three").await;
    rig.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    rig.send(ClientCommand::ChangeSettings {
        request: RequestId(1),
        changes: vec![SettingChange::named("effort", SettingValue::Text("high".into())).unwrap()],
        save: false,
    })
    .await;
    gate.add_permits(1);
    rig.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    rig.provider
        .knobs(|k| k.caps = Some(Caps::Fixed(Some(&HIGH_ONLY))));
    rig.idle(60).await;
    assert_eq!(rig.provider.sent().len(), 3, "the switch is still pending");
    rig.completed("four").await;
    rig.idle(60).await;
    assert_eq!(rig.provider.sent().len(), 5, "the unsent try waits");
    rig.completed("five").await;
    assert_eq!(named(&mut rig).await.as_deref(), Some("Late name"));
    let closed = rig.close().await;
    assert_eq!(names(&closed), [(Some("Late name".into()), None, false)]);
    let ask = &closed.provider.sent()[6];
    assert_eq!(ask.messages, [Message::user_text("one\n\ntwo\n\nthree")]);
    assert_eq!(ask.session.effort, Effort::High);
}

#[tokio::test(start_paused = true)]
async fn session_end_records_a_landed_name_and_a_sent_unfinished_one_as_a_lower_bound() {
    let opening = || vec![answer("msg_1"), answer("msg_2"), answer("msg_3")];
    let mut cases = Vec::new();
    for (sending, release, streaming) in [
        (true, true, false),
        (true, false, false),
        (false, false, false),
        (true, false, true),
    ] {
        let gate = Arc::new(Semaphore::new(0));
        let reply = name_reply("Landed");
        let mut steps = opening();
        steps.push(if streaming {
            let observed = gate.clone();
            Step::from(vec![
                Ok(ProviderEvent::Sending),
                Ok(ProviderEvent::Unknown {
                    provider: "anthropic".into(),
                    raw: json!({"type":"future_name_event"}),
                }),
            ])
            .run(move || observed.add_permits(1))
            .hanging()
        } else if sending {
            reply.gated(&gate)
        } else {
            reply.held(Wait::Gate(gate.clone()))
        });
        let mut rig = naming(steps);
        for said in ["one", "two", "three"] {
            rig.completed(said).await;
        }
        rig.idle(1).await;
        if release {
            gate.add_permits(1);
        } else if streaming {
            gate.acquire().await.unwrap().forget();
        }
        let closed = rig.close().await;
        let rows = log_events(&closed.store);
        assert_eq!(rows.iter().filter(|event| matches!(event, LogEvent::UnknownProviderEvent { event, .. } if event["type"] == "future_name_event")).count(), usize::from(streaming));
        let back = resumed(&closed, &warm(Vec::new()), &shifted());
        let spend = |a: &crate::agent::Agent| {
            (
                a.session_usd,
                a.accounts.clone(),
                a.session_lower_bound,
                a.session_billing,
            )
        };
        assert_eq!(spend(&back), spend(&closed.agent));
        let usage = log_events(&closed.store).into_iter().find_map(|e| match e {
            LogEvent::SessionNamed { usage, .. } => usage,
            _ => None,
        });
        cases.push((
            names(&closed),
            usage.is_some(),
            closed.agent.session_lower_bound,
        ));
    }
    assert_eq!(
        cases,
        [
            (vec![(Some("Landed".into()), None, false)], true, false),
            (vec![(None, Some("interrupted".into()), true)], false, true),
            (Vec::new(), false, false),
            (vec![(None, Some("interrupted".into()), true)], false, true),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn naming_leaves_the_next_turns_request_unchanged() {
    let (mut runs, mut counts) = (Vec::new(), Vec::new());
    let cases = [
        (None, false),
        (Some(name_reply("Cache fix")), true),
        (Some(refused()), true),
        (None, true),
    ];
    for (name, on) in cases {
        let logged = !on || name.is_some();
        let mut steps = vec![answer("msg_1"), answer("msg_2"), answer("msg_3")];
        steps.extend(name);
        steps.push(answer("msg_4"));
        let mut rig = Rig::with(steps, |mut agent| {
            agent.log = agent.log.filter(|_| logged);
            agent.with_naming(on)
        });
        for said in ["one", "two", "three"] {
            rig.completed(said).await;
        }
        if on && logged {
            named(&mut rig).await;
        }
        rig.completed("four").await;
        let sent = rig.close().await.provider.sent();
        counts.push(sent.len());
        let turns = sent
            .iter()
            .filter(|s| s.session.system.as_deref() != Some(INSTRUCTION));
        runs.push(turns.map(|s| s.text()).collect::<Vec<_>>());
    }
    assert_eq!(counts, [4, 5, 5, 4]);
    assert_eq!(runs[1], runs[0]);
    assert_eq!(runs[2], runs[0]);
}

#[test]
fn a_reply_is_cleaned_to_one_masked_line_within_the_limit() {
    assert_eq!(
        cleaned("\n  \"**Cache** `fix`.\"  \nmore").as_deref(),
        Some("Cache fix")
    );
    assert_eq!(cleaned("# 'Rust   build'").as_deref(), Some("Rust build"));
    assert_eq!(cleaned(" \n\n"), None);
    assert_eq!(cleaned("**``**"), None);
    for name in [
        "C# async fix",
        "Issue #42 crash",
        "A* search bug",
        "Async in C#",
    ] {
        assert_eq!(cleaned(name).as_deref(), Some(name));
    }
    assert_eq!(cleaned("```\nCache fix\n```").as_deref(), Some("Cache fix"));
    assert_eq!(cleaned("\"\nCache fix").as_deref(), Some("Cache fix"));
    let key = format!("sk-ant-api03-{}", "a1".repeat(30));
    assert_eq!(
        cleaned(&format!("Fix {key}")).as_deref(),
        Some("Fix sk-ant-api03-\u{2026}")
    );
    let whole = format!("{}end", "word ".repeat(39));
    assert_eq!(cleaned(&whole), Some(whole.clone()));
    let long = cleaned(&"word ".repeat(60)).unwrap();
    assert_eq!(long.chars().count(), NAME_LIMIT);
    let runaway = format!("{}{}", "y ".repeat(90), "x9".repeat(20));
    assert_eq!(
        cleaned(&runaway),
        Some(format!("{}\u{2026}", "y ".repeat(90)))
    );
}

fn turn(turn: u64, text: &str) -> LogEvent {
    serde_json::from_value(json!({"type": "turn_started", "turn": turn, "text": text})).unwrap()
}

fn marked(turn: u64, texts: &[&str]) -> LogEvent {
    let inputs: Vec<_> = texts
        .iter()
        .map(|t| json!({"id": t, "text": t, "note": "sent while working"}))
        .collect();
    serde_json::from_value(
        json!({"type": "turn_started", "turn": turn, "text": "", "inputs": inputs}),
    )
    .unwrap()
}

fn step(turn: u64, accepted: bool) -> LogEvent {
    serde_json::from_value(json!({
        "type": "step_ended", "turn": turn, "step": 1, "usage": null, "cost_usd": null,
        "lower_bound": false, "prompt_tokens": null, "response_id": null, "accepted": accepted,
    }))
    .unwrap()
}

fn rewound(turns: Vec<u64>) -> LogEvent {
    LogEvent::Rewound {
        to: 1,
        point: RewindPoint::Turn { turn: turns[0] },
        reason: RewindReason::User,
        messages: 2,
        blocks: 1,
        dropped: Dropped { turns, messages: 2 },
        cache: CacheEffect::Kept,
    }
}

#[test]
fn a_rewound_unsent_or_multi_input_turn_gives_no_opening_text() {
    let mut naming = Naming::default();
    for event in [
        turn(1, ""),
        step(1, true),
        turn(2, "two"),
        step(2, true),
        turn(3, "three"),
        step(3, true),
        rewound(vec![3]),
        turn(4, "four"),
        step(4, false),
        turn(5, "five"),
        rewound(vec![5]),
        step(5, true),
        marked(6, &["six"]),
        step(6, true),
        marked(7, &["seven", "and more"]),
        step(7, true),
    ] {
        naming.see(&event);
    }
    assert_eq!(
        naming.opening,
        [(2, "two".to_owned()), (6, "six".to_owned())]
    );
}

#[tokio::test(start_paused = true)]
async fn a_name_keeps_unknown_events_and_an_incomplete_completion_is_a_lower_bound() {
    for incomplete in [false, true] {
        let mut reply = name_reply("Side name");
        reply.script.insert(
            1,
            Ok(ProviderEvent::Unknown {
                provider: "anthropic".into(),
                raw: json!({"type":"future_name_event"}),
            }),
        );
        if incomplete {
            reply
                .script
                .retain(|event| !matches!(event, Ok(ProviderEvent::MessageDelta { .. })));
        }
        let mut rig = naming(vec![
            answer("msg_1"),
            answer("msg_2"),
            answer("msg_3"),
            reply,
        ]);
        for text in ["first", "second", "third"] {
            rig.completed(text).await;
        }
        assert_eq!(named(&mut rig).await.is_none(), incomplete);
        let closed = rig.close().await;
        let rows = log_events(&closed.store);
        assert!(rows.iter().any(|event| matches!(event, LogEvent::UnknownProviderEvent { event, .. } if event["type"] == "future_name_event")));
        assert_eq!(names(&closed)[0].2, incomplete);
    }
}

#[tokio::test(start_paused = true)]
async fn a_rename_during_a_name_request_keeps_its_name() {
    use crate::plugin::Make;
    use wisp_protocol::{CommandOutcome, Secret};
    use wisp_tool_text::chat::{COMMAND, renamed};
    let gate = Arc::new(Semaphore::new(0));
    let bundled = wisp_plugin_chat::bundled();
    let make: Make = Arc::new(move |host| (bundled.instantiate)(host));
    let mut rig = Rig::with(
        vec![
            answer("msg_1"),
            answer("msg_2"),
            answer("msg_3"),
            name_reply("Automatic").gated(&gate),
        ],
        |agent| {
            crate::plugin::testing::with_test_hooks(
                agent.with_naming(true),
                vec![(bundled.manifest.to_owned(), make)],
            )
        },
    );
    for said in ["one", "two", "three"] {
        rig.completed(said).await;
    }
    rig.idle(1).await;
    let sent = rig.provider.sent().len();
    rig.send(ClientCommand::RunPluginCommand {
        name: format!("chat:{COMMAND}"),
        args: Secret::new("\"Strada\""),
        request: RequestId(41),
    })
    .await;
    assert!(
        matches!(rig.until(|event| matches!(event, AgentEvent::SessionNamed { .. } | AgentEvent::CommandDone { .. })).await,
        AgentEvent::SessionNamed { name: Some(name), cost: None } if name == "Strada")
    );
    assert_eq!(
        rig.until(|event| matches!(event, AgentEvent::CommandDone { .. }))
            .await,
        AgentEvent::CommandDone {
            answers: RequestId(41),
            outcome: CommandOutcome::Note {
                text: renamed("Strada")
            }
        }
    );
    assert_eq!(rig.provider.sent().len(), sent);
    gate.add_permits(1);
    assert_eq!(named(&mut rig).await, None);
    let closed = rig.close().await;
    assert_eq!(
        names(&closed),
        [
            (Some("Strada".to_owned()), None, false),
            (None, Some("renamed".to_owned()), false),
        ]
    );
    let usage: Vec<bool> = log_events(&closed.store)
        .into_iter()
        .filter_map(|event| match event {
            LogEvent::SessionNamed { usage, .. } => Some(usage.is_some()),
            _ => None,
        })
        .collect();
    assert_eq!(usage, [false, true]);
    let again = back(&closed, Vec::new());
    again.idle(60).await;
    let resumed = again.close().await;
    let spend = |agent: &crate::agent::Agent| {
        (
            agent.session_usd,
            agent.accounts.clone(),
            agent.session_lower_bound,
            agent.session_billing,
        )
    };
    assert_eq!(spend(&resumed.agent), spend(&closed.agent));
    assert!(resumed.provider.sent().is_empty());
}

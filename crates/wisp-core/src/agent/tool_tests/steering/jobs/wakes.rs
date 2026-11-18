use std::sync::Mutex as StdMutex;

use wisp_protocol::log::{LEAD_PROCESSES, LogEvent, LogRecord};
use wisp_protocol::{ExitStatus, ReportedEnd, RequestId, Role, TurnOrigin};

use super::*;
use crate::agent::WAKE_SETTLE;
use crate::agent::retry::RetryPolicy;
use wisp_tool_text::jobs::WAKE_LINE;

mod repaired;
mod replayed;
mod worded;

const SILENCE: Duration = Duration::from_millis(3500);
const SUMMARY: &str = "So far: a process was started.";

fn woken(lines: &[&str]) -> String {
    format!(
        "<processes>\n{WAKE_LINE}\n{}\n</processes>",
        lines.join("\n")
    )
}

fn reported(events: &[AgentEvent]) -> Vec<Vec<ReportedEnd>> {
    pick!(events, AgentEvent::TurnStarted { ends, .. } => ends.clone())
}

fn exited(id: u32, code: i32) -> ReportedEnd {
    ReportedEnd {
        id,
        end: ProcessEnd::Exited {
            exit: ExitStatus::Code { code },
        },
    }
}

fn starts(events: &[AgentEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .count()
}

fn opened(records: &[LogRecord]) -> Vec<(u64, Option<TurnOrigin>, Option<String>, String)> {
    records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::TurnStarted {
                turn,
                origin,
                lead,
                text,
                ..
            } => Some((
                *turn,
                *origin,
                lead.text(LEAD_PROCESSES).map(str::to_owned),
                text.clone(),
            )),
            _ => None,
        })
        .collect()
}

fn sent(message: &Message) -> Vec<String> {
    assert_eq!(message.role, Role::User);
    message
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.clone(),
            other => format!("{other:?}"),
        })
        .collect()
}

fn last_sent(h: &Harness) -> Vec<String> {
    let requests = h.requests();
    sent(requests.last().unwrap().last().unwrap())
}

async fn quiet(h: &mut Harness) -> Vec<AgentEvent> {
    h.drive(Vec::new(), nothing, never, SILENCE).await
}

async fn woke(h: &mut Harness) -> Vec<AgentEvent> {
    let events = h.drive(Vec::new(), nothing, turns(1), PATIENT).await;
    assert_eq!(wakes(&events), 1, "{events:#?}");
    events
}

fn native_summary() -> Step {
    let mut script = vec![Ok(ProviderEvent::Sending), Ok(start_on(DEFAULT_MODEL, 600))];
    script.extend(opaque(0, json!({"type": "compaction", "content": SUMMARY})));
    script.extend(
        reply(&[], StopReason::Compaction)
            .script
            .into_iter()
            .skip(2),
    );
    Step::from(script)
}

#[tokio::test]
async fn a_failed_turn_wakes_on_a_later_end_once_its_held_message_is_withdrawn() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "wake-after-held",
        vec![
            uses(&[background("a", &a.command)]).then(queue("q1")),
            unoffered(),
            unoffered(),
        ],
        true,
    );
    let failed = h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    assert!(matches!(
        outcomes(&failed)[..],
        [TurnOutcome::Error { offer: None, .. }]
    ));
    assert_eq!(held(&failed), [(ids(&["q1"]), WithdrawnBy::TurnFailed)]);
    let waiting = quiet(&mut h).await;
    assert!(process_ended(1)(&waiting), "{waiting:#?}");
    assert_eq!(starts(&waiting), 0, "{waiting:#?}");
    let withdrawn = h
        .drive(
            vec![ClientCommand::Withdraw { ids: ids(&["q1"]) }],
            nothing,
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(wakes(&withdrawn), 1, "{withdrawn:#?}");
    assert_eq!(reported(&withdrawn), [vec![exited(1, 2)]]);
    assert_eq!(last_sent(&h).last().unwrap(), &woken(&[&a.ended(1, 2)]));
    assert!(matches!(
        outcomes(&withdrawn)[..],
        [TurnOutcome::Error { offer: None, .. }]
    ));
    assert_eq!(starts(&quiet(&mut h).await), 0);
}

#[tokio::test]
async fn an_end_after_a_failed_turn_waits_while_its_retry_offer_stands() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "wake-after-offer",
        vec![
            uses(&[background("a", &a.command)]).then(queue("q1")),
            server_error_step(),
        ],
        true,
    );
    let failed = h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    assert!(matches!(
        outcomes(&failed)[..],
        [TurnOutcome::Error { offer: Some(_), .. }]
    ));
    let waiting = quiet(&mut h).await;
    assert!(process_ended(1)(&waiting), "{waiting:#?}");
    let withdrawn = h
        .drive(
            vec![ClientCommand::Withdraw { ids: ids(&["q1"]) }],
            nothing,
            never,
            SILENCE,
        )
        .await;
    assert_eq!(starts(&waiting) + starts(&withdrawn), 0, "{withdrawn:#?}");
}

#[tokio::test]
async fn an_end_after_a_failed_turn_wakes_once_a_compaction_lapses_its_offer() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "wake-after-lapse",
        vec![
            uses(&[background("a", &a.command)]),
            server_error_step(),
            native_summary(),
            done("woke"),
        ],
        false,
    );
    let failed = h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    assert!(matches!(
        outcomes(&failed)[..],
        [TurnOutcome::Error { offer: Some(_), .. }]
    ));
    let waiting = quiet(&mut h).await;
    assert!(process_ended(1)(&waiting), "{waiting:#?}");
    assert_eq!(starts(&waiting), 0, "{waiting:#?}");
    let compacted = h
        .drive(
            vec![ClientCommand::Compact {
                focus: None,
                request: RequestId(1),
            }],
            nothing,
            turns(1),
            PATIENT,
        )
        .await;
    assert!(
        compacted
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. })),
        "{compacted:#?}"
    );
    assert_eq!(wakes(&compacted), 1, "{compacted:#?}");
    assert_eq!(reported(&compacted), [vec![exited(1, 2)]]);
    assert_eq!(starts(&quiet(&mut h).await), 0);
}

#[tokio::test]
async fn an_end_that_does_not_wake_leaves_a_failed_wake_turns_ends_waiting() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "wake-after-stop",
        vec![
            uses(&[
                background("a", &a.command),
                background("b", "exec sleep 30"),
            ]),
            done("started"),
            unoffered(),
        ],
        false,
    );
    h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    let failed = woke(&mut h).await;
    assert!(matches!(
        outcomes(&failed)[..],
        [TurnOutcome::Error { offer: None, .. }]
    ));
    let stopped = h
        .drive(
            vec![ClientCommand::StopProcess { id: 2 }],
            nothing,
            never,
            SILENCE,
        )
        .await;
    assert!(process_ended(2)(&stopped), "{stopped:#?}");
    assert_eq!(starts(&stopped), 0, "{stopped:#?}");
}

#[tokio::test]
async fn an_exit_while_idle_starts_one_turn_after_the_settle_window() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "wake-idle",
        vec![
            uses(&[background("a", &a.command)]),
            done("started"),
            done("woke"),
        ],
        false,
    );
    h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    let times: StdMutex<(Option<Instant>, Option<Instant>)> = StdMutex::new((None, None));
    let events = h
        .drive(
            Vec::new(),
            |e| {
                let mut t = times.lock().unwrap();
                match e {
                    AgentEvent::ProcessEnded { id: 1, .. } => t.0 = Some(Instant::now()),
                    AgentEvent::TurnStarted { .. } => t.1 = Some(Instant::now()),
                    _ => {}
                }
                Vec::new()
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(wakes(&events), 1, "{events:#?}");
    let (ended, started) = *times.lock().unwrap();
    let waited = started.unwrap() - ended.unwrap();
    assert!(
        waited >= WAKE_SETTLE - Duration::from_millis(100),
        "{waited:?}"
    );
    let block = woken(&[&a.ended(1, 2)]);
    let message = last_sent(&h);
    assert_eq!(message.last(), Some(&block), "{message:?}");
    assert!(message.iter().all(|t| !t.is_empty()), "{message:?}");
    let records = records(&h.store);
    assert_eq!(
        opened(&records),
        [
            (1, None, None, "go".to_owned()),
            (2, Some(TurnOrigin::ProcessExit), Some(block), String::new()),
        ]
    );
    assert_eq!(heard(&records), [(2, vec![1])]);
    assert_eq!(starts(&quiet(&mut h).await), 0);
    h.assert_rebuilds();
}

#[tokio::test]
async fn two_exits_a_second_apart_share_one_wake() {
    if !have_bash() {
        return;
    }
    let mut h = jobs(
        "wake-batch",
        vec![
            uses(&[
                background("a", "sleep 1; exit 1"),
                background("b", "sleep 1.5; exit 2"),
            ]),
            done("started"),
            done("woke"),
        ],
        false,
    );
    h.run(vec![steer("go")], 1).await;
    woke(&mut h).await;
    assert_eq!(
        last_sent(&h).last().unwrap(),
        &woken(&[
            "Process 1 (sleep 1; exit 1) exited with code 1.",
            "Process 2 (sleep 1.5; exit 2) exited with code 2.",
        ])
    );
    assert_eq!(starts(&quiet(&mut h).await), 0);
    assert_eq!(heard(&records(&h.store)), [(2, vec![1, 2])]);
}

#[tokio::test]
async fn a_stop_by_the_user_starts_no_turn_and_a_time_limit_stop_does() {
    if !have_bash() {
        return;
    }
    let mut h = jobs(
        "wake-stops",
        vec![
            uses(&[
                background("a", "sleep 30"),
                call(
                    "b",
                    "bash",
                    json!({"command": "sleep 30", "run_in_background": true, "timeout_ms": 1500}),
                ),
            ]),
            done("started"),
            done("woke"),
        ],
        false,
    );
    h.run(vec![steer("go")], 1).await;
    let stopped = h
        .drive(
            vec![ClientCommand::StopProcess { id: 1 }],
            nothing,
            process_ended(1),
            PATIENT,
        )
        .await;
    assert_eq!(starts(&stopped), 0);
    woke(&mut h).await;
    assert_eq!(
        last_sent(&h).last().unwrap(),
        &woken(&[
            "Process 1 (sleep 30) was stopped by the user.",
            "Process 2 (sleep 30) was stopped at its 1,500 ms time limit.",
        ])
    );
    assert_eq!(starts(&quiet(&mut h).await), 0);
}

#[tokio::test]
async fn a_user_stop_alone_waits_for_the_next_message() {
    if !have_bash() {
        return;
    }
    let mut h = jobs(
        "wake-user-stop",
        vec![
            uses(&[background("a", "sleep 30")]),
            done("started"),
            done("next"),
        ],
        false,
    );
    h.run(vec![steer("go")], 1).await;
    h.drive(
        vec![ClientCommand::StopProcess { id: 1 }],
        nothing,
        process_ended(1),
        PATIENT,
    )
    .await;
    assert_eq!(starts(&quiet(&mut h).await), 0);
    h.run(vec![steer("next")], 1).await;
    let message = last_sent(&h);
    assert_eq!(
        message[message.len() - 2..],
        [
            notice(&["Process 1 (sleep 30) was stopped by the user."]),
            "next".to_owned()
        ]
    );
}

#[tokio::test]
async fn an_exit_during_the_last_step_wakes_after_the_turn_ends() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "wake-last-step",
        vec![
            uses(&[background("a", &a.command)]),
            uses(&[call("b", "sleep", json!({"ms": 1500, "text": "x"}))]),
            done("done"),
            done("woke"),
        ],
        false,
    );
    let first = h
        .drive(
            vec![steer("go")],
            a.opens_at(h._env.work.clone(), "b"),
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(terminal(&first), 1, "{first:#?}");
    assert!(process_ended(1)(&first), "{first:#?}");
    let ended = Instant::now();
    woke(&mut h).await;
    assert!(ended.elapsed() >= WAKE_SETTLE - Duration::from_millis(100));
    assert_eq!(last_sent(&h).last().unwrap(), &woken(&[&a.ended(1, 2)]));
    assert_eq!(heard(&records(&h.store)), [(2, vec![1])]);
    h.assert_rebuilds();
}

#[tokio::test]
async fn a_message_inside_the_settle_window_takes_the_ends_instead() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "wake-message-wins",
        vec![
            uses(&[background("a", &a.command)]),
            done("started"),
            done("mine"),
        ],
        true,
    );
    h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    let mut sent_once = false;
    let events = h
        .drive(
            Vec::new(),
            |e| match e {
                AgentEvent::ProcessEnded { id: 1, .. } if !sent_once => {
                    sent_once = true;
                    vec![steer("mine")]
                }
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(wakes(&events), 0, "{events:#?}");
    let message = last_sent(&h);
    assert_eq!(
        message[message.len() - 2..],
        [notice(&[&a.ended(1, 2)]), "mine".to_owned()]
    );
    assert_eq!(starts(&quiet(&mut h).await), 0);
    assert_eq!(heard(&records(&h.store)), [(2, vec![1])]);
}

#[tokio::test]
async fn no_wake_after_an_esc_stopped_turn_and_the_next_message_carries_the_end() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "wake-after-esc",
        vec![
            uses(&[background("a", &a.command)]),
            hanging(&[B::Text("partial")]),
            done("next"),
        ],
        true,
    );
    let events = h
        .drive(
            vec![steer("go")],
            |e| match e {
                AgentEvent::ToolCallFinished { call_id, .. } if call_id == "a" => {
                    vec![ClientCommand::Cancel]
                }
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await;
    a.open(&h._env.work);
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Cancelled { .. }]
    ));
    let later = quiet(&mut h).await;
    assert!(process_ended(1)(&later), "{later:#?}");
    assert_eq!(starts(&later), 0);
    h.run(vec![steer("next")], 1).await;
    let message = last_sent(&h);
    assert_eq!(
        message[message.len() - 2..],
        [notice(&[&a.ended(1, 2)]), "next".to_owned()]
    );
}

#[tokio::test]
async fn a_wake_that_keeps_nothing_wakes_no_more_and_the_next_message_carries_the_end() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "wake-empty",
        vec![
            uses(&[background("a", &a.command)]),
            done("started"),
            blank(),
            done("next"),
        ],
        false,
    );
    h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    let woken = woke(&mut h).await;
    assert!(matches!(
        outcomes(&woken)[..],
        [TurnOutcome::Completed {
            committed: false,
            ..
        }]
    ));
    assert_eq!(starts(&quiet(&mut h).await), 0);
    h.run(vec![steer("next")], 1).await;
    let message = last_sent(&h);
    assert_eq!(
        message[message.len() - 2..],
        [notice(&[&a.ended(1, 2)]), "next".to_owned()]
    );
}

#[tokio::test]
async fn no_wake_while_a_retry_offer_stands() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "wake-offer",
        vec![
            uses(&[background("a", &a.command)]),
            server_error_step(),
            done("next"),
        ],
        false,
    );
    let events = h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Error { offer: Some(_), .. }]
    ));
    let later = quiet(&mut h).await;
    assert!(process_ended(1)(&later));
    assert_eq!(starts(&later), 0);
    h.run(vec![steer("next")], 1).await;
    assert!(last_sent(&h).contains(&notice(&[&a.ended(1, 2)])));
}

#[tokio::test]
async fn esc_stops_a_wake_turn_and_its_notice_waits_for_the_next_message() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "wake-esc",
        vec![
            uses(&[background("a", &a.command)]),
            done("started"),
            hanging(&[B::Text("partial")]),
            done("next"),
        ],
        true,
    );
    h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    let events = h
        .drive(
            Vec::new(),
            |e| match e {
                AgentEvent::TurnStarted {
                    origin: Some(TurnOrigin::ProcessExit),
                    ..
                } => vec![ClientCommand::Cancel],
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(wakes(&events), 1);
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Cancelled { .. }]
    ));
    assert_eq!(starts(&quiet(&mut h).await), 0);
    h.run(vec![steer("next")], 1).await;
    assert!(last_sent(&h).contains(&notice(&[&a.ended(1, 2)])));
    assert_eq!(heard(&records(&h.store)), [(3, vec![1])]);
    h.assert_rebuilds();
}

#[tokio::test]
async fn a_wake_turn_reports_only_the_ends_no_result_carried() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 1);
    let b = ExitGate::new("b.go", 3);
    let mut h = jobs(
        "wake-listed",
        vec![
            uses(&[background("a", &a.command), background("b", &b.command)]),
            uses(&[call("c", "sleep", json!({"ms": 1200}))]),
            uses(&[call("d", "process", json!({}))]),
            done("listed"),
            done("woke"),
        ],
        false,
    );
    let first = h
        .drive(
            vec![steer("go")],
            a.opens_at(h._env.work.clone(), "c"),
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(terminal(&first), 1, "{first:#?}");
    b.open(&h._env.work);
    assert_eq!(reported(&first), [Vec::new()]);
    let listed = h.requests()[3].last().map(sent).unwrap_or_default();
    assert!(
        listed
            .iter()
            .any(|t| t.contains(&format!("1  exited with code 1  {}", a.shown()))),
        "{listed:?}"
    );
    let events = woke(&mut h).await;
    assert_eq!(reported(&events), [vec![exited(2, 3)]]);
    assert_eq!(last_sent(&h).last().unwrap(), &woken(&[&b.ended(2, 3)]));
}

#[tokio::test]
async fn a_retry_reports_the_ends_its_resent_notice_carries() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "wake-retry-ends",
        vec![
            uses(&[background("a", &a.command)]),
            server_error_step(),
            done("next"),
            done("woke"),
        ],
        false,
    );
    h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    assert!(process_ended(1)(&quiet(&mut h).await));
    let retried = h
        .run(
            vec![ClientCommand::Retry {
                turn: 1,
                endpoint: None,
                request: RequestId(1),
            }],
            1,
        )
        .await;
    assert_eq!(reported(&retried), [Vec::new()]);
    let events = woke(&mut h).await;
    assert_eq!(reported(&events), [vec![exited(1, 2)]]);
}

#[tokio::test]
async fn a_failed_wake_turn_does_not_wake_again_and_its_retry_resends_it() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "wake-retry",
        vec![
            uses(&[background("a", &a.command)]),
            done("started"),
            server_error_step(),
            done("woke"),
        ],
        false,
    );
    h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    let failed = woke(&mut h).await;
    assert!(matches!(
        outcomes(&failed)[..],
        [TurnOutcome::Error { offer: Some(_), .. }]
    ));
    assert_eq!(reported(&failed), [vec![exited(1, 2)]]);
    assert_eq!(starts(&quiet(&mut h).await), 0);
    let retried = h
        .run(
            vec![ClientCommand::Retry {
                turn: 2,
                endpoint: None,
                request: RequestId(1),
            }],
            1,
        )
        .await;
    assert!(matches!(
        outcomes(&retried)[..],
        [TurnOutcome::Completed { .. }]
    ));
    assert_eq!(reported(&retried), [vec![exited(1, 2)]]);
    let requests = h.requests();
    assert_eq!(requests.len(), 4);
    let unstamped = |at: usize| -> Vec<String> {
        sent(requests[at].last().unwrap())
            .into_iter()
            .filter(|t| !t.starts_with("Sent:") && !t.starts_with("Context:"))
            .collect()
    };
    let block = woken(&[&a.ended(1, 2)]);
    assert_eq!(unstamped(3), unstamped(2));
    assert_eq!(sent(requests[3].last().unwrap()).last(), Some(&block));
    let records = records(&h.store);
    assert_eq!(
        opened(&records),
        [
            (1, None, None, "go".to_owned()),
            (
                2,
                Some(TurnOrigin::ProcessExit),
                Some(block.clone()),
                String::new()
            ),
            (3, None, Some(block), String::new()),
        ]
    );
    assert_eq!(heard(&records), [(3, vec![1])]);
    h.assert_rebuilds();
}

#[tokio::test]
async fn a_wake_turn_resent_by_the_step_retry_rebuilds_byte_for_byte() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "wake-step-retry",
        vec![
            uses(&[background("a", &a.command)]),
            done("started"),
            turned_away(),
            done("woke"),
        ],
        false,
    );
    h.agent.retry = RetryPolicy {
        step_retries: Some(2),
        base_delay: Duration::from_millis(10),
        jitter: || 0.0,
        ..RetryPolicy::default()
    };
    h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    let events = woke(&mut h).await;
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Completed { .. }]
    ));
    let bodies = h.provider.texts();
    assert_eq!(bodies.len(), 4);
    assert_eq!(bodies[3], bodies[2]);
    assert_eq!(heard(&records(&h.store)), [(2, vec![1])]);
    h.assert_rebuilds();
}

#[tokio::test]
async fn every_wake_in_a_row_fires_with_no_count() {
    if !have_bash() {
        return;
    }
    const ROW: usize = 6;
    let ids = ["a", "b", "c", "d", "e", "f"];
    let replies = ["one", "two", "three", "four", "five", "six"];
    let gates = ids.map(|id| ExitGate::new(&format!("{id}.go"), 1));
    let mut script = Vec::new();
    for ((id, gate), text) in ids.iter().zip(&gates).zip(replies) {
        script.push(uses(&[background(id, &gate.command)]));
        script.push(done(text));
    }
    script.push(done("last"));
    let mut h = jobs("wake-row", script, false);
    h.run(vec![steer("go")], 1).await;
    for gate in &gates {
        gate.open(&h._env.work);
        woke(&mut h).await;
    }
    assert_eq!(starts(&quiet(&mut h).await), 0);
    let records = records(&h.store);
    let wake_turns: Vec<_> = opened(&records)
        .into_iter()
        .skip(1)
        .map(|(_, origin, block, _)| (origin, block))
        .collect();
    let expected: Vec<_> = (1..=ROW)
        .zip(&gates)
        .map(|(n, gate)| {
            let line = gate.ended(u32::try_from(n).unwrap(), 1);
            (Some(TurnOrigin::ProcessExit), Some(woken(&[&line])))
        })
        .collect();
    assert_eq!(wake_turns, expected);
    h.assert_rebuilds();
}

#[tokio::test]
async fn a_steer_into_a_wake_turn_is_delivered_and_the_next_end_still_wakes() {
    if !have_bash() {
        return;
    }
    let (a, b) = (ExitGate::new("a.go", 1), ExitGate::new("b.go", 1));
    let chain = |id, gate: &ExitGate| uses(&[background(id, &gate.command)]);
    let mut h = jobs(
        "wake-steer",
        vec![
            chain("a", &a),
            done("one"),
            chain("b", &b),
            done("two"),
            done("three"),
        ],
        true,
    );
    h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    let mut steered = false;
    let first = h
        .drive(
            Vec::new(),
            |e| match e {
                AgentEvent::TurnStarted {
                    origin: Some(TurnOrigin::ProcessExit),
                    ..
                } if !steered => {
                    steered = true;
                    vec![steer("keep going")]
                }
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(wakes(&first), 1, "{first:#?}");
    assert!(
        first
            .iter()
            .any(|e| matches!(e, AgentEvent::InputDelivered { .. })),
        "{first:#?}"
    );
    assert!(matches!(
        outcomes(&first)[..],
        [TurnOutcome::Completed { .. }]
    ));
    b.open(&h._env.work);
    woke(&mut h).await;
    assert_eq!(last_sent(&h).last().unwrap(), &woken(&[&b.ended(2, 1)]));
}

#[tokio::test]
async fn esc_inside_the_settle_window_cancels_the_wake() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "wake-esc-window",
        vec![
            uses(&[background("a", &a.command)]),
            done("started"),
            done("next"),
        ],
        true,
    );
    h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    let mut cancelled = false;
    let events = h
        .drive(
            Vec::new(),
            |e| match e {
                AgentEvent::ProcessEnded { id: 1, .. } if !cancelled => {
                    cancelled = true;
                    vec![ClientCommand::Cancel]
                }
                _ => Vec::new(),
            },
            process_ended(1),
            PATIENT,
        )
        .await;
    assert!(process_ended(1)(&events));
    let later = quiet(&mut h).await;
    assert_eq!(starts(&events) + starts(&later), 0, "{later:#?}");
    h.run(vec![steer("next")], 1).await;
    let message = last_sent(&h);
    assert_eq!(
        message[message.len() - 2..],
        [notice(&[&a.ended(1, 2)]), "next".to_owned()]
    );
    assert_eq!(heard(&records(&h.store)), [(2, vec![1])]);
    h.assert_rebuilds();
}

#[tokio::test]
async fn a_completed_retry_of_a_wake_turn_lets_the_next_end_wake() {
    if !have_bash() {
        return;
    }
    let (a, b) = (ExitGate::new("a.go", 1), ExitGate::new("b.go", 1));
    let chain = |id, gate: &ExitGate| uses(&[background(id, &gate.command)]);
    let mut h = jobs(
        "wake-retry-rearm",
        vec![
            chain("a", &a),
            done("one"),
            server_error_step(),
            chain("b", &b),
            done("retried"),
            done("woke"),
        ],
        false,
    );
    h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    let failed = h.drive(Vec::new(), nothing, turns(1), PATIENT).await;
    assert_eq!(wakes(&failed), 1, "{failed:#?}");
    assert!(matches!(
        outcomes(&failed)[..],
        [TurnOutcome::Error { offer: Some(_), .. }]
    ));
    let retried = h
        .run(
            vec![ClientCommand::Retry {
                turn: 2,
                endpoint: None,
                request: RequestId(1),
            }],
            1,
        )
        .await;
    assert!(matches!(
        outcomes(&retried)[..],
        [TurnOutcome::Completed { .. }]
    ));
    b.open(&h._env.work);
    woke(&mut h).await;
    assert_eq!(last_sent(&h).last().unwrap(), &woken(&[&b.ended(2, 1)]));
}

#[tokio::test]
async fn closing_inside_the_window_starts_no_turn() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "wake-close",
        vec![uses(&[background("a", &a.command)]), done("started")],
        false,
    );
    h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    let seen = h
        .drive(Vec::new(), nothing, process_ended(1), PATIENT)
        .await;
    assert!(process_ended(1)(&seen));
    let Harness {
        mut agent,
        tx,
        mut rx,
        store,
        ..
    } = h;
    drop(tx);
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    let started = Instant::now();
    agent.serve(&mut rx, &ev_tx).await;
    assert!(started.elapsed() < WAKE_SETTLE);
    drop(ev_tx);
    let mut after = Vec::new();
    while let Some(e) = ev_rx.recv().await {
        after.push(e);
    }
    assert!(
        !after
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnStarted { .. })),
        "{after:#?}"
    );
    let records = records(&store);
    let starts = records
        .iter()
        .filter(|r| matches!(r.event, LogEvent::TurnStarted { .. }))
        .count();
    assert_eq!(starts, 1);
    assert!(matches!(
        records.last().unwrap().event,
        LogEvent::SessionEnded { .. }
    ));
}

#[tokio::test]
async fn esc_while_idle_with_no_window_leaves_a_later_wake_working() {
    if !have_bash() {
        return;
    }
    let mut h = jobs(
        "wake-idle-esc",
        vec![
            uses(&[background("a", "sleep 1.5; exit 2")]),
            done("started"),
            done("woke"),
        ],
        true,
    );
    h.run(vec![steer("go")], 1).await;
    let events = h
        .drive(vec![ClientCommand::Cancel], nothing, turns(1), PATIENT)
        .await;
    assert!(process_ended(1)(&events), "{events:#?}");
    assert_eq!(wakes(&events), 1, "{events:#?}");
    assert_eq!(
        last_sent(&h).last().unwrap(),
        &woken(&["Process 1 (sleep 1.5; exit 2) exited with code 2."])
    );
}

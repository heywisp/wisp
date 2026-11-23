use wisp_protocol::agents::SEND_MESSAGE;

use super::*;
use crate::stack_alarm::{Timer, on_alarm_stack};

pub(super) fn spawned(e: &AgentEvent, agent: &str) -> bool {
    matches!(e, AgentEvent::AgentSpawned { agent: a, .. } if a.path.as_str() == agent)
}

fn idle_as(e: &AgentEvent, agent: &str) -> Option<Option<IdleOutcome>> {
    match e {
        AgentEvent::AgentStateChanged {
            agent: a,
            state: AgentState::Idle { outcome },
            ..
        } if a.as_str() == agent => Some(*outcome),
        _ => None,
    }
}

fn turns_started(run: &Run, agent: &str) -> usize {
    run.records(agent)
        .iter()
        .filter(|e| matches!(e, LogEvent::TurnStarted { .. }))
        .count()
}

fn send_refusals(run: &Run, by: &str) -> Vec<String> {
    run.seen
        .iter()
        .filter_map(|e| match inner(e, by).unwrap_or(e) {
            AgentEvent::AgentRefused {
                by: who,
                action: AgentAction::Send,
                reason,
            } if who.as_str() == by => Some(reason.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn each_route_is_allowed_or_refused_as_decision_one_says() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "messages-routes",
        Setup::default(),
        vec![
            (
                "/a",
                vec![
                    uses(&[spawn_call("s1", "x", "Look.")]),
                    gated(
                        uses(&[
                            send_call("a1", "/b/y", "cousin?"),
                            quiet_call("a2", "../b", "hello sibling"),
                            quiet_call("a3", "..", "hello parent"),
                            quiet_call("a4", "x", "hello child"),
                        ]),
                        &gate,
                    ),
                    uses(&[report_call("r1", "a done")]),
                ],
            ),
            (
                "/a/x",
                vec![
                    gated(
                        uses(&[
                            send_call("x1", "/", "grandparent?"),
                            send_call("x2", "/b", "uncle?"),
                            quiet_call("x3", "..", "hello parent"),
                        ]),
                        &gate,
                    ),
                    uses(&[report_call("r2", "x done")]),
                ],
            ),
            (
                "/b",
                vec![uses(&[spawn_call("s2", "y", "Look.")]), done("waiting")],
            ),
            ("/b/y", vec![hanging("working")]),
            (
                "/",
                vec![
                    uses(&[quiet_call("m1", "/a/x", "hello grandchild")]),
                    done("ok"),
                ],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("a", "Lead a.")).await;
    run.spawn(spawn_request("b", "Lead b.")).await;
    run.until("x", |e| spawned(e, "/a/x")).await;
    run.until("y", |e| spawned(e, "/b/y")).await;
    gate.add_permits(2);
    run.until("the root to wake on a's report", |e| {
        matches!(e, AgentEvent::TurnStarted { .. })
    })
    .await;
    run.root_ended().await;
    run.child_ended("/a/x").await;

    let x = run.requests("/a/x");
    let (is_error, text) = result_of(&x[1], "x1");
    assert!(is_error);
    assert_eq!(
        text,
        "main is above your parent, so you can't message it; send it to your parent (`..`) to pass on"
    );
    let (is_error, text) = result_of(&x[1], "x2");
    assert!(is_error);
    assert!(text.starts_with("/b isn't yours to message"), "{text}");
    assert!(!result_of(&x[1], "x3").0);

    let a = run.requests("/a");
    let (is_error, text) = result_of(&a[2], "a1");
    assert!(is_error, "a sibling's child is out of reach");
    assert!(text.starts_with("/b/y isn't yours to message"), "{text}");
    for id in ["a2", "a3", "a4"] {
        let (is_error, text) = result_of(&a[2], id);
        assert!(!is_error, "{id}: {text}");
    }
    assert_eq!(
        result_of(&a[2], "a3").1,
        "Sent to main (running: it reads this at its next step, or when it next runs if this turn ends first)."
    );
    assert!(result_of(&a[2], "a2").1.starts_with("Sent to /b ("));

    let root = run.requests("/");
    let (is_error, text) = result_of(&root[1], "m1");
    assert!(!is_error, "the root reaches a grandchild: {text}");
    assert!(text.starts_with("Sent to /a/x ("), "{text}");

    assert_eq!(send_refusals(&run, "/a/x").len(), 2);
    assert_eq!(send_refusals(&run, "/a").len(), 1);
    let run = run.quit().await;
    let logged = run
        .records("/a/x")
        .into_iter()
        .filter(|e| {
            matches!(
                e,
                LogEvent::AgentRefused {
                    action: AgentAction::Send,
                    ..
                }
            )
        })
        .count();
    assert_eq!(logged, 2);
    assert!(
        run.mail_to("/a/x")
            .iter()
            .any(|(kind, wake, text)| *kind == MailKind::Message
                && !wake
                && text.contains("from=\"/\"")
                && text.contains("hello grandchild")
                && text.contains("(You can't message main directly: reply through your parent.)"))
    );
    assert!(
        run.mail_to("/b")
            .iter()
            .any(|(kind, _, text)| *kind == MailKind::Message && text.contains("hello sibling"))
    );
}

#[tokio::test(start_paused = true)]
async fn mail_from_the_parent_and_the_grandparent_keeps_a_running_turn_going() {
    let held = Arc::new(Semaphore::new(0));
    let root_gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "messages-keep-going",
        Setup::default(),
        vec![
            (
                "/",
                vec![
                    uses(&[spawn_call("s1", "lead", "Lead.")]),
                    gated(
                        uses(&[quiet_call("g1", "/lead/w", "Also check C.")]),
                        &root_gate,
                    ),
                    done("ok"),
                ],
            ),
            (
                "/lead",
                vec![
                    uses(&[spawn_call("s2", "w", "Check A.")]),
                    gated(
                        uses(&[call(
                            "p1",
                            SEND_MESSAGE,
                            json!({"to": "w", "text": "Also check B.", "wake": false}),
                        )]),
                        &root_gate,
                    ),
                    done("sent"),
                ],
            ),
            (
                "/lead/w",
                vec![
                    gated(done("checked A"), &held),
                    uses(&[report_call("r1", "checked A, B and C")]),
                ],
            ),
        ],
    )
    .await;
    run.say("go").await;
    run.until("w", |e| spawned(e, "/lead/w")).await;
    sent_times(&run, "/lead/w", 1).await;
    root_gate.add_permits(2);
    run.root_ended().await;
    run.child_ended("/lead").await;
    held.add_permits(1);
    let outcome = run.child_ended("/lead/w").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let w = run.requests("/lead/w");
    assert_eq!(w.len(), 2, "one turn, continued by the mail from above");
    assert!(w[1].messages.starts_with(&w[0].messages));
    let last = w[1].last_user_texts().join("\n");
    assert!(last.contains("Also check B."), "{last}");
    assert!(last.contains("Also check C."), "{last}");
    assert!(
        last.contains("(You can't message main directly: reply through your parent.)"),
        "{last}"
    );
    let run = run.quit().await;
    assert_eq!(turns_started(&run, "/lead/w"), 1);
}

#[tokio::test(start_paused = true)]
async fn a_siblings_mail_does_not_keep_a_turn_going_and_wakes_after_the_settle() {
    let held = Arc::new(Semaphore::new(0));
    let mut run = start(
        "messages-peer",
        Setup::default(),
        vec![
            (
                "/a",
                vec![
                    uses(&[send_call("a1", "../b", "Your input, please.")]),
                    uses(&[report_call("r1", "asked b")]),
                ],
            ),
            (
                "/b",
                vec![
                    gated(uses(&[report_call("r2", "b done")]), &held),
                    done("read a's note"),
                ],
            ),
            ("/", vec![done("ok"), done("ok"), done("ok")]),
        ],
    )
    .await;
    run.spawn(spawn_request("b", "Work b.")).await;
    run.spawn(spawn_request("a", "Work a.")).await;
    run.child_ended("/a").await;
    held.add_permits(1);
    run.child_ended("/b").await;
    let ended_at = tokio::time::Instant::now();
    run.child_ended("/b").await;
    let b = run.requests("/b");
    assert_eq!(b.len(), 2);
    assert!(!b[0].all_text().contains("Your input, please."));
    assert!(
        b[1].last_user_texts()
            .join("\n")
            .contains("Your input, please.")
    );
    let second = sent_at(&run.providers.timeline(), "/b")[1].1;
    assert!(
        second.duration_since(ended_at) >= Duration::from_secs(2),
        "the wake waits for the settle"
    );
    let run = run.quit().await;
    assert_eq!(turns_started(&run, "/b"), 2);
}

async fn sent_times(run: &Run, path: &str, n: usize) {
    within("the requests", async {
        while sent_at(&run.providers.timeline(), path).len() < n {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_peer_cap_refuses_a_sibling_until_mail_from_above_resets_it() {
    let gate = Arc::new(Semaphore::new(0));
    let notes: Vec<B> = (0..=MAX_PEER_MESSAGES)
        .map(|i| quiet_call(&format!("n{i}"), "../b", "note"))
        .collect();
    let mut run = start(
        "messages-peer-cap",
        Setup::default(),
        vec![
            (
                "/a",
                vec![
                    uses(&notes),
                    gated(uses(&[quiet_call("again", "../b", "note")]), &gate),
                    uses(&[report_call("r1", "done")]),
                ],
            ),
            ("/b", vec![hanging("busy")]),
        ],
    )
    .await;
    run.spawn(spawn_request("b", "Work b.")).await;
    run.spawn(spawn_request("a", "Work a.")).await;
    let last = format!("n{MAX_PEER_MESSAGES}");
    run.until("a's last note", |e| {
        matches!(inner(e, "/a"), Some(AgentEvent::ToolCallFinished { call_id, .. }) if *call_id == last)
    })
    .await;
    run.send(ClientCommand::Child {
        agent: path("/b"),
        command: Box::new(ClientCommand::send("you-1", "Keep going.")),
    })
    .await;
    run.until("b to accept your message", |e| {
        matches!(
            inner(e, "/b"),
            Some(AgentEvent::InputAccepted { from: None, .. })
        )
    })
    .await;
    gate.add_permits(1);
    run.child_ended("/a").await;
    let a = run.requests("/a");
    for i in 0..MAX_PEER_MESSAGES {
        let (is_error, text) = result_of(&a[1], &format!("n{i}"));
        assert!(!is_error, "{i}: {text}");
    }
    let (is_error, text) = result_of(&a[1], &format!("n{MAX_PEER_MESSAGES}"));
    assert!(is_error);
    assert_eq!(
        text,
        "/b already has 16 messages from you since its parent last wrote to it; ask your parent (`..`) to take this further"
    );
    let (is_error, text) = result_of(&a[2], "again");
    assert!(!is_error, "your message reset the cap: {text}");
    assert_eq!(send_refusals(&run, "/a").len(), 1);
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_child_asking_a_sibling_waits_asked_and_an_unanswered_question_wakes_it() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "messages-sibling-question",
        Setup::default(),
        vec![
            (
                "/a",
                vec![
                    gated(uses(&[send_call("a1", "../b", "Which API?")]), &gate),
                    done("waiting for b"),
                    uses(&[report_call("r1", "b never said")]),
                ],
            ),
            (
                "/b",
                vec![uses(&[report_call("r2", "b done")]), done("I won't answer")],
            ),
            ("/", vec![done("ok"), done("ok"), done("ok")]),
        ],
    )
    .await;
    run.spawn(spawn_request("b", "Work b.")).await;
    run.spawn(spawn_request("a", "Work a.")).await;
    run.child_ended("/b").await;
    gate.add_permits(1);
    run.child_ended("/a").await;
    let asked = run
        .until("a to settle", |e| idle_as(e, "/a").is_some())
        .await;
    assert_eq!(idle_as(&asked, "/a"), Some(Some(IdleOutcome::Asked)));
    assert_eq!(run.requests("/a").len(), 2, "no nudge");
    run.child_ended("/b").await;
    run.child_ended("/a").await;
    let a = run.requests("/a");
    assert_eq!(a.len(), 3);
    assert!(
        a[2].last_user_texts()
            .join("\n")
            .contains("/b ended its turn without answering your message.")
    );
    let run = run.quit().await;
    let notices: Vec<(MailKind, bool, String)> = run
        .mail_to("/a")
        .into_iter()
        .filter(|(kind, _, _)| *kind == MailKind::Notice)
        .collect();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].1, "the notice wakes the asker");
}

#[tokio::test(start_paused = true)]
async fn a_question_from_a_child_wakes_its_parent_and_the_answer_reaches_its_next_step() {
    let mut run = start(
        "messages-question",
        Setup::default(),
        vec![
            (
                "/c",
                vec![
                    uses(&[send_call("q1", "..", "Which file should I edit?")]),
                    done("waiting"),
                    uses(&[report_call("r1", "edited a.rs")]),
                ],
            ),
            (
                "/",
                vec![
                    uses(&[send_call("m1", "c", "Edit a.rs.")]),
                    done("answered"),
                    done("ok"),
                ],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("c", "Edit the right file.")).await;
    run.child_ended("/c").await;
    run.until("the root to wake", |e| {
        matches!(e, AgentEvent::TurnStarted { .. })
    })
    .await;
    run.root_ended().await;
    let root = run.requests("/");
    assert!(root[0].all_text().contains("Which file should I edit?"));
    assert_eq!(
        result_of(&root[1], "m1").1,
        "Sent to /c (idle: woken). Its report will arrive as a message; don't poll it, and end your turn unless you have other independent work."
    );
    run.child_ended("/c").await;
    let c = run.requests("/c");
    assert_eq!(c.len(), 3);
    assert!(c[2].last_user_texts().join("\n").contains("Edit a.rs."));
    let run = run.quit().await;
    assert!(
        run.mail_to("/c")
            .iter()
            .any(|(kind, wake, text)| *kind == MailKind::Task
                && *wake
                && text.contains("Edit a.rs."))
    );
}

#[tokio::test(start_paused = true)]
async fn stop_agent_cascades_and_tells_each_parent_but_the_caller() {
    let gate = Arc::new(Semaphore::new(0));
    let builds = Arc::new(tokio::sync::Notify::new());
    let mut run = start(
        "messages-stop",
        Setup {
            child_gate: Some(builds.clone()),
            ..Setup::default()
        },
        vec![
            (
                "/",
                vec![
                    uses(&[spawn_call("s1", "lead", "Lead.")]),
                    gated(uses(&[stop_call("k1", "lead", false)]), &gate),
                    done("stopped it"),
                ],
            ),
            (
                "/lead",
                vec![
                    uses(&[spawn_call("s2", "helper", "Help.")]),
                    uses(&[spawn_call("s3", "extra", "More.")]),
                ],
            ),
            ("/lead/helper", vec![hanging("helping")]),
        ],
    )
    .await;
    run.say("go").await;
    builds.notify_one();
    run.until("the lead", |e| spawned(e, "/lead")).await;
    builds.notify_one();
    run.until("the helper to stream", |e| {
        matches!(inner(e, "/lead/helper"), Some(AgentEvent::TextDelta { .. }))
    })
    .await;
    run.until("the lead to start its next spawn", |e| {
        matches!(inner(e, "/lead"), Some(AgentEvent::ToolCallStarted { call_id, .. }) if call_id == "s3")
    })
    .await;
    gate.add_permits(1);
    let lead = run.child_ended("/lead").await;
    assert!(matches!(lead, TurnOutcome::Cancelled { .. }), "{lead:?}");
    let helper = run.child_ended("/lead/helper").await;
    assert!(
        matches!(helper, TurnOutcome::Cancelled { .. }),
        "{helper:?}"
    );
    run.root_ended().await;
    let root = run.requests("/");
    assert_eq!(
        result_of(&root[2], "k1"),
        (
            false,
            "Stopped /lead and 1 agent below it. It stays idle and keeps its context; message it to go on.".to_owned()
        )
    );
    tokio::time::sleep(Duration::from_secs(10)).await;
    let run = run.quit().await;
    assert!(
        run.mail_to("/")
            .iter()
            .all(|(kind, _, _)| *kind != MailKind::Notice),
        "the caller isn't told what it did"
    );
    let notices: Vec<(MailKind, bool, String)> = run
        .mail_to("/lead")
        .into_iter()
        .filter(|(kind, _, _)| *kind == MailKind::Notice)
        .collect();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(!notices[0].1);
    assert!(
        notices[0]
            .2
            .contains("/lead/helper was stopped with its parent.")
    );
    assert_eq!(run.requests("/lead").len(), 2, "nothing stopped wakes");
    assert!(
        run.records("/lead")
            .iter()
            .any(|e| matches!(e, LogEvent::ToolCallRequested { call_id, .. } if call_id == "s3")),
        "the lead was inside a tool call when the stop landed"
    );
    assert_eq!(run.requests("/lead/helper").len(), 1);
    assert_eq!(turns_started(&run, "/lead"), 1);
    for agent in ["/", "/lead", "/lead/helper"] {
        let records = run.records(agent);
        let asked = records
            .iter()
            .filter(|e| matches!(e, LogEvent::ToolCallRequested { .. }))
            .count();
        let answered = records
            .iter()
            .filter(|e| matches!(e, LogEvent::ToolCallFinished { .. }))
            .count();
        assert_eq!(asked, answered, "{agent}: one result per call");
    }
}

#[tokio::test(start_paused = true)]
async fn a_stop_from_above_the_parent_tells_the_parent_who_did_it() {
    for retrying in [false, true] {
        let helping = if retrying {
            Step::from(vec![
                Ok(ProviderEvent::Sending),
                Err(classified(
                    ErrorKind::RateLimit,
                    "HTTP 429: Too many concurrent requests",
                    FailurePhase::Response,
                    RetryClass::Transient,
                )),
            ])
        } else {
            hanging("helping")
        };
        let gate = Arc::new(Semaphore::new(0));
        let mut run = start(
            "messages-stop-deep",
            Setup::default(),
            vec![
                (
                    "/",
                    vec![
                        uses(&[spawn_call("s1", "lead", "Lead.")]),
                        gated(uses(&[stop_call("k1", "/lead/helper", false)]), &gate),
                        done("stopped it"),
                    ],
                ),
                (
                    "/lead",
                    vec![
                        uses(&[spawn_call("s2", "helper", "Help.")]),
                        done("waiting"),
                        done("read it"),
                    ],
                ),
                ("/lead/helper", vec![helping]),
            ],
        )
        .await;
        run.say("go").await;
        run.until("the helper to stream or retry", |e| {
            matches!(
                (retrying, inner(e, "/lead/helper")),
                (true, Some(AgentEvent::Retrying { .. }))
                    | (false, Some(AgentEvent::TextDelta { .. }))
            )
        })
        .await;
        gate.add_permits(1);
        let outcome = run.child_ended("/lead/helper").await;
        assert!(
            matches!(outcome, TurnOutcome::Cancelled { .. }),
            "{outcome:?}"
        );
        run.until("the helper to stop", |e| {
            idle_as(e, "/lead/helper") == Some(Some(IdleOutcome::Stopped))
        })
        .await;
        run.root_ended().await;
        tokio::time::sleep(Duration::from_secs(10)).await;
        let run = run.quit().await;
        assert_eq!(
            run.requests("/lead/helper").len(),
            1,
            "no retry after a stop"
        );
        assert_eq!(
            run.requests("/lead").len(),
            2,
            "the stop notice wakes no parent"
        );
        let notices: Vec<(MailKind, bool, String)> = run
            .mail_to("/lead")
            .into_iter()
            .filter(|(kind, _, _)| *kind == MailKind::Notice)
            .collect();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(!notices[0].1);
        assert!(
            notices[0]
                .2
                .contains("/lead/helper was stopped by an agent above you.")
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_coordinator_you_stop_reads_its_workers_notices_and_messages_them_on() {
    let mut run = start(
        "messages-stop-resume",
        Setup::default(),
        vec![
            (
                "/lead",
                vec![
                    uses(&[spawn_call("s1", "helper", "Help.")]),
                    hanging("thinking"),
                    uses(&[send_call("m1", "helper", "Go on.")]),
                    uses(&[report_call("r2", "handed on")]),
                ],
            ),
            (
                "/lead/helper",
                vec![hanging("helping"), uses(&[report_call("r1", "helped")])],
            ),
            ("/", vec![done("ok"), done("ok")]),
        ],
    )
    .await;
    run.spawn(spawn_request("lead", "Lead.")).await;
    run.until("the lead to stream again", |e| {
        matches!(inner(e, "/lead"), Some(AgentEvent::TextDelta { text, .. }) if text == "thinking")
    })
    .await;
    run.until("the helper to stream", |e| {
        matches!(inner(e, "/lead/helper"), Some(AgentEvent::TextDelta { .. }))
    })
    .await;
    run.send(ClientCommand::Child {
        agent: path("/lead"),
        command: Box::new(ClientCommand::Cancel),
    })
    .await;
    run.child_ended("/lead").await;
    run.child_ended("/lead/helper").await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    run.send(ClientCommand::Child {
        agent: path("/lead"),
        command: Box::new(ClientCommand::send("you-1", "Carry on.")),
    })
    .await;
    run.child_ended("/lead").await;
    let lead = run.requests("/lead");
    let first = lead[2].last_user_texts().join("\n");
    assert!(first.contains("Carry on."), "{first}");
    assert!(
        first.contains("/lead/helper was stopped with its parent."),
        "{first}"
    );
    assert_eq!(
        result_of(&lead[3], "m1").1,
        "Sent to /lead/helper (idle: woken). Its report will arrive as a message; don't poll it, and end your turn unless you have other independent work."
    );
    run.child_ended("/lead/helper").await;
    let helper = run.requests("/lead/helper");
    assert_eq!(helper.len(), 2);
    assert!(helper[1].last_user_texts().join("\n").contains("Go on."));
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn esc_on_the_root_leaves_children_running_and_their_reports_wait_for_your_send() {
    let held = Arc::new(Semaphore::new(0));
    let mut run = start(
        "messages-esc",
        Setup::default(),
        vec![
            (
                "/",
                vec![
                    uses(&[spawn_call("s1", "w", "Work.")]),
                    hanging("thinking"),
                    done("got it"),
                ],
            ),
            (
                "/w",
                vec![gated(uses(&[report_call("r1", "worked")]), &held)],
            ),
        ],
    )
    .await;
    run.say("go").await;
    run.until(
        "the root to stream again",
        |e| matches!(e, AgentEvent::TextDelta { text, .. } if text == "thinking"),
    )
    .await;
    run.send(ClientCommand::Cancel).await;
    let root = run.root_ended().await;
    assert!(matches!(root, TurnOutcome::Cancelled { .. }), "{root:?}");
    held.add_permits(1);
    let w = run.child_ended("/w").await;
    assert!(matches!(w, TurnOutcome::Completed { .. }), "{w:?}");
    let held_event = run
        .until("the root's wake to be held", |e| {
            matches!(e, AgentEvent::WakeHeld { .. })
        })
        .await;
    let AgentEvent::WakeHeld { reason, .. } = held_event else {
        unreachable!()
    };
    assert_eq!(reason, HoldReason::Stopped);
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(run.requests("/").len(), 2, "the report waits");
    run.say("next").await;
    run.root_ended().await;
    let root = run.requests("/");
    assert_eq!(root.len(), 3);
    let last = root[2].last_user_texts().join("\n");
    assert!(last.contains("next"), "{last}");
    assert!(last.contains("/w reported"), "{last}");
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn an_ended_agent_refuses_mail_and_frees_its_place() {
    let mut run = start(
        "messages-end",
        Setup {
            limits: Limits {
                max_agents: Some(2),
                ..Limits::default()
            },
            ..Setup::default()
        },
        vec![
            ("/a", vec![uses(&[report_call("r1", "a done")])]),
            (
                "/b",
                vec![
                    uses(&[send_call("b1", "../a", "Still there?")]),
                    uses(&[report_call("r2", "b done")]),
                ],
            ),
            ("/", vec![done("ok"), done("ok"), done("ok")]),
        ],
    )
    .await;
    run.spawn(spawn_request("a", "Work a.")).await;
    run.child_ended("/a").await;
    let full = run.spawn(spawn_request("b", "Work b.")).await;
    run.until(
        "the full tree's refusal",
        |e| matches!(e, AgentEvent::Rejected { request: Some(r), .. } if *r == full),
    )
    .await;
    run.send(ClientCommand::EndAgent { agent: path("/a") })
        .await;
    run.until("a to end", |e| {
        matches!(e, AgentEvent::AgentStateChanged { agent, state: AgentState::Ended, .. } if agent.as_str() == "/a")
    })
    .await;
    run.send(ClientCommand::EndAgent { agent: path("/a") })
        .await;
    let again = run
        .until("ending it again to be refused", |e| {
            matches!(e, AgentEvent::Rejected { .. })
        })
        .await;
    let AgentEvent::Rejected { reason, kind, .. } = again else {
        unreachable!()
    };
    assert_eq!(reason, "/a has ended");
    assert_eq!(kind, Some(RejectKind::Other(NO_SUCH_AGENT.into())));
    run.send(ClientCommand::EndAgent { agent: path("/") }).await;
    let main = run
        .until(
            "ending main to be refused",
            |e| matches!(e, AgentEvent::Rejected { reason, .. } if reason.starts_with("main")),
        )
        .await;
    let AgentEvent::Rejected { reason, kind, .. } = main else {
        unreachable!()
    };
    assert_eq!(reason, "main can't be ended; quit the session to end it");
    assert_eq!(kind, Some(RejectKind::Other(REFUSED.into())));
    run.spawn(spawn_request("b", "Work b.")).await;
    run.until("b", |e| spawned(e, "/b")).await;
    run.child_ended("/b").await;
    let (is_error, text) = result_of(&run.requests("/b")[1], "b1");
    assert!(is_error);
    assert_eq!(text, "/a has ended");
    let run = run.quit().await;
    let ended: Vec<(String, f64)> = pick!(
        run.records("/"),
        LogEvent::AgentEnded {
            agent, spent_usd, ..
        } => (agent.as_str().to_owned(), spent_usd)
    );
    assert_eq!(ended.len(), 1, "{ended:?}");
    assert_eq!(ended[0].0, "/a");
    assert!(
        run.records("/a")
            .iter()
            .any(|e| matches!(e, LogEvent::SessionEnded { .. }))
    );
}

#[tokio::test(start_paused = true)]
async fn a_child_that_reported_takes_follow_up_work_in_the_same_context_until_it_is_ended() {
    let mut run = start(
        "messages-follow-up",
        Setup::default(),
        vec![
            (
                "/",
                vec![
                    uses(&[spawn_call("s1", "reviewer", "Review the change.")]),
                    done("waiting for the review"),
                    uses(&[send_call("m1", "reviewer", "The fix is in; re-check it.")]),
                    done("waiting again"),
                    uses(&[stop_call("k1", "reviewer", true)]),
                    uses(&[send_call("m2", "reviewer", "One more?")]),
                    done("done"),
                ],
            ),
            (
                "/reviewer",
                vec![
                    uses(&[report_call("r1", "two issues")]),
                    uses(&[report_call("r2", "fixed")]),
                ],
            ),
        ],
    )
    .await;
    run.say("go").await;
    run.root_ended().await;
    run.child_ended("/reviewer").await;
    run.root_ended().await;
    run.child_ended("/reviewer").await;
    run.root_ended().await;
    let reviewer = run.requests("/reviewer");
    assert_eq!(reviewer.len(), 2);
    assert!(
        reviewer[1].messages.starts_with(&reviewer[0].messages),
        "the follow-up keeps its earlier turn"
    );
    assert_eq!(reviewer[1].prefix(), reviewer[0].prefix());
    assert!(
        reviewer[1]
            .last_user_texts()
            .join("\n")
            .contains("The fix is in; re-check it.")
    );
    let root = run.requests("/");
    assert_eq!(
        result_of(&root[3], "m1").1,
        "Sent to /reviewer (idle: woken). Its report will arrive as a message; don't poll it, and end your turn unless you have other independent work."
    );
    assert_eq!(
        result_of(&root[5], "k1").1,
        "Ended /reviewer. Ended agents take no more messages and no longer count toward the agent limit."
    );
    let (is_error, text) = result_of(&root[6], "m2");
    assert!(is_error);
    assert_eq!(text, "/reviewer has ended");
    let run = run.quit().await;
    let reports = run
        .mail_to("/")
        .into_iter()
        .filter(|(kind, _, _)| *kind == MailKind::Report)
        .count();
    assert_eq!(reports, 2);
    let records = run.records("/reviewer");
    assert_eq!(
        records
            .iter()
            .filter(|e| matches!(e, LogEvent::SessionStarted { .. }))
            .count(),
        1,
        "one log, one session"
    );
    assert_eq!(turns_started(&run, "/reviewer"), 2);
    assert!(
        run.records("/").iter().any(
            |e| matches!(e, LogEvent::AgentEnded { agent, .. } if agent.as_str() == "/reviewer")
        )
    );
}

pub(super) fn ended_in(run: &Run, agent: &str) -> Vec<String> {
    pick!(run.records(agent), LogEvent::AgentEnded { agent, .. } => agent.as_str().to_owned())
}

fn finished(e: &AgentEvent, agent: Option<&str>, id: &str) -> bool {
    let event = match agent {
        Some(agent) => inner(e, agent),
        None => Some(e),
    };
    matches!(event, Some(AgentEvent::ToolCallFinished { call_id, .. }) if call_id == id)
}

#[tokio::test(start_paused = true)]
async fn mail_from_above_during_the_report_step_starts_a_second_turn_that_reports_again() {
    let held = Arc::new(Semaphore::new(0));
    let lead_gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "messages-report-boundary",
        Setup::default(),
        vec![
            (
                "/lead",
                vec![
                    uses(&[spawn_call("s1", "w", "Check A.")]),
                    gated(uses(&[send_call("p1", "w", "Also check B.")]), &lead_gate),
                    uses(&[report_call("r0", "handed on")]),
                    done("read the first"),
                    done("read the second"),
                    done("x"),
                ],
            ),
            (
                "/lead/w",
                vec![
                    gated(uses(&[report_call("r1", "first")]), &held),
                    uses(&[report_call("r2", "second")]),
                ],
            ),
            ("/", vec![done("ok"), done("ok"), done("ok")]),
        ],
    )
    .await;
    run.spawn(spawn_request("lead", "Lead.")).await;
    run.until("w", |e| spawned(e, "/lead/w")).await;
    sent_times(&run, "/lead/w", 1).await;
    lead_gate.add_permits(1);
    run.until("the lead's message", |e| finished(e, Some("/lead"), "p1"))
        .await;
    held.add_permits(1);
    let first = run.child_ended("/lead/w").await;
    assert!(matches!(first, TurnOutcome::Completed { .. }), "{first:?}");
    let second = run.child_ended("/lead/w").await;
    assert!(
        matches!(second, TurnOutcome::Completed { .. }),
        "{second:?}"
    );
    let w = run.requests("/lead/w");
    assert_eq!(w.len(), 2, "the report ends the first turn");
    let last = w[1].last_user_texts().join("\n");
    assert!(last.contains("Also check B."), "{last}");
    assert!(w[1].messages.starts_with(&w[0].messages));
    tokio::time::sleep(Duration::from_secs(10)).await;
    let run = run.quit().await;
    assert_eq!(turns_started(&run, "/lead/w"), 2);
    let reports: Vec<String> = run
        .mail_to("/lead")
        .into_iter()
        .filter(|(kind, _, _)| *kind == MailKind::Report)
        .map(|(_, _, text)| text)
        .collect();
    assert_eq!(reports.len(), 2, "{reports:?}");
    assert!(reports[0].contains("first"), "{}", reports[0]);
    assert!(reports[1].contains("second"), "{}", reports[1]);
}

#[tokio::test(start_paused = true)]
async fn a_message_to_a_stopped_sibling_waits_held() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "messages-held-sibling",
        Setup::default(),
        vec![
            (
                "/a",
                vec![
                    gated(uses(&[send_call("a1", "../b", "Ping.")]), &gate),
                    uses(&[report_call("r1", "pinged")]),
                ],
            ),
            ("/b", vec![hanging("busy")]),
            ("/", vec![done("ok"), done("ok"), done("ok")]),
        ],
    )
    .await;
    run.spawn(spawn_request("b", "Work b.")).await;
    run.spawn(spawn_request("a", "Work a.")).await;
    run.until("b to stream", |e| {
        matches!(inner(e, "/b"), Some(AgentEvent::TextDelta { .. }))
    })
    .await;
    run.send(ClientCommand::Child {
        agent: path("/b"),
        command: Box::new(ClientCommand::Cancel),
    })
    .await;
    run.child_ended("/b").await;
    gate.add_permits(1);
    run.child_ended("/a").await;
    assert_eq!(
        result_of(&run.requests("/a")[1], "a1").1,
        "Sent to /b (held: it reads this when it next runs)."
    );
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(run.requests("/b").len(), 1, "a sibling doesn't lift a stop");
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_message_that_was_not_a_question_gets_no_unanswered_notice() {
    let held = Arc::new(Semaphore::new(0));
    let mut run = start(
        "messages-fyi",
        Setup::default(),
        vec![
            (
                "/a",
                vec![
                    uses(&[send_call("a1", "../b", "FYI: the API is v2.")]),
                    uses(&[report_call("r1", "told b")]),
                    done("x"),
                ],
            ),
            (
                "/b",
                vec![
                    gated(uses(&[report_call("r2", "b done")]), &held),
                    done("noted"),
                ],
            ),
            ("/", vec![done("ok"), done("ok"), done("ok")]),
        ],
    )
    .await;
    run.spawn(spawn_request("b", "Work b.")).await;
    run.spawn(spawn_request("a", "Work a.")).await;
    run.child_ended("/a").await;
    held.add_permits(1);
    run.child_ended("/b").await;
    run.child_ended("/b").await;
    assert!(
        run.requests("/b")[1]
            .last_user_texts()
            .join("\n")
            .contains("FYI: the API is v2.")
    );
    tokio::time::sleep(Duration::from_secs(10)).await;
    let run = run.quit().await;
    assert!(notices(&run, "/a").is_empty(), "a wasn't waiting on b");
    assert_eq!(run.requests("/a").len(), 2);
    assert_eq!(turns_started(&run, "/a"), 1);
}

#[tokio::test(start_paused = true)]
async fn ending_an_asked_sibling_wakes_the_agent_waiting_on_it() {
    let mut run = start(
        "messages-asked-ended",
        Setup::default(),
        vec![
            (
                "/a",
                vec![
                    uses(&[send_call("a1", "../b", "Which API?")]),
                    done("waiting for b"),
                    uses(&[report_call("r1", "b was ended")]),
                ],
            ),
            ("/b", vec![hanging("busy")]),
            ("/", vec![done("ok"), done("ok"), done("ok")]),
        ],
    )
    .await;
    run.spawn(spawn_request("b", "Work b.")).await;
    run.spawn(spawn_request("a", "Work a.")).await;
    run.child_ended("/a").await;
    let asked = run
        .until("a to settle", |e| idle_as(e, "/a").is_some())
        .await;
    assert_eq!(idle_as(&asked, "/a"), Some(Some(IdleOutcome::Asked)));
    run.send(ClientCommand::EndAgent { agent: path("/b") })
        .await;
    run.child_ended("/a").await;
    let a = run.requests("/a");
    assert_eq!(a.len(), 3);
    let last = a[2].last_user_texts().join("\n");
    assert!(
        last.contains("/b was ended before answering your message."),
        "{last}"
    );
    let run = run.quit().await;
    let notices = notices(&run, "/a");
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].0, "the notice wakes the asker");
}

#[tokio::test(start_paused = true)]
async fn an_ended_childs_parent_is_told_who_ended_it_without_being_woken() {
    let mut run = start(
        "messages-ended-notice",
        Setup::default(),
        vec![
            (
                "/",
                vec![
                    done("noted lead"),
                    uses(&[stop_call("k1", "/lead/v", true)]),
                    done("ended it"),
                    done("ok"),
                ],
            ),
            (
                "/lead",
                vec![
                    uses(&[
                        spawn_call("s1", "w", "Check A."),
                        spawn_call("s2", "v", "Check B."),
                    ]),
                    done("waiting"),
                    uses(&[report_call("r0", "both done")]),
                    done("x"),
                ],
            ),
            ("/lead/w", vec![uses(&[report_call("r1", "A is fine")])]),
            ("/lead/v", vec![uses(&[report_call("r1", "B is fine")])]),
        ],
    )
    .await;
    run.spawn(spawn_request("lead", "Lead.")).await;
    run.child_ended("/lead/w").await;
    run.child_ended("/lead/v").await;
    run.root_ended().await;
    tokio::time::sleep(Duration::from_secs(10)).await;
    run.send(ClientCommand::EndAgent {
        agent: path("/lead/w"),
    })
    .await;
    run.until("w to end", |e| {
        matches!(e, AgentEvent::AgentStateChanged { agent, state: AgentState::Ended, .. } if agent.as_str() == "/lead/w")
    })
    .await;
    run.say("go").await;
    run.root_ended().await;
    assert_eq!(
        result_of(&run.requests("/")[2], "k1").1,
        "Ended /lead/v. Ended agents take no more messages and no longer count toward the agent limit."
    );
    tokio::time::sleep(Duration::from_secs(10)).await;
    let run = run.quit().await;
    let notices = notices(&run, "/lead");
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert!(notices.iter().all(|(wake, _)| !wake), "{notices:?}");
    assert!(notices[0].1.contains("/lead/w was ended by the user."));
    assert!(
        notices[1]
            .1
            .contains("/lead/v was ended by an agent above you.")
    );
    assert!(notices.iter().all(|(_, text)| !text.contains("stopped")));
    assert_eq!(ended_in(&run, "/"), ["/lead/w", "/lead/v"]);
}

#[tokio::test(start_paused = true)]
async fn the_peer_cap_is_reset_by_the_parents_send_message() {
    let gate = Arc::new(Semaphore::new(0));
    let notes: Vec<B> = (0..=MAX_PEER_MESSAGES)
        .map(|i| quiet_call(&format!("n{i}"), "../b", "note"))
        .collect();
    let mut run = start(
        "messages-peer-cap-parent",
        Setup::default(),
        vec![
            (
                "/a",
                vec![
                    uses(&notes),
                    gated(uses(&[quiet_call("again", "../b", "note")]), &gate),
                    uses(&[report_call("r1", "done")]),
                ],
            ),
            ("/b", vec![hanging("busy")]),
            (
                "/",
                vec![
                    uses(&[quiet_call("m1", "b", "Keep going.")]),
                    done("ok"),
                    done("ok"),
                ],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("b", "Work b.")).await;
    run.spawn(spawn_request("a", "Work a.")).await;
    let last = format!("n{MAX_PEER_MESSAGES}");
    run.until("a's last note", |e| finished(e, Some("/a"), &last))
        .await;
    run.say("go").await;
    run.until("your message to b", |e| finished(e, None, "m1"))
        .await;
    gate.add_permits(1);
    run.child_ended("/a").await;
    let a = run.requests("/a");
    assert!(result_of(&a[1], &last).0, "the cap refused the last note");
    let (is_error, text) = result_of(&a[2], "again");
    assert!(!is_error, "the parent's message reset the cap: {text}");
    assert_eq!(send_refusals(&run, "/a").len(), 1);
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn ending_a_running_agent_ends_everything_below_it() {
    let mut run = start(
        "messages-end-running",
        Setup::default(),
        vec![
            (
                "/",
                vec![uses(&[spawn_call("s1", "lead", "Lead.")]), done("spawned")],
            ),
            (
                "/lead",
                vec![
                    uses(&[spawn_call("s2", "w", "Work.")]),
                    uses(&[spawn_call("s3", "v", "Work.")]),
                    hanging("thinking"),
                ],
            ),
            ("/lead/w", vec![hanging("working")]),
            ("/lead/v", vec![hanging("working")]),
        ],
    )
    .await;
    run.say("go").await;
    run.root_ended().await;
    for agent in ["/lead/w", "/lead/v"] {
        run.until(agent, |e| {
            matches!(inner(e, agent), Some(AgentEvent::TextDelta { .. }))
        })
        .await;
    }
    run.until("the lead to stream again", |e| {
        matches!(inner(e, "/lead"), Some(AgentEvent::TextDelta { text, .. }) if text == "thinking")
    })
    .await;
    run.send(ClientCommand::EndAgent {
        agent: path("/lead"),
    })
    .await;
    for agent in ["/lead", "/lead/w", "/lead/v"] {
        run.until("ended", move |e| {
            matches!(
                e,
                AgentEvent::AgentStateChanged {
                    agent: a,
                    state: AgentState::Ended,
                    ..
                } if a.as_str() == agent
            )
        })
        .await;
    }
    tokio::time::sleep(Duration::from_secs(10)).await;
    let run = run.quit().await;
    let mut ended = ended_in(&run, "/");
    ended.sort();
    assert_eq!(ended, ["/lead", "/lead/v", "/lead/w"]);
    for agent in ["/lead", "/lead/w", "/lead/v"] {
        let records = run.records(agent);
        assert!(
            records.iter().any(|e| matches!(
                e,
                LogEvent::TurnEnded {
                    outcome: TurnOutcome::Cancelled { .. },
                    ..
                }
            )),
            "{agent}: its turn was stopped"
        );
        assert!(
            matches!(records.last(), Some(LogEvent::SessionEnded { .. })),
            "{agent}: {:?}",
            records.last()
        );
    }
    let root_notices = notices(&run, "/");
    assert_eq!(root_notices.len(), 1, "{root_notices:?}");
    assert!(!root_notices[0].0);
    assert!(root_notices[0].1.contains("/lead was ended by the user."));
    assert!(
        notices(&run, "/lead").is_empty(),
        "an ended agent is told nothing: {:?}",
        notices(&run, "/lead")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_stop_leaves_the_background_processes_running_and_an_end_stops_them() {
    use super::guarantee::background;
    use crate::test_guard::{alive, gone, pid_in};
    use wisp_protocol::{ProcessEnd, StoppedBy};
    let stop = Arc::new(Semaphore::new(0));
    let end = Arc::new(Semaphore::new(0));
    let mut run = start(
        "messages-end-processes",
        Setup {
            shell: true,
            ..Setup::default()
        },
        vec![
            (
                "/",
                vec![
                    uses(&[spawn_call("s1", "lead", "Lead.")]),
                    gated(uses(&[stop_call("k1", "lead", false)]), &stop),
                    gated(uses(&[stop_call("k2", "lead", true)]), &end),
                    done("ended it"),
                ],
            ),
            (
                "/lead",
                vec![
                    uses(&[
                        spawn_call("s2", "w", "Work."),
                        background(
                            "b1",
                            &format!("trap '' TERM; echo $$ > lead.pid; {}", hold()),
                        ),
                    ]),
                    hanging("thinking"),
                ],
            ),
            (
                "/lead/w",
                vec![
                    uses(&[background("b1", &format!("echo $$ > w.pid; {}", hold()))]),
                    hanging("working"),
                ],
            ),
        ],
    )
    .await;
    run.say("go").await;
    let work = run.env.dir("work");
    let pids = [
        pid_in(&work.join("lead.pid")).await,
        pid_in(&work.join("w.pid")).await,
    ];
    for agent in ["/lead", "/lead/w"] {
        run.until(agent, |e| {
            matches!(inner(e, agent), Some(AgentEvent::TextDelta { .. }))
        })
        .await;
    }
    stop.add_permits(1);
    for agent in ["/lead", "/lead/w"] {
        let outcome = run.child_ended(agent).await;
        assert!(
            matches!(outcome, TurnOutcome::Cancelled { .. }),
            "{outcome:?}"
        );
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    for (agent, pid) in ["/lead", "/lead/w"].into_iter().zip(pids) {
        assert!(alive(pid), "{agent}: a stop ended its process");
        assert!(
            !run.records(agent)
                .iter()
                .any(|e| matches!(e, LogEvent::ProcessEnded { .. })),
            "{agent}"
        );
    }
    end.add_permits(1);
    run.root_ended().await;
    for pid in pids {
        assert!(gone(pid).await, "{pid} outlived its agent's end");
    }
    let run = run.quit().await;
    for agent in ["/lead", "/lead/w"] {
        let records = run.records(agent);
        let ended = records.iter().position(|e| {
            matches!(
                e,
                LogEvent::ProcessEnded {
                    id: 1,
                    end: ProcessEnd::Stopped {
                        by: StoppedBy::SessionEnded
                    },
                    ..
                }
            )
        });
        let session = records
            .iter()
            .position(|e| matches!(e, LogEvent::SessionEnded { .. }));
        assert!(ended.is_some() && ended < session, "{agent}: {records:#?}");
    }
}

#[tokio::test(start_paused = true)]
async fn an_asker_that_was_answered_is_not_told_the_ended_sibling_never_answered() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "messages-answered-ended",
        Setup::default(),
        vec![
            (
                "/a",
                vec![
                    uses(&[send_call("a1", "../b", "Which API?")]),
                    done("waiting for b"),
                    done("x"),
                ],
            ),
            (
                "/b",
                vec![
                    gated(uses(&[quiet_call("b1", "../a", "Use v2.")]), &gate),
                    hanging("busy"),
                ],
            ),
            ("/", vec![done("ok"), done("ok"), done("ok")]),
        ],
    )
    .await;
    run.spawn(spawn_request("b", "Work b.")).await;
    run.spawn(spawn_request("a", "Work a.")).await;
    run.child_ended("/a").await;
    let asked = run
        .until("a to settle", |e| idle_as(e, "/a").is_some())
        .await;
    assert_eq!(idle_as(&asked, "/a"), Some(Some(IdleOutcome::Asked)));
    gate.add_permits(1);
    run.until("b's answer", |e| finished(e, Some("/b"), "b1"))
        .await;
    run.send(ClientCommand::EndAgent { agent: path("/b") })
        .await;
    run.until("b to end", |e| {
        matches!(e, AgentEvent::AgentStateChanged { agent, state: AgentState::Ended, .. } if agent.as_str() == "/b")
    })
    .await;
    tokio::time::sleep(Duration::from_secs(10)).await;
    let run = run.quit().await;
    assert!(notices(&run, "/a").is_empty(), "{:?}", notices(&run, "/a"));
    assert_eq!(run.requests("/a").len(), 2, "nothing woke a");
}

#[test]
fn a_spawn_whose_parent_is_stopped_while_it_builds_is_refused() {
    on_alarm_stack(Timer::Paused, || async {
        let gate = Arc::new(Semaphore::new(0));
        let builds = Arc::new(tokio::sync::Notify::new());
        let mut run = start(
            "messages-stop-spawning",
            Setup {
                child_gate: Some(builds.clone()),
                ..Setup::default()
            },
            vec![
                (
                    "/",
                    vec![
                        uses(&[spawn_call("s1", "lead", "Lead.")]),
                        gated(uses(&[stop_call("k1", "lead", false)]), &gate),
                        done("stopped it"),
                    ],
                ),
                (
                    "/lead",
                    vec![uses(&[spawn_call("s2", "extra", "More.")]), done("x")],
                ),
                ("/lead/extra", vec![hanging("should not run")]),
            ],
        )
        .await;
        run.say("go").await;
        builds.notify_one();
        run.until("the lead", |e| spawned(e, "/lead")).await;
        run.until("the lead to start its spawn", |e| {
        matches!(inner(e, "/lead"), Some(AgentEvent::ToolCallStarted { call_id, .. }) if call_id == "s2")
    })
    .await;
        gate.add_permits(1);
        run.until("the stop", |e| finished(e, None, "k1")).await;
        builds.notify_one();
        run.child_ended("/lead").await;
        run.root_ended().await;
        tokio::time::sleep(Duration::from_secs(10)).await;
        let refusals: Vec<String> = run
            .seen
            .iter()
            .filter_map(|e| match inner(e, "/lead").unwrap_or(e) {
                AgentEvent::AgentRefused {
                    action: AgentAction::Spawn,
                    reason,
                    ..
                } => Some(reason.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            refusals,
            ["/lead was stopped while /lead/extra was starting, so it didn't start"]
        );
        assert!(!run.seen.iter().any(|e| spawned(e, "/lead/extra")));
        assert!(run.requests("/lead/extra").is_empty());
        let run = run.quit().await;
        assert!(run.records("/lead/extra").is_empty());
    });
}

#[tokio::test(start_paused = true)]
async fn ending_a_running_agent_or_same_reply_stop_then_end_is_refused_and_halts_later_calls() {
    use wisp_protocol::{NotRunReason, ToolOutcome};
    use wisp_tool_text::agents::{EndRefusal, LIST_AGENTS, end_refused};
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "messages-end-refused-running",
        Setup::default(),
        vec![
            (
                "/",
                vec![
                    uses(&[spawn_call("s1", "w", "Work.")]),
                    gated(
                        uses(&[
                            call("l0", LIST_AGENTS, json!({})),
                            stop_call("k1", "w", true),
                            send_call("m1", "w", "should not send"),
                            call("l1", LIST_AGENTS, json!({})),
                        ]),
                        &gate,
                    ),
                    uses(&[
                        stop_call("k_miss", "ghost", true),
                        call("l2", LIST_AGENTS, json!({})),
                    ]),
                    uses(&[
                        stop_call("k_stop", "w", false),
                        stop_call("k_same", "w", true),
                        call("l3", LIST_AGENTS, json!({})),
                    ]),
                    uses(&[stop_call("k_end", "w", true)]),
                    done("ended"),
                ],
            ),
            ("/w", vec![hanging("working")]),
        ],
    )
    .await;
    run.say("go").await;
    run.until("w to stream", |e| {
        matches!(inner(e, "/w"), Some(AgentEvent::TextDelta { .. }))
    })
    .await;
    gate.add_permits(1);
    run.root_ended().await;
    let root = run.requests("/");
    let after = crate::agent::not_run_text(&NotRunReason::AfterRefusal);
    let want_running = end_refused("/w", None, EndRefusal::Running);
    assert!(!result_of(&root[2], "l0").0);
    assert_eq!(result_of(&root[2], "k1"), (true, want_running.clone()));
    assert_eq!(result_of(&root[2], "m1"), (true, after.clone()));
    assert_eq!(result_of(&root[2], "l1"), (true, after.clone()));
    assert_eq!(
        root[2]
            .messages
            .last()
            .unwrap()
            .content
            .iter()
            .filter(|b| matches!(b, ContentBlock::ToolResult { .. }))
            .count(),
        4
    );
    assert!(result_of(&root[3], "k_miss").0);
    assert!(!result_of(&root[3], "l2").0);
    assert!(!result_of(&root[4], "k_stop").0);
    assert_eq!(result_of(&root[4], "k_same"), (true, want_running));
    assert_eq!(result_of(&root[4], "l3"), (true, after));
    assert_eq!(
        result_of(&root[5], "k_end"),
        (
            false,
            "Ended /w. Ended agents take no more messages and no longer count toward the agent limit."
                .to_owned()
        )
    );
    for skipped in ["m1", "l1", "l3"] {
        assert!(
            !run.seen.iter().any(|e| matches!(
                e,
                AgentEvent::ToolCallStarted { call_id, .. } if call_id == skipped
            )),
            "{skipped} should not have started"
        );
        assert!(
            run.seen.iter().any(|e| matches!(
                e,
                AgentEvent::ToolCallFinished {
                    call_id,
                    outcome: ToolOutcome::NotRun {
                        reason: NotRunReason::AfterRefusal
                    },
                    ..
                } if call_id == skipped
            )),
            "{skipped} should finish AfterRefusal"
        );
    }
    let stop_refusals = run
        .seen
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::AgentRefused {
                    action: AgentAction::Stop,
                    ..
                }
            )
        })
        .count();
    assert_eq!(stop_refusals, 3);
    let run = run.quit().await;
    assert!(
        run.mail_to("/w")
            .iter()
            .all(|(_, _, text)| !text.contains("should not send"))
    );
    assert_eq!(ended_in(&run, "/"), ["/w"]);
}

#[tokio::test(start_paused = true)]
async fn ending_a_target_with_an_unread_report_or_mid_reply_exit_without_report_is_refused() {
    use wisp_protocol::NotRunReason;
    use wisp_tool_text::agents::{EndRefusal, LIST_AGENTS, end_refused};
    let report_gate = Arc::new(Semaphore::new(0));
    let root_gate1 = Arc::new(Semaphore::new(0));
    let fail_gate = Arc::new(Semaphore::new(0));
    let root_gate2 = Arc::new(Semaphore::new(0));
    let mut run = start(
        "messages-end-unread-and-mid-exit",
        Setup::default(),
        vec![
            (
                "/",
                vec![
                    uses(&[spawn_call("s1", "w", "Report.")]),
                    gated(
                        uses(&[
                            stop_call("k1", "w", true),
                            call("l1", LIST_AGENTS, json!({})),
                        ]),
                        &root_gate1,
                    ),
                    uses(&[stop_call("k2", "w", true)]),
                    uses(&[spawn_call("s2", "f", "Fail.")]),
                    gated(
                        uses(&[
                            stop_call("k3", "f", true),
                            call("l2", LIST_AGENTS, json!({})),
                        ]),
                        &root_gate2,
                    ),
                    uses(&[stop_call("k4", "f", true)]),
                    done("all ended"),
                ],
            ),
            (
                "/w",
                vec![gated(uses(&[report_call("r1", "done")]), &report_gate)],
            ),
            ("/f", vec![gated(refused_request(), &fail_gate)]),
        ],
    )
    .await;
    run.say("go").await;
    while run.requests("/").len() < 2 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    report_gate.add_permits(1);
    run.child_ended("/w").await;
    run.until("w settled idle", |e| {
        idle_as(e, "/w") == Some(Some(IdleOutcome::Reported))
    })
    .await;
    root_gate1.add_permits(1);
    while run.requests("/").len() < 5 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    fail_gate.add_permits(1);
    run.child_ended("/f").await;
    run.until("f settled failed", |e| {
        idle_as(e, "/f") == Some(Some(IdleOutcome::Failed))
    })
    .await;
    root_gate2.add_permits(1);
    run.root_ended().await;
    let root = run.requests("/");
    let after = crate::agent::not_run_text(&NotRunReason::AfterRefusal);
    assert_eq!(
        result_of(&root[2], "k1"),
        (true, end_refused("/w", None, EndRefusal::UnreadReport))
    );
    assert_eq!(result_of(&root[2], "l1"), (true, after.clone()));
    assert!(!result_of(&root[3], "k2").0);
    assert_eq!(
        result_of(&root[5], "k3"),
        (true, end_refused("/f", None, EndRefusal::Running))
    );
    assert_eq!(result_of(&root[5], "l2"), (true, after));
    assert!(!result_of(&root[6], "k4").0);
    let run = run.quit().await;
    let mut ended = ended_in(&run, "/");
    ended.sort();
    assert_eq!(ended, ["/f", "/w"]);
}

#[tokio::test(start_paused = true)]
async fn ending_an_idle_target_with_a_running_descendant_is_refused() {
    use wisp_tool_text::agents::{EndRefusal, end_refused};
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "messages-end-running-descendant",
        Setup::default(),
        vec![
            (
                "/",
                vec![
                    uses(&[spawn_call("s1", "lead", "Lead.")]),
                    gated(uses(&[stop_call("k1", "lead", true)]), &gate),
                    uses(&[stop_call("k2", "lead", false)]),
                    uses(&[stop_call("k3", "lead", true)]),
                    done("ended"),
                ],
            ),
            (
                "/lead",
                vec![uses(&[spawn_call("s2", "w", "Work.")]), done("waiting")],
            ),
            ("/lead/w", vec![hanging("working")]),
        ],
    )
    .await;
    run.say("go").await;
    run.child_ended("/lead").await;
    run.until("w streaming", |e| {
        matches!(inner(e, "/lead/w"), Some(AgentEvent::TextDelta { .. }))
    })
    .await;
    gate.add_permits(1);
    run.root_ended().await;
    let root = run.requests("/");
    assert_eq!(
        result_of(&root[2], "k1"),
        (
            true,
            end_refused("/lead", Some("/lead/w"), EndRefusal::Running)
        )
    );
    assert!(!result_of(&root[3], "k2").0);
    assert_eq!(
        result_of(&root[4], "k3"),
        (
            false,
            "Ended /lead and 1 agent below it. Ended agents take no more messages and no longer count toward the agent limit."
                .to_owned()
        )
    );
    let run = run.quit().await;
    assert!(notices(&run, "/").is_empty(), "the caller isn't told");
    let mut ended = ended_in(&run, "/");
    ended.sort();
    assert_eq!(ended, ["/lead", "/lead/w"]);
}

#[tokio::test(start_paused = true)]
async fn ending_a_coordinator_waking_on_a_grandchild_report_is_refused() {
    use wisp_tool_text::agents::{EndRefusal, end_refused};
    let w_gate = Arc::new(Semaphore::new(0));
    let at_select = pause_at("/lead", "restart-idle-select");
    let mut run = start(
        "messages-end-waking-grandchild-report",
        Setup::default(),
        vec![
            (
                "/",
                vec![
                    uses(&[stop_call("k1", "lead", true)]),
                    done("refused"),
                    done("got lead report"),
                ],
            ),
            (
                "/lead",
                vec![
                    uses(&[spawn_call("s1", "w", "Work.")]),
                    done("waiting"),
                    uses(&[report_call("r0", "relayed")]),
                ],
            ),
            (
                "/lead/w",
                vec![gated(
                    uses(&[report_call("r1", "grandchild findings")]),
                    &w_gate,
                )],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("lead", "Lead.")).await;
    within("/lead idle select", at_select.wait()).await;
    w_gate.add_permits(1);
    run.child_ended("/lead/w").await;
    run.until("/lead/w settled", |e| {
        idle_as(e, "/lead/w") == Some(Some(IdleOutcome::Reported))
    })
    .await;
    run.say("go").await;
    run.root_ended().await;
    assert_eq!(
        result_of(&run.requests("/")[1], "k1"),
        (true, end_refused("/lead", None, EndRefusal::Running))
    );
    within("release /lead", at_select.wait()).await;
    run.child_ended("/lead").await;
    run.root_ended().await;
    assert!(
        run.requests("/lead")[2]
            .all_text()
            .contains("grandchild findings")
    );
    let run = run.quit().await;
    assert!(ended_in(&run, "/").is_empty());
}

#[tokio::test(start_paused = true)]
async fn waking_an_idle_child_and_ending_it_in_the_same_reply_is_refused() {
    use wisp_protocol::NotRunReason;
    use wisp_tool_text::agents::{EndRefusal, LIST_AGENTS, end_refused};
    let at_select = pause_at("/w", "restart-idle-select");
    let mut run = start(
        "messages-end-same-reply-wake",
        Setup::default(),
        vec![
            (
                "/",
                vec![
                    done("got first"),
                    uses(&[
                        send_call("m1", "w", "Second task."),
                        stop_call("k1", "w", true),
                        call("l1", LIST_AGENTS, json!({})),
                    ]),
                    done("refused"),
                    done("got second"),
                ],
            ),
            (
                "/w",
                vec![
                    uses(&[report_call("r1", "first")]),
                    uses(&[report_call("r2", "second")]),
                ],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("w", "First task.")).await;
    run.child_ended("/w").await;
    run.root_ended().await;
    within("/w idle select", at_select.wait()).await;
    run.say("go").await;
    run.root_ended().await;
    let root = run.requests("/");
    assert!(!result_of(&root[2], "m1").0);
    assert_eq!(
        result_of(&root[2], "k1"),
        (true, end_refused("/w", None, EndRefusal::Running))
    );
    assert_eq!(
        result_of(&root[2], "l1"),
        (
            true,
            crate::agent::not_run_text(&NotRunReason::AfterRefusal)
        )
    );
    within("release /w", at_select.wait()).await;
    run.child_ended("/w").await;
    run.root_ended().await;
    assert_eq!(run.requests("/w").len(), 2);
    let run = run.quit().await;
    assert!(ended_in(&run, "/").is_empty());
}

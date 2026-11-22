use tokio::sync::Semaphore;
use wisp_protocol::log::LogRecord;
use wisp_protocol::{AgentPath, MailId, MailKind, RequestId, TurnOrigin, WithdrawnBy};

use super::*;
use crate::agent::mail::Mail;
use crate::agent::tests::question;
use crate::provider::reply_text;
use crate::test_guard::{until_event, until_turn_end};
use crate::tool::ToolEntry;
use crate::tool::question;
use wisp_plugin_ask_user as ask_user;

const KEEP: &str = "The user answered:\n\n1. [Plan] Which plan should go ahead?\n   Keep the plan";
const CHANGE: &str =
    "The user answered:\n\n1. [Plan] Which plan should go ahead?\n   Change the plan";

fn with_ask_user(mut tools: Vec<ToolEntry>) -> Toolbox {
    tools.push(crate::tool::testkit::ask_user());
    Toolbox::new(tools)
}

fn toolbox() -> Toolbox {
    with_ask_user(samples().into_iter().map(|s| s.tool).collect())
}

fn asker(provider: Arc<Fake>, env: &Env, user: bool) -> Agent {
    asker_with(provider, env, toolbox(), user)
}

fn ask(id: &str) -> B {
    call(id, "ask_user", question())
}

fn withdraw(id: &str) -> B {
    call(id, "ask_user", json!({"withdraw": true}))
}

fn user_text(message: &Message) -> String {
    reply_text(&message.content).unwrap_or_default()
}

fn last_text(message: &Message) -> &str {
    message
        .content
        .iter()
        .rev()
        .find_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or_default()
}

fn answered(call_id: &str, choice: u32) -> (String, QuestionEnd) {
    (
        call_id.to_owned(),
        QuestionEnd::Answered {
            answers: vec![Answer::choose(choice)],
        },
    )
}

fn failed_call(call_id: &str, message: &str) -> (String, ToolOutcome) {
    (
        call_id.to_owned(),
        ToolOutcome::Failed {
            message: message.into(),
            before_start: false,
        },
    )
}

fn check_posted(events: &[AgentEvent]) {
    let mut open: Option<String> = None;
    for e in events {
        match e {
            AgentEvent::QuestionAsked { call_id, .. } => {
                assert_eq!(open, None, "{events:#?}");
                open = Some(call_id.clone());
            }
            AgentEvent::QuestionClosed { call_id, .. } => {
                assert_eq!(open.take().as_ref(), Some(call_id), "{events:#?}");
            }
            _ => {}
        }
    }
}

struct Live {
    commands: mpsc::Sender<ClientCommand>,
    events: mpsc::Receiver<AgentEvent>,
    task: tokio::task::JoinHandle<Agent>,
}

impl Live {
    fn start(mut agent: Agent) -> Self {
        let (commands, mut cmd_rx) = mpsc::channel(64);
        let (ev_tx, events) = mpsc::channel(4096);
        let task = tokio::spawn(async move {
            agent.serve(&mut cmd_rx, &ev_tx).await;
            agent
        });
        Self {
            commands,
            events,
            task,
        }
    }

    async fn send(&self, command: ClientCommand) {
        self.commands.send(command).await.unwrap();
    }

    async fn until(&mut self, what: &str, done: impl Fn(&AgentEvent) -> bool) -> Vec<AgentEvent> {
        let mut seen = Vec::new();
        until_event(&mut self.events, &mut seen, what, done).await;
        seen
    }

    async fn turn_end(&mut self) -> Vec<AgentEvent> {
        let mut seen = Vec::new();
        until_turn_end(&mut self.events, &mut seen).await;
        seen
    }

    async fn turn(&mut self, id: &str, text: &str) -> Vec<AgentEvent> {
        self.send(send(id, text)).await;
        self.turn_end().await
    }

    async fn finish(self) -> Agent {
        drop(self.commands);
        let mut events = self.events;
        while events.recv().await.is_some() {}
        self.task.await.unwrap()
    }
}

fn logged_live(name: &str, steps: Vec<Step>) -> (Env, MemoryStore, Arc<Fake>, Live) {
    let env = Env::new(name);
    let store = MemoryStore::default();
    let provider = scripted(steps);
    let agent = asker(provider.clone(), &env, true).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    (env, store, provider, Live::start(agent))
}

fn resumed_agent(
    env: &Env,
    saved: &[LogRecord],
    steps: Vec<Step>,
) -> (MemoryStore, Arc<Fake>, Agent) {
    let (store, log) = continued(saved);
    let provider = scripted(steps);
    let agent = asker(provider.clone(), env, true)
        .with_log(log, log_context())
        .resume(saved, 0)
        .unwrap();
    (store, provider, agent)
}

#[tokio::test]
async fn every_ending_of_ask_user_gives_one_close_one_finish_and_its_exact_text() {
    let arrangements: [&[&str]; 4] = [
        &["ask_user"],
        &["ask_user", "sleep"],
        &["sleep", "ask_user"],
        &["ask_user", "ask_user"],
    ];
    for user in [true, false] {
        for arrangement in arrangements {
            let label = format!("user={user} {arrangement:?}");
            let env = Env::new("ask-user-matrix");
            let ids: Vec<String> = arrangement
                .iter()
                .enumerate()
                .map(|(i, name)| format!("{name}{i}"))
                .collect();
            let reply: Vec<B> = arrangement
                .iter()
                .zip(&ids)
                .map(|(name, id)| match *name {
                    "ask_user" => ask(id),
                    _ => run(id),
                })
                .collect();
            let provider = scripted(vec![uses(&reply), done("ok")]);
            let mut agent = asker(provider.clone(), &env, user);
            let (outcome, events) = reacting(&mut agent, "go", |_| React::Wait).await;
            check_events(&events);
            check_posted(&events);
            assert_layout(&provider);

            let first = arrangement.iter().position(|n| *n == "ask_user").unwrap();
            let asked: Vec<&String> =
                pick!(&events, AgentEvent::QuestionAsked { call_id, .. } => call_id);
            if user {
                assert_eq!(asked, [&ids[first]], "{label}");
            } else {
                assert!(asked.is_empty(), "{label}");
            }
            assert!(closings(&events).is_empty(), "{label}");

            let (outcome_q, text_q) = if user {
                (
                    ToolOutcome::Completed {
                        is_error: false,
                        exit: None,
                    },
                    question::posted(),
                )
            } else {
                (
                    ToolOutcome::Failed {
                        message: question::UNAVAILABLE_MESSAGE.into(),
                        before_start: false,
                    },
                    question::UNAVAILABLE.to_owned(),
                )
            };
            let done_calls = finished(&events);
            assert_eq!(done_calls.len(), ids.len(), "{label}");
            let messages = agent.history();
            let results_message = messages
                .iter()
                .rev()
                .find(|m| m.role == Role::User && !results(m).is_empty())
                .unwrap();
            let texts = results(results_message);
            assert_eq!(texts.len(), ids.len(), "{label}");
            for (i, id) in ids.iter().enumerate() {
                let (_, outcome) = &done_calls[i];
                let (_, text, is_error) = texts[i];
                if i < first {
                    assert!(matches!(outcome, ToolOutcome::Completed { .. }), "{label}");
                } else if i == first {
                    assert_eq!(outcome, &outcome_q, "{label}");
                    assert_eq!(text, text_q, "{label}");
                    assert_eq!(is_error, outcome_q.is_error(), "{label}");
                    assert_eq!(
                        finished_result(&events, id).as_deref(),
                        Some(text_q.as_str()),
                        "{label}"
                    );
                } else {
                    let reason = NotRunReason::AfterQuestion;
                    assert_eq!(text, not_run_text(&reason), "{label}");
                    assert_eq!(outcome, &ToolOutcome::NotRun { reason }, "{label}");
                    assert!(is_error, "{label}");
                }
            }
            assert_eq!(started_ids(&events), ids[..=first].to_vec(), "{label}");
            assert!(
                matches!(outcome, TurnOutcome::Completed { steps: 2, .. }),
                "{label}: {outcome:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_idle_answer_opens_a_turn_with_plain_user_text_and_keeps_one_prefix() {
    let env = Env::new("ask-user-idle-answer");
    let provider = scripted(vec![
        uses(&[ask("q")]),
        done("waiting"),
        done("followed"),
        uses(&[ask("q2")]),
        done("asked-again"),
    ]);
    let mut live = Live::start(asker(provider.clone(), &env, true));
    let first = live.turn("m1", "go").await;
    assert!(closings(&first).is_empty());
    live.send(answer("q", vec![Answer::choose(0)])).await;
    let second = live.turn_end().await;
    assert_eq!(closings(&second), [answered("q", 0)]);
    let origins: Vec<Option<TurnOrigin>> =
        pick!(&second, AgentEvent::TurnStarted { origin, .. } => *origin);
    assert_eq!(origins, [Some(TurnOrigin::Mail)]);
    let third = live.turn("m2", "ask again").await;
    assert_eq!(
        finished_result(&third, "q2").as_deref(),
        Some(question::posted().as_str())
    );
    let agent = live.finish().await;
    let user_turn = &provider.sent()[2];
    let text = user_text(user_turn.messages.last().unwrap());
    assert!(text.contains(KEEP), "{text}");
    assert!(!text.contains("<wisp:message"), "{text}");
    assert!(agent.history().len() >= 6);
    same_prefix(&provider.texts());
}

#[tokio::test]
async fn an_answer_during_work_lands_at_the_next_step_and_keeps_the_turn_going() {
    let env = Env::new("ask-user-mid-turn");
    #[cfg(unix)]
    {
        let tools = crate::tool::testkit::shell(crate::tool::testkit::timing(50, 100))
            .with_entries([crate::tool::testkit::ask_user()]);
        let provider = scripted(vec![
            uses(&[ask("q")]),
            uses(&[call(
                "bg",
                "bash",
                json!({"command": crate::test_guard::hold(), "run_in_background": true}),
            )]),
            uses(&[call("w1", "process", json!({"id": 1, "wait_ms": 10_000}))]),
            uses(&[ask("q2")]),
            done("heard"),
        ]);
        let mut live = Live::start(asker_with(provider.clone(), &env, tools, true));
        live.send(send("m1", "go")).await;
        let to_wait = live
            .until(
                "w1 to start",
                |e| matches!(e, AgentEvent::ToolCallStarted { call_id, .. } if call_id == "w1"),
            )
            .await;
        assert!(closings(&to_wait).is_empty());
        live.send(answer("q", vec![Answer::choose(0)])).await;
        let rest = live.turn_end().await;
        let _ = live.finish().await;
        assert_eq!(closings(&rest), [answered("q", 0)]);
        assert!(
            finished_result(&rest, "w1")
                .is_some_and(|r| r.contains(wisp_plugin_shell::STEERED_NOTE)),
            "{rest:#?}"
        );
        assert_eq!(
            finished_result(&rest, "q2").as_deref(),
            Some(question::posted().as_str())
        );
        let ended = rest.iter().find_map(|e| match e {
            AgentEvent::TurnEnded { outcome, .. } => Some(outcome.clone()),
            _ => None,
        });
        assert!(
            matches!(ended, Some(TurnOutcome::Completed { steps: 5, .. })),
            "{ended:?}"
        );
        let text = user_text(provider.sent()[3].messages.last().unwrap());
        assert!(text.contains(KEEP), "{text}");
        assert!(!text.contains("<wisp:message"), "{text}");
        same_prefix(&provider.texts());
    }
    #[cfg(not(unix))]
    {
        let gate = Arc::new(Semaphore::new(0));
        let provider = scripted(vec![
            uses(&[ask("q")]),
            uses(&[run("s1")]).gated(&gate),
            uses(&[ask("q2")]),
            done("heard"),
        ]);
        let mut live = Live::start(asker(provider.clone(), &env, true));
        live.send(send("m1", "go")).await;
        live.until("step 2 to start", |e| {
            matches!(e, AgentEvent::StepStarted { step: 2, .. })
        })
        .await;
        live.send(answer("q", vec![Answer::choose(0)])).await;
        let closed = live
            .until("question to close", |e| {
                matches!(e, AgentEvent::QuestionClosed { .. })
            })
            .await;
        assert_eq!(closings(&closed), [answered("q", 0)]);
        gate.add_permits(1);
        let rest = live.turn_end().await;
        let _ = live.finish().await;
        assert_eq!(
            finished_result(&rest, "q2").as_deref(),
            Some(question::posted().as_str())
        );
        let text = user_text(provider.sent()[2].messages.last().unwrap());
        assert!(text.contains(KEEP), "{text}");
        assert!(!text.contains("<wisp:message"), "{text}");
        same_prefix(&provider.texts());
    }
}

#[tokio::test]
async fn steer_mail_and_process_end_reach_the_agent_while_a_question_is_open() {
    let env = Env::new("ask-user-saib");
    let gate = Arc::new(Semaphore::new(0));
    #[cfg(unix)]
    let tools = crate::tool::testkit::shell(crate::tool::testkit::timing(50, 100))
        .with_entries([crate::tool::testkit::ask_user()]);
    #[cfg(not(unix))]
    let tools = Toolbox::new(vec![crate::tool::testkit::ask_user()]);
    let provider = scripted(vec![
        uses(&[ask("q")]),
        done("asked").gated(&gate),
        done("steered"),
        done("mailed"),
        #[cfg(unix)]
        uses(&[call(
            "bg",
            "bash",
            json!({"command": format!("{}; echo done", crate::test_guard::wait_for("go")), "run_in_background": true}),
        )]),
        #[cfg(unix)]
        done("started"),
        #[cfg(unix)]
        done("process-woke"),
        done("answered"),
    ]);
    let agent = asker_with(provider.clone(), &env, tools, true);
    let mail = agent.mail_sender();
    let mut live = Live::start(agent);
    live.send(send("m1", "ask first")).await;
    live.until("step 2 to start", |e| {
        matches!(e, AgentEvent::StepStarted { step: 2, .. })
    })
    .await;
    live.send(send("m2", "a steer while open")).await;
    live.until(
        "steer accepted",
        |e| matches!(e, AgentEvent::InputAccepted { id, .. } if id.as_str() == "m2"),
    )
    .await;
    gate.add_permits(1);
    let first = live.turn_end().await;
    assert!(closings(&first).is_empty());
    assert!(first.iter().any(|e| matches!(
        e,
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Completed { steps: 3, .. },
            ..
        }
    )));
    assert!(user_text(provider.sent()[2].messages.last().unwrap()).contains("a steer while open"));

    mail.send(Mail {
        id: MailId("m-1".into()),
        from: AgentPath::new("/worker"),
        to: AgentPath::root(),
        kind: MailKind::Report,
        wake: true,
        text: "child report arrived".into(),
    })
    .unwrap();
    live.turn_end().await;
    assert!(
        user_text(provider.sent()[3].messages.last().unwrap()).contains("child report arrived")
    );

    #[cfg(unix)]
    {
        live.turn("m3", "start bg").await;
        fs::write(env.work.join("go"), "go").unwrap();
        let woke = live.turn_end().await;
        let origins: Vec<Option<TurnOrigin>> =
            pick!(&woke, AgentEvent::TurnStarted { origin, .. } => *origin);
        assert_eq!(origins, [Some(TurnOrigin::ProcessExit)]);
    }

    live.send(answer("q", vec![Answer::choose(0)])).await;
    let answered_turn = live.turn_end().await;
    assert_eq!(closings(&answered_turn), [answered("q", 0)]);
    let _ = live.finish().await;
    let final_text = user_text(provider.sent().last().unwrap().messages.last().unwrap());
    assert!(final_text.contains(KEEP), "{final_text}");
}

#[tokio::test]
async fn withdraw_closes_open_questions_reports_nothing_open_and_second_ask_is_busy() {
    let env = Env::new("ask-user-withdraw-busy");
    let provider = scripted(vec![
        uses(&[withdraw("w0")]),
        uses(&[ask("q1")]),
        uses(&[ask("q2")]),
        uses(&[withdraw("w1")]),
        uses(&[withdraw("w2")]),
        uses(&[ask("q3")]),
        done("done"),
    ]);
    let mut agent = asker(provider.clone(), &env, true);
    let (outcome, events) = reacting(&mut agent, "go", |_| React::Wait).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { steps: 7, .. }),
        "{outcome:?}"
    );
    let done_calls = finished(&events);
    for (id, text) in [
        ("w0", question::NOTHING_OPEN),
        ("q1", question::posted().as_str()),
        ("q2", question::busy().as_str()),
        ("w1", question::WITHDRAWN),
        ("w2", question::NOTHING_OPEN),
        ("q3", question::posted().as_str()),
    ] {
        assert_eq!(finished_result(&events, id).as_deref(), Some(text), "{id}");
    }
    for expected in [
        failed_call("w0", question::NOTHING_OPEN_MESSAGE),
        failed_call("q2", question::BUSY_MESSAGE),
        failed_call("w2", question::NOTHING_OPEN_MESSAGE),
    ] {
        assert!(done_calls.contains(&expected), "{expected:?}");
    }
    assert_eq!(
        closings(&events),
        [("q1".to_owned(), QuestionEnd::Withdrawn)]
    );
    let withdraw_display = events.iter().find_map(|e| match e {
        AgentEvent::ToolCallRequested {
            call_id, display, ..
        } if call_id == "w1" => Some(display.clone()),
        _ => None,
    });
    assert_eq!(
        withdraw_display
            .as_ref()
            .and_then(|d| d.verbs.as_ref())
            .map(|v| v.done.as_str()),
        Some("Withdrew")
    );
    assert_eq!(
        withdraw_display.as_ref().map(|d| d.target.as_str()),
        Some("the question")
    );
    check_posted(&events);
}

#[tokio::test]
async fn stop_idle_and_in_turn_closes_posted_and_next_request_carries_note_once() {
    for in_turn in [false, true] {
        let env = Env::new("ask-user-stop");
        let steps = if in_turn {
            vec![
                uses(&[ask("q")]),
                uses(&[call("s", "sleep", json!({"ms": 5000, "text": ""}))]),
                done("after-stop"),
                uses(&[ask("q2")]),
                done("asked-after-stop"),
            ]
        } else {
            vec![
                uses(&[ask("q")]),
                done("waiting"),
                done("after-stop"),
                uses(&[ask("q2")]),
                done("asked-after-stop"),
            ]
        };
        let provider = scripted(steps);
        let mut live = Live::start(asker(provider.clone(), &env, true));
        if in_turn {
            live.send(send("m1", "go")).await;
            live.until(
                "sleep to start",
                |e| matches!(e, AgentEvent::ToolCallStarted { call_id, .. } if call_id == "s"),
            )
            .await;
            live.send(ClientCommand::Cancel).await;
            let stopped = live.turn_end().await;
            assert_eq!(closings(&stopped), [("q".to_owned(), QuestionEnd::Stopped)]);
        } else {
            live.turn("m1", "go").await;
            live.send(ClientCommand::Cancel).await;
            let closed = live
                .until("posted question to close", |e| {
                    matches!(e, AgentEvent::QuestionClosed { .. })
                })
                .await;
            assert_eq!(closings(&closed), [("q".to_owned(), QuestionEnd::Stopped)]);
        }
        let sent_before = provider.sent().len();
        live.turn("m2", "continue").await;
        let sent_after = provider.sent();
        assert_eq!(sent_after.len(), sent_before + 1);
        let note_req = user_text(sent_after[sent_before].messages.last().unwrap());
        assert!(note_req.contains(question::CLOSED_NOTE), "{note_req}");
        assert!(!note_req.contains("<wisp:message"), "{note_req}");

        let third = live.turn("m3", "ask again").await;
        assert_eq!(
            finished_result(&third, "q2").as_deref(),
            Some(question::posted().as_str())
        );
        let later_req = user_text(provider.sent()[sent_before + 1].messages.last().unwrap());
        assert!(!later_req.contains(question::CLOSED_NOTE), "{later_req}");
        let _ = live.finish().await;
        same_prefix(&provider.texts());
    }
}

#[tokio::test]
async fn a_blocking_confirm_opens_while_a_posted_question_waits_and_each_closes_once() {
    let env = Env::new("ask-user-and-confirm");
    let mut tools: Vec<ToolEntry> = samples().into_iter().map(|s| s.tool).collect();
    tools.push(crate::tool::testkit::ask_user());
    tools.push(confirm_tool());
    let provider = scripted(vec![
        uses(&[ask("q-posted")]),
        uses(&[call("q-block", "confirm", json!({}))]),
        done("after-confirm"),
        done("after-posted"),
    ]);
    let mut live = Live::start(asker_with(
        provider.clone(),
        &env,
        Toolbox::new(tools),
        true,
    ));
    live.send(send("m1", "go")).await;
    live.until(
        "confirm card",
        |e| matches!(e, AgentEvent::QuestionAsked { call_id, .. } if call_id == "q-block"),
    )
    .await;
    live.send(answer("q-block", vec![Answer::choose(0)])).await;
    let ended = live.turn_end().await;
    assert_eq!(closings(&ended), [answered("q-block", 0)]);
    live.send(answer("q-posted", vec![Answer::choose(1)])).await;
    let second = live.turn_end().await;
    assert_eq!(closings(&second), [answered("q-posted", 1)]);
    let agent = live.finish().await;
    let history = agent.history();
    let last_user = history.iter().rev().find(|m| m.role == Role::User).unwrap();
    assert!(user_text(last_user).contains(CHANGE));
}

#[tokio::test]
async fn resume_reopens_a_posted_question_and_still_repairs_an_old_blocking_one() {
    let (env, store, provider, mut live) =
        logged_live("ask-user-resume", vec![uses(&[ask("q1")]), done("waiting")]);
    live.turn("m1", "go").await;
    let first_agent = live.finish().await;
    let before_len = first_agent.history().len();
    let saved = records(&store);

    let view = crate::resume::transcript(&saved, &toolbox(), None);
    assert!(
        view.iter()
            .any(|e| matches!(e, AgentEvent::QuestionAsked { call_id, .. } if call_id == "q1")),
        "resumed view shows the open question row: {view:#?}"
    );

    let (store2, provider2, resumed) =
        resumed_agent(&env, &saved, vec![done("answered-after-resume")]);
    assert_eq!(resumed.history().len(), before_len);
    let mut live2 = Live::start(resumed);
    live2.send(answer("q1", vec![Answer::choose(0)])).await;
    let events2 = live2.turn_end().await;
    check_posted(&events2);
    let asked_again: Vec<&String> =
        pick!(&events2, AgentEvent::QuestionAsked { call_id, .. } => call_id);
    assert_eq!(asked_again, ["q1"]);
    assert_eq!(closings(&events2), [answered("q1", 0)]);
    let _ = live2.finish().await;
    let after_records: Vec<LogEvent> = log_events(&store2)[saved.len()..].to_vec();
    assert!(
        !after_records
            .iter()
            .any(|e| matches!(e, LogEvent::QuestionAsked { .. })),
        "resume announces without writing a new question_asked: {after_records:#?}"
    );
    let mut bodies = provider.texts();
    bodies.extend(provider2.texts());
    same_prefix(&bodies);

    let records_after_answer = records(&store2);
    let (_, _, resumed3) = resumed_agent(
        &env,
        &records_after_answer,
        vec![uses(&[ask("q2")]), done("asked-again")],
    );
    assert!(resumed3.posted_event().is_none());
    let mut live3 = Live::start(resumed3);
    let turn3 = live3.turn("m-next", "ask another").await;
    assert_eq!(
        finished_result(&turn3, "q2").as_deref(),
        Some(question::posted().as_str())
    );
    let _ = live3.finish().await;

    let closed_pos = records_after_answer
        .iter()
        .position(|r| matches!(r.event, LogEvent::QuestionClosed { .. }))
        .unwrap();
    let (crashed_store, _, crashed_close) = resumed_agent(
        &env,
        &records_after_answer[..=closed_pos],
        vec![done("answered-after-crash")],
    );
    let mut crashed_live = Live::start(crashed_close);
    crashed_live
        .until(
            "reopened question",
            |e| matches!(e, AgentEvent::QuestionAsked { call_id, .. } if call_id == "q1"),
        )
        .await;
    crashed_live
        .send(answer("q1", vec![Answer::choose(1)]))
        .await;
    let crashed_events = crashed_live.turn_end().await;
    assert_eq!(closings(&crashed_events), [answered("q1", 1)]);
    let crashed_agent = crashed_live.finish().await;
    let crashed_log = log_events(&crashed_store);
    let asked_count = crashed_log
        .iter()
        .filter(|e| matches!(e, LogEvent::QuestionAsked { .. }))
        .count();
    let closed_count = crashed_log
        .iter()
        .filter(|e| matches!(e, LogEvent::QuestionClosed { .. }))
        .count();
    assert_eq!((asked_count, closed_count), (2, 2));
    assert!(
        crashed_agent
            .compaction
            .said_texts()
            .iter()
            .any(|s| s.contains("Change the plan")),
        "{:?}",
        crashed_agent.compaction.said_texts()
    );

    let finished_pos = saved
        .iter()
        .position(
            |r| matches!(&r.event, LogEvent::ToolCallFinished { call_id, .. } if call_id == "q1"),
        )
        .unwrap();
    let mid_records = saved[..=finished_pos].to_vec();
    let (mid_store, mid_provider, mid_resumed) =
        resumed_agent(&env, &mid_records, vec![done("after-mid-resume")]);
    assert!(matches!(
        mid_resumed.posted_event(),
        Some(AgentEvent::QuestionAsked { ref call_id, .. }) if call_id == "q1"
    ));
    let mut mid_live = Live::start(mid_resumed);
    let mut mid_events = mid_live.turn_end().await;
    assert!(closings(&mid_events).is_empty());
    mid_live.send(answer("q1", vec![Answer::choose(0)])).await;
    mid_events.extend(mid_live.turn_end().await);
    check_posted(&mid_events);
    assert_eq!(closings(&mid_events), [answered("q1", 0)]);
    let _ = mid_live.finish().await;
    let mid_req = &mid_provider.sent()[0];
    assert_eq!(
        mid_req
            .messages
            .iter()
            .flat_map(|m| results(m))
            .find(|(id, _, _)| *id == "q1"),
        Some(("q1", question::posted().as_str(), false))
    );
    assert!(user_text(mid_req.messages.last().unwrap()).contains(KEEP));
    assert!(
        !log_events(&mid_store)[mid_records.len()..]
            .iter()
            .any(|e| matches!(e, LogEvent::QuestionAsked { .. }))
    );

    let asked_pos = saved
        .iter()
        .position(|r| matches!(r.event, LogEvent::QuestionAsked { .. }))
        .unwrap();
    let (_, old_provider, mut old_resumed) =
        resumed_agent(&env, &saved[..=asked_pos], vec![done("after-repair")]);
    let (outcome, old_events) = reacting(&mut old_resumed, "next", |_| React::Wait).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    assert_eq!(
        closings(&old_events),
        [("q1".to_owned(), QuestionEnd::Interrupted)]
    );
    let old_sent = old_provider.sent();
    let repaired = old_sent[0]
        .messages
        .iter()
        .flat_map(|m| results(m))
        .find(|(id, _, _)| *id == "q1");
    assert_eq!(repaired, Some(("q1", question::INTERRUPTED, true)));
}

#[tokio::test]
async fn an_undelivered_answer_survives_a_stop_and_a_resume_and_is_delivered_once() {
    let (env, store, _, mut live) = logged_live(
        "ask-user-undelivered-answer",
        vec![uses(&[ask("q1")]), done("waiting"), Step::hang()],
    );
    live.turn("m1", "go").await;
    live.send(answer("q1", vec![Answer::choose(0)])).await;
    live.until("turn 2 to start", |e| {
        matches!(e, AgentEvent::TurnStarted { turn: 2, .. })
    })
    .await;
    live.send(ClientCommand::Cancel).await;
    live.turn_end().await;
    let _ = live.finish().await;

    let saved = records(&store);
    let (_, provider2, resumed) = resumed_agent(&env, &saved, vec![done("got-it"), done("second")]);
    let mut live2 = Live::start(resumed);
    live2.turn("m2", "resume now").await;
    live2.turn("m3", "later").await;
    let _ = live2.finish().await;

    let sent = provider2.sent();
    assert_eq!(sent.len(), 2, "{sent:#?}");
    assert!(user_text(sent[0].messages.last().unwrap()).contains(KEEP));
    assert!(!user_text(sent[1].messages.last().unwrap()).contains(KEEP));
}

#[tokio::test]
async fn rewind_past_the_ask_withdraws_the_question_and_drops_its_waiting_note() {
    let (_, _, provider, mut live) = logged_live(
        "ask-user-rewind-past",
        vec![
            done("one"),
            uses(&[ask("q1")]),
            done("asked"),
            done("after-rewind"),
        ],
    );
    live.turn("m1", "turn one").await;
    live.turn("m2", "turn two").await;
    live.send(rewind(1, 2)).await;
    let rewound = live
        .until("rewind", |e| matches!(e, AgentEvent::Rewound { .. }))
        .await;
    assert_eq!(
        closings(&rewound),
        [("q1".to_owned(), QuestionEnd::Withdrawn)]
    );
    live.turn("m3", "fresh turn two").await;
    let _ = live.finish().await;
    let fresh = user_text(provider.sent().last().unwrap().messages.last().unwrap());
    assert!(!fresh.contains(question::CLOSED_NOTE), "{fresh}");
    same_prefix(&[provider.texts()[0].clone(), provider.texts()[3].clone()]);

    let (env2, store2, provider2, mut live2) = logged_live(
        "ask-user-rewind-drops-note",
        vec![
            done("one"),
            uses(&[ask("q2")]),
            done("asked"),
            uses(&[ask("q3")]),
            done("after-rewind"),
        ],
    );
    live2.turn("m1", "turn one").await;
    live2.turn("m2", "turn two").await;
    live2.send(ClientCommand::Cancel).await;
    live2
        .until("question closed", |e| {
            matches!(e, AgentEvent::QuestionClosed { .. })
        })
        .await;
    live2.send(rewind(2, 2)).await;
    live2
        .until("rewind", |e| matches!(e, AgentEvent::Rewound { .. }))
        .await;
    let after_rewind = records(&store2);
    let turn3 = live2.turn("m3", "clean turn").await;
    assert_eq!(
        finished_result(&turn3, "q3").as_deref(),
        Some(question::posted().as_str())
    );
    let _ = live2.finish().await;
    let clean = user_text(provider2.sent()[3].messages.last().unwrap());
    assert!(!clean.contains(question::CLOSED_NOTE), "{clean}");
    let logged = log_events(&store2);
    assert!(logged.iter().any(|e| matches!(
        e,
        LogEvent::InputWithdrawn {
            by: WithdrawnBy::Rewound,
            ids,
            ..
        } if ids.iter().any(|id| id.0.starts_with("wisp-question-"))
    )));

    let (_, resumed_provider, resumed) =
        resumed_agent(&env2, &after_rewind, vec![done("clean-after-resume")]);
    assert!(resumed.posted_event().is_none());
    let mut live3 = Live::start(resumed);
    live3.turn("m4", "resumed clean turn").await;
    let _ = live3.finish().await;
    let resumed_text = user_text(resumed_provider.sent()[0].messages.last().unwrap());
    assert!(
        !resumed_text.contains(question::CLOSED_NOTE),
        "{resumed_text}"
    );
}

#[tokio::test]
async fn rewind_to_the_answers_turn_reopens_accepts_a_second_rewind_and_agrees_on_resume() {
    let (env, store, provider, mut live) = logged_live(
        "ask-user-rewind-reopen",
        vec![
            done("one"),
            uses(&[ask("q1")]),
            done("asked"),
            done("answer-one"),
            done("answer-two"),
        ],
    );
    live.turn("m1", "turn one").await;
    live.turn("m2", "turn two").await;
    live.send(answer("q1", vec![Answer::choose(0)])).await;
    live.turn_end().await;

    live.send(rewind(1, 3)).await;
    let reopened = live
        .until(
            "reopened question",
            |e| matches!(e, AgentEvent::QuestionAsked { call_id, .. } if call_id == "q1"),
        )
        .await;
    assert!(
        reopened
            .iter()
            .any(|e| matches!(e, AgentEvent::Rewound { .. }))
    );

    live.send(answer("q1", vec![Answer::choose(1)])).await;
    live.turn_end().await;
    let agent = live.finish().await;
    let recs = records(&store);
    let (_, _, resumed_reopen) = resumed_agent(&env, &recs, vec![]);
    for said in [
        agent.compaction.said_texts(),
        resumed_reopen.compaction.said_texts(),
    ] {
        assert!(
            said.iter().any(|s| s.contains("Change the plan")),
            "{said:?}"
        );
        assert!(
            !said.iter().any(|s| s.contains("Keep the plan")),
            "{said:?}"
        );
    }
    assert!(resumed_reopen.posted_event().is_none());
    same_prefix(&[provider.texts()[2].clone(), provider.texts()[4].clone()]);

    let (_, _, _, mut live2) = logged_live(
        "ask-user-second-rewind",
        vec![
            done("one"),
            uses(&[ask("q1")]),
            done("asked"),
            done("answered"),
        ],
    );
    live2.turn("m1", "turn one").await;
    live2.turn("m2", "turn two").await;
    live2.send(answer("q1", vec![Answer::choose(0)])).await;
    live2.turn_end().await;
    live2.send(rewind(1, 3)).await;
    live2
        .until(
            "reopened question",
            |e| matches!(e, AgentEvent::QuestionAsked { call_id, .. } if call_id == "q1"),
        )
        .await;
    live2.send(rewind(2, 2)).await;
    let second = live2
        .until("second rewind", |e| matches!(e, AgentEvent::Rewound { .. }))
        .await;
    assert_eq!(
        closings(&second),
        [("q1".to_owned(), QuestionEnd::Withdrawn)]
    );
    let _ = live2.finish().await;
}

#[tokio::test]
async fn rewind_of_a_later_turn_keeps_the_question_open_and_compaction_keeps_its_answer() {
    let (_, _, _, mut live) = logged_live(
        "ask-user-rewind-later",
        vec![
            uses(&[ask("q1")]),
            done("asked"),
            done("later"),
            done("answered"),
        ],
    );
    live.turn("m1", "ask").await;
    live.turn("m2", "later work").await;
    live.send(rewind(1, 2)).await;
    let rewound = live
        .until("rewind", |e| matches!(e, AgentEvent::Rewound { .. }))
        .await;
    assert!(!rewound.iter().any(|e| matches!(
        e,
        AgentEvent::QuestionAsked { .. } | AgentEvent::QuestionClosed { .. }
    )));
    live.send(answer("q1", vec![Answer::choose(0)])).await;
    live.turn_end().await;
    let agent = live.finish().await;
    assert!(
        agent
            .compaction
            .said_texts()
            .iter()
            .any(|s| s.contains("Keep the plan")),
        "{:?}",
        agent.compaction.said_texts()
    );
}

#[tokio::test]
async fn the_snapshots_state_carries_the_open_posted_question() {
    let gate = Arc::new(Semaphore::new(0));
    let (_, _, _, mut live) = logged_live(
        "ask-user-snapshot",
        vec![uses(&[ask("q1")]), done("asked").gated(&gate)],
    );
    live.send(send("m1", "go")).await;
    live.until("step 2 to start", |e| {
        matches!(e, AgentEvent::StepStarted { step: 2, .. })
    })
    .await;
    for req in [6u64, 7] {
        live.send(ClientCommand::DescribeSession {
            request: RequestId(req),
        })
        .await;
        let described = live
            .until("session snapshot", |e| {
                matches!(e, AgentEvent::SessionSnapshot { .. })
            })
            .await;
        let state = described
            .into_iter()
            .find_map(|e| match e {
                AgentEvent::SessionSnapshot { state, .. } => Some(state),
                _ => None,
            })
            .unwrap();
        assert!(state.iter().any(|e| matches!(
            e,
            AgentEvent::QuestionAsked { call_id, .. } if call_id == "q1"
        )));
        if req == 6 {
            gate.add_permits(1);
            live.turn_end().await;
        }
    }
    let _ = live.finish().await;
}

#[tokio::test]
async fn the_card_gets_the_questions_exactly_as_the_model_wrote_them() {
    let env = Env::new("ask-user-asked");
    let long = json!({"questions": [{
        "header": "Deployment target",
        "question": "Where will it run?",
        "multi_select": true,
        "options": [
            {"label": "Fly.io", "description": ""},
            {"label": "A VPS", "description": "One box."}
        ]
    }]});
    let provider = scripted(vec![
        uses(&[call("q", "ask_user", long)]),
        done("ok"),
        done("done"),
    ]);
    let mut live = Live::start(asker(provider.clone(), &env, true));
    let first = live.turn("m1", "go").await;
    let asked: Vec<&Vec<wisp_protocol::Question>> = pick!(
        &first,
        AgentEvent::QuestionAsked { questions, .. } => questions
    );
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0][0].header, "Deployment target");
    assert!(asked[0][0].multi_select);
    let display = first.iter().find_map(|e| match e {
        AgentEvent::ToolCallRequested { display, .. } => Some(display.clone()),
        _ => None,
    });
    assert_eq!(display.map(|d| d.target), Some("Deployment …".to_owned()));
    live.send(answer(
        "q",
        vec![Answer {
            chosen: vec![0, 1],
            own: Some("and a Dockerfile".into()),
        }],
    ))
    .await;
    live.turn_end().await;
    let agent = live.finish().await;
    let history = agent.history();
    let last_user = history.iter().rev().find(|m| m.role == Role::User).unwrap();
    assert_eq!(
        last_text(last_user),
        "The user answered:\n\n1. [Deployment target] Where will it run?\n   - Fly.io\n   - A VPS\n   In their own words: and a Dockerfile\n\n\
Read the answers in the user's own words carefully: they may ask for something else, or ask you not to go on. Follow what they say."
    );
}

#[tokio::test]
async fn invalid_ask_user_input_never_opens_a_card_and_still_holds_back_later_calls() {
    let cases = [
        (
            json!({"questions": [{"header": "H", "question": "Q?", "options": [
                {"label": "Only", "description": ""}
            ]}]}),
            ask_user::TOO_FEW_OPTIONS,
        ),
        (
            json!({"withdraw": false}),
            "`withdraw` must be true; to ask, give `questions` instead",
        ),
        (
            json!({"questions": [{"header": "H", "question": "Q?", "options": [
                {"label": "A", "description": ""}, {"label": "B", "description": ""}
            ]}], "withdraw": true}),
            "give either questions or withdraw, not both",
        ),
    ];
    for (bad, message) in cases {
        let env = Env::new("ask-user-invalid");
        let provider = scripted(vec![
            uses(&[call("q", "ask_user", bad), run("s")]),
            done("ok"),
        ]);
        let mut agent = asker(provider.clone(), &env, true);
        let (_, events) = reacting(&mut agent, "go", |_| React::Wait).await;
        check_events(&events);
        assert!(closings(&events).is_empty());
        assert!(started_ids(&events).is_empty());
        assert_eq!(
            finished(&events),
            [
                (
                    "q".to_owned(),
                    ToolOutcome::InvalidInput {
                        message: message.into()
                    }
                ),
                (
                    "s".to_owned(),
                    ToolOutcome::NotRun {
                        reason: NotRunReason::AfterQuestion
                    }
                ),
            ]
        );
    }
}

#[tokio::test]
async fn an_own_answer_of_a_mebibyte_comes_back_whole_through_ask_user() {
    let env = Env::new("ask-user-big");
    let own = format!("{}\nend", "x".repeat(1 << 20));
    let provider = scripted(vec![uses(&[ask("q")]), done("ok"), done("got")]);
    let mut live = Live::start(asker(provider.clone(), &env, true));
    live.turn("m1", "go").await;
    live.send(answer("q", vec![Answer::own(own.clone())])).await;
    live.turn_end().await;
    let agent = live.finish().await;
    let words = own.replace('\n', "\n   ");
    let expected = format!(
        "The user answered:\n\n1. [Plan] Which plan should go ahead?\n   In their own words: {words}\n\n{}",
        question::OWN_WORDS_CAUTION
    );
    let history = agent.history();
    let last_user = history.iter().rev().find(|m| m.role == Role::User).unwrap();
    let text = last_text(last_user);
    assert_eq!(text.len(), expected.len());
    assert_eq!(text, expected);
}

#[tokio::test(start_paused = true)]
async fn ask_user_has_no_timeout() {
    let env = Env::new("ask-user-wait");
    let provider = scripted(vec![uses(&[ask("q")]), done("ok"), done("next")]);
    let mut live = Live::start(asker(provider.clone(), &env, true));
    live.turn("m1", "go").await;
    tokio::time::sleep(Duration::from_secs(24 * 60 * 60)).await;
    live.send(answer("q", vec![Answer::choose(0)])).await;
    let events = live.turn_end().await;
    assert_eq!(closings(&events), [answered("q", 0)]);
    let _ = live.finish().await;
}

#[tokio::test]
async fn ask_user_needs_no_workspace_and_alone_sends_no_environment_block() {
    let env = Env::new("ask-user-only");
    let provider = scripted(vec![uses(&[ask("q")]), done("ok"), done("done")]);
    let agent = asker_with(provider.clone(), &env, with_ask_user(Vec::new()), true);
    fs::remove_dir_all(&env.work).unwrap();
    let mut live = Live::start(agent);
    let first = live.turn("m1", "go").await;
    let cwds: Vec<&str> = pick!(&first, AgentEvent::ToolCallStarted { cwd, .. } => cwd.as_str());
    assert_eq!(cwds, [""]);
    live.send(answer("q", vec![Answer::choose(0)])).await;
    live.turn_end().await;
    let _ = live.finish().await;
    let requests = provider.sent();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        let json = serde_json::to_string(&request.messages).unwrap();
        assert!(!json.contains("<environment>"), "{json}");
        assert_eq!(
            request.tools,
            [crate::tool::testkit::ask_user().spec().clone()]
        );
    }
    same_prefix(&provider.texts());
}

#[tokio::test]
async fn every_logged_question_ending_rebuilds_the_committed_text() {
    let (_, store, _, mut live) = logged_live(
        "ask-user-log",
        vec![
            uses(&[ask("q1"), run("s")]),
            uses(&[ask("q2")]),
            uses(&[withdraw("w1")]),
            uses(&[ask("q3")]),
            done("ok"),
            done("answered"),
        ],
    );
    live.turn("m1", "go").await;
    live.send(answer("q3", vec![Answer::choose(0)])).await;
    live.turn_end().await;
    let _ = live.finish().await;
    let logged = log_events(&store);
    let results: Vec<(String, Option<String>)> = pick!(
        &logged,
        LogEvent::ToolCallFinished {
            call_id, result, ..
        } => (call_id.clone(), result.clone())
    );
    assert_eq!(
        results,
        [
            ("q1".to_owned(), Some(question::posted())),
            ("s".to_owned(), None),
            ("q2".to_owned(), Some(question::busy())),
            ("w1".to_owned(), Some(question::WITHDRAWN.to_owned())),
            ("q3".to_owned(), Some(question::posted())),
        ]
    );
    let questions = logged
        .iter()
        .find_map(|e| match e {
            LogEvent::QuestionAsked {
                call_id, questions, ..
            } if call_id == "q3" => Some(questions.clone()),
            _ => None,
        })
        .unwrap();
    let closed: Vec<(String, QuestionEnd)> = pick!(
        &logged,
        LogEvent::QuestionClosed { call_id, how, .. } => (call_id.clone(), how.clone())
    );
    assert_eq!(
        closed,
        [("q1".to_owned(), QuestionEnd::Withdrawn), answered("q3", 0)]
    );
    assert_eq!(
        question::result_text(&questions, &closed[0].1),
        question::WITHDRAWN
    );
    assert_eq!(question::result_text(&questions, &closed[1].1), KEEP);
}

#[cfg(unix)]
mod with_bash {
    use super::*;

    fn bash(id: &str) -> B {
        call(id, "bash", json!({"command": "touch marker"}))
    }

    fn bash_toolbox() -> Toolbox {
        with_ask_user(vec![crate::tool::testkit::bash_entry()])
    }

    #[tokio::test]
    async fn bash_after_ask_user_never_starts_whatever_the_ending() {
        for user in [true, false] {
            let env = Env::new("ask-user-bash-after");
            let provider = scripted(vec![uses(&[ask("q"), bash("b")]), done("ok")]);
            let mut agent = asker_with(provider.clone(), &env, bash_toolbox(), user);
            let (_, events) = reacting(&mut agent, "go", |_| React::Wait).await;
            check_events(&events);
            assert_eq!(started_ids(&events), ["q"], "user={user}");
            assert!(!env.work.join("marker").exists(), "user={user}");
            let reason = NotRunReason::AfterQuestion;
            let text = not_run_text(&reason);
            let messages = agent.history();
            let last = messages
                .iter()
                .rev()
                .find(|m| m.role == Role::User && !results(m).is_empty())
                .cloned()
                .unwrap();
            assert_eq!(results(&last)[1], ("b", text.as_str(), true), "user={user}");
            assert_eq!(
                finished(&events)[1],
                ("b".to_owned(), ToolOutcome::NotRun { reason }),
                "user={user}"
            );
        }
    }

    #[tokio::test]
    async fn bash_before_ask_user_runs_first_and_the_turn_goes_on_after_the_answer() {
        let env = Env::new("ask-user-bash-before");
        let provider = scripted(vec![
            uses(&[bash("b"), ask("q")]),
            done("ok"),
            done("after"),
        ]);
        let mut live = Live::start(asker_with(provider.clone(), &env, bash_toolbox(), true));
        let first = live.turn("m1", "go").await;
        assert_eq!(started_ids(&first), ["b", "q"]);
        assert!(env.work.join("marker").exists());
        let finished_b = first
            .iter()
            .position(
                |e| matches!(e, AgentEvent::ToolCallFinished { call_id, .. } if call_id == "b"),
            )
            .unwrap();
        let asked = first
            .iter()
            .position(|e| matches!(e, AgentEvent::QuestionAsked { .. }))
            .unwrap();
        assert!(finished_b < asked);
        let last = provider.sent()[1].messages.last().cloned().unwrap();
        assert_eq!(results(&last)[1], ("q", question::posted().as_str(), false));
        let tools: Vec<String> = provider.sent()[0]
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect();
        assert_eq!(tools, ["ask_user", "bash"]);
        live.send(answer("q", vec![Answer::choose(0)])).await;
        live.turn_end().await;
        let _ = live.finish().await;
    }
}

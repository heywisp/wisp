use wisp_protocol::log::LOG_SCHEMA_VERSION;
use wisp_protocol::{Answer, QuestionEnd, RejectKind, WithdrawnBy};

use super::*;
use crate::session::{jsonl_line, parse_jsonl};
use crate::test_guard::records;
use crate::tool::question::{STOPPED, UNAVAILABLE, UNAVAILABLE_MESSAGE};
use crate::tool::{Toolbox, confirm_tool};

const AFTER: &str = "Not run: it came after a question call in the same reply, so it wasn't run. \
Call it again if it still applies.";

fn toolbox() -> Toolbox {
    let mut tools: Vec<crate::tool::ToolEntry> = samples().into_iter().map(|s| s.tool).collect();
    tools.push(confirm_tool());
    Toolbox::new(tools)
}

fn asker_with(provider: Arc<Fake>, env: &Env, toolbox: Toolbox, user: bool) -> Agent {
    Agent::new(provider, default_session())
        .with_clock(TickingClock::new())
        .with_tools(toolbox, env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300))
        .with_user_input(user)
}

fn asker(provider: Arc<Fake>, env: &Env) -> Agent {
    asker_with(provider, env, toolbox(), true)
}

fn ask(id: &str) -> B {
    call(id, "confirm", json!({}))
}

fn run(id: &str) -> B {
    call(id, "sleep", json!({"ms": 0, "text": "ran\n"}))
}

fn answer(call_id: &str, answers: Vec<Answer>) -> ClientCommand {
    ClientCommand::Answer {
        call_id: call_id.into(),
        answers,
    }
}

enum React {
    Wait,
    Send(Vec<ClientCommand>),
    Later(Duration, Vec<ClientCommand>),
    HangUp,
}

async fn reacting(
    agent: &mut Agent,
    text: &str,
    mut react: impl FnMut(&AgentEvent) -> React + Send + 'static,
) -> (TurnOutcome, Vec<AgentEvent>) {
    let (cmd_tx, mut cmd_rx) = mpsc::channel(64);
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    let collector = tokio::spawn(async move {
        let mut tx = Some(cmd_tx);
        let mut seen = Vec::new();
        while let Some(e) = ev_rx.recv().await {
            let (wait, commands) = match react(&e) {
                React::Wait => (None, Vec::new()),
                React::Send(commands) => (None, commands),
                React::Later(wait, commands) => (Some(wait), commands),
                React::HangUp => {
                    tx = None;
                    (None, Vec::new())
                }
            };
            if let Some(wait) = wait {
                tokio::time::sleep(wait).await;
            }
            if let Some(tx) = &tx {
                for command in commands {
                    let _ = tx.send(command).await;
                }
            }
            seen.push(e);
        }
        seen
    });
    let outcome = agent
        .run_turn(text.into(), &mut cmd_rx, &ev_tx)
        .await
        .unwrap();
    drop(ev_tx);
    (outcome, collector.await.unwrap())
}

fn on_question(
    mut reply: impl FnMut(&str) -> React + Send + 'static,
) -> impl FnMut(&AgentEvent) -> React + Send + 'static {
    move |e| match e {
        AgentEvent::QuestionAsked { call_id, .. } => reply(call_id),
        _ => React::Wait,
    }
}

fn steer_then_answer() -> impl FnMut(&AgentEvent) -> React + Send + 'static {
    on_question(|id| {
        React::Send(vec![
            ClientCommand::send(format!("m-{id}"), "do this instead"),
            answer(id, vec![Answer::choose(0)]),
        ])
    })
}

fn check_questions(events: &[AgentEvent]) {
    let mut open: Option<String> = None;
    let mut closed = std::collections::BTreeSet::new();
    for e in events {
        match e {
            AgentEvent::QuestionAsked { call_id, .. } => {
                assert_eq!(open, None, "{events:#?}");
                open = Some(call_id.clone());
            }
            AgentEvent::QuestionClosed { call_id, .. } => {
                assert_eq!(open.take().as_ref(), Some(call_id), "{events:#?}");
                assert!(closed.insert(call_id.clone()), "closed twice: {call_id}");
            }
            AgentEvent::ToolCallFinished { call_id, .. } => {
                assert_ne!(open.as_ref(), Some(call_id), "{events:#?}");
            }
            AgentEvent::TurnEnded { .. } => assert_eq!(open, None),
            _ => {}
        }
    }
    assert_eq!(open, None, "a question was never closed");
}

fn closings(events: &[AgentEvent]) -> Vec<(String, QuestionEnd)> {
    pick!(events, AgentEvent::QuestionClosed { call_id, how } => (call_id.clone(), how.clone()))
}

fn finished_result(events: &[AgentEvent], id: &str) -> Option<String> {
    events.iter().find_map(|e| match e {
        AgentEvent::ToolCallFinished {
            call_id, result, ..
        } if call_id == id => Some(result.clone()),
        _ => None,
    })?
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    Answered,
    Esc,
    HangUp,
    NoChannel,
}

impl Ending {
    const ALL: [Ending; 4] = [Self::Answered, Self::Esc, Self::HangUp, Self::NoChannel];

    fn react(self) -> impl FnMut(&AgentEvent) -> React + Send + 'static {
        on_question(move |id| match self {
            Self::Answered => React::Send(vec![answer(id, vec![Answer::choose(0)])]),
            Self::Esc => React::Send(vec![ClientCommand::Cancel]),
            Self::HangUp => React::HangUp,
            Self::NoChannel => React::Wait,
        })
    }

    fn stops(self) -> bool {
        matches!(self, Self::Esc | Self::HangUp)
    }

    fn question(self) -> (ToolOutcome, String) {
        match self {
            Self::Answered => (
                ToolOutcome::Completed {
                    is_error: false,
                    exit: None,
                },
                "The user answered:\nKeep the plan".to_owned(),
            ),
            Self::Esc | Self::HangUp => (ToolOutcome::Cancelled, STOPPED.to_owned()),
            Self::NoChannel => (
                ToolOutcome::Failed {
                    message: UNAVAILABLE_MESSAGE.into(),
                    before_start: false,
                },
                UNAVAILABLE.to_owned(),
            ),
        }
    }

    fn closed(self) -> Option<QuestionEnd> {
        match self {
            Self::Answered => Some(QuestionEnd::Answered {
                answers: vec![Answer::choose(0)],
            }),
            Self::Esc | Self::HangUp => Some(QuestionEnd::Stopped),
            Self::NoChannel => None,
        }
    }
}

#[tokio::test]
async fn every_question_closes_once_before_its_call_finishes_whatever_its_ending() {
    let arrangements: [&[&str]; 6] = [
        &["confirm"],
        &["confirm", "sleep"],
        &["sleep", "confirm"],
        &["sleep", "confirm", "sleep"],
        &["confirm", "confirm"],
        &["confirm", "echo", "sleep"],
    ];
    for ending in Ending::ALL {
        for arrangement in arrangements {
            let label = format!("{ending:?} {arrangement:?}");
            let env = Env::new("ask-matrix");
            let ids: Vec<String> = arrangement
                .iter()
                .enumerate()
                .map(|(i, name)| format!("{name}{i}"))
                .collect();
            let reply: Vec<B> = arrangement
                .iter()
                .zip(&ids)
                .map(|(name, id)| match *name {
                    "confirm" => ask(id),
                    "echo" => call(id, "echo", json!({"text": "hi\n"})),
                    _ => run(id),
                })
                .collect();
            let provider = scripted(vec![uses(&reply), done("ok")]);
            let mut agent = asker_with(
                provider.clone(),
                &env,
                toolbox(),
                ending != Ending::NoChannel,
            );
            let (outcome, events) = reacting(&mut agent, "go", ending.react()).await;
            check_events(&events);
            check_questions(&events);
            assert_layout(&provider);

            let first = arrangement.iter().position(|n| *n == "confirm").unwrap();
            let question = &ids[first];
            let asked = events
                .iter()
                .filter(|e| matches!(e, AgentEvent::QuestionAsked { .. }))
                .count();
            assert_eq!(asked, usize::from(ending != Ending::NoChannel), "{label}");
            let expected_close: Vec<(String, QuestionEnd)> = ending
                .closed()
                .map(|how| (question.clone(), how))
                .into_iter()
                .collect();
            assert_eq!(closings(&events), expected_close, "{label}");

            let (outcome_q, text_q) = ending.question();
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
                    assert!(
                        matches!(outcome, ToolOutcome::Completed { .. }),
                        "{label}: {outcome:?}"
                    );
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
                    let reason = if ending.stops() {
                        NotRunReason::TurnStopped
                    } else {
                        NotRunReason::AfterQuestion
                    };
                    assert_eq!(text, not_run_text(&reason), "{label}");
                    assert_eq!(outcome, &ToolOutcome::NotRun { reason }, "{label}");
                    assert!(is_error, "{label}");
                }
            }
            let started = started_ids(&events);
            assert_eq!(started, ids[..=first].to_vec(), "{label}");
            if ending.stops() {
                assert!(
                    matches!(
                        outcome,
                        TurnOutcome::Cancelled {
                            committed_steps: 1,
                            ..
                        }
                    ),
                    "{label}: {outcome:?}"
                );
                assert_eq!(provider.sent().len(), 1, "{label}");
            } else {
                assert!(
                    matches!(outcome, TurnOutcome::Completed { steps: 2, .. }),
                    "{label}: {outcome:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_call_after_a_question_never_starts_and_saves_no_output() {
    let env = Env::new("ask-after");
    let provider = scripted(vec![uses(&[ask("q"), run("s")]), done("ok")]);
    let mut agent = asker(provider.clone(), &env);
    let (_, events) = reacting(&mut agent, "go", Ending::Answered.react()).await;
    assert_eq!(started_ids(&events), ["q"]);
    assert!(!events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolOutput { call_id, .. } if call_id == "s"
    )));
    let dir = agent.output_dir().unwrap();
    let files = fs::read_dir(dir.path()).map_or(0, |d| d.count());
    assert_eq!(
        files, 0,
        "neither the question nor the skipped call saved output"
    );
    let last = provider.sent()[1].messages.last().cloned().unwrap();
    assert_eq!(
        results(&last),
        [
            ("q", "The user answered:\nKeep the plan", false),
            ("s", AFTER, true)
        ]
    );
    let output = events.iter().find_map(|e| match e {
        AgentEvent::ToolCallFinished {
            call_id, output, ..
        } if call_id == "q" => Some(output.clone()),
        _ => None,
    });
    assert_eq!(
        output,
        Some(ToolOutputView {
            inlined: true,
            ..ToolOutputView::default()
        })
    );
}

#[tokio::test]
async fn a_question_with_invalid_input_still_holds_back_later_calls() {
    let env = Env::new("ask-invalid");
    let provider = scripted(vec![
        uses(&[call("q", "confirm", json!({"x": 1})), run("s")]),
        done("ok"),
    ]);
    let mut agent = asker(provider.clone(), &env);
    let (_, events) = reacting(&mut agent, "go", Ending::Answered.react()).await;
    check_events(&events);
    assert!(started_ids(&events).is_empty());
    assert_eq!(
        finished(&events),
        [
            (
                "q".to_owned(),
                ToolOutcome::InvalidInput {
                    message: "unknown field `x`".into()
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

#[tokio::test]
async fn a_question_needs_no_workspace() {
    let env = Env::new("ask-no-root");
    let provider = scripted(vec![uses(&[ask("q")]), done("ok")]);
    let mut agent = asker(provider.clone(), &env);
    fs::remove_dir_all(&env.work).unwrap();
    let (_, events) = reacting(&mut agent, "go", Ending::Answered.react()).await;
    let cwds: Vec<&str> = pick!(&events, AgentEvent::ToolCallStarted { cwd, .. } => cwd.as_str());
    assert_eq!(cwds, [""]);
    assert_eq!(closings(&events).len(), 1);
    let history = agent.history();
    let (_, text, is_error) = results(&history[2])[0];
    assert_eq!(text, "The user answered:\nKeep the plan");
    assert!(!is_error);
}

#[tokio::test]
async fn a_session_whose_only_tool_asks_sends_no_environment_block() {
    let env = Env::new("ask-only");
    let provider = scripted(vec![uses(&[ask("q")]), done("ok")]);
    let only = Toolbox::new([confirm_tool()]);
    let mut agent = asker_with(provider.clone(), &env, only, true);
    let (_, events) = reacting(&mut agent, "go", Ending::Answered.react()).await;
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::HookRan { hook, outcome: wisp_protocol::HookOutcome::None, .. }
            if hook == wisp_protocol::log::LEAD_ENVIRONMENT
    )));
    for request in provider.sent() {
        let json = serde_json::to_string(&request.messages).unwrap();
        assert!(!json.contains("<environment>"), "{json}");
        assert_eq!(request.tools.len(), 1);
    }
    let first = &provider.sent()[0].messages[0];
    assert_eq!(
        first.content.len(),
        3,
        "the stamp, the context line and the text only"
    );
}

#[tokio::test]
async fn an_own_answer_of_a_mebibyte_comes_back_whole() {
    let env = Env::new("ask-big");
    let own = format!("{}\nend", "x".repeat(1 << 20));
    let provider = scripted(vec![uses(&[ask("q")]), done("ok")]);
    let mut agent = asker(provider.clone(), &env);
    let reply = own.clone();
    let (_, events) = reacting(
        &mut agent,
        "go",
        on_question(move |id| React::Send(vec![answer(id, vec![Answer::own(reply.clone())])])),
    )
    .await;
    let expected = format!("The user answered:\nIn their own words: {own}");
    let messages = agent.history();
    let (_, text, _) = results(&messages[2])[0];
    assert_eq!(text.len(), expected.len());
    assert_eq!(text, expected);
    assert_eq!(
        finished_result(&events, "q").map(|t| t.len()),
        Some(expected.len())
    );
}

#[tokio::test]
async fn bad_answers_are_refused_and_the_question_stays_open() {
    let env = Env::new("ask-check");
    let provider = scripted(vec![uses(&[ask("q")]), done("ok")]);
    let mut agent = asker(provider.clone(), &env);
    let (outcome, events) = reacting(
        &mut agent,
        "go",
        on_question(|id| {
            React::Send(vec![
                answer(id, vec![Answer::choose(2)]),
                answer(
                    id,
                    vec![Answer {
                        chosen: vec![0, 1],
                        own: None,
                    }],
                ),
                answer(
                    id,
                    vec![Answer {
                        chosen: vec![1],
                        own: Some("both".into()),
                    }],
                ),
                answer(id, vec![Answer::own(" \n ")]),
                answer(id, vec![Answer::choose(0), Answer::choose(0)]),
                answer(id, Vec::new()),
                answer("nope", vec![Answer::choose(0)]),
                answer(id, vec![Answer::choose(1)]),
                answer(id, vec![Answer::choose(0)]),
            ])
        }),
    )
    .await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    check_questions(&events);
    assert_eq!(
        rejected(&events),
        [
            "answer for q refused: answer 1 chooses option 2, but question 1 has 2 options",
            "answer for q refused: question 1 takes one choice, but answer 1 has 2",
            "answer for q refused: question 1 takes a choice or your own answer, not both",
            "answer for q refused: answer 1's own text is blank",
            "answer for q refused: expected 1 answer, one per question, but got 2",
            "answer for q refused: expected 1 answer, one per question, but got 0",
            "no question nope is open",
            "no question q is open",
        ]
    );
    let bad = (None, Some(RejectKind::BadAnswer));
    let none = (None, Some(RejectKind::NoQuestion));
    assert_eq!(
        rejected_kinds(&events),
        [
            bad.clone(),
            bad.clone(),
            bad.clone(),
            bad.clone(),
            bad.clone(),
            bad,
            none.clone(),
            none
        ]
    );
    assert_eq!(
        closings(&events),
        [(
            "q".to_owned(),
            QuestionEnd::Answered {
                answers: vec![Answer::choose(1)]
            }
        )]
    );
    let closed_at = events
        .iter()
        .position(|e| matches!(e, AgentEvent::QuestionClosed { .. }))
        .unwrap();
    let refused: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, AgentEvent::Rejected { .. }))
        .map(|(i, _)| i)
        .collect();
    assert!(refused[..7].iter().all(|i| *i < closed_at));
    assert!(refused[7] > closed_at);
    let messages = agent.history();
    let (_, text, _) = results(&messages[2])[0];
    assert_eq!(text, "The user answered:\nChange the plan");
}

#[tokio::test]
async fn an_answer_with_nothing_running_is_refused() {
    let provider = scripted(Vec::new());
    let agent = Agent::new(provider, default_session()).with_user_input(true);
    let (tx, mut rx, _task) = spawn(agent);
    tx.send(answer("q", vec![Answer::choose(0)])).await.unwrap();
    let event = rx.recv().await.unwrap();
    assert_eq!(
        event,
        AgentEvent::Rejected {
            request: None,
            reason: "no question q is open".into(),
            id: None,
            kind: Some(RejectKind::NoQuestion),
        }
    );
}

#[tokio::test(start_paused = true)]
async fn a_question_has_no_timeout() {
    let env = Env::new("ask-wait");
    let provider = scripted(vec![uses(&[ask("q")]), done("ok")]);
    let mut agent = asker(provider.clone(), &env);
    let day = Duration::from_secs(24 * 60 * 60);
    let (outcome, events) = reacting(
        &mut agent,
        "go",
        on_question(move |id| React::Later(day, vec![answer(id, vec![Answer::choose(0)])])),
    )
    .await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        finished(&events),
        [(
            "q".to_owned(),
            ToolOutcome::Completed {
                is_error: false,
                exit: None
            }
        )]
    );
    assert_eq!(
        closings(&events)[0].1,
        QuestionEnd::Answered {
            answers: vec![Answer::choose(0)]
        }
    );
}

#[tokio::test]
async fn a_steer_waits_for_an_open_question_and_lands_after_the_answer() {
    let env = Env::new("ask-steer");
    let provider = scripted(vec![uses(&[ask("q"), run("s")]), done("ok")]);
    let mut agent = asker(provider.clone(), &env);
    let (outcome, events) = reacting(&mut agent, "go", steer_then_answer()).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { steps: 2, .. }),
        "{outcome:?}"
    );
    assert_eq!(
        closings(&events),
        [(
            "q".to_owned(),
            QuestionEnd::Answered {
                answers: vec![Answer::choose(0)]
            }
        )]
    );
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::InputDelivered { ids, step: 2 } if ids == &[InputId("m-q".into())]
    )));
    let last = provider.sent()[1].messages.last().cloned().unwrap();
    assert_eq!(
        results(&last),
        [
            ("q", "The user answered:\nKeep the plan", false),
            ("s", AFTER, true)
        ]
    );
    assert_eq!(
        last.content[last.content.len() - 2..],
        ["do this instead", STEER_NOTE].map(crate::test_guard::text)
    );
}

#[tokio::test]
async fn a_stop_before_the_boundary_withdraws_the_steer_and_keeps_a_truthful_result() {
    let env = Env::new("ask-steer-stop");
    let provider = scripted(vec![uses(&[ask("q"), run("s")]), done("never")]);
    let mut agent = asker(provider.clone(), &env);
    let (outcome, events) = reacting(
        &mut agent,
        "go",
        on_question(|id| {
            React::Send(vec![
                ClientCommand::send(format!("m-{id}"), "do this instead"),
                ClientCommand::Cancel,
            ])
        }),
    )
    .await;
    check_events(&events);
    check_questions(&events);
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    assert_eq!(closings(&events), [("q".to_owned(), QuestionEnd::Stopped)]);
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::InputWithdrawn { ids, by: WithdrawnBy::TurnStopped } if ids == &[InputId("m-q".into())]
    )));
    let messages = agent.history();
    let last = messages.last().unwrap();
    assert_eq!(
        results(last),
        [
            ("q", STOPPED, true),
            ("s", not_run_text(&NotRunReason::TurnStopped).as_str(), true)
        ]
    );
    let json = serde_json::to_string(&*agent.history()).unwrap();
    assert!(!json.contains("do this instead"));
    assert_eq!(provider.sent().len(), 1);
}

#[tokio::test]
async fn a_queued_message_leaves_the_question_open() {
    let env = Env::new("ask-queue");
    let provider = scripted(vec![uses(&[ask("q")]), done("ok")]);
    let mut agent = asker(provider.clone(), &env);
    let (_, events) = reacting(
        &mut agent,
        "go",
        on_question(|id| {
            React::Send(vec![
                ClientCommand::queue("later", "after the turn"),
                answer(id, vec![Answer::choose(0)]),
            ])
        }),
    )
    .await;
    assert_eq!(
        closings(&events),
        [(
            "q".to_owned(),
            QuestionEnd::Answered {
                answers: vec![Answer::choose(0)]
            }
        )]
    );
    let json = serde_json::to_string(&provider.sent()[1].messages).unwrap();
    assert!(!json.contains("after the turn"));
}

#[tokio::test]
async fn the_question_is_synced_before_it_reaches_the_client() {
    let env = Env::new("ask-sync");
    let provider = scripted(vec![uses(&[ask("q")]), done("ok")]);
    let ops: Ops = Arc::default();
    let store = MemoryStore::default();
    let recording = RecordingStore {
        read_gate: None,
        inner: store.clone(),
        ops: ops.clone(),
    };
    let mut agent =
        asker(provider, &env).with_log(SessionLog::new(Box::new(recording), "s1"), log_context());
    let seen: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    let snapshot = seen.clone();
    let (_, events) = reacting(&mut agent, "go", move |e| match e {
        AgentEvent::QuestionAsked { call_id, .. } => {
            *snapshot.lock().unwrap() = ops.lock().unwrap().clone();
            React::Send(vec![answer(call_id, vec![Answer::choose(0)])])
        }
        _ => React::Wait,
    })
    .await;
    check_questions(&events);
    let seen = seen.lock().unwrap().clone();
    assert!(seen.ends_with(&["question", "sync"]), "{seen:?}");

    let logged = records(&store);
    let text: String = logged.iter().map(|r| jsonl_line(r).unwrap()).collect();
    let back = parse_jsonl(&text).unwrap();
    assert_eq!(back, logged);
    assert!(back.iter().all(|r| r.schema_version == LOG_SCHEMA_VERSION));
    let kinds: Vec<&LogEvent> = back
        .iter()
        .map(|r| &r.event)
        .filter(|e| {
            matches!(
                e,
                LogEvent::QuestionAsked { .. }
                    | LogEvent::QuestionClosed { .. }
                    | LogEvent::ToolCallStarted { .. }
                    | LogEvent::ToolCallFinished { .. }
            )
        })
        .collect();
    assert!(matches!(
        kinds.as_slice(),
        [
            LogEvent::ToolCallStarted { .. },
            LogEvent::QuestionAsked { turn: 1, .. },
            LogEvent::QuestionClosed { .. },
            LogEvent::ToolCallFinished {
                result: Some(_),
                ..
            }
        ]
    ));
}

#[tokio::test]
async fn question_fields_are_logged_verbatim_while_the_rest_is_redacted() {
    let env = Env::new("ask-redact");
    let store = MemoryStore::default();
    let provider = scripted(vec![uses(&[ask("call-plan")]), done("ok")]);
    let context = LogContext {
        redactions: Redactions::new(["plan".to_owned()]),
        ..log_context()
    };
    let mut agent =
        asker(provider, &env).with_log(SessionLog::new(Box::new(store.clone()), "s1"), context);
    let (_, events) = reacting(
        &mut agent,
        "go",
        on_question(|id| React::Send(vec![answer(id, vec![Answer::own("my plan C")])])),
    )
    .await;
    let live = finished_result(&events, "call-plan").unwrap();
    assert_eq!(live, "The user answered:\nIn their own words: my plan C");
    let mut seen = 0;
    for record in records(&store) {
        match record.event {
            LogEvent::QuestionAsked {
                call_id, questions, ..
            } => {
                assert_eq!(call_id, "call-plan");
                assert_eq!(
                    questions,
                    [crate::plugin::convert::question(
                        wisp_plugin_testkit::question()
                    )]
                );
                assert!(questions[0].question.contains("plan"));
                assert!(questions[0].options[0].label.contains("plan"));
                seen += 1;
            }
            LogEvent::QuestionClosed { call_id, how, .. } => {
                assert_eq!(call_id, "call-plan");
                assert_eq!(
                    how,
                    QuestionEnd::Answered {
                        answers: vec![Answer::own("my plan C")]
                    }
                );
                seen += 1;
            }
            LogEvent::ToolCallFinished {
                call_id, result, ..
            } => {
                assert_eq!(call_id, "call-plan");
                assert_eq!(result.as_deref(), Some(live.as_str()));
                seen += 1;
            }
            _ => {}
        }
    }
    assert_eq!(seen, 3);
}

fn same_prefix(bodies: &[String]) {
    for pair in bodies.windows(2) {
        assert_eq!(tools_part(&pair[0]), tools_part(&pair[1]));
        let (head_a, msgs_a) = head_and_messages(&pair[0]);
        let (head_b, msgs_b) = head_and_messages(&pair[1]);
        assert_eq!(head_a, head_b);
        let open = msgs_a.strip_suffix(']').unwrap();
        let tail_open = msgs_a.strip_suffix("]}]").unwrap();
        assert!(
            msgs_b.starts_with(open) || msgs_b.starts_with(tail_open),
            "\n{msgs_a}\n{msgs_b}"
        );
    }
}

#[tokio::test]
async fn questions_keep_one_prefix_and_only_append() {
    let env = Env::new("ask-prefix");
    let provider = scripted(vec![
        uses(&[ask("q1"), run("s1")]),
        uses(&[run("s2")]),
        done("one"),
        uses(&[ask("q3")]),
        done("two"),
        uses(&[ask("q4")]),
        done("three"),
        uses(&[
            call("s5", "sleep", json!({"ms": 300, "text": "ran\n"})),
            ask("q5"),
        ]),
        done("four"),
    ]);
    let mut agent = asker(provider.clone(), &env);
    reacting(&mut agent, "first", Ending::Answered.react()).await;
    reacting(&mut agent, "second", steer_then_answer()).await;
    let (outcome, _) = reacting(&mut agent, "third", Ending::Esc.react()).await;
    assert!(matches!(outcome, TurnOutcome::Cancelled { .. }));
    let (_, _) = turn(&mut agent, "fourth").await;
    let mut answer = Ending::Answered.react();
    reacting(&mut agent, "fifth", move |e| match e {
        AgentEvent::ToolCallStarted { call_id, .. } if call_id == "s5" => {
            React::Send(vec![ClientCommand::send("early", "use plan B")])
        }
        _ => answer(e),
    })
    .await;
    let bodies = provider.texts();
    assert_eq!(bodies.len(), 9);
    same_prefix(&bodies);
    assert_layout(&provider);
    let last = provider.sent()[6].messages.last().cloned().unwrap();
    assert_eq!(results(&last), [("q4", STOPPED, true)]);
    assert_eq!(
        last.content.last(),
        Some(&ContentBlock::Text {
            text: "fourth".into()
        })
    );
    let steered = provider.sent()[4].messages.last().cloned().unwrap();
    assert_eq!(
        results(&steered),
        [("q3", "The user answered:\nKeep the plan", false)]
    );
    assert_eq!(
        steered.content[steered.content.len() - 2..],
        ["do this instead", STEER_NOTE].map(crate::test_guard::text)
    );
    let early = provider.sent()[8].messages.last().cloned().unwrap();
    assert_eq!(
        results(&early)[1],
        ("q5", "The user answered:\nKeep the plan", false)
    );
    assert_eq!(
        early.content[early.content.len() - 2..],
        ["use plan B", STEER_NOTE].map(crate::test_guard::text)
    );
}

struct TwiceTool;

impl TwiceTool {
    fn toolbox() -> Toolbox {
        crate::plugin::testing::toolbox(vec![Arc::new(Self)])
    }
}

impl crate::plugin::testing::TestTool for TwiceTool {
    fn name(&self) -> String {
        "twice".into()
    }

    fn description(&self) -> String {
        "Ask the same question twice at once.".into()
    }

    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn meta(&self) -> wisp_plugin_api::tools::ToolMeta {
        wisp_plugin_api::tools::ToolMeta {
            saves_output: false,
            needs_workspace: false,
            ends_reply: true,
            ..crate::plugin::testing::meta(60_000, None)
        }
    }

    fn prepare(&self, input: &str) -> Result<Box<dyn wisp_plugin_api::tools::Prepared>, String> {
        Ok(crate::plugin::testing::Call::boxed(
            crate::plugin::testing::fallback("twice", input),
            (),
        ))
    }

    fn run<'a>(
        &'a self,
        _call: Box<dyn wisp_plugin_api::tools::Prepared>,
        ctx: &'a dyn wisp_plugin_api::host::CallContext,
    ) -> futures::future::BoxFuture<'a, wisp_plugin_api::types::ToolRun> {
        use futures::FutureExt;
        async move {
            let (first, second) = tokio::join!(
                ctx.ask(vec![wisp_plugin_testkit::question()]),
                ctx.ask(vec![wisp_plugin_testkit::question()])
            );
            let text = format!("{first:?} | {second:?} | {}", ctx.cwd());
            wisp_plugin_api::types::ToolRun {
                result: Some(text),
                ..wisp_plugin_api::types::ToolRun::completed()
            }
        }
        .boxed()
    }
}

#[tokio::test]
async fn a_second_question_from_the_same_call_is_unavailable_while_one_is_open() {
    let env = Env::new("ask-twice");
    let provider = scripted(vec![uses(&[call("w", "twice", json!({}))]), done("ok")]);
    let mut agent = asker_with(provider, &env, TwiceTool::toolbox(), true);
    let (_, events) = reacting(&mut agent, "go", Ending::Answered.react()).await;
    check_questions(&events);
    let asked = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::QuestionAsked { .. }))
        .count();
    assert_eq!(asked, 1);
    assert_eq!(closings(&events).len(), 1);
    let answered =
        wisp_plugin_api::types::AskReply::Answered(vec![crate::plugin::convert::answer_to_api(
            &Answer::choose(0),
        )]);
    let expected = format!(
        "{answered:?} | {:?} | {}",
        wisp_plugin_api::types::AskReply::Unavailable,
        env.work.display()
    );
    assert_eq!(finished_result(&events, "w"), Some(expected));
    let cwds: Vec<&str> = pick!(&events, AgentEvent::ToolCallStarted { cwd, .. } => cwd.as_str());
    assert_eq!(cwds, [""], "the row shows no directory");
}

#[tokio::test]
async fn a_question_gets_the_root_as_its_directory_and_the_gate_sees_it() {
    let env = Env::new("ask-gate");
    let gate = Arc::new(CountingGate(Mutex::default()));
    let provider = scripted(vec![uses(&[ask("q")]), done("ok")]);
    let mut agent = asker(provider, &env).with_gate(gate.clone());
    reacting(&mut agent, "go", Ending::Answered.react()).await;
    assert_eq!(
        gate.0.lock().unwrap().clone(),
        [("confirm".to_owned(), env.work.clone())]
    );
}

#[tokio::test]
async fn a_steer_waiting_when_a_question_opens_waits_for_the_answer() {
    let env = Env::new("ask-steer-first");
    let provider = scripted(vec![
        uses(&[
            call("s", "sleep", json!({"ms": 300, "text": "ran\n"})),
            ask("q"),
            run("t"),
        ]),
        done("ok"),
    ]);
    let mut agent = asker(provider.clone(), &env);
    let mut answer = Ending::Answered.react();
    let turn = reacting(&mut agent, "go", move |e| match e {
        AgentEvent::ToolCallStarted { call_id, .. } if call_id == "s" => {
            React::Send(vec![ClientCommand::send("early", "use plan B")])
        }
        _ => answer(e),
    });
    let (outcome, events) = tokio::time::timeout(Duration::from_secs(20), turn)
        .await
        .expect("the answered question must end the step");
    check_events(&events);
    check_questions(&events);
    let accepted = events
        .iter()
        .position(|e| matches!(e, AgentEvent::InputAccepted { id, .. } if id.as_str() == "early"))
        .expect("the steer was accepted");
    let asked = events
        .iter()
        .position(|e| matches!(e, AgentEvent::QuestionAsked { .. }))
        .expect("the question opened");
    assert!(accepted < asked, "{events:#?}");
    assert!(
        matches!(outcome, TurnOutcome::Completed { steps: 2, .. }),
        "{outcome:?}"
    );
    let (_, answered) = Ending::Answered.question();
    assert_eq!(
        closings(&events),
        [("q".to_owned(), Ending::Answered.closed().unwrap())]
    );
    assert_eq!(started_ids(&events), ["s", "q"]);
    let last = provider.sent()[1].messages.last().cloned().unwrap();
    assert_eq!(
        results(&last),
        [
            ("s", "Output:\nran", false),
            ("q", answered.as_str(), false),
            ("t", AFTER, true)
        ]
    );
    assert_eq!(
        last.content[last.content.len() - 2..],
        ["use plan B", STEER_NOTE].map(crate::test_guard::text)
    );
}

mod ask_user;

fn screen_report(call_id: &str) -> ClientCommand {
    ClientCommand::ScreenReport {
        call_id: call_id.into(),
        report: json!({"width": 380, "height": 560, "zoom": 1}),
        images: Vec::new(),
    }
}

#[tokio::test(start_paused = true)]
async fn a_root_without_user_input_hands_its_waiting_call_the_screen_report() {
    let env = Env::new("screen-report");
    let page = call("p", "show_page", json!({"html": "<p>3/4</p>"}));
    let provider = scripted(vec![uses(&[page]), done("ok")]);
    let toolbox = crate::tool::testkit::bundled_exact(&["show_page"]);
    let mut agent = asker_with(provider, &env, toolbox, false);
    let soon = Duration::from_millis(50);
    let (outcome, events) = reacting(&mut agent, "go", move |e| match e {
        AgentEvent::ToolCallStarted { call_id, .. } => {
            React::Later(soon, vec![screen_report(call_id)])
        }
        _ => React::Wait,
    })
    .await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let answer = finished_result(&events, "p");
    assert_eq!(answer.as_deref(), Some("Shown on the board, 380 × 560 px."));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Rejected { .. }))
    );
}

#[tokio::test]
async fn a_screen_report_no_call_waits_for_is_refused() {
    let provider = scripted(Vec::new());
    let (tx, mut rx, _task) = spawn(Agent::new(provider, default_session()));
    tx.send(screen_report("p")).await.unwrap();
    assert_eq!(
        rx.recv().await.unwrap(),
        AgentEvent::Rejected {
            request: None,
            reason: "no call p is waiting for a screen report".into(),
            id: None,
            kind: Some(RejectKind::UnknownId),
        }
    );
}

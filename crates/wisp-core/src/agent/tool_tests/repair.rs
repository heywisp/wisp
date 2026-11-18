use wisp_protocol::log::{LogRecord, LoggedBlock};
use wisp_protocol::{QuestionEnd, WithdrawnBy};

use super::*;
use crate::agent::INTERRUPTED_MESSAGE;
use crate::resume::{NOT_RUN, UNKNOWN};
use crate::tool::question::INTERRUPTED;
use crate::tool::{Toolbox, confirm_tool};

fn toolbox() -> Toolbox {
    let mut tools: Vec<crate::tool::ToolEntry> = samples().into_iter().map(|s| s.tool).collect();
    tools.push(confirm_tool());
    tools.push(crate::tool::snap::tool());
    Toolbox::new(tools)
}

fn base(provider: Arc<Fake>, env: &Env) -> Agent {
    Agent::new(provider, default_session())
        .with_clock(TickingClock::new())
        .with_tools(toolbox(), env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300))
        .with_user_input(true)
}

struct Resumed {
    agent: Agent,
    provider: Arc<Fake>,
    store: MemoryStore,
    from: usize,
}

impl Resumed {
    fn new(env: &Env, records: &[LogRecord]) -> Self {
        let (store, log) = continued(records);
        Self::in_store(env, records, store, log)
    }

    fn in_store(env: &Env, records: &[LogRecord], store: MemoryStore, log: SessionLog) -> Self {
        let provider = scripted(vec![done("after")]);
        let agent = base(provider.clone(), env)
            .with_log(log, log_context())
            .resume(records, 0)
            .unwrap();
        Self {
            agent,
            provider,
            store,
            from: records.len(),
        }
    }

    async fn next(&mut self) -> Vec<LogEvent> {
        let (outcome, _) = turn(&mut self.agent, "next").await;
        assert!(
            matches!(outcome, TurnOutcome::Completed { .. }),
            "{outcome:?}"
        );
        log_events(&self.store)[self.from..].to_vec()
    }

    fn sent_blocks(&self, call_id: &str) -> Vec<ContentBlock> {
        let requests = self.provider.sent();
        result_content(&requests[0].messages, call_id)
    }

    fn sent_results(&self) -> Vec<(String, String, bool)> {
        let requests = self.provider.sent();
        let messages = &requests[0].messages;
        let last_results = messages
            .iter()
            .rev()
            .find(|m| !results(m).is_empty())
            .unwrap();
        results(last_results)
            .into_iter()
            .map(|(id, text, error)| (id.to_owned(), text.to_owned(), error))
            .collect()
    }
}

fn three_calls() -> Vec<B> {
    vec![
        B::Text("running"),
        call("c1", "echo", samples()[0].input.clone()),
        call("c2", "sleep", json!({"ms": 0, "text": "slept\n"})),
        call("c3", "echo", samples()[0].input.clone()),
    ]
}

async fn live_turn(env: &Env, content: Vec<B>) -> (Vec<LogRecord>, Arc<Fake>) {
    let provider = scripted(vec![uses(&content), done("ok")]);
    let store = MemoryStore::default();
    let mut agent = base(provider.clone(), env).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    let (outcome, _) = turn(&mut agent, "go").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    (records(&store), provider)
}

fn live_results(provider: &Fake) -> Vec<(String, String, bool)> {
    let requests = provider.sent();
    results(requests[1].messages.last().unwrap())
        .into_iter()
        .map(|(id, text, error)| (id.to_owned(), text.to_owned(), error))
        .collect()
}

fn interrupted(events: &[LogEvent]) -> &TurnOutcome {
    events
        .iter()
        .find_map(|e| match e {
            LogEvent::TurnEnded {
                turn: 1, outcome, ..
            } => Some(outcome),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn a_crash_during_the_second_of_three_calls_closes_the_turn() {
    let env = Env::new("repair-three");
    let (records, live) = live_turn(&env, three_calls()).await;
    let cut = before(
        &records,
        |e| matches!(e, LogEvent::ToolCallFinished { call_id, .. } if call_id == "c2"),
    );
    let mut resumed = Resumed::new(&env, &cut);
    let written = resumed.next().await;
    let sent = resumed.sent_results();
    assert_eq!(
        sent[0],
        live_results(&live)[0],
        "a finished call keeps its text"
    );
    assert_eq!(
        sent[1..],
        [
            ("c2".to_owned(), UNKNOWN.to_owned(), true),
            ("c3".to_owned(), NOT_RUN.to_owned(), true),
        ]
    );
    let TurnOutcome::Error {
        kind: ErrorKind::Interrupted,
        message,
        cost: Some(cost),
        usage: Some(usage),
        committed_steps: 1,
        offer: Some(_),
        ..
    } = interrupted(&written)
    else {
        panic!("{written:#?}");
    };
    assert_eq!(message, INTERRUPTED_MESSAGE);
    assert!(cost.turn_lower_bound && cost.turn_usd > 0.0);
    assert!(usage.output_tokens > 0, "{usage:?}");
    let duration = written.iter().find_map(|e| match e {
        LogEvent::TurnEnded { turn: 1, stats, .. } => Some(stats.duration_ms),
        _ => None,
    });
    assert!(duration.is_some_and(|ms| ms > 0), "{duration:?}");
    let kinds: Vec<String> = written
        .iter()
        .map(|e| {
            serde_json::to_value(e).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    let committed = kinds.iter().position(|k| k == "message_committed").unwrap();
    let ended = kinds.iter().position(|k| k == "turn_ended").unwrap();
    assert!(
        kinds[0] == "session_resumed" && committed < ended,
        "{kinds:?}"
    );
}

#[tokio::test]
async fn a_crash_right_after_the_reply_was_saved_runs_none_of_its_calls() {
    let env = Env::new("repair-none");
    let (records, _) = live_turn(&env, three_calls()).await;
    let cut = before(&records, |e| matches!(e, LogEvent::ToolCallStarted { .. }));
    let mut resumed = Resumed::new(&env, &cut);
    resumed.next().await;
    let texts: Vec<(String, String, bool)> = ["c1", "c2", "c3"]
        .into_iter()
        .map(|id| (id.to_owned(), NOT_RUN.to_owned(), true))
        .collect();
    assert_eq!(resumed.sent_results(), texts);
}

#[tokio::test]
async fn finished_calls_get_the_exact_text_the_live_turn_sent() {
    let env = Env::new("repair-exact");
    fs::create_dir_all(env.work.join("sub")).unwrap();
    let content = vec![
        call(
            "c1",
            "sleep",
            json!({"ms": 0, "text": "moved\n", "cd": "sub"}),
        ),
        call("c2", "sleep", json!({"ms": "soon"})),
        call("c3", "sleep", json!({"ms": 0, "text": "", "exit": 3})),
    ];
    let (records, live) = live_turn(&env, content).await;
    let cut = before(
        &records,
        |e| matches!(e, LogEvent::MessageCommitted { message, .. } if message.role == Role::User && message.content.iter().any(|b| matches!(b, LoggedBlock::ToolResult { .. }))),
    );
    let mut resumed = Resumed::new(&env, &cut);
    resumed.next().await;
    let sent = resumed.sent_results();
    assert_eq!(sent, live_results(&live));
    assert!(
        sent[0].1.ends_with("Working directory: ~/work/sub"),
        "{sent:?}"
    );
    assert!(sent[1].1.starts_with("Invalid input: "), "{sent:?}");
}

#[tokio::test]
async fn an_open_question_is_interrupted_and_closed_in_the_log() {
    let env = Env::new("repair-ask");
    let provider = scripted(vec![uses(&[call("q1", "confirm", json!({}))])]);
    let store = MemoryStore::default();
    let mut agent = base(provider, &env).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    let (outcome, _) = turn_with(
        &mut agent,
        "go",
        Some(|e: &AgentEvent| matches!(e, AgentEvent::QuestionAsked { .. })),
    )
    .await;
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    let logged = records(&store);
    let cut = before(&logged, |e| matches!(e, LogEvent::QuestionClosed { .. }));
    let mut resumed = Resumed::new(&env, &cut);
    let written = resumed.next().await;
    assert!(written.iter().any(|e| matches!(
        e,
        LogEvent::QuestionClosed { call_id, how: QuestionEnd::Interrupted, .. } if call_id == "q1"
    )));
    assert_eq!(
        resumed.sent_results(),
        [("q1".to_owned(), INTERRUPTED.to_owned(), true)]
    );
    const KEY: &str = "CANARY-repaired-question-credential";
    crate::session::register_secrets([KEY.to_owned()]);
    let mut answered = logged
        .iter()
        .find(|r| matches!(r.event, LogEvent::QuestionClosed { .. }))
        .unwrap()
        .clone();
    if let LogEvent::QuestionClosed { how, .. } = &mut answered.event {
        *how = QuestionEnd::Answered {
            answers: vec![wisp_protocol::Answer::own(KEY)],
        };
    }
    let mut cut = cut;
    cut.push(answered);
    let mut resumed = Resumed::new(&env, &cut);
    resumed.next().await;
    let sent = resumed.sent_results();
    assert!(!sent[0].1.contains(KEY));
    assert!(sent[0].1.contains(crate::session::REDACTED));
    let note = wisp_tool_text::output::HELD_KEY_NOTE;
    assert_eq!(sent[0].1.matches(note).count(), 1);
    assert_eq!(sent[0].1.lines().last(), Some(note));
    let logged = records(&resumed.store);
    let rebuilt = rebuild_history(logged.iter().map(|r| &r.event), no_images)
        .unwrap()
        .messages;
    assert_eq!(
        serde_json::to_string(&rebuilt).unwrap(),
        serde_json::to_string(&*resumed.agent.history()).unwrap()
    );
    assert!(!serde_json::to_string(&rebuilt).unwrap().contains(KEY));
}

#[tokio::test]
async fn a_message_still_waiting_when_wisp_stopped_is_withdrawn_and_handed_back() {
    let store = MemoryStore::default();
    let mut live = rig(
        vec![
            uses(&[steps(
                "a",
                &["steer:also this", "sleep:200", "write:done\n"],
            )]),
            done("ok"),
        ],
        Duration::from_secs(10),
        Some(store.clone()),
    );
    run_rig(&mut live, "go").await;
    let logged = records(&store);
    let accepted = logged
        .iter()
        .position(|r| matches!(r.event, LogEvent::InputAccepted { .. }))
        .unwrap();
    let cut = logged[..=accepted].to_vec();

    let (copy, log) = continued(&cut);
    let (tx, _rx) = mpsc::channel(8);
    let provider = scripted(vec![done("after")]);
    let agent = Agent::new(provider, default_session())
        .with_clock(TickingClock::new())
        .with_tools(
            ScriptTool::toolbox(tx, Duration::from_secs(10), Ops::default()),
            live._env.workspace(),
        )
        .with_platform("Linux")
        .with_resources(None)
        .with_log(log, log_context())
        .resume(&cut, 0)
        .unwrap();
    let returned: Vec<(String, String)> = agent
        .returned_inputs()
        .iter()
        .map(|r| (r.id.as_str().to_owned(), r.text.clone()))
        .collect();
    assert_eq!(returned, [("also this".to_owned(), "also this".to_owned())]);
    let mut agent = agent;
    turn(&mut agent, "next").await;
    let written: Vec<LogEvent> = log_events(&copy)[cut.len()..].to_vec();
    assert!(written.iter().any(|e| matches!(
        e,
        LogEvent::InputWithdrawn { ids, by: WithdrawnBy::SessionEnded, .. } if ids == &[InputId("also this".into())]
    )), "{written:#?}");
    let logged = records(&copy);
    let cut = logged
        .iter()
        .position(|r| matches!(r.event, LogEvent::SessionResumed { .. }))
        .unwrap();
    let again = Resumed::new(&live._env, &logged[..=cut]);
    assert_eq!(again.agent.returned_inputs().len(), 1);
    assert_eq!(
        again.agent.returned_inputs()[0].id,
        InputId("also this".into())
    );
}

fn result_content(messages: &[Message], call_id: &str) -> Vec<ContentBlock> {
    messages
        .iter()
        .rev()
        .flat_map(|m| m.content.iter())
        .find_map(|b| match b {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } if tool_use_id == call_id => Some(content.clone()),
            _ => None,
        })
        .unwrap()
}

async fn attached_then_cut(env: &Env) -> (Vec<LogRecord>, MemoryStore, Arc<Fake>) {
    let provider = scripted(vec![
        uses(&[
            call("c1", "snap", json!({})),
            call("c2", "sleep", json!({"ms": 0, "text": "slept\n"})),
        ]),
        done("ok"),
    ]);
    let store = MemoryStore::default();
    let mut agent = base(provider.clone(), env).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    let (outcome, _) = turn(&mut agent, "go").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let records = records(&store);
    let cut = before(
        &records,
        |e| matches!(e, LogEvent::ToolCallFinished { call_id, .. } if call_id == "c2"),
    );
    (cut, store, provider)
}

#[tokio::test]
async fn a_finished_calls_attachment_is_sent_again_after_a_crash() {
    let env = Env::new("repair-attachment");
    let (cut, live_store, live) = attached_then_cut(&env).await;
    let live_blocks = result_content(&live.sent()[1].messages, "c1");
    assert!(
        matches!(
            live_blocks.as_slice(),
            [ContentBlock::Text { .. }, ContentBlock::Media { .. }]
        ),
        "{live_blocks:?}"
    );
    let png = crate::tool::snap::png();
    let sha256 = crate::session::sha256_bytes_hex(&png);
    let media_type = crate::tool::snap::mime("image/png");
    assert!(live_store.get_blob(&sha256, &media_type).is_ok());

    let (mut store, log) = continued(&cut);
    store.put_blob(&sha256, &media_type, &png).unwrap();
    let mut resumed = Resumed::in_store(&env, &cut, store, log);
    let written = resumed.next().await;
    assert_eq!(resumed.sent_blocks("c1"), live_blocks);
    assert_eq!(
        resumed.sent_blocks("c2"),
        vec![ContentBlock::Text {
            text: UNKNOWN.into()
        }]
    );
    let committed = written
        .iter()
        .find_map(|e| match e {
            LogEvent::MessageCommitted { message, .. } if message.role == Role::User => {
                Some(message.clone())
            }
            _ => None,
        })
        .unwrap();
    assert!(
        serde_json::to_string(&committed)
            .unwrap()
            .contains(r#""type":"blob""#),
        "the repaired attachment is logged by reference"
    );
    let rebuilt = crate::session::rebuild(&resumed.store, log_events(&resumed.store).iter())
        .unwrap()
        .messages;
    assert_eq!(&rebuilt, &*resumed.agent.history());

    let mut gone = Resumed::new(&env, &cut);
    gone.next().await;
    let blocks = gone.sent_blocks("c1");
    let [
        ContentBlock::Text { text },
        ContentBlock::Text { text: note },
    ] = blocks.as_slice()
    else {
        panic!("{blocks:?}");
    };
    assert!(matches!(&live_blocks[0], ContentBlock::Text { text: live } if live == text));
    assert_eq!(
        note,
        "[snap.png (image/png) couldn't be processed: its saved copy was gone when Wisp resumed. \
It can't be read in this session.]"
    );
}

#[tokio::test]
async fn a_kill_after_a_hooked_run_resumes_with_the_hooked_result() {
    use crate::plugin::testing::{hook_body, hook_source, replying, with_test_hooks};
    use wisp_plugin_api::hooks::Reply;
    use wisp_plugin_testkit::hooks::Calls;

    const KEY: &str = "CANARY-killed-hook-result-credential";
    crate::session::register_secrets([KEY.to_owned()]);
    for result in ["hooked result", KEY] {
        let env = Env::new("repair-hooked");
        let provider = scripted(vec![uses(&three_calls()), done("ok")]);
        let store = MemoryStore::default();
        let calls = Calls::default();
        let sources = vec![hook_source(
            "aa",
            &hook_body("wrap", "after-tool", ""),
            replying(Reply::Rewrite(result.into())),
            &calls,
        )];
        let mut agent = with_test_hooks(base(provider.clone(), &env), sources).with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        );
        turn(&mut agent, "go").await;
        let records = records(&store);
        let live = live_results(&provider);
        let expected = if result == KEY {
            wisp_tool_text::output::with_held_key_note(crate::session::REDACTED)
        } else {
            result.to_owned()
        };
        assert!(
            live.iter().all(|(_, text, _)| text == &expected),
            "{live:?}"
        );
        let cut = before(
            &records,
            |e| matches!(e, LogEvent::MessageCommitted { message, .. } if message.role == Role::User && message.content.iter().any(|b| matches!(b, LoggedBlock::ToolResult { .. }))),
        );
        let mut resumed = Resumed::new(&env, &cut);
        resumed.next().await;
        assert_eq!(resumed.sent_results(), live);
    }
}

#[tokio::test]
async fn an_after_tool_change_an_attach_refusal_replaced_writes_no_change_and_repairs_as_logged() {
    use crate::plugin::testing::{hook_body, hook_source, replying, with_test_hooks};
    use wisp_plugin_api::hooks::Reply;
    use wisp_plugin_testkit::hooks::Calls;

    let env = Env::new("repair-hooked-refused");
    let provider = scripted(vec![
        uses(&[call(
            "a",
            "snap",
            json!({"name": "song.mp3", "type": "audio/mpeg"}),
        )]),
        done("ok"),
    ]);
    let store = MemoryStore::default();
    let calls = Calls::default();
    let sources = vec![hook_source(
        "aa",
        &hook_body("wrap", "after-tool", ""),
        replying(Reply::Rewrite("hooked result".into())),
        &calls,
    )];
    let agent = Agent::new(provider.clone(), default_session())
        .with_clock(TickingClock::new())
        .with_tools(
            crate::tool::Toolbox::new([crate::tool::snap::tool()]),
            env.workspace(),
        )
        .with_platform("Linux")
        .with_resources(None);
    let mut agent = with_test_hooks(agent, sources).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    let (_, events) = turn(&mut agent, "listen").await;
    assert_eq!(calls.count("wrap"), 1);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::HookApplied { .. }))
    );
    let records = records(&store);
    assert!(
        !records
            .iter()
            .any(|r| matches!(r.event, LogEvent::HookApplied { .. }))
    );
    let live = live_results(&provider);
    assert_eq!(live.len(), 1);
    assert!(live[0].1.starts_with("song.mp3 is audio/mpeg"), "{live:?}");
    let finished = records
        .iter()
        .find_map(|r| match &r.event {
            LogEvent::ToolCallFinished { result, .. } => result.clone(),
            _ => None,
        })
        .unwrap();
    assert_eq!(finished, live[0].1);
    let cut = before(
        &records,
        |e| matches!(e, LogEvent::MessageCommitted { message, .. } if message.role == Role::User && message.content.iter().any(|b| matches!(b, LoggedBlock::ToolResult { .. }))),
    );
    let mut resumed = Resumed::new(&env, &cut);
    resumed.next().await;
    assert_eq!(resumed.sent_results(), live);
}

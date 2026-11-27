use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use wisp_protocol::AgentPath;

use futures::stream::{self, BoxStream};
use jiff::tz::TimeZone;
use serde_json::{Value, json};
use wisp_protocol::log::{LEAD_ENVIRONMENT, LogEvent, LogRecord, no_images, rebuild_history};
use wisp_protocol::settings::{EndpointSummary, SettingValue};
use wisp_protocol::{
    CacheEffect, FailurePhase, FailureSource, RebuildReason, RejectKind, RequestId, RetryClass,
    RewindPoint, ToolAction,
};

use super::*;
use crate::agent::testing::{
    Caps, Fake, Knobs, Script, Sent, Step, Wait, classified, ended, fallback_marker, opaque,
    started, text, thinking, tool_use,
};
use crate::agent::tests::usage;
use crate::clock::{ShiftedClock, TickingClock};
use crate::provider::anthropic::{build_body, echoed_blocks};
use crate::session::{MemoryStore, SessionStore};
use crate::test_guard::{
    self, ScratchDir, continued, head_and_messages, is_turn_end, log_context, log_events, pick,
    records, streamed, until_event, until_turn_end, within,
};
use crate::tool::{samples, test_toolbox};
use wisp_tool_text::STEER_NOTE;
use wisp_tool_text::jobs::LIST_TARGET_CHARS;
use wisp_tool_text::output::cut_chars;

type Trigger = fn(&AgentEvent) -> bool;

fn scripted(steps: Vec<Step>) -> Arc<Fake> {
    Fake::with(
        Knobs {
            media: true,
            ..Knobs::default()
        },
        steps,
    )
}

#[derive(Clone)]
enum B {
    Think,
    Redacted,
    Text(&'static str),
    Call(String, String, Value),
}

fn call(id: &str, name: &str, input: Value) -> B {
    B::Call(id.into(), name.into(), input)
}

fn start_on(model: &str, input: u64) -> ProviderEvent {
    started(Some(&format!("msg_{input}")), model, usage(input, 0, 0))
}

fn blocks(content: &[B]) -> Script {
    let mut script = Vec::new();
    for (index, b) in content.iter().enumerate() {
        script.extend(match b {
            B::Think => thinking(
                index,
                &format!("thought {index}"),
                &format!("sig{index}+/=="),
            ),
            B::Redacted => vec![
                Ok(ProviderEvent::BlockStart {
                    index,
                    block: ContentBlock::RedactedThinking {
                        data: format!("Encrypted{index}=="),
                    },
                }),
                Ok(ProviderEvent::BlockStop { index }),
            ],
            B::Text(words) => text(index, words),
            B::Call(id, name, input) => {
                let json = serde_json::to_string(input).unwrap();
                let (a, b) = json.split_at(json.len() / 2);
                tool_use(index, id, name, &[a, b])
            }
        });
    }
    script
}

fn rebuilt_body(last: &Sent, messages: &[Message]) -> Vec<u8> {
    build_body(&Request {
        session: &last.session,
        tools: &last.tools,
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering: &crate::provider::UNRENDERED,
        compaction: None,
    })
    .unwrap()
}

fn reply_on(model: &str, input: u64, content: &[B], stop: StopReason) -> Script {
    let mut script = vec![Ok(ProviderEvent::Sending), Ok(start_on(model, input))];
    script.extend(blocks(content));
    script.extend(ended(stop, usage(input, 0, 10), Vec::new()));
    script
}

fn reply(content: &[B], stop: StopReason) -> Step {
    Step::from(reply_on(DEFAULT_MODEL, 600, content, stop))
}

fn uses(content: &[B]) -> Step {
    reply(content, StopReason::ToolUse)
}

fn done(text: &'static str) -> Step {
    reply(&[B::Text(text)], StopReason::EndTurn)
}

fn hanging(content: &[B]) -> Step {
    let mut script = vec![Ok(ProviderEvent::Sending), Ok(start_on(DEFAULT_MODEL, 600))];
    script.extend(blocks(content));
    Step::from(script).hanging()
}

struct ExitGate {
    mark: String,
    command: String,
}

impl ExitGate {
    fn new(mark: &str, code: i32) -> Self {
        Self {
            mark: mark.to_owned(),
            command: format!("{}; exit {code}", crate::test_guard::wait_for(mark)),
        }
    }

    fn open(&self, work: &Path) {
        std::fs::write(work.join(&self.mark), "").unwrap();
    }

    fn opens_at(
        &self,
        work: PathBuf,
        call: &'static str,
    ) -> impl FnMut(&AgentEvent) -> Vec<ClientCommand> + '_ {
        move |e| {
            if matches!(e, AgentEvent::ToolCallStarted { call_id, .. } if call_id == call) {
                self.open(&work);
            }
            Vec::new()
        }
    }

    fn ended(&self, id: u32, code: i32) -> String {
        format!("Process {id} ({}) exited with code {code}.", self.shown())
    }

    fn shown(&self) -> String {
        cut_chars(&self.command, LIST_TARGET_CHARS)
    }
}

struct Env {
    root: PathBuf,
    work: PathBuf,
    temp: PathBuf,
    scratch: ScratchDir,
}

impl Env {
    fn new(name: &str) -> Self {
        let scratch = ScratchDir::new("wisp-loop-test-", name);
        Self {
            root: scratch.root.clone(),
            work: scratch.dir("work"),
            temp: scratch.dir("tmp"),
            scratch,
        }
    }

    fn workspace(&self) -> Workspace {
        self.scratch.workspace()
    }
}

const GRACE: Duration = Duration::from_millis(300);

fn agent_with(provider: Arc<dyn Provider>, env: &Env, toolbox: Toolbox, grace: Duration) -> Agent {
    Agent::new(provider, default_session())
        .with_clock(TickingClock::new())
        .with_tools(toolbox, env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(grace)
}

fn agent(provider: Arc<Fake>, env: &Env) -> Agent {
    agent_with(provider, env, test_toolbox(), GRACE)
}

fn logged(provider: Arc<Fake>, env: &Env, store: &MemoryStore) -> Agent {
    let context = log_context();
    agent(provider, env).with_log(SessionLog::new(Box::new(store.clone()), "s1"), context)
}

async fn turn_with(
    agent: &mut Agent,
    text: &str,
    cancel_when: Option<Trigger>,
) -> (TurnOutcome, Vec<AgentEvent>) {
    turn_shown(agent, text, cancel_when, None).await
}

async fn turn_shown(
    agent: &mut Agent,
    text: &str,
    cancel_when: Option<Trigger>,
    shown: Option<ClientCommand>,
) -> (TurnOutcome, Vec<AgentEvent>) {
    let (cmd_tx, mut cmd_rx) = mpsc::channel(4);
    let (ev_tx, mut ev_rx) = mpsc::channel(1024);
    if let Some(shown) = shown {
        cmd_tx.send(shown).await.unwrap();
    }
    let collector = tokio::spawn(async move {
        let mut seen = Vec::new();
        let mut sent = false;
        while let Some(e) = ev_rx.recv().await {
            if !sent && cancel_when.is_some_and(|f| f(&e)) {
                sent = true;
                cmd_tx.send(ClientCommand::Cancel).await.unwrap();
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

async fn turn(agent: &mut Agent, text: &str) -> (TurnOutcome, Vec<AgentEvent>) {
    turn_with(agent, text, None).await
}

struct Quick {
    name: String,
    input: Value,
}

fn quick_samples() -> Vec<Quick> {
    samples()
        .into_iter()
        .map(|s| Quick {
            name: s.tool.spec().name.clone(),
            input: s.input,
        })
        .collect()
}

fn expected_text(input: &Value) -> String {
    let text = input["text"].as_str().unwrap_or_default();
    format!("Output:\n{}", text.strip_suffix('\n').unwrap_or(text))
}

fn assert_ran(sample: &Quick, events: &[AgentEvent], provider: &Fake, completed: usize) {
    let ids: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallRequested { call_id, tool, .. } if *tool == sample.name => {
                Some(call_id.as_str())
            }
            _ => None,
        })
        .collect();
    let ran: Vec<String> = finished(events)
        .into_iter()
        .filter(|(id, outcome)| {
            ids.contains(&id.as_str()) && !matches!(outcome, ToolOutcome::NotRun { .. })
        })
        .map(|(id, outcome)| {
            assert_eq!(
                outcome,
                ToolOutcome::Completed {
                    is_error: false,
                    exit: None
                },
                "{}: {id}",
                sample.name
            );
            id
        })
        .collect();
    assert_eq!(ran.len(), completed, "{}: {events:#?}", sample.name);
    let expected = expected_text(&sample.input);
    let requests = provider.sent();
    for id in &ran {
        let sent: Vec<(&str, bool)> = requests
            .iter()
            .flat_map(|r| &r.messages)
            .flat_map(|m| results(m))
            .filter(|(result_id, _, _)| result_id == id)
            .map(|(_, text, is_error)| (text, is_error))
            .collect();
        assert!(
            !sent.is_empty(),
            "{}: {id} was never sent back",
            sample.name
        );
        for (text, is_error) in sent {
            assert_eq!(text, expected, "{}: {id}", sample.name);
            assert!(!is_error, "{}: {id}", sample.name);
        }
    }
}

pub(super) fn on_started(e: &AgentEvent) -> bool {
    matches!(e, AgentEvent::ToolCallStarted { .. })
}

fn finished(events: &[AgentEvent]) -> Vec<(String, ToolOutcome)> {
    pick!(
        events,
        AgentEvent::ToolCallFinished {
            call_id, outcome, ..
        } => (call_id.clone(), outcome.clone())
    )
}

fn results(message: &Message) -> Vec<(&str, &str, bool)> {
    message
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => match content.as_slice() {
                [ContentBlock::Text { text }] => {
                    Some((tool_use_id.as_str(), text.as_str(), *is_error))
                }
                other => panic!("{other:?}"),
            },
            _ => None,
        })
        .collect()
}

fn tool_uses(messages: &[Message]) -> usize {
    messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .flat_map(|m| echoed_blocks(&m.content, &Default::default()))
        .filter(|b| tool_call(b).is_some())
        .count()
}

fn check_events(events: &[AgentEvent]) {
    let mut open_step: Option<u32> = None;
    let mut last_step = 0;
    let mut step_of_call = std::collections::BTreeMap::new();
    let mut ended_steps = Vec::new();
    let mut calls: std::collections::BTreeMap<String, Vec<&'static str>> = Default::default();
    let mut order = Vec::new();
    for e in events {
        match e {
            AgentEvent::StepStarted { step } => {
                assert_eq!(open_step, None, "{events:#?}");
                assert_eq!(*step, last_step + 1);
                open_step = Some(*step);
                last_step = *step;
            }
            AgentEvent::StepEnded { step, .. } => {
                assert_eq!(open_step, Some(*step), "{events:#?}");
                open_step = None;
                ended_steps.push(*step);
            }
            AgentEvent::BlockStarted { .. }
            | AgentEvent::TextDelta { .. }
            | AgentEvent::ThinkingDelta { .. }
            | AgentEvent::BlockStopped { .. }
            | AgentEvent::Retrying { .. } => assert!(open_step.is_some(), "{e:?}"),
            AgentEvent::ToolCallRequested { step, call_id, .. } => {
                assert_eq!(open_step, Some(*step));
                assert!(step_of_call.insert(call_id.clone(), *step).is_none());
                calls.insert(call_id.clone(), vec!["requested"]);
                order.push(call_id.clone());
            }
            AgentEvent::ToolCallStarted { call_id, .. } => {
                assert_eq!(open_step, None);
                calls.get_mut(call_id).unwrap().push("started");
            }
            AgentEvent::ToolOutput { call_id, .. } => {
                calls.get_mut(call_id).unwrap().push("output");
            }
            AgentEvent::ToolCallFinished {
                call_id,
                outcome,
                output,
                ..
            } => {
                assert_eq!(open_step, None);
                let seq = calls.get_mut(call_id).unwrap();
                seq.push("finished");
                let started = seq.contains(&"started");
                match outcome {
                    ToolOutcome::NotRun { .. } | ToolOutcome::InvalidInput { .. } => {
                        assert_eq!(seq, &["requested", "finished"]);
                        assert_eq!(
                            output,
                            &ToolOutputView {
                                inlined: true,
                                ..ToolOutputView::default()
                            }
                        );
                    }
                    ToolOutcome::Completed { .. }
                    | ToolOutcome::TimedOut { .. }
                    | ToolOutcome::Cancelled
                    | ToolOutcome::Backgrounded { .. } => assert!(started, "{seq:?}"),
                    ToolOutcome::Failed { .. } => {}
                }
            }
            _ => {}
        }
    }
    assert_eq!(open_step, None, "a step never ended");
    for seq in calls.values() {
        assert_eq!(seq.first(), Some(&"requested"));
        assert_eq!(
            seq.iter().filter(|s| **s == "finished").count(),
            1,
            "{seq:?}"
        );
        assert_eq!(seq.last(), Some(&"finished"));
    }
    let finished_order: Vec<String> = finished(events).into_iter().map(|(id, _)| id).collect();
    assert_eq!(
        finished_order, order,
        "calls finish in the order they were asked"
    );
}

fn assert_layout(provider: &Fake) {
    for (k, r) in provider.sent().iter().enumerate() {
        if let Err(e) = check_layout(&r.messages, &[], &Default::default(), provider) {
            panic!("request {k}: {e}");
        }
    }
}

#[tokio::test]
async fn every_test_tool_runs_in_one_step_and_in_three() {
    for sample in samples() {
        let name = sample.tool.spec().name.clone();
        let input = sample.input.clone();
        let env = Env::new("samples");
        let provider = scripted(vec![
            uses(&[B::Think, call("a", &name, input.clone())]),
            uses(&[
                call("b", &name, input.clone()),
                call("c", &name, input.clone()),
            ]),
            done("all done"),
        ]);
        let mut agent = agent(provider.clone(), &env);
        let (outcome, events) = turn(&mut agent, "go").await;
        assert!(
            matches!(
                outcome,
                TurnOutcome::Completed {
                    committed: true,
                    steps: 3,
                    tool_calls: 3,
                    stop_reason: StopReason::EndTurn,
                    ..
                }
            ),
            "{name}: {outcome:?}"
        );
        check_events(&events);
        let quick = Quick {
            name: name.clone(),
            input: input.clone(),
        };
        assert_ran(&quick, &events, &provider, 3);
        assert_layout(&provider);
        let history = agent.history();
        assert_eq!(history.len(), 6, "{name}");
        let ids: Vec<&str> = results(&history[2])
            .into_iter()
            .chain(results(&history[4]))
            .map(|(id, text, is_error)| {
                assert!(!is_error, "{name}: {text}");
                assert_eq!(text, expected_text(&input), "{name}");
                id
            })
            .collect();
        assert_eq!(
            ids,
            ["a", "b", "c"],
            "parallel calls answered in one message, in order"
        );
        assert_eq!(tool_uses(&history), 3);
        for (_, outcome) in finished(&events) {
            assert!(
                matches!(
                    outcome,
                    ToolOutcome::Completed {
                        is_error: false,
                        ..
                    }
                ),
                "{name}"
            );
        }
        let files: Vec<String> = fs::read_dir(agent.output_dir().unwrap().path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(
            files,
            [1, 2, 3]
                .map(|n| crate::call::OutputDir::file_name(n, &name))
                .to_vec()
        );
    }
}

#[tokio::test]
async fn unknown_tools_and_invalid_input_are_answered_without_failing_the_turn() {
    let env = Env::new("invalid");
    let provider = scripted(vec![
        uses(&[
            call("u", "teleport", json!({"to": "mars"})),
            call("i", "sleep", json!({"ms": "soon"})),
        ]),
        done("ok"),
    ]);
    let mut agent = agent(provider.clone(), &env);
    let (outcome, events) = turn(&mut agent, "go").await;
    assert!(matches!(
        outcome,
        TurnOutcome::Completed {
            committed: true,
            steps: 2,
            ..
        }
    ));
    check_events(&events);
    let displays: Vec<&ToolAction> = pick!(
        &events,
        AgentEvent::ToolCallRequested { display, .. } => &display.action
    );
    assert_eq!(
        displays,
        [
            &ToolAction::Other {
                name: "teleport".into()
            },
            &ToolAction::Other {
                name: "sleep".into()
            }
        ]
    );
    assert_eq!(
        results(&agent.history()[2]),
        [
            (
                "u",
                "Invalid input: there is no tool named `teleport`",
                true
            ),
            ("i", "Invalid input: `ms` must be a whole number", true),
        ]
    );
    assert!(
        events
            .iter()
            .all(|e| !matches!(e, AgentEvent::ToolCallStarted { .. }))
    );
}

#[tokio::test]
async fn cancel_while_the_first_reply_streams_keeps_nothing() {
    let env = Env::new("cancel1");
    let provider = scripted(vec![hanging(&[B::Text("partial")])]);
    let mut agent = agent(provider, &env);
    let (outcome, events) = turn_with(
        &mut agent,
        "go",
        Some(|e| matches!(e, AgentEvent::TextDelta { .. })),
    )
    .await;
    assert!(matches!(
        outcome,
        TurnOutcome::Cancelled {
            committed_steps: 0,
            usage: Some(_),
            ..
        }
    ));
    check_events(&events);
    assert!(agent.history().is_empty());
}

#[tokio::test]
async fn cancel_while_a_later_reply_streams_keeps_the_committed_steps() {
    let env = Env::new("cancel3");
    let provider = scripted(vec![
        uses(&[call("a", "sleep", json!({"ms": 0, "text": "one\n"}))]),
        uses(&[call("b", "sleep", json!({"ms": 0, "text": "two\n"}))]),
        hanging(&[B::Text("partial")]),
        done("next"),
    ]);
    let mut agent = agent(provider.clone(), &env);
    let (outcome, events) = turn_with(
        &mut agent,
        "go",
        Some(|e| matches!(e, AgentEvent::TextDelta { .. })),
    )
    .await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                committed_steps: 2,
                ..
            }
        ),
        "{outcome:?}"
    );
    check_events(&events);
    let kept = agent.history().to_vec();
    assert_eq!(kept.len(), 5);
    assert_eq!(kept[4].role, Role::User);

    turn(&mut agent, "and now").await;
    let requests = provider.sent();
    let last = &requests[3].messages;
    assert_eq!(last.len(), 5, "the new text joins the open tail");
    assert_eq!(last[..4], kept[..4]);
    assert_eq!(last[4].content[..1], kept[4].content[..]);
    assert!(
        matches!(&last[4].content[2], ContentBlock::Text { text } if text.starts_with("Context: "))
    );
    assert!(matches!(&last[4].content[3], ContentBlock::Text { text } if text == "and now"));
    assert_layout(&provider);
}

#[tokio::test]
async fn cancel_while_a_tool_runs_keeps_its_partial_output_and_answers_the_rest() {
    let env = Env::new("cancel-tool");
    let provider = scripted(vec![
        uses(&[
            call("a", "sleep", json!({"ms": 60_000, "text": "working\n"})),
            call("b", "sleep", json!({"ms": 0})),
        ]),
        done("after"),
    ]);
    let mut agent = agent(provider.clone(), &env);
    let started = Instant::now();
    let (outcome, events) = turn_with(&mut agent, "go", Some(on_started)).await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                committed_steps: 1,
                ..
            }
        ),
        "{outcome:?}"
    );
    check_events(&events);
    assert_eq!(
        finished(&events),
        [
            ("a".to_owned(), ToolOutcome::Cancelled),
            (
                "b".to_owned(),
                ToolOutcome::NotRun {
                    reason: NotRunReason::TurnStopped
                }
            ),
        ]
    );
    assert_eq!(
        results(&agent.history()[2]),
        [
            ("a", "Output:\nworking\n[stopped by the user]", true),
            (
                "b",
                "Not run: the turn was stopped before this call could run.",
                true
            ),
        ]
    );
    let kept = agent.history().to_vec();
    turn(&mut agent, "continue").await;
    let bodies = provider.texts();
    let (_, before) = head_and_messages(&bodies[0]);
    let (_, after) = head_and_messages(&bodies[1]);
    assert!(after.len() > before.len());
    let kept_json = serde_json::to_string(&kept[..2]).unwrap();
    assert!(!kept_json.is_empty());
    assert_eq!(provider.sent()[1].messages[..2], kept[..2]);
    let result_bytes = serde_json::to_string(&kept[2].content).unwrap();
    let next = serde_json::to_string(&provider.sent()[1].messages[2].content[..2]).unwrap();
    assert_eq!(
        result_bytes, next,
        "the synthetic results go back byte for byte"
    );
    assert_layout(&provider);
}

#[tokio::test]
async fn a_tool_that_ignores_cancel_is_abandoned_after_the_grace() {
    let env = Env::new("stubborn");
    let provider = scripted(vec![uses(&[call(
        "a",
        "sleep",
        json!({"ms": 60_000, "stubborn": true}),
    )])]);
    let mut agent = agent(provider, &env);
    let started = Instant::now();
    let (outcome, events) = turn_with(&mut agent, "go", Some(on_started)).await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(matches!(
        outcome,
        TurnOutcome::Cancelled {
            committed_steps: 1,
            ..
        }
    ));
    check_events(&events);
    let history = agent.history();
    let (_, text, _) = results(&history[2])[0];
    assert!(
        text.ends_with(&format!("[stopped by the user]\n{ABANDONED_NOTE}")),
        "{text}"
    );
}

#[tokio::test]
async fn a_provider_error_at_step_two_keeps_step_one() {
    for sample in quick_samples() {
        let env = Env::new("error2");
        let mut failing = vec![Ok(ProviderEvent::Sending), Ok(start_on(DEFAULT_MODEL, 600))];
        failing.push(Err(ProviderError::new(ErrorKind::Overloaded, "Overloaded")));
        let provider = scripted(vec![
            uses(&[call("a", &sample.name, sample.input.clone())]),
            Step::from(failing),
        ]);
        let mut agent = agent(provider.clone(), &env);
        let (outcome, events) = turn(&mut agent, "go").await;
        assert!(
            matches!(
                outcome,
                TurnOutcome::Error {
                    kind: ErrorKind::Overloaded,
                    committed_steps: 1,
                    ..
                }
            ),
            "{outcome:?}"
        );
        check_events(&events);
        assert_eq!(agent.history().len(), 3);
        assert_ran(&sample, &events, &provider, 1);
    }
}

#[tokio::test]
async fn every_committed_call_gets_one_result_whatever_the_stop_reason() {
    for sample in quick_samples() {
        let stops = [
            StopReason::ToolUse,
            StopReason::EndTurn,
            StopReason::ContextWindowExceeded,
            StopReason::StopSequence,
        ];
        for stop in stops {
            for n in 0..=2 {
                if stop == StopReason::ContextWindowExceeded && n > 0 {
                    continue;
                }
                let env = Env::new("stops");
                let mut content = vec![B::Text("before")];
                for k in 0..n {
                    content.push(call(&format!("c{k}"), &sample.name, sample.input.clone()));
                }
                let mut scripts = vec![reply(&content, stop.clone())];
                if stop == StopReason::ToolUse && n > 0 {
                    scripts.push(done("after"));
                }
                let provider = scripted(scripts);
                let mut agent = agent(provider.clone(), &env);
                let (outcome, events) = turn(&mut agent, "go").await;
                let label = format!("{} with {n} calls", stop.as_str());
                check_events(&events);
                assert_ran(
                    &sample,
                    &events,
                    &provider,
                    if stop == StopReason::ToolUse { n } else { 0 },
                );
                assert_layout(&provider);
                let history = agent.history();
                assert_eq!(tool_uses(&history), n, "{label}");
                assert_eq!(finished(&events).len(), n, "{label}");
                let expected_stop = if stop == StopReason::ToolUse && n > 0 {
                    StopReason::EndTurn
                } else {
                    stop.clone()
                };
                let TurnOutcome::Completed {
                    stop_reason,
                    committed: true,
                    tool_calls,
                    ..
                } = &outcome
                else {
                    panic!("{label}: {outcome:?}")
                };
                assert_eq!(stop_reason, &expected_stop, "{label}");
                assert_eq!(*tool_calls as usize, n, "{label}");
                for (_, outcome) in finished(&events) {
                    if stop == StopReason::ToolUse {
                        assert!(matches!(outcome, ToolOutcome::Completed { .. }), "{label}");
                    } else {
                        assert_eq!(
                            outcome,
                            ToolOutcome::NotRun {
                                reason: NotRunReason::ReplyStopped {
                                    stop_reason: stop.clone()
                                }
                            },
                            "{label}"
                        );
                    }
                }
                if stop != StopReason::ToolUse && n > 0 {
                    let (_, text, is_error) = results(&history[2])[0];
                    assert!(is_error);
                    assert_eq!(
                        text,
                        format!(
                            "Not run: the reply stopped ({}) before this call could run.",
                            stop.as_str()
                        )
                    );
                    assert_eq!(history.len(), 3, "{label}: an open tail");
                    assert!(
                        events
                            .iter()
                            .all(|e| !matches!(e, AgentEvent::ToolCallStarted { .. })),
                        "{label}"
                    );
                }
            }
        }
    }
}

fn refusal() -> Step {
    let mut script = vec![Ok(ProviderEvent::Sending), Ok(start_on(DEFAULT_MODEL, 50))];
    script.extend(ended(StopReason::Refusal, Usage::default(), Vec::new()));
    Step::from(script)
}

#[tokio::test]
async fn a_refusal_commits_nothing_from_its_step() {
    for sample in quick_samples() {
        let env = Env::new("refusal");
        let provider = scripted(vec![
            uses(&[call("a", &sample.name, sample.input.clone())]),
            refusal(),
        ]);
        let mut agent = agent(provider.clone(), &env);
        let (outcome, events) = turn(&mut agent, "go").await;
        assert!(matches!(
            outcome,
            TurnOutcome::Error {
                kind: ErrorKind::Refusal,
                committed_steps: 1,
                ..
            }
        ));
        check_events(&events);
        assert_eq!(agent.history().len(), 3);
        assert_ran(&sample, &events, &provider, 1);
    }
}

#[tokio::test]
async fn a_reply_that_cannot_be_committed_ends_the_turn_and_keeps_earlier_steps() {
    for sample in quick_samples() {
        for at in [1usize, 3] {
            let env = Env::new("uncommittable");
            let mut scripts = Vec::new();
            for k in 1..at {
                scripts.push(uses(&[call(
                    &format!("c{k}"),
                    &sample.name,
                    sample.input.clone(),
                )]));
            }
            scripts.push(reply(&[B::Text("   ")], StopReason::StopSequence));
            let provider = scripted(scripts);
            let mut agent = agent(provider.clone(), &env);
            let (outcome, events) = turn(&mut agent, "go").await;
            check_events(&events);
            assert_ran(&sample, &events, &provider, at - 1);
            let TurnOutcome::Completed {
                committed: false,
                steps,
                ..
            } = outcome
            else {
                panic!("{outcome:?}")
            };
            assert_eq!(steps as usize, at);
            assert_eq!(
                agent.history().len(),
                if at == 1 { 0 } else { 1 + 2 * (at - 1) }
            );
        }
    }
}

#[tokio::test]
async fn a_cut_reply_fails_the_turn_commits_nothing_and_resends_only_a_capped_try() {
    let neon = "gemini-4-neon-low";
    let cap = crate::models::ModelTable::shared().reply_cap(neon).unwrap();
    let full = DEFAULT_MAX_TOKENS;
    let prices = |model: &str| {
        format!("[models.\"{model}\"]\nprice_input = 1\nprice_output = 4\nprice_cache_read = 0.1\n")
    };
    let config =
        crate::models::ConfigModels::parse(&(prices(neon) + &prices(DEFAULT_MODEL))).unwrap();
    let models =
        crate::context::ModelSettings::new(crate::models::ModelTable::shipped().unwrap(), config);
    let cut = |model: &str, content: &[B], input| {
        Step::from(reply_on(model, input, content, StopReason::MaxTokens))
    };
    let draft = [B::Text("cut draft")];
    let tool_step = |model: &str| {
        Step::from(reply_on(
            model,
            600,
            &[call("c1", "sleep", json!({"ms": 0}))],
            StopReason::ToolUse,
        ))
    };
    let cut_call = [B::Text("cut draft"), call("c2", "sleep", json!({"ms": 0}))];
    let cases = [
        (
            neon,
            vec![cut(neon, &draft, 200), cut(neon, &draft, 300)],
            full,
            2,
            0,
            vec![cap, full],
            0,
            usage(500, 0, 20),
        ),
        (
            neon,
            vec![
                tool_step(neon),
                cut(neon, &draft, 200),
                cut(neon, &draft, 300),
            ],
            full,
            2,
            1,
            vec![cap, cap, full],
            3,
            usage(1100, 0, 30),
        ),
        (
            neon,
            vec![cut(neon, &draft, 200)],
            cap,
            1,
            0,
            vec![cap],
            0,
            usage(200, 0, 10),
        ),
        (
            DEFAULT_MODEL,
            vec![cut(DEFAULT_MODEL, &[B::Think], 200)],
            full,
            1,
            0,
            vec![full],
            0,
            usage(200, 0, 10),
        ),
        (
            DEFAULT_MODEL,
            vec![cut(DEFAULT_MODEL, &draft, 200)],
            full,
            1,
            0,
            vec![full],
            0,
            usage(200, 0, 10),
        ),
        (
            DEFAULT_MODEL,
            vec![tool_step(DEFAULT_MODEL), cut(DEFAULT_MODEL, &cut_call, 200)],
            full,
            1,
            1,
            vec![full, full],
            3,
            usage(800, 0, 20),
        ),
    ];
    for (
        model,
        steps,
        max_tokens,
        want_attempts,
        want_steps,
        want_limits,
        want_history,
        want_usage,
    ) in cases
    {
        let env = Env::new("reply-cap");
        let provider = scripted(steps);
        let mut agent = agent(provider.clone(), &env)
            .with_model_settings(models.clone())
            .with_retry_policy(super::retry::quick());
        agent.session.model = model.to_owned();
        agent.session.max_tokens = max_tokens;
        *agent.settings = settings::SettingsState::new(&agent.session);
        let (outcome, events) = turn(&mut agent, "go").await;
        check_events(&events);
        let retries: Vec<(Option<f64>, bool)> = pick!(
            &events,
            AgentEvent::Retrying {
                cost_usd,
                lower_bound,
                ..
            } => (*cost_usd, *lower_bound)
        );
        assert_eq!(retries.len(), (want_attempts - 1) as usize);
        for (cost_usd, lower_bound) in retries.into_iter().chain(costs(&events)) {
            assert!(cost_usd.is_some());
            assert!(!lower_bound);
        }
        let TurnOutcome::Error {
            kind,
            usage: got_usage,
            cost: Some(cost),
            committed_steps,
            attempts,
            gave_up,
            ..
        } = outcome
        else {
            panic!("{outcome:?}");
        };
        assert_eq!(kind, ErrorKind::InvalidResponse);
        assert_eq!(gave_up, Some(GaveUp::NotRetryable));
        assert_eq!(attempts, want_attempts);
        assert_eq!(committed_steps, want_steps);
        assert_eq!(got_usage, Some(want_usage));
        let exact =
            want_usage.input_tokens as f64 / 1e6 + want_usage.output_tokens as f64 * 4.0 / 1e6;
        assert!(!cost.turn_lower_bound && !cost.session_lower_bound);
        assert!((cost.turn_usd - exact).abs() < 1e-12, "{cost:?}");
        assert_eq!(cost.session_usd, cost.turn_usd);
        let limits: Vec<u32> = provider
            .sent()
            .iter()
            .map(|s| s.session.max_tokens)
            .collect();
        assert_eq!(limits, want_limits);
        assert_eq!(agent.history().len(), want_history);
        assert!(
            !serde_json::to_string(&agent.history())
                .unwrap()
                .contains("cut draft")
        );
    }
}

#[tokio::test]
async fn cut_off_tool_input_and_unfinished_blocks_are_invalid_responses() {
    let env = Env::new("truncated");
    let mut cut = vec![Ok(ProviderEvent::Sending), Ok(start_on(DEFAULT_MODEL, 600))];
    cut.push(Ok(ProviderEvent::BlockStart {
        index: 0,
        block: ContentBlock::ToolUse {
            id: "a".into(),
            name: "sleep".into(),
            input: json!({}),
        },
    }));
    cut.push(Ok(ProviderEvent::BlockDelta {
        index: 0,
        delta: Delta::InputJson("{\"ms\": 1".into()),
    }));
    let mut unfinished = cut.clone();
    cut.push(Ok(ProviderEvent::BlockStop { index: 0 }));
    unfinished.extend(ended(StopReason::MaxTokens, Usage::default(), Vec::new()));
    let provider = scripted(vec![
        uses(&[call("x", "sleep", json!({"ms": 0}))]),
        Step::from(cut),
        Step::from(unfinished),
    ]);
    let mut agent = agent(provider, &env);
    for expected_steps in [1, 0] {
        let (outcome, events) = turn(&mut agent, "go").await;
        check_events(&events);
        let TurnOutcome::Error {
            kind,
            committed_steps,
            ..
        } = &outcome
        else {
            panic!("{outcome:?}")
        };
        assert_eq!(*kind, ErrorKind::InvalidResponse, "{outcome:?}");
        assert_eq!(*committed_steps, expected_steps);
    }
    assert_eq!(agent.history().len(), 3);
}

#[tokio::test]
async fn calls_run_in_the_tracked_directory_and_fall_back_to_the_root_then_home() {
    let env = Env::new("cwd");
    fs::create_dir(env.work.join("sub")).unwrap();
    let provider = scripted(vec![
        uses(&[call("a", "sleep", json!({"ms": 0, "cd": "sub"}))]),
        uses(&[call("b", "sleep", json!({"ms": 0}))]),
        done("ok"),
        uses(&[call("c", "sleep", json!({"ms": 0}))]),
        done("ok"),
        uses(&[call("d", "sleep", json!({"ms": 0}))]),
        done("ok"),
    ]);
    let store = MemoryStore::default();
    let mut agent = logged(provider, &env, &store);
    let (_, events) = turn(&mut agent, "one").await;
    let cwds: Vec<&str> = pick!(&events, AgentEvent::ToolCallStarted { cwd, .. } => cwd.as_str());
    assert_eq!(cwds, ["~/work", "~/work/sub"]);
    let history = agent.history();
    let (_, first, _) = results(&history[2])[0];
    assert!(first.ends_with("Working directory: ~/work/sub"), "{first}");

    fs::remove_dir(env.work.join("sub")).unwrap();
    turn(&mut agent, "two").await;
    let messages = agent.history();
    let (_, text, _) = results(&messages[8])[0];
    assert!(
        text.ends_with("~/work/sub no longer exists; this ran in ~/work"),
        "{text}"
    );

    fs::remove_dir_all(&env.work).unwrap();
    let (_, events) = turn(&mut agent, "three").await;
    check_events(&events);
    let cwds: Vec<&str> = pick!(&events, AgentEvent::ToolCallStarted { cwd, .. } => cwd.as_str());
    assert_eq!(cwds, ["~"]);
    let messages = agent.history();
    let (_, text, _) = results(&messages[messages.len() - 2])[0];
    assert!(
        text.ends_with("~/work/sub no longer exists; this ran in ~"),
        "{text}"
    );
    let started = records(&store)
        .iter()
        .filter(|r| matches!(r.event, LogEvent::ToolCallStarted { .. }))
        .count();
    assert_eq!(started, 4);
}

struct CountingGate(Mutex<Vec<(String, PathBuf)>>);

impl Gate for CountingGate {
    fn check(&self, call: &GateCall<'_>) -> Verdict {
        self.0
            .lock()
            .unwrap()
            .push((call.tool.to_owned(), call.cwd.to_owned()));
        Verdict::Allow
    }
    fn check_host(&self, _call: &crate::policy::HostCall<'_>) -> Verdict {
        Verdict::Allow
    }
}

#[tokio::test]
async fn the_gate_sees_each_started_call_once_with_its_directory() {
    for sample in quick_samples() {
        let env = Env::new("gate");
        let provider = scripted(vec![
            uses(&[
                call("a", &sample.name, sample.input.clone()),
                call("b", "nope", json!({})),
                call("c", &sample.name, sample.input.clone()),
            ]),
            done("ok"),
        ]);
        let gate = Arc::new(CountingGate(Mutex::new(Vec::new())));
        let mut agent = agent(provider.clone(), &env).with_gate(gate.clone());
        let (_, events) = turn(&mut agent, "go").await;
        assert_ran(&sample, &events, &provider, 2);
        let started = events.iter().filter(|e| on_started(e)).count();
        let seen = gate.0.lock().unwrap().clone();
        assert_eq!(seen.len(), started);
        assert_eq!(
            seen,
            [
                (sample.name.clone(), env.work.clone()),
                (sample.name.clone(), env.work.clone())
            ]
        );
    }
}

#[tokio::test]
async fn a_deleted_output_directory_is_made_again_before_the_next_call() {
    let env = Env::new("outdir");
    let provider = scripted(vec![
        uses(&[call("a", "sleep", json!({"ms": 0, "text": "x"}))]),
        done("ok"),
        uses(&[call("b", "sleep", json!({"ms": 0, "text": "y"}))]),
        done("ok"),
    ]);
    let mut agent = agent(provider, &env);
    turn(&mut agent, "one").await;
    let dir = agent.output_dir().unwrap();
    fs::remove_dir_all(dir.path()).unwrap();
    let (_, events) = turn(&mut agent, "two").await;
    let AgentEvent::ToolCallFinished { output, .. } = events
        .iter()
        .find(|e| matches!(e, AgentEvent::ToolCallFinished { .. }))
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(output.file_problem, None);
    assert_eq!(
        fs::read_to_string(dir.path().join("0002-sleep.out")).unwrap(),
        "y"
    );
}

#[tokio::test]
async fn a_large_result_points_at_its_file_under_a_shortened_home() {
    let env = Env::new("large");
    let big: String = (1..=3000).map(|i| format!("row {i}\n")).collect();
    let provider = scripted(vec![
        uses(&[call("a", "sleep", json!({"ms": 0, "text": big}))]),
        done("ok"),
    ]);
    let mut agent = agent(provider, &env);
    let (_, events) = turn(&mut agent, "go").await;
    let messages = agent.history();
    let (_, text, _) = results(&messages[2])[0];
    let short = format!("~/tmp/wisp-{}/0001-sleep.out", agent.session_id);
    assert!(
        text.contains(&format!("saved in full to {short}")),
        "{text}"
    );
    assert!(text.ends_with("row 3000"), "{text}");
    let output = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolCallFinished { output, .. } => Some(output.clone()),
            _ => None,
        })
        .unwrap();
    assert!(!output.inlined);
    assert_eq!(output.lines, 3000);
    assert_eq!(fs::read_to_string(output.file.unwrap()).unwrap(), big);
    let streamed = streamed(&events);
    assert_eq!(streamed, big);
}

#[tokio::test]
async fn calls_before_a_fallback_boundary_are_not_run() {
    for sample in quick_samples() {
        let env = Env::new("fallback");
        let mut script = vec![Ok(ProviderEvent::Sending), Ok(start_on(DEFAULT_MODEL, 600))];
        script.extend(blocks(&[call("early", &sample.name, sample.input.clone())]));
        script.extend(opaque(1, fallback_marker(DEFAULT_MODEL, "claude-opus-4-8")));
        let mut rest = blocks(&[B::Text("x"), B::Text("answer")]);
        rest.drain(..3);
        for item in &mut rest {
            if let Ok(
                ProviderEvent::BlockStart { index, .. }
                | ProviderEvent::BlockDelta { index, .. }
                | ProviderEvent::BlockStop { index },
            ) = item
            {
                *index = 2;
            }
        }
        script.extend(rest);
        script.extend(ended(StopReason::EndTurn, Usage::default(), Vec::new()));
        let provider = scripted(vec![Step::from(script)]);
        let mut agent = agent(provider.clone(), &env);
        let (outcome, events) = turn(&mut agent, "go").await;
        assert!(matches!(
            outcome,
            TurnOutcome::Completed {
                committed: true,
                tool_calls: 0,
                ..
            }
        ));
        assert!(finished(&events).is_empty());
        check_events(&events);
        assert_ran(&sample, &events, &provider, 0);
    }
}

async fn scenario(env: &Env, store: &MemoryStore) -> (Agent, Arc<Fake>) {
    sidebar_scenario(env, store, false, None).await
}

async fn line_hooks(agent: Agent, calls: Option<&wisp_plugin_testkit::hooks::Calls>) -> Agent {
    use wisp_plugin_testkit::hooks as kit;
    let mut sources = crate::plugin::testing::turn_line_sources();
    if let Some(calls) = calls {
        let body = kit::section("sec", &kit::every_point());
        let make: crate::plugin::Make = kit::sections(kit::none(), kit::busy(), calls.clone());
        sources.push((kit::slotted("zz", &body), make));
        let body = "[capabilities]\nrequested = [\"usage\"]\n".to_owned()
            + &kit::section("limits", "marks = [\"usage-changed\"]\n");
        let render: kit::Render = Arc::new(|host, name, view| {
            assert_eq!(host.usage().accounts().len(), 1);
            kit::busy()(host, name, view)
        });
        sources.push((
            kit::manifest_in("0.4.0", "todos", &body),
            kit::sections(kit::none(), render, calls.clone()),
        ));
    }
    let (registry, problems) =
        crate::plugin::Registry::from_sources(sources, &crate::plugin::PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry
        .start(
            &std::collections::BTreeSet::new(),
            Arc::new(crate::plugin::KernelHost::new()),
        )
        .await;
    agent.with_hooks(started.hooks, started.host)
}

fn sidebar(busy: bool, page: bool) -> Option<ClientCommand> {
    let name = || {
        vec![
            "zz/sec".to_owned(),
            "context/window".to_owned(),
            "todos/limits".to_owned(),
        ]
    };
    busy.then(|| ClientCommand::SlotsShown {
        sections: name(),
        pages: if page { name() } else { Vec::new() },
    })
}

async fn sidebar_scenario(
    env: &Env,
    store: &MemoryStore,
    lines: bool,
    calls: Option<&wisp_plugin_testkit::hooks::Calls>,
) -> (Agent, Arc<Fake>) {
    let busy = calls.is_some();
    let provider = scripted(vec![
        uses(&[
            B::Think,
            B::Redacted,
            call("a", "sleep", json!({"ms": 0, "text": "one\r\ntwo\n"})),
            call("b", "teleport", json!({})),
        ]),
        reply(&[B::Think, B::Text("one")], StopReason::EndTurn),
        uses(&[B::Think, call("c", "sleep", json!({"ms": 60_000}))]),
        reply(
            &[B::Text("stopping"), call("d", "sleep", json!({"ms": 0}))],
            StopReason::StopSequence,
        ),
        done("fine"),
    ]);
    let mut agent = logged(provider.clone(), env, store);
    if lines || busy {
        let now = agent.clock.now();
        agent = agent.with_clock(Arc::new(ShiftedClock::at(now, None)));
        agent = agent.with_plan_usage(
            crate::plan_usage::PlanUsageCache::default(),
            crate::plan_usage::Display::Enabled,
        );
        agent.settings.layers.endpoints = vec![list_accounts::summary(
            agent.endpoint_name(),
            "anthropic",
            "anthropic_subscription",
            true,
        )];
        agent = line_hooks(agent, calls).await;
    }
    let turns: [(&str, Option<Trigger>, bool); 4] = [
        ("first", None, true),
        ("second", Some(on_started), false),
        ("third", None, true),
        ("fourth", None, false),
    ];
    for (text, cancel_when, page) in turns {
        turn_shown(&mut agent, text, cancel_when, sidebar(busy, page)).await;
        if busy {
            let alias = agent.endpoint_name();
            let generation = agent.usage_generations[alias];
            agent.plan_usage.observe(
                alias,
                generation,
                Ok(wisp_protocol::plan_usage::PlanUsage::new(None, vec![]).unwrap()),
                agent.clock.now().as_second(),
            );
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
    (agent, provider)
}

#[tokio::test]
async fn synthetic_results_are_deterministic() {
    let (env_a, env_b) = (Env::new("det-a"), Env::new("det-b"));
    let (a, _) = scenario(&env_a, &MemoryStore::default()).await;
    let (b, _) = scenario(&env_b, &MemoryStore::default()).await;
    let bytes = |agent: &Agent| {
        serde_json::to_string(&*agent.history())
            .unwrap()
            .replace(&agent.session_id, "SID")
    };
    assert_eq!(bytes(&a), bytes(&b));
}

#[tokio::test]
async fn a_v3_log_rebuilds_the_exact_history_and_the_last_request() {
    let env = Env::new("rebuild");
    let store = MemoryStore::default();
    let (agent, provider) = scenario(&env, &store).await;
    let records = records(&store);
    let rebuilt = rebuild_history(records.iter().map(|r| &r.event), no_images)
        .unwrap()
        .messages;
    assert_eq!(
        serde_json::to_string(&rebuilt).unwrap(),
        serde_json::to_string(&*agent.history()).unwrap()
    );
    let requests = provider.sent();
    let last = requests.last().unwrap();
    assert_eq!(
        rebuilt[..last.messages.len() - 1],
        last.messages[..last.messages.len() - 1]
    );
    let replayed: Vec<Message> = rebuilt[..last.messages.len()].to_vec();
    assert_eq!(
        rebuilt_body(last, &replayed),
        rebuilt_body(last, &last.messages)
    );
    for (k, request) in requests.iter().enumerate() {
        check_layout(
            &request.messages,
            &[],
            &Default::default(),
            provider.as_ref(),
        )
        .unwrap_or_else(|e| panic!("{k}: {e}"));
    }
    let kinds: Vec<&str> = records
        .iter()
        .map(|r| match &r.event {
            LogEvent::ToolCallRequested { .. } => "requested",
            LogEvent::ToolCallStarted { .. } => "started",
            LogEvent::ToolCallFinished { .. } => "finished",
            _ => "",
        })
        .filter(|k| !k.is_empty())
        .collect();
    assert_eq!(kinds.iter().filter(|k| **k == "requested").count(), 4);
    assert_eq!(kinds.iter().filter(|k| **k == "finished").count(), 4);
    let LogEvent::SessionStarted {
        tools,
        tools_sha256,
        cwd,
        output_dir,
        ..
    } = &records[0].event
    else {
        panic!()
    };
    assert_eq!(tools, &agent.toolbox().names());
    assert_eq!(tools_sha256, &agent.toolbox().sha256());
    assert_eq!(cwd.as_deref(), Some("~/work"));
    assert_eq!(
        output_dir.as_deref(),
        Some(env.temp.join("wisp-s1").to_str().unwrap())
    );
}

#[test]
fn version_one_and_two_logs_still_rebuild() {
    let lines = [
        r#"{"schema_version":1,"seq":0,"at":"x","type":"turn_started","turn":1,"text":"a"}"#,
        r#"{"schema_version":1,"seq":1,"at":"x","type":"message_committed","turn":1,"message":{"role":"assistant","content":[{"type":"text","text":"A"}]}}"#,
        r#"{"schema_version":2,"seq":2,"at":"x","type":"turn_started","turn":2,"text":"b","stamp":"Sent: x\n"}"#,
        r#"{"schema_version":2,"seq":3,"at":"x","type":"turn_started","turn":3,"text":"c"}"#,
        r#"{"schema_version":2,"seq":4,"at":"x","type":"message_committed","turn":3,"message":{"role":"assistant","content":[{"type":"text","text":"C"}]}}"#,
    ];
    let records = crate::session::parse_jsonl(lines.join("\n") + "\n").unwrap();
    let rebuilt = rebuild_history(records.iter().map(|r| &r.event), no_images)
        .unwrap()
        .messages;
    assert_eq!(
        rebuilt,
        [
            Message::user_text("a"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text { text: "A".into() }]
            },
            Message::user_text("c"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text { text: "C".into() }]
            },
        ]
    );
}

fn tools_part(body: &str) -> &str {
    let start = body.find(r#""tools":"#).unwrap();
    let end = body.find(r#","system":"#).unwrap();
    &body[start..end]
}

#[tokio::test]
async fn a_tool_loop_keeps_one_prefix_and_extends_it() {
    let env = Env::new("prefix");
    let store = MemoryStore::default();
    let (_, provider) = scenario(&env, &store).await;
    let bodies = provider.texts();
    assert_eq!(bodies.len(), 5);
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
    let v: Value = serde_json::from_str(&bodies[4]).unwrap();
    let tools = v["tools"].as_array().unwrap();
    assert_eq!(tools.len(), test_toolbox().specs().len());
    assert!(tools.iter().all(|t| t.get("cache_control").is_none()));
    assert_eq!(
        v["system"][0]["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
    let rendered = serde_json::to_string(&v["messages"]).unwrap();
    for needle in [
        r#"{"type":"thinking","thinking":"thought 0","signature":"sig0+/=="}"#,
        r#"{"type":"redacted_thinking","data":"Encrypted1=="}"#,
        r#"{"type":"tool_result","tool_use_id":"b","content":[{"type":"text","text":"Invalid input: there is no tool named `teleport`"}],"is_error":true}"#,
        r#"{"type":"tool_result","tool_use_id":"d","content":[{"type":"text","text":"Not run: the reply stopped (stop_sequence) before this call could run."}],"is_error":true}"#,
    ] {
        assert!(bodies[4].contains(needle), "{needle}\n{}", bodies[4]);
    }
    assert!(!rendered.contains("cache_control"));
}

fn masked(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map.iter_mut() {
                if key == "at" || key.ends_with("_at") || key == "ms" || key.ends_with("_ms") {
                    *inner = Value::Null;
                } else {
                    masked(inner);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(masked),
        _ => {}
    }
}

fn masked_log(store: &MemoryStore, env: &Env, session: &str) -> Vec<Value> {
    records(store)
        .iter()
        .map(|record| {
            let text = serde_json::to_string(record)
                .unwrap()
                .replace(env.root.to_str().unwrap(), "ROOT")
                .replace(session, "SID");
            let mut value: Value = serde_json::from_str(&text).unwrap();
            masked(&mut value);
            value
        })
        .collect()
}

#[tokio::test]
async fn a_busy_sidebar_changes_no_request_byte_and_no_log_record() {
    let (quiet_env, busy_env) = (Env::new("sidebar-quiet"), Env::new("sidebar-busy"));
    let (quiet_store, busy_store) = (MemoryStore::default(), MemoryStore::default());
    let (quiet, quiet_provider) = sidebar_scenario(&quiet_env, &quiet_store, true, None).await;
    let calls = wisp_plugin_testkit::hooks::Calls::default();
    let (busy, busy_provider) = sidebar_scenario(&busy_env, &busy_store, true, Some(&calls)).await;
    let renders = calls.renders();
    let pages = renders
        .iter()
        .filter(|(_, v)| *v == wisp_plugin_api::slots::SlotView::Page)
        .count();
    assert!(renders.len() > 4 && pages >= 2, "{renders:?}");
    assert!(calls.rendered("limits", wisp_plugin_api::slots::SlotView::Panel) > 1);
    let bodies = |provider: &Fake, env: &Env, agent: &Agent| {
        provider
            .texts()
            .iter()
            .map(|b| {
                b.replace(env.root.to_str().unwrap(), "ROOT")
                    .replace(&agent.session_id, "SID")
            })
            .collect::<Vec<_>>()
    };
    let quiet_bodies = bodies(&quiet_provider, &quiet_env, &quiet);
    let busy_bodies = bodies(&busy_provider, &busy_env, &busy);
    assert!(
        quiet_bodies.iter().all(|b| b.contains("Sent: ")),
        "{quiet_bodies:?}"
    );
    assert_eq!(busy_bodies.len(), 5);
    assert_eq!(quiet_bodies, busy_bodies);
    for body in &busy_bodies {
        for needle in [wisp_plugin_testkit::hooks::CANARY, "zz/sec", "\"sec\""] {
            assert!(!body.contains(needle), "{needle}\n{body}");
        }
    }
    let quiet_log = masked_log(&quiet_store, &quiet_env, &quiet.session_id);
    let busy_log = masked_log(&busy_store, &busy_env, &busy.session_id);
    assert_eq!(quiet_log.len(), busy_log.len());
    for (a, b) in quiet_log.iter().zip(&busy_log) {
        assert_eq!(a, b);
    }
    assert!(!format!("{busy_log:?}").contains(wisp_plugin_testkit::hooks::CANARY));
}

#[tokio::test]
async fn the_context_block_is_only_in_the_first_user_message() {
    let env = Env::new("context");
    fs::write(env.work.join("CLAUDE.md"), "CLAUDE-INSTRUCTIONS-CANARY").unwrap();
    fs::write(env.work.join("AGENTS.md"), "AGENTS-INSTRUCTIONS-CANARY").unwrap();
    let provider = scripted(vec![
        hanging(&[B::Text("x")]),
        uses(&[call("a", "sleep", json!({"ms": 0}))]),
        done("one"),
        done("two"),
    ]);
    let store = MemoryStore::default();
    let mut agent = logged(provider.clone(), &env, &store).with_test_instructions(
        crate::agent::InstructionParts::auto(),
        Some(&env.root.join("config")),
    );
    turn_with(
        &mut agent,
        "cancelled",
        Some(|e| matches!(e, AgentEvent::TextDelta { .. })),
    )
    .await;
    turn(&mut agent, "first").await;
    turn(&mut agent, "second").await;
    let block = "<environment>\nWorking directory: ~/work\nPlatform: Linux\n</environment>\n";
    let requests = provider.sent();
    for r in &requests {
        let first = &r.messages[0].content[0];
        assert_eq!(first, &ContentBlock::Text { text: block.into() });
        let json = serde_json::to_string(&r.messages).unwrap();
        assert_eq!(json.matches("<environment>").count(), 1);
        assert!(!json.contains("CANARY"));
        let system = serde_json::to_string(&r.session.system).unwrap();
        assert!(!system.contains("CANARY") && !system.contains("work"));
        let project = r.session.instructions.clone().unwrap();
        assert!(project.contains("AGENTS-INSTRUCTIONS-CANARY"), "{project}");
        assert!(!project.contains("CLAUDE") && !project.contains("work"));
    }
    assert!(
        matches!(&requests[0].messages[0].content[3], ContentBlock::Text { text } if text == "cancelled")
    );
    assert!(
        matches!(&requests[1].messages[0].content[3], ContentBlock::Text { text } if text == "first")
    );
    let contexts: Vec<Option<String>> = pick!(
        log_events(&store),
        LogEvent::TurnStarted { lead, .. } => lead.text(LEAD_ENVIRONMENT).map(str::to_owned)
    );
    assert_eq!(
        contexts,
        [Some(block.to_owned()), Some(block.to_owned()), None]
    );
    let bodies = provider.texts();
    assert!(
        head_and_messages(&bodies[3])
            .1
            .starts_with(head_and_messages(&bodies[2]).1.strip_suffix(']').unwrap())
    );
}

#[tokio::test]
async fn a_resume_keeps_the_logged_resources_line_whatever_this_run_reads() {
    let env = Env::new("resources");
    let store = MemoryStore::default();
    let first = scripted(vec![done("one")]);
    let mut live = logged(first.clone(), &env, &store)
        .with_resources(Some("Resources: 24 CPUs, 47 GiB memory"));
    turn(&mut live, "first").await;
    let block = "<environment>\nWorking directory: ~/work\nPlatform: Linux\nResources: 24 CPUs, 47 GiB memory\n</environment>\n";
    assert_eq!(
        first.sent()[0].messages[0].content[0],
        ContentBlock::Text { text: block.into() }
    );
    let records = records(&store);
    let (_, log) = continued(&records);
    let resumed = scripted(vec![done("two")]);
    let mut again = agent(resumed.clone(), &env)
        .with_resources(Some(
            "Resources: 1 CPU (cgroup limit; nproc shows 24), 47 GiB memory",
        ))
        .with_log(log, log_context())
        .resume(&records, 0)
        .unwrap();
    turn(&mut again, "second").await;
    let request = &resumed.sent()[0];
    assert_eq!(
        request.messages[0].content[0],
        ContentBlock::Text { text: block.into() }
    );
    let json = serde_json::to_string(&request.messages).unwrap();
    assert_eq!(json.matches("<environment>").count(), 1);
    assert!(!json.contains("1 CPU"));
}

#[tokio::test]
async fn a_session_without_tools_sends_no_tools_and_no_context() {
    let provider = scripted(vec![done("hi")]);
    let mut agent = Agent::new(provider.clone(), default_session()).with_clock(TickingClock::new());
    turn(&mut agent, "hello").await;
    let body = &provider.texts()[0];
    assert!(!body.contains(r#""tools""#));
    assert!(!body.contains("environment"));
    assert_eq!(agent.history()[0].content.len(), 3);
}

pub(super) fn costs(events: &[AgentEvent]) -> Vec<(Option<f64>, bool)> {
    pick!(
        events,
        AgentEvent::StepEnded {
            cost_usd,
            lower_bound,
            ..
        } => (*cost_usd, *lower_bound)
    )
}

#[tokio::test]
async fn each_request_is_priced_once_and_the_turn_is_their_sum() {
    for sample in quick_samples() {
        let env = Env::new("pricing");
        let haiku = "claude-haiku-5-5";
        let provider = scripted(vec![
            Step::from(reply_on(
                haiku,
                60_000,
                &[call("a", &sample.name, sample.input.clone())],
                StopReason::ToolUse,
            )),
            Step::from(reply_on(
                haiku,
                120_000,
                &[call("b", &sample.name, sample.input.clone())],
                StopReason::ToolUse,
            )),
            Step::from(reply_on(
                haiku,
                60_000,
                &[B::Text("done")],
                StopReason::EndTurn,
            )),
        ]);
        let mut agent = agent(provider.clone(), &env);
        let (outcome, events) = turn(&mut agent, "go").await;
        let steps = costs(&events);
        assert_ran(&sample, &events, &provider, 2);
        assert_eq!(steps.len(), 3);
        let prices: Vec<f64> = steps.iter().map(|(c, _)| c.unwrap()).collect();
        let short = 60_000.0 * 0.10 / 1e6 + 10.0 * 0.50 / 1e6;
        let long = 120_000.0 * 0.50 / 1e6 + 10.0 * 2.50 / 1e6;
        assert!((prices[0] - short).abs() < 1e-12, "{prices:?}");
        assert!((prices[1] - long).abs() < 1e-12, "{prices:?}");
        let TurnOutcome::Completed {
            cost: Some(cost),
            usage,
            ..
        } = outcome
        else {
            panic!("{outcome:?}")
        };
        assert_eq!(cost.turn_usd, prices[0] + prices[1] + prices[2]);
        assert_eq!(cost.session_usd, cost.turn_usd);
        assert!(!cost.turn_lower_bound);
        assert_eq!(usage.input_tokens, 240_000);
        assert_eq!(usage.output_tokens, 30);
    }
}

#[tokio::test]
async fn every_request_sent_ends_one_step_even_when_it_fails() {
    for sample in quick_samples() {
        let env = Env::new("steps");
        let marker = ProviderEvent::Unknown {
            provider: "anthropic".into(),
            raw: json!({"type": "marker"}),
        };
        let billed = ProviderError::new(ErrorKind::Network, "lost after sending").possibly_billed();
        let provider = scripted(vec![
            turned_away(),
            uses(&[call("a", &sample.name, sample.input.clone())]),
            Step::from(vec![Ok(ProviderEvent::Sending), Ok(marker)]).hanging(),
            uses(&[call("b", &sample.name, sample.input.clone())]),
            Step::from(vec![Ok(ProviderEvent::Sending), Err(billed)]),
            uses(&[call("c", &sample.name, sample.input.clone())]),
            hanging(&[B::Text("partial")]),
        ]);
        let mut agent = agent(provider.clone(), &env).with_retry_policy(super::retry::quick());

        let (outcome, events) = turn_with(
            &mut agent,
            "a",
            Some(|e| matches!(e, AgentEvent::UnknownProviderEvent { .. })),
        )
        .await;
        check_events(&events);
        let steps = costs(&events);
        assert_ran(&sample, &events, &provider, 1);
        assert_eq!(
            steps.len(),
            2,
            "one step for the retried request, one for the cancelled one"
        );
        assert!(steps[0].0.is_some() && !steps[0].1);
        assert_eq!(steps[1], (None, true));
        let TurnOutcome::Cancelled {
            cost: Some(cost),
            committed_steps: 1,
            ..
        } = outcome
        else {
            panic!("{outcome:?}")
        };
        assert_eq!(cost.turn_usd, steps[0].0.unwrap());
        assert!(cost.turn_lower_bound && cost.session_lower_bound);
        let ends: Vec<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e, AgentEvent::StepEnded { .. }))
            .map(|(i, _)| i)
            .collect();
        assert!(matches!(
            events[ends[1] - 1],
            AgentEvent::UnknownProviderEvent { .. }
        ));

        let (outcome, events) = turn(&mut agent, "b").await;
        let steps = costs(&events);
        assert_ran(&sample, &events, &provider, 1);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[1], (None, true));
        assert!(matches!(
            outcome,
            TurnOutcome::Error {
                committed_steps: 1,
                cost: Some(Cost {
                    turn_lower_bound: true,
                    ..
                }),
                ..
            }
        ));

        let (outcome, events) = turn_with(
            &mut agent,
            "c",
            Some(|e| matches!(e, AgentEvent::TextDelta { .. })),
        )
        .await;
        let steps = costs(&events);
        assert_ran(&sample, &events, &provider, 1);
        assert_eq!(steps.len(), 2);
        assert!(
            steps[1].0.is_some() && steps[1].1,
            "a cancel after message_start is a lower bound"
        );
        let TurnOutcome::Cancelled {
            cost: Some(cost), ..
        } = outcome
        else {
            panic!()
        };
        assert_eq!(cost.turn_usd, steps[0].0.unwrap() + steps[1].0.unwrap());
    }
}

#[tokio::test]
async fn each_turn_ends_in_exactly_one_turn_ended() {
    for sample in quick_samples() {
        let env = Env::new("spawned");
        let provider = scripted(vec![
            uses(&[call("a", &sample.name, sample.input.clone())]),
            done("one"),
            done("two"),
        ]);
        let agent = agent(provider.clone(), &env);
        let (commands, mut events, _task) = spawn(agent);
        for text in ["a", "b"] {
            commands
                .send(ClientCommand::send(text, text))
                .await
                .unwrap();
            let mut seen = Vec::new();
            until_turn_end(&mut events, &mut seen).await;
            let (body, last) = seen.split_at(seen.len() - 1);
            assert!(matches!(last[0], AgentEvent::TurnEnded { .. }));
            assert!(
                !body
                    .iter()
                    .any(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            );
            check_events(body);
            assert_ran(&sample, body, &provider, usize::from(text == "a"));
        }
        drop(commands);
        while events.recv().await.is_some() {}
    }
}

#[test]
fn the_layout_check_catches_unanswered_and_misplaced_results() {
    let provider = scripted(Vec::new());
    let ask = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "t".into(),
            name: "sleep".into(),
            input: json!({}),
        }],
    };
    let answer = |text_first: bool| {
        let result = ContentBlock::ToolResult {
            tool_use_id: "t".into(),
            content: vec![ContentBlock::Text { text: "r".into() }],
            is_error: false,
        };
        let text = ContentBlock::Text { text: "hi".into() };
        Message {
            role: Role::User,
            content: if text_first {
                vec![text, result]
            } else {
                vec![result, text]
            },
        }
    };
    let user = Message::user_text("go");
    assert!(
        check_layout(
            &[user.clone(), ask.clone(), answer(false)],
            &[],
            &Default::default(),
            provider.as_ref()
        )
        .is_ok()
    );
    assert!(
        check_layout(
            &[user.clone(), ask.clone(), answer(true)],
            &[],
            &Default::default(),
            provider.as_ref()
        )
        .is_err()
    );
    assert!(
        check_layout(
            &[user.clone(), ask.clone(), Message::user_text("no")],
            &[],
            &Default::default(),
            provider.as_ref()
        )
        .is_err()
    );
    assert!(
        check_layout(
            &[user.clone(), ask.clone()],
            &[],
            &Default::default(),
            provider.as_ref()
        )
        .is_err()
    );
    assert!(
        check_layout(
            &[user.clone(), user],
            &[],
            &Default::default(),
            provider.as_ref()
        )
        .is_err()
    );
}

#[tokio::test]
async fn a_scripted_matrix_of_stops_keeps_every_request_well_formed() {
    let triggers: [Option<Trigger>; 3] = [
        None,
        Some(on_started),
        Some(|e| matches!(e, AgentEvent::TextDelta { .. })),
    ];
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    for trigger in triggers {
        for stop in [StopReason::MaxTokens, StopReason::ToolUse] {
            let env = Env::new("matrix");
            let store = MemoryStore::default();
            let provider = scripted(vec![
                uses(&[
                    call("a", "sleep", json!({"ms": 50, "text": "a"})),
                    call("b", "x", json!({})),
                ]),
                Step::from(reply_on(
                    DEFAULT_MODEL,
                    600,
                    &[B::Text("t"), call("c", "sleep", json!({"ms": 50}))],
                    stop.clone(),
                )),
                done("after"),
                done("again"),
                done("more"),
            ]);
            let mut agent = logged(provider.clone(), &env, &store);
            let (_, events) = turn_with(&mut agent, "one", trigger).await;
            check_events(&events);
            turn(&mut agent, "two").await;
            assert_layout(&provider);
            let logged_events = log_events(&store);
            let rebuilt = rebuild_history(logged_events.iter(), no_images)
                .unwrap()
                .messages;
            assert_eq!(rebuilt, &*agent.history());
            assert_eq!(
                tool_uses(&agent.history()),
                agent
                    .history()
                    .iter()
                    .map(|m| results(m).len())
                    .sum::<usize>()
            );
            RUNS.fetch_add(1, Ordering::Relaxed);
        }
    }
    assert_eq!(RUNS.load(Ordering::Relaxed), 6);
}

#[tokio::test(start_paused = true)]
async fn a_call_past_its_maximum_is_timed_out_and_its_result_stays_byte_stable() {
    for stubborn in [false, true] {
        let env = Env::new("timeout");
        let provider = scripted(vec![
            uses(&[
                call(
                    "t",
                    "sleep",
                    json!({"ms": 900_000, "text": "waiting\n", "stubborn": stubborn}),
                ),
                call("i", "sleep", json!({"ms": -1})),
            ]),
            reply(
                &[B::Text("cut"), call("n", "sleep", json!({"ms": 0}))],
                StopReason::StopSequence,
            ),
            done("next"),
            done("again"),
        ]);
        let mut agent = agent(provider.clone(), &env);
        let (outcome, events) = turn(&mut agent, "go").await;
        assert!(
            matches!(outcome, TurnOutcome::Completed { steps: 2, .. }),
            "{outcome:?}"
        );
        check_events(&events);
        assert_eq!(
            finished(&events)[0].1,
            ToolOutcome::TimedOut { after_ms: 600_000 }
        );
        let messages = agent.history();
        let (_, text, is_error) = results(&messages[2])[0];
        assert!(is_error);
        assert!(text.starts_with("Output:\nwaiting\n"), "{text}");
        assert!(
            text.contains("Timed out after 600 s; the call was stopped."),
            "{text}"
        );
        turn(&mut agent, "more").await;
        turn(&mut agent, "and more").await;
        let bodies = provider.texts();
        for pair in bodies.windows(2) {
            let (head_a, msgs_a) = head_and_messages(&pair[0]);
            let (head_b, msgs_b) = head_and_messages(&pair[1]);
            assert_eq!(head_a, head_b);
            let open = msgs_a.strip_suffix(']').unwrap();
            let tail_open = msgs_a.strip_suffix("]}]").unwrap();
            assert!(msgs_b.starts_with(open) || msgs_b.starts_with(tail_open));
        }
        assert_layout(&provider);
    }
}

type Ops = Arc<Mutex<Vec<&'static str>>>;
type ReadGate = Arc<Mutex<Option<std::sync::mpsc::Receiver<()>>>>;

#[derive(Clone)]
struct RecordingStore {
    inner: MemoryStore,
    ops: Ops,
    read_gate: Option<ReadGate>,
}

impl SessionStore for RecordingStore {
    fn append(
        &mut self,
        session: &str,
        record: &wisp_protocol::log::LogRecord,
    ) -> std::io::Result<()> {
        let op = match record.event {
            LogEvent::ToolCallStarted { .. } => "started",
            LogEvent::QuestionAsked { .. } => "question",
            LogEvent::MessageCommitted { .. } => "committed",
            _ => "append",
        };
        self.ops.lock().unwrap().push(op);
        self.inner.append(session, record)
    }

    fn sync(&mut self, session: &str) -> std::io::Result<()> {
        self.ops.lock().unwrap().push("sync");
        self.inner.sync(session)
    }

    fn list(&self) -> std::io::Result<Vec<String>> {
        self.inner.list()
    }

    fn read(&self, session: &str) -> std::io::Result<Vec<wisp_protocol::log::LogRecord>> {
        self.inner.read(session)
    }

    fn reader(&self, session: &str, before: u64) -> Option<crate::session::LogReader> {
        let gate = self.read_gate.as_ref()?.lock().unwrap().take()?;
        let store = self.inner.clone();
        let session = session.to_owned();
        Some(Box::new(move || {
            gate.recv()
                .map_err(|_| std::io::Error::other("test reader closed"))?;
            let mut records = store.read(&session)?;
            records.retain(|record| record.seq < before);
            Ok(records)
        }))
    }

    fn put_blob(
        &mut self,
        sha256: &str,
        media_type: &wisp_protocol::Mime,
        bytes: &[u8],
    ) -> std::io::Result<()> {
        self.ops.lock().unwrap().push("blob");
        self.inner.put_blob(sha256, media_type, bytes)
    }

    fn get_blob(
        &self,
        sha256: &str,
        media_type: &wisp_protocol::Mime,
    ) -> std::io::Result<crate::session::Blob> {
        self.inner.get_blob(sha256, media_type)
    }
}

struct ScriptTool {
    max: Duration,
    commands: mpsc::Sender<ClientCommand>,
    ops: Ops,
}

impl ScriptTool {
    fn toolbox(
        commands: mpsc::Sender<ClientCommand>,
        max: Duration,
        ops: Ops,
    ) -> crate::tool::Toolbox {
        crate::plugin::testing::toolbox(vec![Arc::new(Self { max, commands, ops })])
    }
}

impl crate::plugin::testing::TestTool for ScriptTool {
    fn name(&self) -> String {
        "script".into()
    }

    fn description(&self) -> String {
        "Run a list of test steps.".into()
    }

    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"steps": {"type": "array"}}})
    }

    fn meta(&self) -> wisp_plugin_api::tools::ToolMeta {
        let ms = u64::try_from(self.max.as_millis()).unwrap();
        crate::plugin::testing::meta(ms, Some(ms))
    }

    fn prepare(&self, input: &str) -> Result<Box<dyn wisp_plugin_api::tools::Prepared>, String> {
        let value: Value = serde_json::from_str(input).map_err(|e| e.to_string())?;
        let steps: Vec<String> = value["steps"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        Ok(crate::plugin::testing::Call::boxed(
            crate::plugin::testing::fallback("script", input),
            steps,
        ))
    }

    fn run<'a>(
        &'a self,
        call: Box<dyn wisp_plugin_api::tools::Prepared>,
        ctx: &'a dyn wisp_plugin_api::host::CallContext,
    ) -> futures::future::BoxFuture<'a, wisp_plugin_api::types::ToolRun> {
        use futures::FutureExt;
        async move {
            let steps = crate::plugin::testing::Call::<Vec<String>>::take(call).unwrap_or_default();
            for step in steps {
                if let Some(text) = step.strip_prefix("write:") {
                    ctx.write_output(text.as_bytes());
                } else if let Some(ms) = step.strip_prefix("sleep:") {
                    tokio::time::sleep(Duration::from_millis(ms.parse().unwrap())).await;
                } else if step == "cancel" {
                    self.commands.try_send(ClientCommand::Cancel).unwrap();
                } else if step == "say" {
                    static SAID: AtomicUsize = AtomicUsize::new(0);
                    let n = SAID.fetch_add(1, Ordering::Relaxed);
                    self.commands
                        .try_send(ClientCommand::send(format!("say-{n}"), "hello?"))
                        .unwrap();
                } else if let Some(text) = step.strip_prefix("steer:") {
                    self.commands
                        .try_send(ClientCommand::send(text, text))
                        .unwrap();
                } else if let Some(text) = step.strip_prefix("queue:") {
                    self.commands
                        .try_send(ClientCommand::queue(text, text))
                        .unwrap();
                } else if step == "cancel_on_stop" {
                    ctx.wait_cancelled().await;
                    self.commands.try_send(ClientCommand::Cancel).unwrap();
                    std::future::pending::<()>().await;
                } else if step == "check_synced" {
                    let ops = self.ops.lock().unwrap().clone();
                    let synced = ops.ends_with(&["started", "sync"]);
                    let text: &[u8] = if synced { b"synced\n" } else { b"unsynced\n" };
                    ctx.write_output(text);
                }
            }
            wisp_plugin_api::types::ToolRun::completed()
        }
        .boxed()
    }
}

fn steps(id: &str, steps: &[&str]) -> B {
    call(id, "script", json!({ "steps": steps }))
}

struct Rig {
    agent: Agent,
    provider: Arc<Fake>,
    commands: mpsc::Receiver<ClientCommand>,
    ops: Ops,
    _env: Env,
}

fn rig(scripts: Vec<Step>, max: Duration, store: Option<MemoryStore>) -> Rig {
    let env = Env::new("script");
    let provider = scripted(scripts);
    let (tx, rx) = mpsc::channel(8);
    let ops: Ops = Arc::default();
    let toolbox = ScriptTool::toolbox(tx, max, ops.clone());
    let mut agent = agent_with(provider.clone(), &env, toolbox, GRACE);
    if let Some(store) = store {
        let recording = RecordingStore {
            read_gate: None,
            inner: store,
            ops: ops.clone(),
        };
        let context = log_context();
        agent = agent.with_log(SessionLog::new(Box::new(recording), "s1"), context);
    }
    Rig {
        agent,
        provider,
        commands: rx,
        ops,
        _env: env,
    }
}

async fn run_rig(rig: &mut Rig, text: &str) -> (TurnOutcome, Vec<AgentEvent>) {
    let (ev_tx, mut ev_rx) = mpsc::channel(1024);
    let collector = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(e) = ev_rx.recv().await {
            seen.push(e);
        }
        seen
    });
    let outcome = rig
        .agent
        .run_turn(text.into(), &mut rig.commands, &ev_tx)
        .await
        .unwrap();
    drop(ev_tx);
    (outcome, collector.await.unwrap())
}

fn outputs(events: &[AgentEvent]) -> Vec<&str> {
    pick!(events, AgentEvent::ToolOutput { text, .. } => text.as_str())
}

#[tokio::test]
async fn a_cancel_queued_by_one_call_stops_the_next_from_starting() {
    let mut rig = rig(
        vec![
            uses(&[steps("a", &["cancel"]), steps("b", &["write:ran\n"])]),
            done("never"),
        ],
        Duration::from_secs(10),
        None,
    );
    let (outcome, events) = run_rig(&mut rig, "go").await;
    check_events(&events);
    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                committed_steps: 1,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        finished(&events),
        [
            (
                "a".to_owned(),
                ToolOutcome::Completed {
                    is_error: false,
                    exit: None
                }
            ),
            (
                "b".to_owned(),
                ToolOutcome::NotRun {
                    reason: NotRunReason::TurnStopped
                }
            ),
        ]
    );
    assert_eq!(events.iter().filter(|e| on_started(e)).count(), 1);
    assert_eq!(rig.provider.sent().len(), 1);
}

#[tokio::test]
async fn a_cancel_during_the_timeout_grace_stops_the_turn() {
    let mut rig = rig(
        vec![uses(&[steps("a", &["cancel_on_stop"])]), done("never")],
        Duration::from_millis(200),
        None,
    );
    let (outcome, events) = run_rig(&mut rig, "go").await;
    check_events(&events);
    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                committed_steps: 1,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        finished(&events)[0].1,
        ToolOutcome::TimedOut { after_ms: 200 }
    );
    assert_eq!(rig.provider.sent().len(), 1);
}

#[tokio::test]
async fn a_steer_sent_while_a_tool_runs_waits_for_it_and_lands_after_its_result() {
    let mut rig = rig(
        vec![
            uses(&[steps(
                "a",
                &["steer:also this", "sleep:200", "write:done\n"],
            )]),
            done("ok"),
        ],
        Duration::from_secs(10),
        None,
    );
    let (outcome, events) = run_rig(&mut rig, "go").await;
    check_events(&events);
    assert!(
        matches!(outcome, TurnOutcome::Completed { steps: 2, .. }),
        "{outcome:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Rejected { .. }))
    );
    assert_eq!(
        finished(&events),
        [(
            "a".to_owned(),
            ToolOutcome::Completed {
                is_error: false,
                exit: None
            }
        )]
    );
    let accepted = events
        .iter()
        .position(|e| matches!(e, AgentEvent::InputAccepted { opening: false, .. }))
        .expect("the steer was accepted");
    let finished_at = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolCallFinished { .. }))
        .unwrap();
    let delivered = events
        .iter()
        .position(|e| matches!(e, AgentEvent::InputDelivered { step: 2, ids } if ids == &[InputId("also this".into())]))
        .expect("the steer was delivered at step 2");
    assert!(
        accepted < finished_at && finished_at < delivered,
        "{events:#?}"
    );
    let requests = rig.provider.sent();
    let last = requests[1].messages.last().unwrap();
    assert_eq!(results(last).len(), 1);
    let steered = ["also this", STEER_NOTE].map(crate::test_guard::text);
    assert_eq!(last.content[last.content.len() - 2..], steered);
}

#[tokio::test(start_paused = true)]
async fn output_is_sent_while_a_tool_is_quiet() {
    let mut rig = rig(
        vec![
            uses(&[steps(
                "a",
                &[
                    "write:first\n",
                    "write:second\n",
                    "sleep:60000",
                    "write:third\n",
                ],
            )]),
            done("ok"),
        ],
        Duration::from_secs(600),
        None,
    );
    let (_, events) = run_rig(&mut rig, "go").await;
    assert_eq!(outputs(&events), ["first\n", "second\n", "third\n"]);
}

#[tokio::test]
async fn the_started_record_is_synced_before_the_tool_runs() {
    let mut rig = rig(
        vec![uses(&[steps("a", &["check_synced"])]), done("ok")],
        Duration::from_secs(10),
        Some(MemoryStore::default()),
    );
    let (_, events) = run_rig(&mut rig, "go").await;
    assert_eq!(outputs(&events), ["synced\n"]);
    assert!(rig.ops.lock().unwrap().contains(&"started"));
}

#[tokio::test]
async fn every_committed_message_is_synced_at_once() {
    let mut rig = rig(
        vec![
            uses(&[steps("a", &["write:one\n"]), steps("b", &["write:two\n"])]),
            done("ok"),
        ],
        Duration::from_secs(10),
        Some(MemoryStore::default()),
    );
    run_rig(&mut rig, "go").await;
    let ops = rig.ops.lock().unwrap().clone();
    let committed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| **op == "committed")
        .map(|(at, _)| at)
        .collect();
    assert_eq!(committed.len(), 3, "{ops:?}");
    for at in committed {
        assert_eq!(ops.get(at + 1), Some(&"sync"), "{ops:?}");
    }
}

#[tokio::test]
async fn a_quit_writes_its_records_without_waiting_on_the_disk() {
    for (sent, turns) in [(vec![ClientCommand::send("go", "go")], 1), (Vec::new(), 0)] {
        let env = Env::new("quit-sync");
        let ops: Ops = Arc::default();
        let recording = RecordingStore {
            read_gate: None,
            inner: MemoryStore::default(),
            ops: ops.clone(),
        };
        let provider = scripted(vec![hanging(&[B::Text("partial")])]);
        let mut agent = agent(provider, &env).with_log(
            SessionLog::new(Box::new(recording.clone()), "s1"),
            log_context(),
        );
        let (tx, mut rx) = mpsc::channel(16);
        for command in sent {
            tx.try_send(command).unwrap();
        }
        drop(tx);
        let (ev_tx, _ev_rx) = mpsc::channel(16);
        agent.serve(&mut rx, &ev_tx).await;
        let ops = ops.lock().unwrap().clone();
        assert!(!ops.contains(&"sync"), "{ops:?}");
        let records = recording.read("s1").unwrap();
        let ended = |r: &LogRecord| matches!(r.event, LogEvent::TurnEnded { .. });
        assert_eq!(records.iter().filter(|r| ended(r)).count(), turns);
        assert!(matches!(
            records.last().map(|r| &r.event),
            Some(LogEvent::SessionEnded { .. })
        ));
    }
}

#[tokio::test]
async fn an_unpriced_step_keeps_the_turn_a_lower_bound() {
    let env = Env::new("lower");
    let provider = scripted(vec![
        Step::from(reply_on(
            "mystery-model",
            600,
            &[call("a", "sleep", json!({"ms": 0}))],
            StopReason::ToolUse,
        )),
        done("priced"),
    ]);
    let mut agent = agent(provider, &env);
    let (outcome, events) = turn(&mut agent, "go").await;
    assert_eq!(costs(&events)[0], (None, true));
    assert!(costs(&events)[1].0.is_some() && !costs(&events)[1].1);
    let TurnOutcome::Completed {
        cost: Some(cost), ..
    } = outcome
    else {
        panic!("{outcome:?}")
    };
    assert!(cost.turn_lower_bound, "{cost:?}");
}

#[tokio::test]
async fn a_broken_layout_is_refused_before_anything_is_sent() {
    let env = Env::new("layout");
    let provider = scripted(vec![done("never")]);
    let mut agent = agent(provider.clone(), &env);
    agent.history = vec![
        Message::user_text("x"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "t".into(),
                name: "sleep".into(),
                input: json!({}),
            }],
        },
    ];
    let (outcome, events) = turn(&mut agent, "go").await;
    check_events(&events);
    assert!(
        matches!(
            outcome,
            TurnOutcome::Error {
                kind: ErrorKind::InvalidRequest,
                committed_steps: 0,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(provider.sent().is_empty());
}

#[tokio::test]
async fn an_empty_toolbox_sends_no_tools_and_no_environment() {
    let env = Env::new("empty");
    let provider = scripted(vec![done("hi")]);
    let mut agent = Agent::new(provider.clone(), default_session())
        .with_clock(TickingClock::new())
        .with_tools(crate::tool::Toolbox::default(), env.workspace())
        .with_platform("Linux")
        .with_resources(None);
    turn(&mut agent, "hello").await;
    let body = &provider.texts()[0];
    assert!(!body.contains(r#""tools""#), "{body}");
    assert!(!body.contains("environment"), "{body}");
    assert_eq!(agent.history()[0].content.len(), 3);
    assert!(agent.output_dir().is_none());
}

#[tokio::test]
async fn a_steer_sent_between_two_calls_waits_for_every_call_of_the_step() {
    let mut rig = rig(
        vec![
            uses(&[steps("a", &["say"]), steps("b", &["write:ran\n"])]),
            done("ok"),
        ],
        Duration::from_secs(10),
        None,
    );
    let (outcome, events) = run_rig(&mut rig, "go").await;
    check_events(&events);
    assert!(
        matches!(outcome, TurnOutcome::Completed { steps: 2, .. }),
        "{outcome:?}"
    );
    assert_eq!(outputs(&events), ["ran\n"]);
    let accepted = events
        .iter()
        .position(|e| matches!(e, AgentEvent::InputAccepted { opening: false, .. }))
        .expect("the steer was accepted");
    let second_started = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolCallStarted { call_id, .. } if call_id == "b"))
        .unwrap();
    assert!(accepted < second_started, "{events:#?}");
    let requests = rig.provider.sent();
    let last = requests[1].messages.last().unwrap();
    assert_eq!(
        results(last).iter().map(|r| r.0).collect::<Vec<_>>(),
        ["a", "b"]
    );
    let steered = ["hello?", STEER_NOTE].map(crate::test_guard::text);
    assert_eq!(last.content[last.content.len() - 2..], steered);
}

const QUIET: Duration = Duration::from_millis(400);
const PATIENT: Duration = Duration::from_secs(20);

mod attachments;
#[cfg(unix)]
mod bash_jobs;
mod commands;
mod context_note;
mod endpoint_saves;
mod image_turns;
mod instructions;
mod lanes;
mod list_accounts;
mod list_settings;
mod plugin_http;
mod plugin_model;
mod plugin_prices;
mod plugins;
mod purpose;
mod questions;
mod repair;
mod retry_choice;
mod settings_queue;
mod show_diff;
mod slots;
pub(in crate::agent) mod steering;
mod todos;
mod tool_hooks;

fn broke_mid_stream() -> ProviderError {
    let mut detail = ErrorDetail::new(
        FailurePhase::Stream,
        FailureSource::Endpoint,
        RetryClass::Transient,
    );
    detail.possibly_billed = true;
    detail.status = Some(200);
    detail.error_type = Some("api_error".into());
    ProviderError::classified(
        ErrorKind::Server,
        "Upstream provider stream was interrupted.",
        detail,
    )
}

fn turned_away() -> Step {
    let error = classified(
        ErrorKind::Overloaded,
        "Overloaded",
        FailurePhase::Response,
        RetryClass::Transient,
    );
    Step::from(vec![Ok(ProviderEvent::Sending), Err(error)])
}

#[tokio::test]
async fn a_failure_at_step_three_retries_only_that_step() {
    for sample in quick_samples() {
        let env = Env::new("retry3");
        let mut broken = vec![Ok(ProviderEvent::Sending), Ok(start_on(DEFAULT_MODEL, 900))];
        broken.extend(blocks(&[
            B::Think,
            call("never", &sample.name, sample.input.clone()),
        ]));
        broken.pop();
        broken.push(Err(broke_mid_stream()));
        let provider = scripted(vec![
            uses(&[call("a", &sample.name, sample.input.clone())]),
            uses(&[call("b", &sample.name, sample.input.clone())]),
            Step::from(broken),
            done("finished"),
        ]);
        let mut agent = agent(provider.clone(), &env).with_retry_policy(super::retry::quick());
        let (outcome, events) = turn(&mut agent, "go").await;
        assert!(
            matches!(
                outcome,
                TurnOutcome::Completed {
                    committed: true,
                    steps: 3,
                    tool_calls: 2,
                    ..
                }
            ),
            "{outcome:?}"
        );
        check_events(&events);
        assert_ran(&sample, &events, &provider, 2);
        assert!(
            !events.iter().any(|e| matches!(
                e,
                AgentEvent::ToolCallRequested { call_id, .. } if call_id == "never"
            )),
            "nothing from the discarded reply runs"
        );
        let retry: Vec<&AgentEvent> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Retrying { .. }))
            .collect();
        assert!(
            matches!(
                retry.as_slice(),
                [AgentEvent::Retrying {
                    step: 3,
                    discarded_blocks: 2,
                    ..
                }]
            ),
            "{retry:?}"
        );
        let requests = provider.sent();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[2].messages, requests[3].messages);
        let bodies = provider.texts();
        assert_eq!(bodies[2], bodies[3], "the retried step re-sends its bytes");
        let history = agent.history();
        assert_eq!(history.len(), 6);
        assert_eq!(history[..5], requests[2].messages[..]);
        assert!(!serde_json::to_string(&history).unwrap().contains("never"));
    }
}

#[tokio::test]
async fn eleven_failures_across_a_turn_stop_at_the_turn_limit() {
    for sample in quick_samples() {
        let env = Env::new("turnlimit");
        let mut scripts = Vec::new();
        for id in ["a", "b"] {
            scripts.extend((0..5).map(|_| turned_away()));
            scripts.push(uses(&[call(id, &sample.name, sample.input.clone())]));
        }
        scripts.push(turned_away());
        let provider = scripted(scripts);
        let mut agent = agent(provider.clone(), &env).with_retry_policy(super::retry::quick());
        let (outcome, events) = turn(&mut agent, "go").await;
        assert!(
            matches!(
                outcome,
                TurnOutcome::Error {
                    kind: ErrorKind::Overloaded,
                    committed_steps: 2,
                    attempts: 1,
                    gave_up: Some(GaveUp::TurnLimit),
                    ..
                }
            ),
            "{outcome:?}"
        );
        check_events(&events);
        assert_ran(&sample, &events, &provider, 2);
        assert_eq!(provider.sent().len(), 13);
        assert_eq!(agent.last_turn_stats().unwrap().retries, 10);
    }
}

#[tokio::test]
async fn a_started_call_names_its_file_and_a_streamed_call_its_writing() {
    let env = Env::new("started-file");
    let provider = scripted(vec![
        uses(&[
            call("a", "sleep", json!({"ms": 0, "text": "x"})),
            call("b", "nope", json!({"input": "marker-31d"})),
        ]),
        done("ok"),
    ]);
    let store = MemoryStore::default();
    let mut agent = logged(provider, &env, &store);
    let (_, events) = turn(&mut agent, "go").await;
    let writing: Vec<(String, Option<String>)> = pick!(
        &events,
        AgentEvent::BlockStarted {
            kind: BlockKind::ToolUse { name, writing },
            ..
        } => (name.clone(), writing.clone())
    );
    assert_eq!(
        writing,
        [
            ("sleep".to_owned(), Some("a nap".to_owned())),
            ("nope".to_owned(), None)
        ]
    );
    let started: Vec<Option<String>> = pick!(
        &events,
        AgentEvent::ToolCallStarted { file, .. } => file.clone()
    );
    let finished = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolCallFinished {
                call_id, output, ..
            } if call_id == "a" => Some(output.file.clone()),
            _ => None,
        })
        .unwrap();
    assert!(finished.is_some());
    assert_eq!(started, std::slice::from_ref(&finished));
    let logged: Vec<Option<String>> = pick!(
        &log_events(&store),
        LogEvent::ToolCallStarted { file, .. } => file.clone()
    );
    assert_eq!(logged, [finished]);
    let refused = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolCallRequested {
                call_id, display, ..
            } if call_id == "b" => Some(display.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(refused.target, "{}");
}

#[test]
fn typed_and_file_survive_the_log_and_older_records_read_as_none() {
    let mut finished = AgentEvent::call_finished("a", ToolOutcome::Cancelled);
    if let AgentEvent::ToolCallFinished { typed, .. } = &mut finished {
        *typed = Some(Typed {
            echoed: true,
            unsent: false,
            signalled: true,
        });
    }
    let record = log_record(&finished, Some(4)).unwrap();
    let LogEvent::ToolCallFinished { typed, .. } = &record else {
        panic!("{record:?}")
    };
    assert_eq!(
        typed.map(|t| (t.echoed, t.unsent, t.signalled)),
        Some((true, false, true))
    );
    let back = crate::resume::replayed(&record).unwrap();
    assert_eq!(back, (4, finished));
    let mut started = AgentEvent::call_started("a", "~/p");
    if let AgentEvent::ToolCallStarted { file, .. } = &mut started {
        *file = Some("/tmp/out/0001-bash.out".into());
    }
    let record = log_record(&started, Some(4)).unwrap();
    assert_eq!(crate::resume::replayed(&record).unwrap(), (4, started));
    let old_finished: LogEvent = serde_json::from_value(json!({
        "type": "tool_call_finished", "turn": 2, "call_id": "a",
        "outcome": {"status": "cancelled"}, "output": {"tail": "", "bytes": 0, "lines": 0,
        "file": null, "file_problem": null, "inlined": true}, "facts": [], "state": null,
        "duration_ms": 1
    }))
    .unwrap();
    let Some((_, AgentEvent::ToolCallFinished { typed, .. })) =
        crate::resume::replayed(&old_finished)
    else {
        panic!("{old_finished:?}")
    };
    assert_eq!(typed, None);
    let old_started: LogEvent = serde_json::from_value(
        json!({"type": "tool_call_started", "turn": 2, "call_id": "a", "cwd": "~/p"}),
    )
    .unwrap();
    let Some((_, AgentEvent::ToolCallStarted { file, .. })) = crate::resume::replayed(&old_started)
    else {
        panic!("{old_started:?}")
    };
    assert_eq!(file, None);
}

mod plan_usage;

pub(super) fn rewind(request: u64, turn: u64) -> ClientCommand {
    ClientCommand::Rewind {
        to: RewindPoint::Turn { turn },
        request: RequestId(request),
    }
}

fn summary(name: &str, default: bool) -> EndpointSummary {
    EndpointSummary {
        models: Vec::new(),
        mark: None,
        name: name.into(),
        provider: "anthropic".into(),
        credential: None,
        official: Some(name == "direct"),
        host: Some(format!("{name}.example.com")),
        unavailable: None,
        priced: true,
        default,
        keys_file: false,
        expired: false,
        key_var: None,
        voice_model: None,
        complete: false,
        default_model: None,
    }
}

fn wakes(events: &[AgentEvent]) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::TurnStarted {
                    origin: Some(TurnOrigin::ProcessExit),
                    ..
                }
            )
        })
        .count()
}

pub(super) fn terminal(events: &[AgentEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .count()
}

pub(super) fn send(id: &str, text: &str) -> ClientCommand {
    ClientCommand::Send {
        id: InputId(id.into()),
        text: text.into(),
        images: Vec::new(),
        mode: DeliveryMode::Steer,
        turn: None,
        replaces: None,
    }
}

fn texts(message: &Message) -> Vec<&str> {
    pick!(&message.content, ContentBlock::Text { text } => text.as_str())
}

const NEON: &str = "gemini-4-neon-high";

fn neon_form() -> &'static crate::models::TurnForm {
    crate::models::ModelTable::shared().turn_form(NEON).unwrap()
}

fn is_data(text: &str) -> bool {
    let form = neon_form();
    let Some(inner) = text
        .strip_prefix(form.data_open.as_str())
        .and_then(|t| t.strip_suffix(form.data_close.as_str()))
    else {
        return false;
    };
    let lines: Vec<&str> = inner.split_inclusive('\n').collect();
    let [time, fill] = lines[..] else {
        return false;
    };
    let stamp = time
        .strip_prefix("The current local time is: ")
        .and_then(|t| t.strip_suffix(".\n"));
    stamp.is_some_and(|s| s.parse::<jiff::Timestamp>().is_ok())
        && fill.starts_with("Context window: ")
        && fill.ends_with(".\n")
}

fn bare_lines(text: &str) -> bool {
    text.starts_with("Sent: ") || text.starts_with("Context: ")
}

pub(super) fn rejected(events: &[AgentEvent]) -> Vec<String> {
    pick!(events, AgentEvent::Rejected { reason, .. } => reason.clone())
}

fn started_ids(events: &[AgentEvent]) -> Vec<String> {
    pick!(events, AgentEvent::ToolCallStarted { call_id, .. } => call_id.clone())
}

fn rejected_kinds(events: &[AgentEvent]) -> Vec<(Option<InputId>, Option<RejectKind>)> {
    pick!(events, AgentEvent::Rejected { id, kind, .. } => (id.clone(), kind.clone()))
}

fn delivered(events: &[AgentEvent]) -> Vec<(Vec<InputId>, u32)> {
    pick!(events, AgentEvent::InputDelivered { ids, step } => (ids.clone(), *step))
}

fn reasons(events: &[AgentEvent]) -> Vec<&str> {
    pick!(events, AgentEvent::Rejected { reason, .. } => reason.as_str())
}

pub(super) fn flag(name: &str, value: &str) -> crate::settings::Given {
    crate::settings::Given {
        target: wisp_protocol::settings::Target::Name(wisp_protocol::settings::find(name).unwrap()),
        value: SettingValue::Text(value.into()),
        origin: format!("--{name}"),
    }
}

pub(super) fn retry_turn(turn: u64, endpoint: Option<&str>, request: u64) -> ClientCommand {
    ClientCommand::Retry {
        turn,
        endpoint: endpoint.map(str::to_owned),
        request: RequestId(request),
    }
}

fn before(records: &[LogRecord], pick: impl Fn(&LogEvent) -> bool) -> Vec<LogRecord> {
    let end = records.iter().position(|r| pick(&r.event)).unwrap();
    records[..end].to_vec()
}

pub(super) fn still_clock() -> ShiftedClock {
    ShiftedClock::at("2026-10-22T03:58:00Z".parse().unwrap(), Some(TimeZone::UTC))
}

pub(super) fn rebuilt(reason: RebuildReason) -> CacheEffect {
    CacheEffect::Rebuilt { reason }
}

pub(super) fn on_text(e: &AgentEvent) -> bool {
    matches!(e, AgentEvent::TextDelta { .. })
}

fn job_tools() -> Toolbox {
    crate::tool::testkit::shell(crate::tool::testkit::timing(300, 3_000))
}

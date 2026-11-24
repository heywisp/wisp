use std::collections::BTreeSet;
use std::fs;

use serde_json::{Value, json};
use wisp_protocol::log::{Closed, LogRecord, no_images, rebuild_history};
use wisp_protocol::{Answer, FailurePhase, RetryClass, SearchAction, SearchEnd, SearchOutcome};

use super::super::*;
use super::{open_calls, paused_tail};
use crate::agent::testing::{
    self, B, Caps, Fake, Knobs, Script, Step, adaptations, block_of, classified, done, events_of,
    failing, failing_after, failure, fallback_marker, found, script, step,
};
use crate::agent::tests::failing as refused;
use crate::agent::tests::{search_off, usage};
use crate::clock::TickingClock;
use crate::session::{MemoryStore, SessionStore};
use crate::test_guard::search_use;
use crate::test_guard::{ScratchDir, continued, log_context};
use crate::test_guard::{log_events, opaque, pick, records};
use crate::tool::{ToolEntry, samples};

fn start(input: u64) -> ProviderEvent {
    crate::agent::tests::start(input, 0)
}

fn paused(content: &[B]) -> Step {
    step(content, StopReason::PauseTurn)
}

fn uses(content: &[B]) -> Step {
    step(content, StopReason::ToolUse)
}

fn hanging(content: &[B]) -> Step {
    let mut script = vec![Ok(ProviderEvent::Sending), Ok(start(600))];
    script.extend(events_of(content));
    Step::from(script).hanging()
}

fn mid_stream(kind: ErrorKind, retry: RetryClass) -> ProviderError {
    failure(kind, retry, true)
}

fn echo(id: &'static str) -> B {
    let input = samples()
        .into_iter()
        .find(|s| s.tool.spec().name == "echo")
        .unwrap()
        .input;
    B::Call(id, "echo", input)
}

fn env(name: &str) -> ScratchDir {
    ScratchDir::new("wisp-search-test-", name)
}

struct Rig {
    agent: Agent,
    provider: Arc<Fake>,
    rx: mpsc::Receiver<ClientCommand>,
    store: MemoryStore,
    _env: ScratchDir,
}

fn rig_with(steps: Vec<Step>, ask: bool, retry: RetryPolicy) -> Rig {
    let env = env("search");
    let (tx, rx) = mpsc::channel(64);
    let provider = searching(steps, tx);
    let mut tools: Vec<ToolEntry> = samples().into_iter().map(|s| s.tool).collect();
    if ask {
        tools.push(crate::tool::confirm_tool());
    }
    let store = MemoryStore::default();
    let agent = Agent::new(provider.clone(), default_session())
        .with_clock(TickingClock::new())
        .with_tools(Toolbox::new(tools), env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300))
        .with_user_input(ask)
        .with_retry_policy(retry)
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            implicit_context(),
        );
    Rig {
        agent,
        provider,
        rx,
        store,
        _env: env,
    }
}

fn rig(steps: Vec<Step>) -> Rig {
    rig_with(steps, false, retry::quick())
}

impl Rig {
    async fn turn(&mut self, text: &str) -> (TurnOutcome, Vec<AgentEvent>) {
        let (ev_tx, mut ev_rx) = mpsc::channel(4096);
        let outcome = self
            .agent
            .run_turn(text.into(), &mut self.rx, &ev_tx)
            .await
            .unwrap();
        drop(ev_tx);
        let mut events = Vec::new();
        while let Some(e) = ev_rx.recv().await {
            events.push(e);
        }
        (outcome, events)
    }

    async fn end(&mut self) -> Vec<AgentEvent> {
        let (ev_tx, mut ev_rx) = mpsc::channel(4096);
        self.agent.end_log(&ev_tx).await;
        drop(ev_tx);
        let mut events = Vec::new();
        while let Some(e) = ev_rx.recv().await {
            events.push(e);
        }
        events
    }

    fn kinds(&self) -> Vec<String> {
        log_events(&self.store)
            .iter()
            .map(|e| {
                serde_json::to_value(e).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }

    fn messages(&self, request: usize) -> Vec<Value> {
        self.provider.bodies()[request]["messages"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn sent(&self) -> usize {
        self.provider.bodies().len()
    }
}

fn ended(events: &[AgentEvent]) -> Vec<(String, SearchEnd)> {
    pick!(
        events,
        AgentEvent::SearchEnded {
            call_id, outcome, ..
        } => (call_id.clone(), outcome.clone())
    )
}

fn unfinished(reason: UnfinishedReason, possibly_billed: bool) -> SearchEnd {
    SearchEnd::Unfinished {
        reason,
        possibly_billed,
    }
}

fn position(kinds: &[String], kind: &str) -> Vec<usize> {
    kinds
        .iter()
        .enumerate()
        .filter(|(_, k)| *k == kind)
        .map(|(i, _)| i)
        .collect()
}

fn closed_text(query: &str) -> Value {
    json!({"type": "text", "text": format!("[web search for \"{query}\": the turn ended before its result arrived]")})
}

fn rebuilt_rendering(rig: &Rig) -> (Vec<Message>, Rendering) {
    let events = log_events(&rig.store);
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap();
    let len = rebuilt.messages.len();
    let rendering = Rendering {
        closed: rebuilt.closed.calls.clone(),
        cuts: rebuilt.cuts.clone(),
        native_as_text: vec![false; len],
        paused: (0..len).map(|i| rebuilt.paused.contains(&i)).collect(),
        drop_base_marker: false,
        form: wisp_protocol::RenderingForm::Text,
        provisional: false,
        ..Rendering::default()
    };
    (rebuilt.messages, rendering)
}

fn body_of(rig: &Rig, messages: &[Message], rendering: &Rendering) -> Value {
    let request = Request {
        session: rig.agent.session(),
        tools: rig.agent.toolbox().specs(),
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering,
        compaction: None,
    };
    testing::body_of(&request)
}

fn resumed_from(rig: &Rig) -> Agent {
    let records = records(&rig.store);
    let (_, log) = continued(&records);
    let tools: Vec<crate::tool::ToolEntry> = samples().into_iter().map(|s| s.tool).collect();
    Agent::new(rig.provider.clone(), default_session())
        .with_clock(TickingClock::new())
        .with_tools(Toolbox::new(tools), rig._env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_log(log, implicit_context())
        .resume(&records, 0)
        .unwrap()
}

fn assert_resumes_byte_for_byte(rig: &Rig) {
    if rig.agent.compaction.head.is_none() {
        let (messages, rendering) = rebuilt_rendering(rig);
        assert_eq!(messages, &*rig.agent.history());
        assert_eq!(rendering.closed, rig.agent.search.closed.calls);
        assert_eq!(
            rendering.paused,
            rig.agent.paused_flags(rig.agent.history().len())
        );
        let live = rig.agent.rendering(rig.agent.history().len());
        assert_eq!(rendering.cuts, live.cuts);
        assert_eq!(
            body_of(rig, &messages, &rendering),
            body_of(rig, &rig.agent.history(), &live)
        );
        return;
    }
    let live_view = rig.agent.view_messages();
    let live = rig.agent.rendering(live_view.len());
    let resumed = resumed_from(rig);
    let view = resumed.view_messages();
    assert_eq!(view, live_view);
    let rendering = resumed.rendering(view.len());
    assert_eq!(rendering.closed, live.closed);
    assert_eq!(rendering.paused, live.paused);
    assert_eq!(rendering.cuts, live.cuts);
    assert_eq!(
        body_of(rig, &view, &rendering),
        body_of(rig, &live_view, &live)
    );
}

#[tokio::test]
async fn a_paused_reply_is_continued_with_nothing_after_it() {
    let mut rig = rig(vec![
        paused(&[B::Think("plan"), B::Search("s1", "rust ripgrep")]),
        done(&[
            B::Found("s1"),
            B::Cited("ripgrep is fast", "https://example.com/s1"),
        ]),
    ]);
    let (outcome, events) = rig.turn("search").await;
    let TurnOutcome::Completed {
        stop_reason, steps, ..
    } = &outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(*stop_reason, StopReason::EndTurn);
    assert_eq!(*steps, 2);
    assert_eq!(rig.sent(), 2);
    let second = rig.messages(1);
    let last = second.last().unwrap();
    assert_eq!(last["role"], "assistant");
    assert_eq!(last["content"][1], search_use("s1", "rust ripgrep"));
    assert_eq!(rig.messages(0)[..], second[..second.len() - 1]);
    let history = rig.agent.history();
    assert_eq!(history.len(), 3);
    assert_eq!(history[1].role, Role::Assistant);
    assert_eq!(history[2].role, Role::Assistant);
    assert!(rig.agent.search.paused.contains(&1));
    check_layout(
        &history[..2],
        &[false, true],
        &Default::default(),
        rig.agent.provider().as_ref(),
    )
    .unwrap();

    let started: Vec<(u32, u32, &str, &str)> = pick!(
        &events,
        AgentEvent::SearchStarted {
            step,
            attempt,
            call_id,
            query,
            ..
        } => (*step, *attempt, call_id.as_str(), query.as_str())
    );
    assert_eq!(started, vec![(1, 0, "s1", "rust ripgrep")]);
    let outcomes = ended(&events);
    assert_eq!(outcomes.len(), 1);
    assert!(matches!(
        &outcomes[0].1,
        SearchEnd::Done(SearchOutcome::Results { hits }) if hits.len() == 1
    ));
    let cited: Vec<(u32, usize)> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Citations {
                step,
                index,
                sources,
                ..
            } => {
                assert_eq!(sources[0].url, "https://example.com/s1");
                Some((*step, *index))
            }
            _ => None,
        })
        .collect();
    assert_eq!(cited, vec![(2, 1)]);

    let kinds = rig.kinds();
    assert!(kinds.contains(&"search_started".to_owned()));
    assert!(kinds.contains(&"search_ended".to_owned()));
    assert!(kinds.contains(&"citations".to_owned()));
    assert!(!kinds.contains(&"tail_closed".to_owned()));
    let paused_records = records(&rig.store)
        .iter()
        .filter(|r| {
            matches!(
                r.event,
                LogEvent::MessageCommitted {
                    stop_reason: Some(StopReason::PauseTurn),
                    ..
                }
            )
        })
        .count();
    assert_eq!(paused_records, 1);
    assert_resumes_byte_for_byte(&rig);
}

fn pause_chain() -> Vec<Step> {
    vec![
        paused(&[B::Search("s1", "q1")]),
        paused(&[B::Found("s1"), B::Search("s2", "q2")]),
        paused(&[B::Found("s2"), B::Search("s3", "q3")]),
        paused(&[B::Found("s3"), B::Search("s4", "q4")]),
        paused(&[B::Found("s4"), B::Search("s5", "q5")]),
        paused(&[B::Found("s5"), B::Search("s6", "q6"), B::Think("more")]),
    ]
}

#[tokio::test]
async fn six_pauses_continue_five_times_and_the_cap_closes_the_last() {
    let mut steps = pause_chain();
    steps.push(done(&[B::Text("next answer")]));
    steps.push(done(&[B::Text("third answer")]));
    let mut rig = rig(steps);
    let (outcome, events) = rig.turn("search a lot").await;
    assert!(
        matches!(
            &outcome,
            TurnOutcome::Completed {
                stop_reason: StopReason::PauseTurn,
                steps: 6,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(rig.sent(), 6);
    assert_eq!(
        ended(&events).last().unwrap(),
        &("s6".to_owned(), unfinished(UnfinishedReason::Paused, false))
    );
    assert_eq!(
        ended(&events)
            .iter()
            .filter(|(_, e)| matches!(e, SearchEnd::Unfinished { .. }))
            .count(),
        1
    );
    let kinds = rig.kinds();
    let closed = position(&kinds, "tail_closed");
    assert_eq!(closed.len(), 1);
    assert!(closed[0] < *position(&kinds, "turn_ended").last().unwrap());
    let LogEvent::TailClosed {
        through,
        calls,
        reason,
    } = &records(&rig.store)[closed[0]].event
    else {
        panic!();
    };
    assert_eq!(*through, 6);
    assert_eq!(calls, &vec!["s6".to_owned()]);
    assert_eq!(*reason, UnfinishedReason::Paused);

    rig.turn("next").await;
    let seventh = rig.messages(6);
    let reply = &seventh[6];
    assert_eq!(reply["role"], "assistant");
    assert_eq!(
        reply["content"],
        json!([found("s5"), closed_text("q6")]),
        "the thinking after the stopped call is cut"
    );
    assert_eq!(seventh[7]["role"], "user");
    let kinds = rig.kinds();
    let cut = position(&kinds, "thinking_cut");
    assert_eq!(
        cut,
        vec![closed[0] + 1],
        "the cut is written with the closure"
    );

    rig.turn("again").await;
    let eighth = rig.messages(7);
    assert_eq!(eighth[..seventh.len()], seventh[..]);
    assert_resumes_byte_for_byte(&rig);
    let mut steps = pause_chain();
    steps.push(done(&[B::Found("s6"), B::Text("researched answer")]));
    let mut unlimited = self::rig(steps);
    unlimited.provider.knobs(|k| k.pause_limit = Some(u32::MAX));
    let (outcome, events) = unlimited.turn("search a lot").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Completed {
                stop_reason: StopReason::EndTurn,
                steps: 7,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(unlimited.sent(), 7);
    assert!(
        ended(&events)
            .iter()
            .all(|(_, end)| matches!(end, SearchEnd::Done(_)))
    );
    assert!(position(&unlimited.kinds(), "tail_closed").is_empty());
    assert_resumes_byte_for_byte(&unlimited);
}

#[tokio::test]
async fn a_steer_waits_while_a_pause_is_live() {
    let mut rig = rig(vec![
        paused(&[B::Search("s1", "q1")]).then(ClientCommand::send("steer-1", "also this")),
        done(&[B::Found("s1"), B::Text("found it")]),
        done(&[B::Text("and the steer")]),
    ]);
    let (outcome, _) = rig.turn("go").await;
    assert!(matches!(outcome, TurnOutcome::Completed { steps: 3, .. }));
    let second = rig.messages(1);
    assert_eq!(second.last().unwrap()["role"], "assistant");
    assert!(
        !serde_json::to_string(&second)
            .unwrap()
            .contains("also this")
    );
    let third = rig.messages(2);
    assert_eq!(third.last().unwrap()["role"], "user");
    assert!(third.last().unwrap().to_string().contains("also this"));
}

#[tokio::test]
async fn a_steer_waits_at_a_pause_whose_search_already_finished() {
    let mut rig = rig(vec![
        paused(&[B::Search("s1", "q1"), B::Found("s1"), B::Text("half")])
            .then(ClientCommand::send("steer-1", "also this")),
        done(&[B::Text("whole")]),
        done(&[B::Text("and the steer")]),
    ]);
    rig.turn("go").await;
    assert!(
        !serde_json::to_string(&rig.messages(1))
            .unwrap()
            .contains("also this")
    );
    assert_eq!(rig.messages(1).last().unwrap()["role"], "assistant");
    assert!(
        serde_json::to_string(&rig.messages(2))
            .unwrap()
            .contains("also this")
    );
}

#[tokio::test]
async fn a_steer_waits_at_a_mixed_boundary_until_the_search_finishes() {
    let mut rig = rig(vec![
        uses(&[B::Search("s1", "q1"), echo("e1")])
            .then(ClientCommand::send("steer-1", "also this")),
        done(&[B::Found("s1"), B::Text("found it")]),
        done(&[B::Text("and the steer")]),
    ]);
    let (outcome, _) = rig.turn("go").await;
    assert!(matches!(outcome, TurnOutcome::Completed { steps: 3, .. }));
    let second = rig.messages(1);
    let results = second.last().unwrap();
    assert_eq!(results["role"], "user");
    assert!(
        results["content"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["type"] == "tool_result")
    );
    assert!(
        rig.messages(2)
            .last()
            .unwrap()
            .to_string()
            .contains("also this")
    );
}

#[tokio::test]
async fn a_stop_during_bash_closes_the_search_and_later_turns_render_it() {
    let mut rig = rig(vec![
        uses(&[
            B::Think("before"),
            B::Search("s1", "rust"),
            B::Think("after"),
            echo("e1"),
        ])
        .then(ClientCommand::Cancel),
        done(&[B::Think("fresh"), B::Text("ok")]),
        done(&[B::Text("ok again")]),
    ]);
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        ended(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Stopped, false))]
    );
    let kinds = rig.kinds();
    let closed = position(&kinds, "tail_closed")[0];
    assert!(closed < *position(&kinds, "turn_ended").last().unwrap());
    assert_eq!(position(&kinds, "thinking_cut"), vec![closed + 1]);
    assert_resumes_byte_for_byte(&rig);

    rig.turn("next").await;
    let second = rig.messages(1);
    let reply = &second[1];
    assert_eq!(
        reply["content"],
        json!([
            {"type": "thinking", "thinking": "before", "signature": "sig-before"},
            closed_text("rust"),
            {"type": "tool_use", "id": "e1", "name": "echo", "input": {"text": "hello from echo\n"}}
        ])
    );
    let user = &second[2]["content"];
    assert_eq!(user[0]["type"], "tool_result");
    assert!(user.to_string().contains("next"));
    let kinds = rig.kinds();
    let cut = position(&kinds, "thinking_cut");
    assert_eq!(cut.len(), 1);
    let LogEvent::ThinkingCut { from, through, .. } = &records(&rig.store)[cut[0]].event else {
        panic!();
    };
    assert_eq!(
        *from,
        wisp_protocol::log::BlockPosition {
            message: 1,
            block: 1
        }
    );
    assert_eq!(*through, 2);
    assert!(cut[0] < position(&kinds, "turn_started")[1]);

    rig.turn("again").await;
    let third = rig.messages(2);
    assert_eq!(third[..second.len()], second[..]);
    assert_eq!(
        third[3]["content"][0],
        json!({"type": "thinking", "thinking": "fresh", "signature": "sig-fresh"}),
        "thinking after the cut is kept"
    );
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn a_stop_while_the_results_request_is_in_flight_may_have_billed_the_search() {
    let mut rig = rig(vec![
        uses(&[B::Search("s1", "rust"), echo("e1")]),
        hanging(&[]).then(ClientCommand::Cancel),
        done(&[B::Text("ok")]),
        done(&[B::Text("ok again")]),
    ]);
    let (outcome, events) = rig.turn("go").await;
    let TurnOutcome::Cancelled { cost, .. } = &outcome else {
        panic!("{outcome:?}");
    };
    assert!(cost.as_ref().is_some_and(|c| c.turn_lower_bound));
    assert_eq!(
        ended(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Stopped, true))]
    );
    assert_eq!(rig.messages(1)[1]["content"][0], search_use("s1", "rust"));
    rig.turn("next").await;
    let third = rig.messages(2);
    assert_eq!(third[1]["content"][0], closed_text("rust"));
    assert_eq!(third[0], rig.messages(1)[0]);
    rig.turn("again").await;
    assert_eq!(rig.messages(3)[..third.len()], third[..]);
}

#[tokio::test]
async fn a_steer_waiting_at_the_cap_gets_another_step_after_the_closure() {
    let mut steps = pause_chain();
    let last = steps.pop().unwrap();
    steps.push(last.then(ClientCommand::send("steer-1", "stop searching")));
    steps.push(done(&[B::Text("stopped")]));
    let mut rig = rig(steps);
    let (outcome, events) = rig.turn("search a lot").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Completed {
                stop_reason: StopReason::EndTurn,
                steps: 7,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        ended(&events).last().unwrap(),
        &("s6".to_owned(), unfinished(UnfinishedReason::Paused, false))
    );
    let seventh = rig.messages(6);
    assert_eq!(
        seventh[6]["content"],
        json!([found("s5"), closed_text("q6")])
    );
    assert!(
        serde_json::to_string(&seventh[7])
            .unwrap()
            .contains("stop searching")
    );
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn a_stop_during_a_pause_continuation_renders_like_the_cap() {
    let mut rig = rig(vec![
        paused(&[B::Search("s1", "q1")]),
        hanging(&[]).then(ClientCommand::Cancel),
        done(&[B::Text("ok")]),
    ]);
    let (outcome, events) = rig.turn("go").await;
    assert!(matches!(outcome, TurnOutcome::Cancelled { .. }));
    assert_eq!(
        ended(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Stopped, true))]
    );
    rig.turn("next").await;
    let third = rig.messages(2);
    assert_eq!(third[1]["content"], json!([closed_text("q1")]));
    assert_eq!(third[2]["role"], "user");
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn an_error_end_leaves_the_search_open_until_the_next_turn() {
    let mut rig = rig(vec![
        uses(&[B::Search("s1", "rust"), echo("e1")]),
        failing(
            &[],
            mid_stream(ErrorKind::InvalidResponse, RetryClass::Never),
        ),
        done(&[B::Text("ok")]),
    ]);
    let (outcome, events) = rig.turn("go").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    assert!(ended(&events).is_empty());
    assert!(!rig.kinds().contains(&"tail_closed".to_owned()));
    assert!(rig.agent.tail_live());

    let (_, events) = rig.turn("next").await;
    assert_eq!(
        ended(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Failed, true))]
    );
    let kinds = rig.kinds();
    let closed = position(&kinds, "tail_closed");
    assert_eq!(closed.len(), 1);
    let ended_at = position(&kinds, "search_ended");
    assert_eq!(ended_at.len(), 1);
    let second_turn = position(&kinds, "turn_started")[1];
    assert!(ended_at[0] < closed[0] && closed[0] < second_turn);
    let LogEvent::SearchEnded { turn, .. } = &records(&rig.store)[ended_at[0]].event else {
        panic!();
    };
    assert_eq!(*turn, None);
    let third = rig.messages(2);
    assert_eq!(third[1]["content"][0], closed_text("rust"));
    assert!(third[2].to_string().contains("next"));
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn an_error_end_then_the_sessions_end_closes_the_search_once() {
    let mut rig = rig(vec![
        uses(&[B::Search("s1", "rust"), echo("e1")]),
        failing(
            &[],
            mid_stream(ErrorKind::InvalidResponse, RetryClass::Never),
        ),
    ]);
    rig.turn("go").await;
    let events = rig.end().await;
    assert_eq!(
        ended(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Failed, true))]
    );
    let kinds = rig.kinds();
    assert_eq!(position(&kinds, "tail_closed").len(), 1);
    assert!(position(&kinds, "tail_closed")[0] < position(&kinds, "session_ended")[0]);
    assert!(!rig.agent.tail_live());
}

fn compacted_step() -> Step {
    let mut compacted = step(&[], StopReason::Compaction);
    compacted.script.splice(
        2..2,
        testing::opaque(0, json!({"type": "compaction", "content": "So far."})),
    );
    compacted
}

async fn compact(rig: &mut Rig) -> Vec<AgentEvent> {
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    rig.agent
        .manual_compaction(None, wisp_protocol::RequestId(1), &mut rig.rx, &ev_tx)
        .await;
    drop(ev_tx);
    let mut events = Vec::new();
    while let Some(e) = ev_rx.recv().await {
        events.push(e);
    }
    events
}

#[tokio::test]
async fn a_search_after_a_compaction_resumes_with_the_same_view_byte_for_byte() {
    let mut rig = rig(vec![
        done(&[
            B::Search("s1", "rust"),
            B::Found("s1"),
            B::Cited("rust is fast", "https://example.com/s1"),
        ]),
        compacted_step(),
        paused(&[B::Think("plan"), B::Search("s2", "ripgrep")]),
        done(&[
            B::Found("s2"),
            B::Cited("ripgrep is fast", "https://example.com/s2"),
        ]),
    ]);
    assert!(matches!(
        rig.turn("first").await.0,
        TurnOutcome::Completed { .. }
    ));
    let events = compact(&mut rig).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. })),
        "{events:#?}"
    );
    assert!(matches!(
        rig.turn("second").await.0,
        TurnOutcome::Completed { .. }
    ));
    assert!(rig.agent.compaction.head.is_some());
    assert!(
        rig.agent
            .paused_flags(rig.agent.view_messages().len())
            .contains(&true)
    );
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn a_compaction_closes_an_open_search_before_its_head() {
    let compacted = compacted_step();
    let mut rig = rig(vec![
        uses(&[B::Search("s1", "rust"), echo("e1")]),
        failing(
            &[],
            mid_stream(ErrorKind::InvalidResponse, RetryClass::Never),
        ),
        compacted,
    ]);
    rig.turn("go").await;
    assert!(rig.agent.tail_live());
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    rig.agent
        .manual_compaction(None, wisp_protocol::RequestId(1), &mut rig.rx, &ev_tx)
        .await;
    drop(ev_tx);
    let mut events = Vec::new();
    while let Some(e) = ev_rx.recv().await {
        events.push(e);
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. })),
        "{events:#?}"
    );
    assert_eq!(
        ended(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Failed, true))]
    );
    let kinds = rig.kinds();
    let closed = position(&kinds, "tail_closed");
    assert_eq!(closed.len(), 1);
    assert!(closed[0] < position(&kinds, "context_compacted")[0]);
    assert!(!rig.agent.tail_live());
}

#[tokio::test]
async fn a_completed_mixed_exchange_is_sent_unchanged() {
    let mut rig = rig(vec![
        uses(&[B::Think("t"), B::Search("s1", "rust"), echo("e1")]),
        done(&[B::Found("s1"), B::Text("found")]),
        done(&[B::Text("ok")]),
    ]);
    rig.turn("go").await;
    let (_, events) = rig.turn("next").await;
    assert!(ended(&events).is_empty());
    let third = rig.messages(2);
    assert_eq!(third[1]["content"][1], search_use("s1", "rust"));
    assert_eq!(third[3]["content"][0], found("s1"));
    assert_eq!(third[1]["content"][0]["type"], "thinking");
    assert!(!rig.kinds().contains(&"tail_closed".to_owned()));
    assert!(!rig.kinds().contains(&"thinking_cut".to_owned()));
}

#[tokio::test]
async fn a_search_stopped_in_one_turn_no_longer_holds_steers_later() {
    let mut rig = rig(vec![
        uses(&[B::Search("s1", "rust"), echo("e1")]).then(ClientCommand::Cancel),
        done(&[B::Text("two")]),
        uses(&[echo("e2")]).then(ClientCommand::send("steer-3", "a steer")),
        done(&[B::Text("three")]),
    ]);
    rig.turn("one").await;
    rig.turn("two").await;
    rig.turn("three").await;
    assert!(
        rig.messages(3)
            .last()
            .unwrap()
            .to_string()
            .contains("a steer")
    );
}

#[tokio::test]
async fn a_steer_during_a_search_question_waits_for_the_answer_and_the_search() {
    let mut rig = rig_with(
        vec![
            uses(&[B::Search("s1", "rust"), B::Call("a1", "confirm", json!({}))]),
            done(&[B::Found("s1"), B::Text("found")]),
            done(&[B::Text("read your message")]),
        ],
        true,
        retry::quick(),
    );
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    let commands = rig.provider.commands();
    let watcher = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(e) = ev_rx.recv().await {
            if matches!(e, AgentEvent::QuestionAsked { .. }) {
                for command in [
                    ClientCommand::send("steer-1", "do this instead"),
                    ClientCommand::Answer {
                        call_id: "a1".into(),
                        answers: vec![Answer::choose(0)],
                    },
                ] {
                    commands.send(command).await.unwrap();
                }
            }
            seen.push(e);
        }
        seen
    });
    let outcome = rig
        .agent
        .run_turn("go".into(), &mut rig.rx, &ev_tx)
        .await
        .unwrap();
    drop(ev_tx);
    watcher.await.unwrap();
    assert!(
        matches!(outcome, TurnOutcome::Completed { steps: 3, .. }),
        "{outcome:?}"
    );
    let second = rig.messages(1);
    let results = second.last().unwrap();
    assert_eq!(results["content"].as_array().unwrap().len(), 1);
    assert_eq!(
        results["content"][0]["content"][0]["text"],
        "The user answered:\nKeep the plan"
    );
    assert!(
        !serde_json::to_string(&second)
            .unwrap()
            .contains("do this instead")
    );
    assert!(
        rig.messages(2)
            .last()
            .unwrap()
            .to_string()
            .contains("do this instead")
    );
    assert_resumes_byte_for_byte(&rig);
}

fn spent_check(rig: &Rig) -> Vec<(bool, Option<f64>)> {
    pick!(
        &log_events(&rig.store),
        LogEvent::Retrying {
            lower_bound,
            cost_usd,
            ..
        } => (*lower_bound, *cost_usd)
    )
}

#[tokio::test]
async fn a_failed_attempt_that_searched_counts_the_search_and_is_a_lower_bound() {
    let mut rig = rig(vec![
        failing(
            &[B::Search("s1", "rust")],
            mid_stream(ErrorKind::Network, RetryClass::Transient),
        ),
        done(&[B::Search("s2", "rust"), B::Found("s2"), B::Text("ok")]),
    ]);
    let (outcome, events) = rig.turn("go").await;
    let TurnOutcome::Completed { cost, .. } = &outcome else {
        panic!("{outcome:?}");
    };
    assert!(cost.as_ref().is_some_and(|c| c.turn_lower_bound));
    assert_eq!(spent_check(&rig).len(), 1);
    assert!(spent_check(&rig)[0].0);
    let attempts: Vec<(String, u32)> = pick!(
        &events,
        AgentEvent::SearchStarted {
            call_id, attempt, ..
        } => (call_id.clone(), *attempt)
    );
    assert_eq!(attempts, vec![("s1".into(), 0), ("s2".into(), 1)]);

    let mut spent = Vec::new();
    let reported = Usage {
        input_tokens: 600,
        web_search_requests: 1,
        ..Usage::default()
    };
    for (searches, usage) in [
        (0, None),
        (1, None),
        (0, Some(reported)),
        (1, Some(reported)),
    ] {
        rig.agent.track = Some(TurnTrack::new(9));
        let failure = Failure {
            error: mid_stream(ErrorKind::Network, RetryClass::Transient),
            usage,
            model: DEFAULT_MODEL.into(),
            iterations: Vec::new(),
            discarded: Default::default(),
            request_tokens: 600,
            attempts: 0,
            gave_up: None,
            searches,
        };
        let (ev_tx, _ev_rx) = mpsc::channel(64);
        let after = rig
            .agent
            .after_failure(
                1,
                &failure,
                crate::agent::retry::Mode::Request,
                &mut rig.rx,
                &ev_tx,
            )
            .await;
        assert!(matches!(after, AfterFailure::Retry));
        spent.push(rig.agent.track().retry_spent);
    }
    let charge = spent[1] - spent[0];
    assert!((charge - 0.01).abs() < 1e-9, "{spent:?}");
    assert!((spent[3] - spent[2]).abs() < 1e-9, "{spent:?}");
}

fn search_events(action: SearchAction, id: &str) -> Script {
    let signal = |signal| Ok(ProviderEvent::Search { index: 0, signal });
    let outcome = SearchOutcome::Results { hits: Vec::new() };
    vec![
        signal(SearchSignal::Started {
            call_id: id.into(),
            query: "example".into(),
            action,
        }),
        signal(SearchSignal::Ended {
            call_id: id.into(),
            outcome,
        }),
    ]
}

async fn retried_within(first: Step) -> TurnOutcome {
    let steps = vec![first, done(&[B::Text("ok")])];
    let mut rig = rig_with(steps, false, retry::quick().with_budget(Some(0.015)));
    rig.turn("go").await.0
}

#[tokio::test]
async fn provider_search_events_become_rows_and_price_only_searches() {
    let mut feed = script(&[B::Text("ok")], StopReason::EndTurn);
    let mut extra = search_events(SearchAction::OpenPage, "ws_open");
    extra.extend(search_events(SearchAction::Search, "ws_find"));
    let sources = vec![wisp_protocol::CitedSource {
        url: "https://example.com/a".into(),
        title: None,
    }];
    let cited = SearchSignal::Cited { sources };
    extra.push(Ok(ProviderEvent::Search {
        index: 0,
        signal: cited,
    }));
    feed.splice(2..2, extra);
    let mut rig = rig(vec![Step::from(feed)]);
    let (_, events) = rig.turn("go").await;
    let rows: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SearchStarted {
                call_id, action, ..
            } => Some(format!("start {call_id} {action:?}")),
            AgentEvent::SearchEnded { call_id, .. } => Some(format!("end {call_id}")),
            AgentEvent::Citations { index, sources, .. } => {
                Some(format!("cite {index} {}", sources.len()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        rows,
        [
            "start ws_open OpenPage",
            "end ws_open",
            "start ws_find Search",
            "end ws_find",
            "cite 0 1"
        ]
    );
    let retried = async |events: Script| {
        let failed = failing_after(
            events,
            mid_stream(ErrorKind::Network, RetryClass::Transient),
        );
        matches!(retried_within(failed).await, TurnOutcome::Completed { .. })
    };
    assert!(retried(search_events(SearchAction::OpenPage, "ws_open")).await);
    assert!(!retried(search_events(SearchAction::Search, "ws_find")).await);
}

#[tokio::test]
async fn the_search_charge_can_push_a_retry_over_the_budget() {
    let transient = || mid_stream(ErrorKind::Network, RetryClass::Transient);
    let outcome = retried_within(failing(&[], transient())).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let outcome = retried_within(failing(&[B::Search("s1", "rust")], transient())).await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Error {
                gave_up: Some(GaveUp::Budget),
                ..
            }
        ),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_failed_results_request_counts_the_open_call_it_carried() {
    let mut rig = rig_with(
        vec![
            uses(&[B::Search("s1", "rust"), echo("e1")]),
            failing(&[], mid_stream(ErrorKind::Network, RetryClass::Transient)),
            done(&[B::Text("ok")]),
        ],
        false,
        retry::quick().with_budget(Some(0.015)),
    );
    let (outcome, _) = rig.turn("go").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Error {
                gave_up: Some(GaveUp::Budget),
                ..
            }
        ),
        "{outcome:?}"
    );
}

fn message(role: Role, blocks: &[B]) -> Message {
    Message {
        role,
        content: blocks.iter().map(block_of).collect(),
    }
}

#[test]
fn check_layout_allows_a_paused_tail_and_its_continuation_only() {
    let provider = searching(Vec::new(), mpsc::channel(1).0);
    let user = Message::user_text("go");
    let open = message(Role::Assistant, &[B::Search("s1", "q")]);
    let settled = message(Role::Assistant, &[B::Search("s1", "q"), B::Found("s1")]);
    let after = message(Role::Assistant, &[B::Found("s1"), B::Text("done")]);
    for paused in [&open, &settled] {
        let tail = [user.clone(), paused.clone()];
        check_layout(&tail, &[false, true], &Default::default(), &*provider).unwrap();
        assert!(check_layout(&tail, &[false, false], &Default::default(), &*provider).is_err());
        let continued = [user.clone(), paused.clone(), after.clone()];
        assert!(
            check_layout(
                &continued,
                &[false, true, false],
                &Default::default(),
                &*provider
            )
            .is_err()
        );
        let next = [
            user.clone(),
            paused.clone(),
            after.clone(),
            Message::user_text("next"),
        ];
        check_layout(
            &next,
            &[false, true, false, false],
            &Default::default(),
            &*provider,
        )
        .unwrap();
        assert!(check_layout(&next, &[false; 4], &Default::default(), &*provider).is_err());
    }
    let marker = Message::effort(EffortMarker {
        level: Effort::Low,
        rendered: true,
    });
    let marked = [
        user.clone(),
        marker,
        Message::user_text("more"),
        open.clone(),
        after.clone(),
        Message::user_text("next"),
    ];
    check_layout(
        &marked,
        &[false, false, false, true, false, false],
        &Default::default(),
        &*provider,
    )
    .unwrap();
    assert!(
        check_layout(
            &marked,
            &[false, false, false, false, true, false],
            &Default::default(),
            &*provider
        )
        .is_err()
    );
}

#[test]
fn a_marker_between_users_before_a_paused_reply_keeps_the_layout_verdicts() {
    let provider = searching(Vec::new(), mpsc::channel(1).0);
    let marker = Message::effort(EffortMarker {
        level: Effort::Low,
        rendered: true,
    });
    let open = message(Role::Assistant, &[B::Search("s1", "q")]);
    let after = message(Role::Assistant, &[B::Found("s1"), B::Text("done")]);
    let marked = [
        Message::user_text("go"),
        marker.clone(),
        Message::user_text("more"),
        open,
        after,
        Message::user_text("next"),
    ];
    let paused = [false, false, false, true, false, false];
    assert_eq!(
        check_request_layout(&marked, (&paused, &[]), &Default::default(), &*provider),
        Ok(())
    );
    assert_eq!(
        check_request_layout(&marked, (&[false; 6], &[]), &Default::default(), &*provider),
        Err(LayoutError {
            message: Some(4),
            problem: "message 2 breaks the user and assistant alternation".into(),
        })
    );
    let answered = [
        Message::user_text("go"),
        marker,
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: Vec::new(),
                is_error: false,
            }],
        },
    ];
    assert_eq!(
        check_request_layout(
            &answered,
            (&[false; 3], &[]),
            &Default::default(),
            &*provider
        ),
        Err(LayoutError {
            message: Some(0),
            problem: "message 0 answers [\"t1\"], but the reply before it asked for []".into(),
        })
    );
}

#[test]
fn the_live_tail_ignores_closed_calls_and_answered_ones() {
    let provider = searching(Vec::new(), mpsc::channel(1).0);
    let user = Message::user_text("go");
    let mixed = message(
        Role::Assistant,
        &[B::Search("s1", "q"), B::Call("c1", "echo", json!({}))],
    );
    let results = Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "c1".into(),
            content: vec![ContentBlock::Text { text: "ok".into() }],
            is_error: false,
        }],
    };
    let history = vec![user.clone(), mixed.clone(), results.clone()];
    let mut closed = Closed::default();
    assert_eq!(
        open_calls(&*provider, &history, &closed),
        vec![(
            wisp_protocol::log::BlockPosition {
                message: 1,
                block: 0
            },
            "s1".to_owned()
        )]
    );
    closed.calls.insert("s1".into());
    assert!(open_calls(&*provider, &history, &closed).is_empty());
    let mut appended = results;
    appended.content.push(ContentBlock::Text {
        text: "next".into(),
    });
    let later = vec![user, mixed, appended];
    assert!(open_calls(&*provider, &later, &Closed::default()).is_empty());
    let answered = vec![
        Message::user_text("go"),
        message(Role::Assistant, &[B::Search("s1", "q"), B::Found("s1")]),
    ];
    assert!(open_calls(&*provider, &answered, &Closed::default()).is_empty());
    let paused_set: BTreeSet<usize> = [1].into();
    assert!(paused_tail(&answered, &paused_set, &Closed::default()));
    let closed_tail = Closed {
        calls: BTreeSet::new(),
        tails: [1].into(),
    };
    assert!(!paused_tail(&answered, &paused_set, &closed_tail));
}

fn searching(steps: Vec<Step>, commands: mpsc::Sender<ClientCommand>) -> Arc<Fake> {
    let provider = Fake::with(
        Knobs {
            documented: Some(false),
            search: true,
            wire_print: true,
            ..Knobs::default()
        },
        steps,
    );
    provider.send_to(commands);
    provider
}

fn implicit_context() -> LogContext {
    LogContext {
        endpoint_name: IMPLICIT_ENDPOINT.into(),
        ..log_context()
    }
}

fn connection(name: &str, provider: Arc<Fake>) -> Connection {
    Connection {
        name: name.into(),
        provider,
        context: LogContext {
            endpoint: format!("https://{name}.example.com"),
            endpoint_name: name.into(),
            ..log_context()
        },
        priced: true,
    }
}

#[tokio::test]
async fn a_stopped_search_is_sent_as_text_after_an_endpoint_switch() {
    let env = env("switch");
    let (tx, mut rx) = mpsc::channel(64);
    let first = searching(
        vec![uses(&[B::Search("s1", "rust"), echo("e1")]).then(ClientCommand::Cancel)],
        tx.clone(),
    );
    let second = searching(vec![done(&[B::Text("ok")])], tx);
    let tools: Vec<crate::tool::ToolEntry> = samples().into_iter().map(|s| s.tool).collect();
    let mut agent = Agent::new(first.clone(), default_session())
        .with_clock(TickingClock::new())
        .with_tools(Toolbox::new(tools), env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_connections(
            vec![
                connection("a", first.clone()),
                connection("b", second.clone()),
            ],
            Vec::new(),
            "a",
        );
    let (ev_tx, _ev_rx) = mpsc::channel(4096);
    agent.run_turn("one".into(), &mut rx, &ev_tx).await.unwrap();
    agent
        .change_settings(Some("b".into()), None, None, &ev_tx)
        .await;
    agent.run_turn("two".into(), &mut rx, &ev_tx).await.unwrap();
    let body = &second.bodies()[0];
    assert!(body.get("tools").is_some_and(|t| {
        t.as_array()
            .unwrap()
            .iter()
            .all(|tool| tool["name"] != "web_search")
    }));
    assert_eq!(body["messages"][1]["content"][0], closed_text("rust"));
    assert!(second.sent()[0].rendering.closed.contains("s1"));
    assert!(first.sent()[0].rendering.closed.is_empty());
}

fn rejected() -> ProviderError {
    classified(
        ErrorKind::InvalidRequest,
        "refused",
        FailurePhase::Connect,
        RetryClass::Never,
    )
}

fn stopped_with_thinking() -> Step {
    uses(&[
        B::Think("before"),
        B::Search("s1", "rust"),
        B::Think("after"),
        echo("e1"),
    ])
    .then(ClientCommand::Cancel)
}

#[tokio::test]
async fn a_closure_cut_survives_the_sessions_end() {
    let mut rig = rig(vec![stopped_with_thinking()]);
    rig.turn("go").await;
    assert_resumes_byte_for_byte(&rig);
    rig.end().await;
    assert_resumes_byte_for_byte(&rig);
    assert_eq!(rig.agent.search.cuts.len(), 1);
}

#[tokio::test]
async fn a_closure_cut_survives_a_refused_request_and_the_sessions_end() {
    let mut rig = rig(vec![stopped_with_thinking(), refused(rejected())]);
    rig.turn("go").await;
    let (outcome, _) = rig.turn("next").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    rig.end().await;
    assert_eq!(position(&rig.kinds(), "thinking_cut").len(), 1);
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn a_reply_that_stops_after_a_search_is_closed_before_a_waiting_steer() {
    let mut rig = rig(vec![
        step(&[B::Search("s1", "rust")], StopReason::StopSequence)
            .then(ClientCommand::send("steer-1", "and this")),
        done(&[B::Text("ok")]),
    ]);
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Completed {
                stop_reason: StopReason::EndTurn,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        ended(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Stopped, false))]
    );
    let second = rig.messages(1);
    assert_eq!(second[1]["content"], json!([closed_text("rust")]));
    assert!(second[2].to_string().contains("and this"));
    let kinds = rig.kinds();
    assert!(position(&kinds, "tail_closed")[0] < position(&kinds, "input_committed")[0]);
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn a_reply_that_stops_with_a_client_call_is_closed_before_a_waiting_steer() {
    let mut rig = rig(vec![
        step(
            &[B::Search("s1", "rust"), echo("e1")],
            StopReason::StopSequence,
        )
        .then(ClientCommand::send("steer-1", "and this")),
        done(&[B::Text("ok")]),
    ]);
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        ended(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Stopped, false))]
    );
    let second = rig.messages(1);
    assert_eq!(second[1]["content"][0], closed_text("rust"));
    let user = second[2].to_string();
    assert!(user.contains("Not run: the reply stopped (stop_sequence)"));
    assert!(user.contains("and this"));
    assert_resumes_byte_for_byte(&rig);
}

#[test]
fn check_layout_refuses_an_open_server_call_outside_the_live_tail() {
    let provider = searching(Vec::new(), mpsc::channel(1).0);
    let user = Message::user_text("go");
    let reply = message(Role::Assistant, &[B::Search("s1", "rust")]);
    let steer = Message::user_text("and this");
    let sent = [user.clone(), reply.clone(), steer];
    let problem = check_layout(&sent, &[], &Default::default(), &*provider).unwrap_err();
    assert!(problem.to_string().contains("s1"), "{problem}");
    let closed: BTreeSet<String> = ["s1".to_owned()].into();
    check_layout(&sent, &[], &closed, &*provider).unwrap();
    let ended = [user, reply];
    assert!(check_layout(&ended, &[false, true], &Default::default(), &*provider).is_ok());
}

#[tokio::test]
async fn a_stop_during_backoff_after_a_billed_failure_may_have_billed_the_search() {
    let mut rig = rig_with(
        vec![
            uses(&[B::Search("s1", "rust"), echo("e1")]),
            failing(&[], mid_stream(ErrorKind::Network, RetryClass::Transient))
                .then(ClientCommand::Cancel),
            done(&[B::Found("s1"), B::Text("ok")]),
        ],
        false,
        RetryPolicy {
            base_delay: std::time::Duration::from_secs(60),
            jitter: || 0.0,
            ..RetryPolicy::default()
        },
    );
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Error {
                gave_up: None,
                offer: Some(_),
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(
        ended(&events).is_empty(),
        "the failed search stays retryable"
    );
    assert_eq!(rig.provider.sent().len(), 2, "Esc sends nothing more");
    let retried = rig.serve_for(retry_choice::retry(None)).await;
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 3);
    assert_eq!(
        bodies[2], bodies[1],
        "Retry preserves the paused search request"
    );
    let ends = ended(&retried);
    assert_eq!(ends.len(), 1);
    assert!(matches!(ends[0].1, SearchEnd::Done(_)), "{ends:?}");
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn a_give_up_after_a_billed_attempt_may_have_billed_the_search() {
    let mut rig = rig(vec![
        uses(&[B::Search("s1", "rust"), echo("e1")]),
        failing(&[], mid_stream(ErrorKind::Network, RetryClass::Transient)),
        refused(rejected()),
        done(&[B::Text("ok")]),
    ]);
    let (outcome, _) = rig.turn("go").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    let (_, events) = rig.turn("next").await;
    assert_eq!(
        ended(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Failed, true))]
    );
}

#[tokio::test]
async fn a_failed_continuation_of_a_paused_reply_counts_its_unrun_call() {
    let mut rig = rig_with(
        vec![
            paused(&[B::Search("s1", "rust")]),
            failing(&[], mid_stream(ErrorKind::Network, RetryClass::Transient)),
            done(&[B::Text("ok")]),
        ],
        false,
        retry::quick().with_budget(Some(0.015)),
    );
    let (outcome, _) = rig.turn("go").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Error {
                gave_up: Some(GaveUp::Budget),
                ..
            }
        ),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_retried_request_that_succeeds_marks_no_later_call_billed() {
    let mut rig = rig(vec![
        uses(&[B::Search("s1", "rust"), echo("e1")]),
        failing(&[], mid_stream(ErrorKind::Network, RetryClass::Transient)),
        step(
            &[B::Found("s1"), B::Search("s2", "more")],
            StopReason::StopSequence,
        ),
    ]);
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        ended(&events)
            .into_iter()
            .filter(|(_, e)| matches!(e, SearchEnd::Unfinished { .. }))
            .collect::<Vec<_>>(),
        vec![("s2".into(), unfinished(UnfinishedReason::Stopped, false))]
    );
}

#[tokio::test]
async fn only_the_carried_call_of_a_tail_may_have_been_billed() {
    let mut rig = rig(vec![
        paused(&[B::Search("s1", "rust")]),
        failing(&[], mid_stream(ErrorKind::Network, RetryClass::Transient)),
        step(&[B::Search("s2", "more")], StopReason::StopSequence),
        done(&[B::Text("ok")]),
    ]);
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        ended(&events)
            .into_iter()
            .filter(|(_, e)| matches!(e, SearchEnd::Unfinished { .. }))
            .collect::<Vec<_>>(),
        vec![
            ("s1".into(), unfinished(UnfinishedReason::Stopped, true)),
            ("s2".into(), unfinished(UnfinishedReason::Stopped, false)),
        ]
    );
    rig.turn("next").await;
    let fourth = rig.messages(3);
    assert_eq!(fourth[1]["content"], json!([closed_text("rust")]));
    assert_eq!(fourth[2]["content"], json!([closed_text("more")]));
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn a_pause_after_a_steer_at_the_cap_is_continued_again() {
    let mut steps = pause_chain();
    let last = steps.pop().unwrap();
    steps.push(last.then(ClientCommand::send("steer-1", "one more")));
    steps.push(paused(&[B::Search("s7", "q7")]));
    steps.push(done(&[B::Found("s7"), B::Text("done")]));
    let mut rig = rig(steps);
    let (outcome, events) = rig.turn("search a lot").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Completed {
                stop_reason: StopReason::EndTurn,
                steps: 8,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(rig.sent(), 8);
    assert!(
        !ended(&events)
            .iter()
            .any(|(id, e)| id == "s7" && matches!(e, SearchEnd::Unfinished { .. }))
    );
    assert_resumes_byte_for_byte(&rig);
}

fn closed_ends(events: &[AgentEvent]) -> Vec<(String, SearchEnd)> {
    ended(events)
        .into_iter()
        .filter(|(_, e)| matches!(e, SearchEnd::Unfinished { .. }))
        .collect()
}

#[tokio::test]
async fn a_refused_reply_after_a_carried_call_may_have_billed_it() {
    let mut rig = rig(vec![
        uses(&[B::Search("s1", "rust"), echo("e1")]),
        step(&[B::Text("no")], StopReason::Refusal),
        done(&[B::Text("ok")]),
    ]);
    let (outcome, _) = rig.turn("go").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    let (_, events) = rig.turn("next").await;
    assert_eq!(
        closed_ends(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Failed, true))]
    );
}

#[tokio::test]
async fn continuations_that_carried_a_call_may_have_billed_it() {
    let mut steps = vec![paused(&[B::Search("s1", "rust")])];
    steps.extend((0..5).map(|_| paused(&[B::Text("still going")])));
    let mut rig = rig(steps);
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Completed {
                stop_reason: StopReason::PauseTurn,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        closed_ends(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Paused, true))]
    );
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn a_paused_reply_with_a_client_call_is_closed_as_paused() {
    let mut rig = rig(vec![
        paused(&[B::Search("s1", "rust"), echo("e1")]),
        done(&[B::Text("ok")]),
    ]);
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        closed_ends(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Paused, false))]
    );
    let kinds = rig.kinds();
    let closed = position(&kinds, "tail_closed")[0];
    let LogEvent::TailClosed { reason, .. } = &records(&rig.store)[closed].event else {
        panic!();
    };
    assert_eq!(*reason, UnfinishedReason::Paused);
    rig.turn("next").await;
    let second = rig.messages(1);
    assert_eq!(second[1]["content"][0], closed_text("rust"));
    assert_eq!(second[1]["content"][1]["type"], "tool_use");
    assert!(second[2].to_string().contains("next"));
    assert_resumes_byte_for_byte(&rig);
}

#[tokio::test]
async fn a_search_dropped_by_a_fallback_still_ends() {
    let fallback = fallback_marker("claude-opus-5-5", "claude-opus-5");
    let mut events = vec![Ok(ProviderEvent::Sending), Ok(start(600))];
    events.extend(events_of(&[B::Search("s1", "rust")]));
    events.push(Ok(ProviderEvent::Fallback {
        from: "claude-opus-5-5".into(),
        to: "claude-opus-5".into(),
    }));
    events.extend(testing::opaque(1, fallback));
    events.push(Ok(ProviderEvent::BlockStart {
        index: 2,
        block: ContentBlock::Text {
            text: "answered without it".into(),
        },
    }));
    events.push(Ok(ProviderEvent::BlockStop { index: 2 }));
    let ending = script(&[], StopReason::EndTurn);
    events.extend(ending.into_iter().skip(2));
    let mut rig = rig(vec![Step::from(events), done(&[B::Text("ok")])]);
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Completed {
                stop_reason: StopReason::EndTurn,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        closed_ends(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Failed, true))]
    );
    assert!(!rig.agent.tail_live());
    assert!(!rig.kinds().contains(&"tail_closed".to_owned()));
}

fn started_ids(events: &[AgentEvent]) -> Vec<String> {
    pick!(events, AgentEvent::SearchStarted { call_id, .. } => call_id.clone())
}

#[tokio::test]
async fn a_search_in_a_stopped_attempt_ends_as_stopped() {
    let mut rig = rig(vec![
        hanging(&[B::Search("s1", "rust")]).then(ClientCommand::Cancel),
    ]);
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    assert_eq!(started_ids(&events), vec!["s1".to_owned()]);
    assert_eq!(
        ended(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Stopped, true))]
    );
}

#[tokio::test]
async fn a_search_in_a_failed_attempt_that_gives_up_ends_as_failed() {
    let mut rig = rig(vec![
        failing(
            &[B::Search("s1", "rust")],
            mid_stream(ErrorKind::InvalidResponse, RetryClass::Never),
        ),
        done(&[B::Text("ok")]),
    ]);
    let (outcome, events) = rig.turn("go").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    assert_eq!(
        ended(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Failed, true))]
    );
    let (_, later) = rig.turn("next").await;
    assert!(ended(&later).is_empty());
}

#[tokio::test]
async fn a_search_in_a_refused_reply_ends_as_failed() {
    let mut rig = rig(vec![step(
        &[B::Search("s1", "rust"), B::Text("no")],
        StopReason::Refusal,
    )]);
    let (outcome, events) = rig.turn("go").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    assert_eq!(
        ended(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Failed, true))]
    );
}

#[tokio::test]
async fn a_search_in_a_retried_attempt_ends_before_the_retry() {
    let mut rig = rig(vec![
        failing(
            &[B::Search("s1", "rust")],
            mid_stream(ErrorKind::Network, RetryClass::Transient),
        ),
        done(&[B::Search("s2", "rust"), B::Found("s2"), B::Text("ok")]),
    ]);
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let ends: Vec<(String, u32, bool)> = pick!(
        &events,
        AgentEvent::SearchEnded {
            call_id,
            attempt,
            outcome,
            ..
        } => (
            call_id.clone(),
            *attempt,
            matches!(outcome, SearchEnd::Unfinished { .. }),
        )
    );
    assert_eq!(ends, vec![("s1".into(), 0, true), ("s2".into(), 1, false)]);
    let first_end = events
        .iter()
        .position(|e| matches!(e, AgentEvent::SearchEnded { .. }))
        .unwrap();
    let retrying = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Retrying { .. }))
        .unwrap();
    assert!(first_end < retrying);
}

#[tokio::test]
async fn a_result_after_a_fallback_does_not_end_the_search_twice() {
    let fallback = fallback_marker("claude-opus-5-5", "claude-opus-5");
    let mut events = vec![Ok(ProviderEvent::Sending), Ok(start(600))];
    events.extend(events_of(&[B::Search("s1", "rust")]));
    events.push(Ok(ProviderEvent::Fallback {
        from: "claude-opus-5-5".into(),
        to: "claude-opus-5".into(),
    }));
    for (index, block) in [(1, opaque(fallback)), (2, block_of(&B::Found("s1")))] {
        events.push(Ok(ProviderEvent::BlockStart { index, block }));
        events.push(Ok(ProviderEvent::BlockStop { index }));
    }
    events.push(Ok(ProviderEvent::BlockStart {
        index: 3,
        block: ContentBlock::Text { text: "ok".into() },
    }));
    events.push(Ok(ProviderEvent::BlockStop { index: 3 }));
    events.extend(script(&[], StopReason::EndTurn).into_iter().skip(2));
    let mut rig = rig(vec![Step::from(events)]);
    let (_, events) = rig.turn("go").await;
    assert_eq!(
        ended(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Failed, true))]
    );
}

fn ends_of(events: &[AgentEvent], id: &str) -> usize {
    ended(events).iter().filter(|(call, _)| call == id).count()
}

#[tokio::test]
async fn a_carried_result_in_a_failed_attempt_that_gives_up_ends_once() {
    let mut rig = rig(vec![
        uses(&[B::Search("s1", "rust"), echo("e1")]),
        failing(
            &[B::Found("s1")],
            mid_stream(ErrorKind::InvalidResponse, RetryClass::Never),
        ),
        done(&[B::Text("ok")]),
    ]);
    let (outcome, first) = rig.turn("go").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    let (_, second) = rig.turn("next").await;
    assert_eq!(ends_of(&first, "s1"), 0);
    assert_eq!(
        ended(&second),
        vec![("s1".into(), unfinished(UnfinishedReason::Failed, true))]
    );
}

#[tokio::test]
async fn a_carried_result_in_a_stopped_attempt_ends_once() {
    let mut rig = rig(vec![
        uses(&[B::Search("s1", "rust"), echo("e1")]),
        hanging(&[B::Found("s1")]).then(ClientCommand::Cancel),
    ]);
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        ended(&events),
        vec![("s1".into(), unfinished(UnfinishedReason::Stopped, true))]
    );
}

#[tokio::test]
async fn a_carried_result_in_a_retried_attempt_ends_once() {
    let mut rig = rig(vec![
        uses(&[B::Search("s1", "rust"), echo("e1")]),
        failing(
            &[B::Found("s1")],
            mid_stream(ErrorKind::Network, RetryClass::Transient),
        ),
        done(&[B::Found("s1"), B::Text("ok")]),
    ]);
    let (outcome, events) = rig.turn("go").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let ends: Vec<(String, u32)> = pick!(
        &events,
        AgentEvent::SearchEnded {
            call_id,
            attempt,
            outcome: SearchEnd::Done(_),
            ..
        } => (call_id.clone(), *attempt)
    );
    assert_eq!(ends, vec![("s1".into(), 1)]);
    assert_eq!(ends_of(&events, "s1"), 1);
}

mod by_call;

mod dropped;

mod forms;

mod hooked;

mod kept_tools;

mod native;

mod probe;

mod refused_thinking;

mod resume;

mod retry_choice;

mod transcript;

mod unreadable;

fn records_of(rig: &Rig, kind: &str) -> Vec<LogEvent> {
    let kinds = rig.kinds();
    let records = records(&rig.store);
    position(&kinds, kind)
        .into_iter()
        .map(|i| records[i].event.clone())
        .collect()
}

fn overloaded_before_any_event() -> Step {
    refused(classified(
        ErrorKind::Overloaded,
        "overloaded",
        FailurePhase::Connect,
        RetryClass::Transient,
    ))
}

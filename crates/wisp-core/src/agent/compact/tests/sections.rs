use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tokio::sync::Notify;
use wisp_plugin_api::prompt::{At, Prior};
use wisp_plugin_testkit::hooks::{self as kit, Calls, Knob, Section};
use wisp_protocol::log::{LEAD_ENVIRONMENT, LogRecord, SectionSnapshot};

use super::auto::{LOW, no_retries};
use super::background::{
    Landing, NEAR, POINT, WIDE_NEAR, WIDE_POINT, agent_with, background, discarded, extends,
    overflowing, small_band, swapped_in, tool_call,
};
use super::overflow::cut_off;
use super::rewind::started_at;
use super::*;
use crate::plugin::testing::hook_body;
use crate::plugin::{KernelHost, Make, PluginConfig, Registry};

const NOTES: &str = "bb/notes";
const WISP: &str = "[models.\"claude-opus-5-5\"]\nnative_compaction = false\n";

#[derive(Default)]
struct Gate {
    armed: AtomicBool,
    skip: AtomicUsize,
    started: Notify,
}

impl Gate {
    fn arm(&self, skip: usize) {
        self.skip.store(skip, Ordering::SeqCst);
        self.armed.store(true, Ordering::SeqCst);
    }
}

fn gated(knob: Knob, gate: Arc<Gate>) -> Section {
    let inner = kit::changing(knob);
    Arc::new(move |host, call| {
        let inner = inner.clone();
        let gate = gate.clone();
        Box::pin(async move {
            if call.at == At::Head && gate.armed.load(Ordering::SeqCst) {
                if gate.skip.load(Ordering::SeqCst) == 0 {
                    gate.armed.store(false, Ordering::SeqCst);
                    gate.started.notify_one();
                    std::future::pending::<()>().await;
                }
                gate.skip.fetch_sub(1, Ordering::SeqCst);
            }
            inner(host, call).await
        })
    })
}

struct Notes {
    knob: Knob,
    calls: Calls,
    gate: Arc<Gate>,
}

impl Notes {
    fn new() -> Self {
        Self {
            knob: kit::knob("v1"),
            calls: Calls::default(),
            gate: Arc::default(),
        }
    }

    fn turn_to(&self, value: &str) {
        kit::turn_to(&self.knob, value);
    }

    fn source(&self) -> (String, Make) {
        let make: Make = kit::prompted(
            kit::none(),
            gated(self.knob.clone(), self.gate.clone()),
            self.calls.clone(),
        );
        let body = kit::prompt_section("notes", "lead", "");
        (kit::manifest_in("0.2.0", "bb", &body), make)
    }

    async fn on(&self, agent: Agent) -> Agent {
        let (registry, problems) =
            Registry::from_sources(vec![self.source()], &PluginConfig::default());
        assert!(problems.is_empty(), "{problems:?}");
        let started = registry
            .start(&BTreeSet::new(), Arc::new(KernelHost::new()))
            .await;
        agent.with_hooks(started.hooks, started.host)
    }

    fn cancel_when_started(&self, live: &Live) -> tokio::task::JoinHandle<()> {
        let commands = live.commands.clone().unwrap();
        let gate = self.gate.clone();
        tokio::spawn(async move {
            within("the hook's start", gate.started.notified()).await;
            commands.send(ClientCommand::Cancel).await.unwrap();
        })
    }
}

fn notes_text(value: &str) -> String {
    kit::section_text("notes", value)
}

fn count_in(body: &Value, value: &str) -> usize {
    body["messages"]
        .to_string()
        .matches(&serde_json::to_string(&notes_text(value)).unwrap())
        .count()
}

fn logged_sections(store: &MemoryStore) -> Vec<Vec<SectionSnapshot>> {
    pick!(log_events(store), LogEvent::ContextCompacted { sections, .. } => sections)
}

fn snapshot(value: &str) -> Vec<SectionSnapshot> {
    vec![SectionSnapshot {
        name: NOTES.into(),
        snapshot: value.into(),
    }]
}

fn turn_entries(store: &MemoryStore) -> Vec<(u64, Option<String>)> {
    pick!(
        log_events(store),
        LogEvent::TurnStarted { turn, lead, .. } => (
            turn,
            lead.entries
                .iter()
                .find(|e| e.by == NOTES)
                .and_then(|e| e.snapshot.clone()),
        )
    )
}

pub(super) fn cancelled(outcome: &TurnOutcome) -> bool {
    matches!(outcome, TurnOutcome::Cancelled { .. })
}

pub(super) fn ends(seen: &[AgentEvent]) -> usize {
    seen.iter()
        .filter(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .count()
}

async fn resumed(store: &MemoryStore, provider: Arc<Fake>, notes: &Notes) -> Agent {
    resumed_from(&store.read("s1").unwrap(), store, provider, notes).await
}

async fn resumed_from(
    records: &[LogRecord],
    store: &MemoryStore,
    provider: Arc<Fake>,
    notes: &Notes,
) -> Agent {
    let seq = u64::try_from(records.len()).unwrap();
    let log = SessionLog::continuing(Box::new(store.clone()), "s1", seq);
    let agent = Agent::new(provider, default_session()).with_log(log, log_context());
    notes.on(agent).await.resume(records, 0).unwrap()
}

fn spliced(mut records: Vec<LogRecord>, mut extra: Vec<(usize, Value)>) -> Vec<LogRecord> {
    extra.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    for (at, event) in extra {
        let mut record = records[at].clone();
        record.event = serde_json::from_value(event).unwrap();
        record.seq = u64::MAX;
        records.insert(at, record);
    }
    let seqs: BTreeMap<u64, u64> = (0..)
        .zip(&records)
        .map(|(seq, record)| (record.seq, seq))
        .collect();
    let mut records = super::processes::renumbered(records);
    for record in &mut records {
        if let LogEvent::Rewound { to, .. } = &mut record.event {
            *to = seqs[to];
        }
    }
    records
}

#[tokio::test]
async fn resumed_points_hold_what_the_log_held_before_each_turn() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        compaction(SUMMARY),
        answer("Three.", 300),
        answer("Four.", 400),
        failing(server_error()),
        answer("Five.", 500),
        compaction(SUMMARY),
        answer("Six.", 300),
        answer("Seven.", 400),
        compaction(SUMMARY),
        answer("Eight.", 400),
    ]);
    let mut live = Live::start(notes.on(logged(provider.clone(), &store)).await);
    assert!(completed(&live.turn("u1", "one").await));
    notes.turn_to("v2");
    assert!(completed(&live.turn("u2", "two").await));
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    assert!(completed(&live.turn("u3", "three").await));
    notes.turn_to("v3");
    assert!(completed(&live.turn("u4", "four").await));
    assert!(offered(&live.turn("u5", "five").await));
    live.send(super::retry::retry(5, 3)).await;
    let retried = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        matches!(retried, AgentEvent::TurnEnded { .. }),
        "{retried:?}"
    );
    let done = live.compact(4, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    assert!(completed(&live.turn("u6", "six").await));
    assert!(completed(&live.turn("u7", "seven").await));
    live.send(crate::agent::tool_tests::rewind(5, 8)).await;
    let done = live
        .until(|e| matches!(e, AgentEvent::Rewound { .. } | AgentEvent::Rejected { .. }))
        .await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    let done = live.compact(6, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    assert!(completed(&live.turn("u8", "seven again").await));
    live.close().await;
    let mut records = store.read("s1").unwrap();
    if let Some(LogEvent::ContextCompacted { covers, .. }) = records
        .iter_mut()
        .rev()
        .map(|r| &mut r.event)
        .find(|e| matches!(e, LogEvent::ContextCompacted { .. }))
    {
        *covers = None;
    }
    let todo = |text: &str| json!({"type": "todos_updated", "turn": 1, "call_id": "t", "items": [{"text": text, "status": "in_progress"}]});
    let cwd = |path: &str| {
        json!({
            "type": "tool_call_finished", "turn": 1, "call_id": "c",
            "outcome": {"status": "completed", "is_error": false, "exit": null},
            "output": {"tail": "", "bytes": 0, "lines": 0, "file": null, "file_problem": null, "inlined": false},
            "facts": [], "state": {"type": "cwd", "path": path}, "duration_ms": 1,
        })
    };
    let process = |id: u32| json!({"type": "process_started", "turn": 1, "id": id, "call_id": "p", "reason": "started", "tty": false, "stdin": false});
    let extra = vec![
        (started_at(&records, 2), process(1)),
        (started_at(&records, 3), cwd("/a")),
        (started_at(&records, 4), todo("plan")),
        (
            started_at(&records, 4),
            json!({"type": "process_heard", "turn": 3, "ids": [1], "typed": []}),
        ),
        (
            started_at(&records, 5),
            json!({"type": "tail_closed", "through": 9, "calls": ["gone"], "reason": "stopped"}),
        ),
        (started_at(&records, 7), cwd("/b")),
        (started_at(&records, 8) + 1, todo("left")),
        (started_at(&records, 8) + 1, process(4)),
    ];
    let records = spliced(records, extra);
    let agent = resumed_from(&records, &store, provider, &notes).await;
    let marks: Vec<String> = agent
        .rewind
        .marks
        .iter()
        .map(|m| {
            format!(
                "{} {:?} {:?} {:?} {:?} {} {:?} {:?}",
                m.turn, m.head, m.cwd, m.closed, m.heard, m.numbered_from, m.todos, m.sections
            )
        })
        .collect();
    let expected = [
        "1 None None Closed { calls: {}, tails: {} } {} 1 [] {}",
        "2 None None Closed { calls: {}, tails: {} } {} 2 [] {\"bb/notes\": Snapshot(\"v1\")}",
        "3 Some(0) Some(\"/a\") Closed { calls: {}, tails: {} } {} 2 [] {\"bb/notes\": Snapshot(\"v2\")}",
        "4 Some(0) Some(\"/a\") Closed { calls: {}, tails: {} } {1} 2 [TodoItem { text: \"plan\", status: InProgress, section: None }] {\"bb/notes\": Snapshot(\"v2\")}",
        "6 Some(0) Some(\"/a\") Closed { calls: {\"gone\"}, tails: {9} } {1} 2 [TodoItem { text: \"plan\", status: InProgress, section: None }] {\"bb/notes\": Snapshot(\"v3\")}",
        "7 Some(1) Some(\"/b\") Closed { calls: {\"gone\"}, tails: {9} } {1} 2 [TodoItem { text: \"plan\", status: InProgress, section: None }] {\"bb/notes\": Snapshot(\"v3\")}",
        "9 Some(2) Some(\"/b\") Closed { calls: {\"gone\"}, tails: {9} } {1} 5 [TodoItem { text: \"plan\", status: InProgress, section: None }] {\"bb/notes\": Snapshot(\"v3\")}",
    ];
    assert_eq!(marks, expected);
    assert_eq!(agent.rewind.left, BTreeSet::from([8]));
    let spans: Vec<(u64, u64)> = agent.rewind.heads.iter().map(|h| h.turns).collect();
    assert_eq!(spans, [(1, 2), (3, 6), (3, 7)]);
}

#[tokio::test]
async fn a_native_head_carries_the_section_after_its_block_and_a_resume_restores_it() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Again.", 300),
        answer("Back.", 400),
    ]);
    let mut live = Live::start(notes.on(logged(provider.clone(), &store)).await);
    assert!(completed(&live.turn("u1", "first").await));
    notes.turn_to("v2");
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, _) = live.close().await;
    assert_eq!(logged_sections(&store), [snapshot("v2")]);
    let head = &agent.compaction.head.as_ref().unwrap().message.content;
    assert!(matches!(head[0], ContentBlock::Opaque { .. }), "{head:?}");
    assert_eq!(
        head[1],
        ContentBlock::Text {
            text: notes_text("v2")
        }
    );
    let sent = provider.sent();
    assert_eq!(
        (
            count_in(&sent[2].body(), "v1"),
            count_in(&sent[2].body(), "v2")
        ),
        (0, 1)
    );
    assert_eq!(turn_entries(&store)[1], (2, None));
    assert_rebuilds(&store, &agent);
    let calls = notes.calls.sections().len();
    let mut live = Live::start(resumed(&store, provider.clone(), &notes).await);
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    let after = notes.calls.sections();
    assert!(
        after[calls..]
            .iter()
            .filter(|c| c.at == At::Turn)
            .all(|c| c.previous == Prior::Snapshot("v2".into())),
        "{after:?}"
    );
    assert_eq!(count_in(&provider.body(3), "v2"), 1);
}

#[tokio::test]
async fn a_wisp_head_places_the_section_before_the_compacted_context() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        answer(WRITTEN, 1_200),
        answer("Again.", 300),
    ]);
    let agent = logged(provider.clone(), &store).with_model_settings(models(WISP));
    let mut live = Live::start(notes.on(agent).await);
    assert!(completed(&live.turn("u1", "first").await));
    notes.turn_to("v2");
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, _) = live.close().await;
    let head = head_text(&agent);
    let section = head.find(&notes_text("v2")).unwrap();
    let context = head.find("<compacted_context").unwrap();
    assert!(section < context, "{head}");
    assert_eq!(logged_sections(&store), [snapshot("v2")]);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_swap_between_turns_carries_the_section_and_commits_it() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY),
        answer("Again.", 300),
        answer("More.", 400),
    ]);
    let mut live = Live::start(notes.on(background(&provider, &store)).await);
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    notes.turn_to("v2");
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, _) = live.close().await;
    assert_eq!(logged_sections(&store), [snapshot("v2")]);
    assert_eq!(count_in(&provider.body(2), "v2"), 1);
    assert_eq!(count_in(&provider.body(3), "v2"), 1);
    extends(&provider, 2, 3);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_swap_within_a_turn_carries_the_section() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let precompute = Landing::new();
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        precompute.gated(compaction(SUMMARY)),
        tool_call(NEAR + 200, "Looking.").gated(&step),
        answer("Done.", 400),
    ]);
    let mut live = Live::start(notes.on(background(&provider, &store)).await);
    assert!(completed(&live.turn("u1", "first").await));
    notes.turn_to("v2");
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    precompute.land().await;
    step.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (agent, _) = live.close().await;
    assert_eq!(logged_sections(&store), [snapshot("v2")]);
    let last = provider.body(3);
    assert_eq!(count_in(&last, "v2"), 2, "{last}");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn an_overflow_head_renders_to_the_unsaved_messages_target() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = scripted(vec![
        answer("One.", 50_000),
        answer("Two.", 100_000),
        answer("Three.", 160_000),
        failing(ProviderError::new(
            ErrorKind::ContextOverflow,
            "prompt is too long: 250000 tokens > 200000 maximum",
        )),
        compaction(SUMMARY),
        answer("Four.", 900),
    ]);
    let agent = logged(provider.clone(), &store).with_auto_compact(false);
    let mut live = Live::start(notes.on(agent).await);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    notes.turn_to("v2");
    assert!(completed(&live.turn("u4", "fourth").await));
    let (agent, _) = live.close().await;
    let heads = notes.calls.sections_at(At::Head);
    assert!(
        heads
            .iter()
            .any(|c| c.target.as_deref() == Some("v2") && c.previous == Prior::None),
        "{heads:?}"
    );
    assert_eq!(logged_sections(&store), [snapshot("v2")]);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn esc_in_a_manual_compactions_head_step_keeps_the_head_and_opens_no_turn() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let calls = Calls::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let sources = vec![notes.source(), head_hook(100, &calls)];
    let agent = with_sources(logged(provider.clone(), &store), sources).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    notes.gate.arm(0);
    let canceller = notes.cancel_when_started(&live);
    let done = live.compact(2, None).await;
    canceller.await.unwrap();
    assert!(
        matches!(done, AgentEvent::Compacted { turn: None, .. }),
        "{done:?}"
    );
    assert_eq!(logged_sections(&store), [Vec::new()]);
    assert_eq!(turn_entries(&store).len(), 1);
    let compacted: Vec<Option<u64>> = pick!(
        log_events(&store),
        LogEvent::ContextCompacted { turn, .. } => turn
    );
    assert_eq!(compacted, [None]);
    let (agent, seen) = live.close().await;
    assert!(agent.track.is_none());
    assert_eq!(calls.count("head"), 0);
    assert!(!head_text(&agent).contains(&"h".repeat(300)));
    assert_eq!(ends(&seen), 1);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, _) = live.close().await;
    assert_eq!(count_in(&provider.body(2), "v1"), 1);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn esc_in_an_in_turn_heads_step_sends_nothing_more_and_cancels_the_turn() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off(2_000, "Looking around.", true),
        compaction(SUMMARY),
        answer("unused", 900),
    ]);
    let agent = logged(provider.clone(), &store).with_auto_compact(false);
    let mut live = Live::start(notes.on(agent).await);
    assert!(completed(&live.turn("u1", "first").await));
    notes.gate.arm(2);
    let canceller = notes.cancel_when_started(&live);
    let outcome = live.turn("u2", "second").await;
    canceller.await.unwrap();
    let (_, seen) = live.close().await;
    assert!(cancelled(&outcome), "{outcome:?}");
    assert_eq!(ends(&seen), 2);
    assert_eq!(provider.sent().len(), 3);
}

#[tokio::test]
async fn esc_in_the_refresh_before_a_pre_turn_compaction_sends_no_summariser_request() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = scripted(vec![answer("Hello.", 5_000), compaction(SUMMARY)]);
    let agent = no_retries(logged(provider.clone(), &store).with_model_settings(models(LOW)));
    let mut live = Live::start(notes.on(agent).await);
    assert!(completed(&live.turn("u1", "first").await));
    notes.gate.arm(0);
    let canceller = notes.cancel_when_started(&live);
    let outcome = live.turn("u2", "second").await;
    canceller.await.unwrap();
    let (_, seen) = live.close().await;
    assert!(cancelled(&outcome), "{outcome:?}");
    assert_eq!(ends(&seen), 2);
    assert_eq!(provider.sent().len(), 1);
}

#[tokio::test]
async fn a_stop_before_the_wait_for_a_running_precompute_ends_the_turn_at_once() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let precompute = Landing::new();
    let provider = documented(vec![
        answer("Hello.", 20_500),
        precompute.gated(compaction(SUMMARY)),
    ]);
    let mut live = Live::start(notes.on(background(&provider, &store)).await);
    assert!(completed(&live.turn("u1", "first").await));
    notes.gate.arm(0);
    let canceller = notes.cancel_when_started(&live);
    let outcome = live.turn("u2", "second").await;
    canceller.await.unwrap();
    let (_, seen) = live.close().await;
    assert!(cancelled(&outcome), "{outcome:?}");
    assert_eq!(provider.sent().len(), 2);
    assert!(
        discarded(&seen).iter().any(|(_, turn)| turn.is_none()),
        "{:?}",
        discarded(&seen)
    );
    let _ = POINT;
}

fn replaying(calls: &Calls) -> (String, Make) {
    let make: Make = kit::instantiate(kit::append("R\n"), calls.clone());
    (
        kit::manifest_in("0.2.0", "cc", &kit::replay_hook("r")),
        make,
    )
}

async fn with_sources(agent: Agent, sources: Vec<(String, Make)>) -> Agent {
    let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry
        .start(&BTreeSet::new(), Arc::new(KernelHost::new()))
        .await;
    agent.with_hooks(started.hooks, started.host)
}

fn last_turn_previous(notes: &Notes) -> Option<Prior> {
    notes
        .calls
        .sections_at(At::Turn)
        .last()
        .map(|c| c.previous.clone())
}

#[tokio::test]
async fn a_failed_first_request_and_a_cancelled_turn_commit_nothing() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = scripted(vec![
        answer("One.", 1_000),
        failing(server_error()),
        Step::hang(),
        answer("Four.", 1_200),
    ]);
    let agent = no_retries(logged(provider.clone(), &store));
    let mut live = Live::start(notes.on(agent).await);
    assert!(completed(&live.turn("u1", "first").await));
    notes.turn_to("v2");
    assert!(!completed(&live.turn("u2", "second").await));
    assert_eq!(
        last_turn_previous(&notes),
        Some(Prior::Snapshot("v1".into()))
    );
    live.send(send("u3", "third")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    live.send(ClientCommand::Cancel).await;
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        matches!(
            ended,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Cancelled { .. },
                ..
            }
        ),
        "{ended:?}"
    );
    assert_eq!(
        last_turn_previous(&notes),
        Some(Prior::Snapshot("v1".into()))
    );
    assert!(completed(&live.turn("u4", "fourth").await));
    let (agent, _) = live.close().await;
    assert_eq!(
        last_turn_previous(&notes),
        Some(Prior::Snapshot("v1".into()))
    );
    let last = provider.body(3);
    assert_eq!((count_in(&last, "v1"), count_in(&last, "v2")), (1, 1));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_retry_replays_a_replay_hooks_entry_without_calling_it() {
    let store = MemoryStore::default();
    let calls = Calls::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        failing(server_error()),
        answer("Two.", 1_200),
    ]);
    let agent = no_retries(super::retry::still(provider.clone(), &store));
    let mut live = Live::start(with_sources(agent, vec![replaying(&calls)]).await);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(!completed(&live.turn("u2", "second").await));
    assert_eq!(calls.count("r"), 2);
    live.send(super::retry::retry(2, 1)).await;
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        matches!(
            ended,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Completed { .. },
                ..
            }
        ),
        "{ended:?}"
    );
    let (agent, _) = live.close().await;
    assert_eq!(calls.count("r"), 2);
    let retried = provider.body(2)["messages"].to_string();
    assert_eq!(retried.matches("R\\n").count(), 2, "{retried}");
    let replayed = log_events(&store).into_iter().find_map(|e| match e {
        LogEvent::TurnStarted { turn: 3, lead, .. } => Some(lead),
        _ => None,
    });
    let replayed = replayed.unwrap();
    assert!(
        replayed.entries.iter().any(|e| e.by == "cc/r" && e.replay),
        "{replayed:?}"
    );
    assert_rebuilds(&store, &agent);
}

fn bb_enabled() -> wisp_protocol::settings::SettingTarget {
    wisp_protocol::settings::SettingTarget::Path(
        wisp_protocol::settings::SitePath::plugin_switch("bb", "enabled").unwrap(),
    )
}

fn bb_off_at_start() -> crate::settings::host::StartLayers {
    crate::settings::host::StartLayers {
        flags: vec![crate::settings::Given {
            target: wisp_protocol::settings::Target::Path(
                wisp_protocol::settings::SitePath::plugin_switch("bb", "enabled").unwrap(),
            ),
            value: SettingValue::Bool(false),
            origin: "--set".into(),
        }],
        ..Default::default()
    }
}

async fn switch_bb(live: &mut Live, on: bool, request: u64) {
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes: vec![SettingChange {
            target: bb_enabled(),
            value: Some(SettingValue::Bool(on)),
        }],
        save: false,
    })
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(r)),
                ..
            } if *r == request
        )
    })
    .await;
}

async fn effort_low(live: &mut Live) {
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(70),
        changes: vec![SettingChange::named("effort", SettingValue::Text("low".into())).unwrap()],
        save: false,
    })
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(70)),
                ..
            }
        )
    })
    .await;
}

#[tokio::test]
async fn a_section_off_and_on_in_one_window_sends_only_its_change() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        answer("Three.", 1_200),
    ]);
    let mut live = Live::start(notes.on(logged(provider.clone(), &store)).await);
    assert!(completed(&live.turn("u1", "first").await));
    switch_bb(&mut live, false, 71).await;
    effort_low(&mut live).await;
    let before = notes.calls.sections().len();
    notes.turn_to("v2");
    assert!(completed(&live.turn("u2", "second").await));
    assert_eq!(notes.calls.sections().len(), before);
    switch_bb(&mut live, true, 72).await;
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    assert_eq!(
        last_turn_previous(&notes),
        Some(Prior::Snapshot("v1".into()))
    );
    assert_eq!(count_in(&provider.body(1), "v2"), 0);
    let last = provider.body(2);
    assert_eq!((count_in(&last, "v1"), count_in(&last, "v2")), (1, 1));
}

#[tokio::test]
async fn a_section_off_across_a_compaction_and_off_from_the_start_sends_its_whole_text() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = scripted(vec![
        answer("One.", 1_000),
        compaction(SUMMARY),
        answer("Two.", 1_100),
    ]);
    let agent = notes
        .on(logged(provider.clone(), &store))
        .await
        .with_start_layers(bb_off_at_start());
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    assert!(notes.calls.sections().is_empty());
    assert_eq!(logged_sections(&store), [Vec::new()]);
    switch_bb(&mut live, true, 73).await;
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    assert_eq!(last_turn_previous(&notes), Some(Prior::None));
    assert_eq!(count_in(&provider.body(0), "v1"), 0);
    assert_eq!(count_in(&provider.body(2), "v1"), 1);
}

#[tokio::test]
async fn esc_in_a_step_boundarys_refresh_sends_no_further_request() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = scripted(vec![
        answer("One.", 1_000),
        tool_call(1_100, "Looking."),
        answer("unused", 1_200),
    ]);
    let agent = logged(provider.clone(), &store).with_auto_compact(false);
    let mut live = Live::start(notes.on(agent).await);
    assert!(completed(&live.turn("u1", "first").await));
    notes.gate.arm(1);
    let canceller = notes.cancel_when_started(&live);
    let outcome = live.turn("u2", "second").await;
    canceller.await.unwrap();
    let (_, seen) = live.close().await;
    assert!(cancelled(&outcome), "{outcome:?}");
    assert_eq!(ends(&seen), 2);
    assert_eq!(provider.sent().len(), 2);
}

#[tokio::test]
async fn a_stop_before_a_switch_compaction_cancels_it_with_no_request() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        summarised("Fresh.", 2_000, 0, vec![]),
        answer("unused", 400),
    ]);
    let agent = super::marker::other_without_native(&store, &provider);
    let mut live = Live::start(notes.on(agent).await);
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    super::marker::change(&mut live, 3, Some("other"), None).await;
    notes.gate.arm(0);
    let canceller = notes.cancel_when_started(&live);
    let outcome = live.turn("u3", "third").await;
    canceller.await.unwrap();
    let (agent, seen) = live.close().await;
    assert!(cancelled(&outcome), "{outcome:?}");
    assert_eq!(ends(&seen), 3);
    assert_eq!(provider.sent().len(), 3);
    assert!(
        seen.iter().any(|e| matches!(
            e,
            AgentEvent::CompactionFailed {
                kind: CompactionFailure::Cancelled,
                trigger: CompactionTrigger::Switch,
                ..
            }
        )),
        "{seen:#?}"
    );
    assert_eq!(agent.endpoint_name(), "env");
}

pub(super) fn swapping_for_an_overflow(
    precompute: &Arc<Semaphore>,
    step: &Arc<Semaphore>,
) -> Vec<Step> {
    vec![
        answer("Hello.", WIDE_NEAR),
        compaction(SUMMARY).gated(precompute),
        tool_call(WIDE_NEAR + 100, "Looking."),
        overflowing(step),
        answer("Done.", 900),
    ]
}

pub(super) async fn overflow_at_step_two(
    live: &mut Live,
    precompute: &Arc<Semaphore>,
    step: &Arc<Semaphore>,
) -> TurnOutcome {
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 2 }))
        .await;
    precompute.add_permits(1);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    step.add_permits(1);
    match live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await
    {
        AgentEvent::TurnEnded { outcome, .. } => outcome,
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn a_swap_for_an_overflow_carries_the_section_in_its_head() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let precompute = Arc::new(Semaphore::new(0));
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(swapping_for_an_overflow(&precompute, &step));
    let agent = agent_with(&provider, &store, WIDE_POINT, small_band());
    let mut live = Live::start(notes.on(agent).await);
    assert!(completed(&live.turn("u1", "first").await));
    let outcome = overflow_at_step_two(&mut live, &precompute, &step).await;
    let (agent, seen) = live.close().await;
    assert!(completed(&outcome), "{outcome:?}");
    assert!(swapped_in(&seen, 2), "{seen:#?}");
    assert_eq!(logged_sections(&store), [snapshot("v1")]);
    let head = agent.compaction.head.as_ref().unwrap();
    assert_eq!(head.placement, wisp_protocol::Placement::Leading);
    assert_eq!(
        head.additions[0],
        ContentBlock::Text {
            text: notes_text("v1")
        }
    );
    assert_eq!(provider.sent().len(), 5);
    assert_eq!(count_in(&provider.body(4), "v1"), 1);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn esc_in_a_swap_for_an_overflows_head_step_sends_nothing_more_and_cancels_the_turn() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let precompute = Arc::new(Semaphore::new(0));
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(swapping_for_an_overflow(&precompute, &step));
    let agent = agent_with(&provider, &store, WIDE_POINT, small_band());
    let mut live = Live::start(notes.on(agent).await);
    assert!(completed(&live.turn("u1", "first").await));
    let heads = notes.calls.sections_at(At::Head).len();
    notes.gate.arm(2);
    let canceller = notes.cancel_when_started(&live);
    let outcome = overflow_at_step_two(&mut live, &precompute, &step).await;
    canceller.await.unwrap();
    let (_, seen) = live.close().await;
    assert!(cancelled(&outcome), "{outcome:?}");
    assert_eq!(notes.calls.sections_at(At::Head).len(), heads + 3);
    assert!(swapped_in(&seen, 2), "{seen:#?}");
    assert_eq!(logged_sections(&store), [Vec::new()]);
    assert_eq!(ends(&seen), 2);
    assert_eq!(provider.sent().len(), 4);
}

pub(super) fn converting() -> Vec<Step> {
    vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        block_refused(),
        answer("Two.", 300),
    ]
}

pub(super) fn converted(seen: &[AgentEvent]) -> bool {
    seen.iter().any(|e| {
        matches!(
            e,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Convert,
                turn: Some(2),
                ..
            }
        )
    })
}

#[tokio::test]
async fn a_convert_in_a_turn_carries_the_section_in_its_head() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = scripted(converting());
    let mut live = Live::start(notes.on(logged(provider.clone(), &store)).await);
    assert!(completed(&live.turn("u1", "first").await));
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    notes.turn_to("v2");
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    assert!(converted(&seen), "{seen:#?}");
    assert_eq!(logged_sections(&store), [snapshot("v1"), snapshot("v2")]);
    let head = head_text(&agent);
    let section = head.find(&notes_text("v2")).unwrap();
    assert!(section < head.find("<compacted_context").unwrap(), "{head}");
    let resent = provider.body(3);
    assert_eq!(
        (count_in(&resent, "v1"), count_in(&resent, "v2")),
        (0, 2),
        "{resent}"
    );
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn esc_in_a_converts_head_step_sends_nothing_more_and_cancels_the_turn() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = scripted(converting());
    let mut live = Live::start(notes.on(logged(provider.clone(), &store)).await);
    assert!(completed(&live.turn("u1", "first").await));
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    let heads = notes.calls.sections_at(At::Head).len();
    notes.gate.arm(1);
    let canceller = notes.cancel_when_started(&live);
    let outcome = live.turn("u2", "second").await;
    canceller.await.unwrap();
    let (_, seen) = live.close().await;
    assert!(cancelled(&outcome), "{outcome:?}");
    assert_eq!(notes.calls.sections_at(At::Head).len(), heads + 2);
    assert!(converted(&seen), "{seen:#?}");
    assert_eq!(logged_sections(&store), [snapshot("v1"), Vec::new()]);
    assert_eq!(ends(&seen), 2);
    assert_eq!(provider.sent().len(), 3);
}

fn with_lines(agent: Agent, lines: &[&str]) -> Agent {
    let dir = std::env::temp_dir();
    let workspace = crate::workspace::Workspace {
        root: dir.clone(),
        home: None,
        temp: dir,
    };
    agent
        .with_tools(crate::tool::testkit::toolbox(&["echo"]), workspace)
        .with_platform("Linux")
        .with_resources(None)
        .with_agents(
            crate::agents::AgentsHandle::none(),
            Some(lines.iter().map(|l| (*l).to_owned()).collect()),
        )
}

const LINES: &[&str] = &["Agent: /a", "Parent: main"];
const EDITED: &[&str] = &[
    "Agent: /a",
    "Parent: main",
    "Presets for spawn_agent: rev (Reviews)",
];

fn resumed_with(store: &MemoryStore, provider: Arc<Fake>, lines: &[&str]) -> Agent {
    let records = store.read("s1").unwrap();
    let seq = u64::try_from(records.len()).unwrap();
    let log = SessionLog::continuing(Box::new(store.clone()), "s1", seq);
    with_lines(Agent::new(provider, default_session()), lines)
        .with_log(log, log_context())
        .resume(&records, 0)
        .unwrap()
}

fn environments_in_last_message(body: &Value) -> Vec<String> {
    let messages = body["messages"].as_array().unwrap();
    messages.last().unwrap()["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["text"].as_str())
        .filter(|t| t.starts_with("<environment>"))
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn a_resume_after_a_preset_edit_re_sends_the_environment_a_head_carried() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(with_lines(logged(provider.clone(), &store), LINES));
    assert!(completed(&live.turn("u1", "first").await));
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    live.close().await;
    let head = logged_sections(&store);
    assert_eq!(head.len(), 1);
    assert_eq!(head[0][0].name, LEAD_ENVIRONMENT);
    assert!(!head[0][0].snapshot.contains("Presets"), "{head:?}");
    let mut live = Live::start(resumed_with(&store, provider.clone(), EDITED));
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    let sent = environments_in_last_message(&provider.body(2));
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert_eq!(sent[0], head[0][0].snapshot);
    assert!(
        sent[1].contains("Presets for spawn_agent: rev (Reviews)\n"),
        "{sent:?}"
    );
}

#[tokio::test]
async fn a_head_with_the_current_lines_resumed_unchanged_re_sends_nothing() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        answer("Edited.", 1_100),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(with_lines(logged(provider.clone(), &store), LINES));
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    let mut live = Live::start(resumed_with(&store, provider.clone(), EDITED));
    assert!(completed(&live.turn("u2", "second").await));
    assert_eq!(environments_in_last_message(&provider.body(1)).len(), 1);
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    live.close().await;
    let head = logged_sections(&store);
    assert!(head[0][0].snapshot.contains("Presets"), "{head:?}");
    let mut live = Live::start(resumed_with(&store, provider.clone(), EDITED));
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    assert_eq!(
        environments_in_last_message(&provider.body(3)),
        [head[0][0].snapshot.clone()]
    );
}

fn failing_after_its_head() -> Vec<Step> {
    vec![
        answer("One.", 50_000),
        answer("Two.", 100_000),
        failing(ProviderError::new(
            ErrorKind::ContextOverflow,
            "prompt is too long: 250000 tokens > 200000 maximum",
        )),
        compaction(SUMMARY),
        failing(server_error()),
        answer("Three.", 900),
    ]
}

async fn retried_after_its_head(resume: bool) -> Value {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = scripted(failing_after_its_head());
    let agent = no_retries(logged(provider.clone(), &store)).with_auto_compact(false);
    let mut live = Live::start(notes.on(agent).await);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    notes.turn_to("v2");
    assert!(!completed(&live.turn("u3", "third").await));
    if resume {
        live.close().await;
        let records = store.read("s1").unwrap();
        let seq = u64::try_from(records.len()).unwrap();
        let log = SessionLog::continuing(Box::new(store.clone()), "s1", seq);
        let agent = no_retries(Agent::new(provider.clone(), default_session()))
            .with_auto_compact(false)
            .with_log(log, log_context());
        live = Live::start(notes.on(agent).await.resume(&records, 0).unwrap());
    }
    live.send(super::retry::retry(3, 1)).await;
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        matches!(
            ended,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Completed { .. },
                ..
            }
        ),
        "{ended:?}"
    );
    live.close().await;
    assert_eq!(logged_sections(&store), [snapshot("v2")]);
    assert_eq!(provider.sent().len(), 6);
    provider.body(5)
}

#[tokio::test]
async fn a_resumed_retry_after_its_turns_head_sends_what_the_live_retry_sends() {
    let live = retried_after_its_head(false).await;
    let resumed = retried_after_its_head(true).await;
    assert_eq!(count_in(&live, "v2"), 1, "{live}");
    assert_eq!(resumed, live);
}

#[tokio::test]
async fn a_stop_before_the_wait_between_steps_for_a_running_precompute_ends_the_turn_at_once() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let precompute = Landing::new();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        precompute.gated(compaction(SUMMARY)),
        tool_call(20_500, "Looking."),
        answer("unused", 300),
    ]);
    let mut live = Live::start(notes.on(background(&provider, &store)).await);
    assert!(completed(&live.turn("u1", "first").await));
    notes.gate.arm(1);
    let canceller = notes.cancel_when_started(&live);
    let outcome = live.turn("u2", "second").await;
    canceller.await.unwrap();
    let (_, seen) = live.close().await;
    assert!(cancelled(&outcome), "{outcome:?}");
    assert_eq!(provider.sent().len(), 3);
    assert!(
        discarded(&seen).iter().any(|(_, turn)| turn.is_none()),
        "{:?}",
        discarded(&seen)
    );
}

#[tokio::test]
async fn a_swap_on_a_stopped_turn_calls_no_section_in_its_head_step() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY),
        answer("unused", 300),
    ]);
    let mut live = Live::start(notes.on(background(&provider, &store)).await);
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    let heads = notes.calls.sections_at(At::Head).len();
    notes.gate.arm(0);
    let canceller = notes.cancel_when_started(&live);
    let outcome = live.turn("u2", "second").await;
    canceller.await.unwrap();
    let (_, seen) = live.close().await;
    assert!(cancelled(&outcome), "{outcome:?}");
    assert!(swapped_in(&seen, 2), "{seen:#?}");
    assert_eq!(notes.calls.sections_at(At::Head).len(), heads + 1);
    assert_eq!(logged_sections(&store), [Vec::new()]);
    assert_eq!(provider.sent().len(), 2);
}

#[tokio::test]
async fn esc_in_a_switchs_head_step_keeps_the_head_and_the_switch_and_sends_nothing_more() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        summarised("Fresh.", 2_000, 0, vec![]),
        answer("unused", 400),
    ]);
    let agent = super::marker::other_without_native(&store, &provider);
    let mut live = Live::start(notes.on(agent).await);
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    super::marker::change(&mut live, 3, Some("other"), None).await;
    let heads = notes.calls.sections_at(At::Head).len();
    notes.gate.arm(1);
    let canceller = notes.cancel_when_started(&live);
    let outcome = live.turn("u3", "third").await;
    canceller.await.unwrap();
    let (agent, seen) = live.close().await;
    assert!(cancelled(&outcome), "{outcome:?}");
    assert_eq!(notes.calls.sections_at(At::Head).len(), heads + 2);
    assert!(
        seen.iter().any(|e| matches!(
            e,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Switch,
                ..
            }
        )),
        "{seen:#?}"
    );
    assert_eq!(
        logged_sections(&store),
        [snapshot("v1"), Vec::new(), Vec::new()]
    );
    assert_eq!(agent.endpoint_name(), "other");
    assert_eq!(provider.sent().len(), 4);
}

#[tokio::test]
async fn esc_in_the_head_step_of_a_swap_before_a_turn_sends_nothing_more_and_cancels_it() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let precompute = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", 20_500),
        compaction(SUMMARY).gated(&precompute),
        answer("unused", 300),
    ]);
    let mut live = Live::start(notes.on(background(&provider, &store)).await);
    assert!(completed(&live.turn("u1", "first").await));
    let heads = notes.calls.sections_at(At::Head).len();
    notes.gate.arm(1);
    let canceller = notes.cancel_when_started(&live);
    live.send(send("u2", "second")).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    precompute.add_permits(1);
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    canceller.await.unwrap();
    let (_, seen) = live.close().await;
    assert!(
        matches!(
            ended,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Cancelled { .. },
                ..
            }
        ),
        "{ended:?}"
    );
    assert!(swapped_in(&seen, 2), "{seen:#?}");
    assert_eq!(notes.calls.sections_at(At::Head).len(), heads + 2);
    assert_eq!(logged_sections(&store), [Vec::new()]);
    assert_eq!(provider.sent().len(), 2);
}

fn fill_reading(seen: Arc<Mutex<Vec<bool>>>) -> Section {
    let inner = kit::changing(kit::knob("v1"));
    Arc::new(move |host, call| {
        if call.at == At::Head {
            seen.lock().unwrap().push(host.current_fill().auto_compact);
        }
        inner(host, call)
    })
}

#[tokio::test]
async fn a_head_render_at_compact_reads_what_changed_between_turns() {
    let store = MemoryStore::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let make: Make = kit::prompted(kit::none(), fill_reading(seen.clone()), Calls::default());
    let source = (
        kit::manifest_in("0.2.0", "bb", &kit::prompt_section("notes", "lead", "")),
        make,
    );
    let provider = scripted(vec![answer("Hello.", 1_000), compaction(SUMMARY)]);
    let agent = with_sources(logged(provider.clone(), &store), vec![source]).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let (mut agent, _) = live.close().await;
    agent.set_auto_compact(false);
    let mut live = Live::start(agent);
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    live.close().await;
    assert_eq!(*seen.lock().unwrap(), [true, true, false]);
}

fn fill_reading_at(seen: Arc<Mutex<Vec<(At, bool)>>>) -> Section {
    let inner = kit::changing(kit::knob("v1"));
    Arc::new(move |host, call| {
        seen.lock()
            .unwrap()
            .push((call.at, host.current_fill().auto_compact));
        inner(host, call)
    })
}

#[tokio::test]
async fn a_turns_first_refresh_render_reads_what_changed_between_turns() {
    let store = MemoryStore::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let make: Make = kit::prompted(kit::none(), fill_reading_at(seen.clone()), Calls::default());
    let source = (
        kit::manifest_in("0.2.0", "bb", &kit::prompt_section("notes", "lead", "")),
        make,
    );
    let provider = scripted(vec![answer("Hello.", 1_000), answer("Again.", 1_100)]);
    let agent = with_sources(logged(provider.clone(), &store), vec![source]).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let (mut agent, _) = live.close().await;
    let before = seen.lock().unwrap().len();
    agent.set_auto_compact(false);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    let after = seen.lock().unwrap().clone();
    assert_eq!(after[before], (At::Turn, false), "{after:?}");
}

#[tokio::test]
async fn a_resume_refreshes_the_sections_before_its_first_turn() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = scripted(vec![answer("Hello.", 1_000)]);
    let mut live = Live::start(notes.on(logged(provider.clone(), &store)).await);
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    let before = notes.calls.sections().len();
    let mut live = Live::start(resumed(&store, provider.clone(), &notes).await);
    live.send(ClientCommand::ContextReport {
        detail: ReportDetail::Summary,
        request: RequestId(5),
    })
    .await;
    live.until(|e| matches!(e, AgentEvent::ContextReport(_)))
        .await;
    let (_, seen) = live.close().await;
    let after = notes.calls.sections();
    let ats: Vec<At> = after[before..].iter().map(|c| c.at).collect();
    assert_eq!(ats, [At::Turn, At::Head], "{after:?}");
    assert_eq!(after[before].previous, Prior::Snapshot("v1".into()));
    assert!(
        !seen.iter().any(|e| matches!(e, AgentEvent::HookRan { .. })),
        "{seen:#?}"
    );
}

#[tokio::test]
async fn esc_in_a_resumes_refresh_cancels_the_render_and_the_session_goes_on() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let provider = scripted(vec![answer("Hello.", 1_000), answer("Again.", 300)]);
    let mut live = Live::start(notes.on(logged(provider.clone(), &store)).await);
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    notes.gate.arm(0);
    let mut live = Live::start(resumed(&store, provider.clone(), &notes).await);
    let canceller = notes.cancel_when_started(&live);
    let ran = live
        .until(|e| matches!(e, AgentEvent::HookRan { .. }))
        .await;
    canceller.await.unwrap();
    assert!(
        matches!(
            ran,
            AgentEvent::HookRan {
                outcome: wisp_protocol::HookOutcome::Cancelled,
                ..
            }
        ),
        "{ran:?}"
    );
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    assert_eq!(provider.sent().len(), 2);
}

async fn switch_workspace(live: &mut Live, key: &str, on: bool, request: u64) {
    live.send(crate::plugin::testing::plugin_switch(
        request,
        "workspace",
        key,
        on,
    ))
    .await;
    let done = live
        .until(|e| {
            matches!(
                e,
                AgentEvent::SettingsChanged { answers: Some(RequestId(r)), .. }
                    | AgentEvent::Rejected { request: Some(RequestId(r)), .. }
                    if *r == request
            )
        })
        .await;
    assert!(
        matches!(done, AgentEvent::SettingsChanged { .. }),
        "{done:?}"
    );
}

fn environments_in(body: &Value) -> usize {
    body["messages"]
        .to_string()
        .matches("<environment>")
        .count()
}

#[tokio::test]
async fn the_workspace_switched_off_sends_no_environment_until_on_and_then_the_whole_block_once() {
    use wisp_protocol::settings::ENABLED_KEY;
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        compaction(SUMMARY),
        answer("Three.", 300),
        answer("Four.", 400),
    ]);
    let mut live = Live::start(with_lines(logged(provider.clone(), &store), LINES));
    assert!(completed(&live.turn("u1", "first").await));
    let first = environments_in_last_message(&provider.body(0));
    assert_eq!(first.len(), 1, "{first:?}");
    switch_workspace(&mut live, ENABLED_KEY, false, 81).await;
    assert!(completed(&live.turn("u2", "second").await));
    assert!(environments_in_last_message(&provider.body(1)).is_empty());
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    assert!(completed(&live.turn("u3", "third").await));
    assert_eq!(
        environments_in(&provider.body(3)),
        0,
        "{}",
        provider.body(3)
    );
    switch_workspace(&mut live, ENABLED_KEY, true, 82).await;
    assert!(completed(&live.turn("u4", "fourth").await));
    live.close().await;
    let last = provider.body(4);
    assert_eq!(environments_in(&last), 1, "{last}");
    assert_eq!(environments_in_last_message(&last), first);
}

#[tokio::test]
async fn a_session_started_with_the_workspace_removed_sends_no_environment() {
    use wisp_protocol::settings::INSTALLED_KEY;
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        compaction(SUMMARY),
        answer("Two.", 300),
    ]);
    let layers = crate::settings::host::StartLayers {
        flags: vec![crate::plugin::testing::plugin_flag(
            "workspace",
            INSTALLED_KEY,
            false,
        )],
        ..Default::default()
    };
    let agent = with_lines(logged(provider.clone(), &store), LINES).with_start_layers(layers);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    for n in 0..3 {
        assert_eq!(
            environments_in(&provider.body(n)),
            0,
            "{}",
            provider.body(n)
        );
    }
}

fn head_hook(tokens: u64, calls: &Calls) -> (String, Make) {
    let text = "h".repeat(usize::try_from(tokens * 3).unwrap());
    let script = super::gate::replying(vec![(
        "head",
        wisp_plugin_api::hooks::Reply::Append(vec![text]),
    )]);
    let make: Make = kit::instantiate(script, calls.clone());
    let body = hook_body("head", "compaction-head", "");
    (kit::manifest("aa", &body), make)
}

async fn head_after(value: &str, tokens: u64) -> (Agent, MemoryStore) {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let calls = Calls::default();
    let provider = scripted(vec![answer("Hello.", 1_000), answer(WRITTEN, 1_200)]);
    let agent = logged(provider.clone(), &store).with_model_settings(models(WISP));
    let agent = with_sources(agent, vec![notes.source(), head_hook(tokens, &calls)]).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    notes.turn_to(value);
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    (live.close().await.0, store)
}

fn head_ran(store: &MemoryStore) -> Vec<wisp_protocol::HookOutcome> {
    super::gate::ran(store, "aa/head")
}

#[tokio::test]
async fn the_hook_texts_follow_the_sections_and_the_compacted_context() {
    let (agent, store) = head_after("v2", 100).await;
    let head = head_text(&agent);
    let section = head.find(&notes_text("v2")).unwrap();
    let context = head.find("<compacted_context").unwrap();
    let hooked = head.find(&"h".repeat(300)).unwrap();
    assert!(section < context && context < hooked, "{head}");
    assert_eq!(head_ran(&store), [wisp_protocol::HookOutcome::Applied]);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_section_whose_head_render_grew_shrinks_the_room_by_as_much() {
    let grown = "g".repeat(1_800);
    let (_, kept) = head_after("v2", 700).await;
    assert_eq!(head_ran(&kept), [wisp_protocol::HookOutcome::Applied]);
    let (_, refused) = head_after(&grown, 700).await;
    assert_eq!(
        head_ran(&refused),
        [wisp_protocol::HookOutcome::Rejected {
            rule: "not_smaller".into()
        }]
    );
}

#[tokio::test]
async fn a_track_stopped_before_a_compaction_asks_no_before_compact_hook() {
    let store = MemoryStore::default();
    let notes = Notes::new();
    let calls = Calls::default();
    let provider = scripted(vec![answer("Hello.", 5_000), compaction(SUMMARY)]);
    let agent = no_retries(logged(provider.clone(), &store).with_model_settings(models(LOW)));
    let make: Make = kit::instantiate(kit::none(), calls.clone());
    let gate = (
        kit::manifest("aa", &hook_body("gate", "before-compact", "")),
        make,
    );
    let mut live = Live::start(with_sources(agent, vec![notes.source(), gate]).await);
    assert!(completed(&live.turn("u1", "first").await));
    notes.gate.arm(0);
    let canceller = notes.cancel_when_started(&live);
    let outcome = live.turn("u2", "second").await;
    canceller.await.unwrap();
    live.close().await;
    assert!(cancelled(&outcome), "{outcome:?}");
    assert_eq!(provider.sent().len(), 1);
    assert_eq!(calls.count("gate"), 0);
}

fn terminal(cols: u16, rows: u16) -> ClientCommand {
    ClientCommand::Terminal {
        size: wisp_protocol::TermSize { cols, rows },
    }
}

const WIDE_LINE: &str = "Terminal: replies are 121 columns wide (wider code blocks wrap), 40 rows; don't mention this.\n";
const NARROW_LINE: &str = "Terminal: replies are 80 columns wide (wider code blocks wrap), 24 rows; don't mention this.\n";

fn terminal_lines(body: &Value) -> Vec<String> {
    let messages = body["messages"].as_array().unwrap();
    messages.last().unwrap()["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["text"].as_str())
        .filter(|t| t.starts_with("Terminal:"))
        .map(str::to_owned)
        .collect()
}

fn terminal_count(body: &Value) -> usize {
    body["messages"]
        .to_string()
        .matches("Terminal: replies are")
        .count()
}

#[tokio::test]
async fn a_resize_between_turns_appends_one_terminal_line_and_keeps_the_prefix() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        answer("Three.", 1_200),
    ]);
    let mut live = Live::start(with_lines(logged(provider.clone(), &store), LINES));
    live.send(terminal(100, 30)).await;
    live.send(terminal(121, 40)).await;
    assert!(completed(&live.turn("u1", "first").await));
    live.send(terminal(80, 24)).await;
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    assert_eq!(terminal_lines(&provider.body(0)), [WIDE_LINE]);
    assert_eq!(terminal_lines(&provider.body(1)), [NARROW_LINE]);
    assert!(terminal_lines(&provider.body(2)).is_empty());
    let first = provider.body(0);
    let first_text = first["messages"][0]["content"].to_string();
    let environment = first_text.find("<environment>").unwrap();
    let line = first_text.find("Terminal:").unwrap();
    let sent = first_text.find("Sent:").unwrap();
    assert!(environment < line && line < sent, "{first_text}");
    for n in 1..3 {
        let body = provider.body(n);
        let before = provider.body(n - 1);
        assert_eq!(body["system"], first["system"]);
        assert_eq!(body["tools"], first["tools"]);
        let sent = strip_cache_control(&before["messages"]);
        let now = strip_cache_control(&body["messages"]);
        let (sent, now) = (sent.as_array().unwrap(), now.as_array().unwrap());
        assert_eq!(
            now[..sent.len()],
            sent[..],
            "request {n} changed an earlier message"
        );
    }
}

#[tokio::test]
async fn a_zero_size_is_ignored_and_a_child_never_sees_the_terminal() {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("One.", 1_000), answer("Two.", 1_100)]);
    let mut live = Live::start(with_lines(logged(provider.clone(), &store), LINES));
    live.send(terminal(0, 40)).await;
    live.send(terminal(121, 0)).await;
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    assert!(terminal_lines(&provider.body(0)).is_empty());

    let store = MemoryStore::default();
    let child = scripted(vec![answer("One.", 1_000)]);
    let agent =
        with_lines(logged(child.clone(), &store), LINES).with_parent(super::hooks::child_link());
    let mut live = Live::start(agent);
    live.send(terminal(121, 40)).await;
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    assert_eq!(terminal_count(&child.body(0)), 0, "{}", child.body(0));
}

#[tokio::test]
async fn a_terminal_size_sent_to_a_child_is_refused() {
    let store = MemoryStore::default();
    let provider = scripted(Vec::new());
    let mut live = Live::start(logged(provider, &store));
    live.send(ClientCommand::Child {
        agent: wisp_protocol::AgentPath::new("/a"),
        command: Box::new(terminal(121, 40)),
    })
    .await;
    let refused = live
        .until(|e| matches!(e, AgentEvent::Rejected { .. }))
        .await;
    let AgentEvent::Rejected { reason, .. } = refused else {
        unreachable!()
    };
    assert_eq!(reason, "this is the session's; send it to main");
    live.close().await;
}

#[tokio::test]
async fn a_head_after_a_resize_carries_the_latest_size_once() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        compaction(SUMMARY),
        answer("Two.", 300),
        answer("Three.", 400),
    ]);
    let mut live = Live::start(with_lines(logged(provider.clone(), &store), LINES));
    live.send(terminal(121, 40)).await;
    assert!(completed(&live.turn("u1", "first").await));
    live.send(terminal(80, 24)).await;
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    let after = provider.body(2);
    assert_eq!(terminal_count(&after), 1, "{after}");
    assert!(
        after["messages"].to_string().contains("80 columns wide"),
        "{after}"
    );
    assert_eq!(terminal_count(&provider.body(3)), 1);
}

#[tokio::test]
async fn a_resume_and_a_rewind_restore_the_seen_size_without_sending_it_again() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        answer("Two again.", 1_100),
        answer("Three.", 1_200),
    ]);
    let mut live = Live::start(with_lines(logged(provider.clone(), &store), LINES));
    live.send(terminal(121, 40)).await;
    assert!(completed(&live.turn("u1", "first").await));
    live.send(terminal(80, 24)).await;
    assert!(completed(&live.turn("u2", "second").await));
    live.send(terminal(121, 40)).await;
    live.send(ClientCommand::Rewind {
        to: wisp_protocol::RewindPoint::Turn { turn: 2 },
        request: RequestId(7),
    })
    .await;
    let done = live
        .until(|e| matches!(e, AgentEvent::Rewound { .. } | AgentEvent::Rejected { .. }))
        .await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    assert!(completed(&live.turn("u2b", "second again").await));
    live.close().await;
    assert_eq!(terminal_lines(&provider.body(1)), [NARROW_LINE]);
    assert!(
        terminal_lines(&provider.body(2)).is_empty(),
        "{}",
        provider.body(2)
    );

    let mut live = Live::start(resumed_with(&store, provider.clone(), LINES));
    live.send(terminal(121, 40)).await;
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    assert!(
        terminal_lines(&provider.body(3)).is_empty(),
        "{}",
        provider.body(3)
    );
}

use wisp_protocol::log::LogRecord;
use wisp_protocol::{
    Dropped, Effort, NOTHING_TO_REWIND, PointInfo, REWIND_WHILE_COMPACTING, REWIND_WHILE_RUNNING,
    ReturnedInput, RewindPoint, Warmth, WithdrawnBy, not_a_point, rewound_away,
};

use super::*;
use crate::agent::LastRequest;
use crate::agent::tool_tests::rewind;
use crate::clock::StillClock;
use crate::test_guard::{BLUE_PIXEL as PIXEL, records};
use wisp_protocol::{CompactionNote, RewindQuote};

fn verdict(event: &AgentEvent, request: u64) -> bool {
    match event {
        AgentEvent::Rewound { answers, .. } => *answers == Some(RequestId(request)),
        AgentEvent::RewindPoints { answers, .. } => *answers == RequestId(request),
        other => answers(other, request),
    }
}

fn still(provider: Arc<Fake>, store: &MemoryStore) -> Agent {
    Agent::new(provider, default_session())
        .with_clock(Arc::new(StillClock))
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        )
}

pub(super) fn resumed(provider: Arc<Fake>, records: &[LogRecord]) -> Agent {
    let (_, log) = continued(records);
    Agent::new(provider, default_session())
        .with_clock(Arc::new(StillClock))
        .with_log(log, log_context())
        .resume(records, 0)
        .unwrap()
}

impl Live {
    async fn rewind(&mut self, request: u64, turn: u64) -> AgentEvent {
        self.send(rewind(request, turn)).await;
        self.until(|e| verdict(e, request)).await
    }

    async fn points(&mut self, request: u64) -> Vec<PointInfo> {
        self.send(ClientCommand::RewindPoints {
            request: RequestId(request),
        })
        .await;
        match self.until(|e| verdict(e, request)).await {
            AgentEvent::RewindPoints { points, .. } => points,
            other => panic!("{other:?}"),
        }
    }
}

fn swapped(body: &Value, from: &str, to: &str) -> String {
    without_context(body).to_string().replace(from, to)
}

fn refused(event: &AgentEvent) -> (RejectKind, String) {
    match event {
        AgentEvent::Rejected {
            kind: Some(kind),
            reason,
            ..
        } => (kind.clone(), reason.clone()),
        other => panic!("{other:?}"),
    }
}

fn returned_texts(event: &AgentEvent) -> Vec<(String, String)> {
    match event {
        AgentEvent::Rewound { returned, .. } => returned
            .iter()
            .map(|r| (r.id.as_str().to_owned(), r.text.clone()))
            .collect(),
        other => panic!("{other:?}"),
    }
}

fn answers_n(words: &[&str]) -> Arc<Fake> {
    scripted(
        words
            .iter()
            .enumerate()
            .map(|(n, w)| answer(w, 1_000 + 100 * n as u64))
            .collect(),
    )
}

fn without_context(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| match v {
                    Value::String(text) if k == "text" && text.starts_with("Context: ") => {
                        (k.clone(), Value::String("Context".into()))
                    }
                    other => (k.clone(), without_context(other)),
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_context).collect()),
        other => other.clone(),
    }
}

#[tokio::test]
async fn a_rewind_drops_the_later_turns_and_resends_the_points_prefix_byte_for_byte() {
    let store = MemoryStore::default();
    let provider = answers_n(&["One.", "Two.", "Three.", "Again."]);
    let mut live = Live::start(still(provider.clone(), &store));
    three_turns(&mut live).await;
    let done = live.rewind(9, 2).await;
    let AgentEvent::Rewound {
        point,
        dropped,
        returned,
        cache,
        ..
    } = &done
    else {
        panic!("{done:?}");
    };
    assert_eq!(*point, RewindPoint::Turn { turn: 2 });
    assert_eq!(
        *dropped,
        Dropped {
            turns: vec![2, 3],
            messages: 4,
        }
    );
    assert_eq!(
        *returned,
        [ReturnedInput {
            id: InputId("u2".into()),
            text: "second".into(),
            images: Vec::new(),
            missing: Vec::new(),
        }]
    );
    assert_eq!(*cache, CacheEffect::Kept);
    assert!(completed(&live.turn("u4", "second, edited").await));
    let (agent, seen) = live.close().await;
    let turns: Vec<u64> = pick!(&seen, AgentEvent::TurnStarted { turn, .. } => *turn);
    assert_eq!(turns, [1, 2, 3, 4]);
    assert_eq!(
        swapped(&provider.body(3), "second, edited", "second"),
        without_context(&provider.body(1)).to_string()
    );
    assert_eq!(agent.history().len(), 4);
    assert_eq!(rebuilt(&store), &*agent.history());
    assert!(log_events(&store).iter().any(|e| matches!(
        e,
        LogEvent::Rewound {
            point: RewindPoint::Turn { turn: 2 },
            messages: 2,
            ..
        }
    )));
}

#[tokio::test]
async fn a_rewind_to_the_last_turn_drops_only_that_turn() {
    let store = MemoryStore::default();
    let provider = answers_n(&["One.", "Two.", "Three.", "Again."]);
    let mut live = Live::start(still(provider.clone(), &store));
    three_turns(&mut live).await;
    let done = live.rewind(9, 3).await;
    assert!(matches!(
        &done,
        AgentEvent::Rewound { dropped, .. } if dropped.turns == [3] && dropped.messages == 2
    ));
    assert_eq!(returned_texts(&done), [("u3".into(), "third".into())]);
    assert!(completed(&live.turn("u4", "third").await));
    let (agent, _) = live.close().await;
    assert_eq!(provider.body(3), provider.body(2));
    assert_eq!(rebuilt(&store), &*agent.history());
}

#[tokio::test]
async fn a_rewind_to_turn_one_sends_what_turn_one_sent() {
    let store = MemoryStore::default();
    let provider = answers_n(&["One.", "Two.", "Again."]);
    let mut live = Live::start(still(provider.clone(), &store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let done = live.rewind(9, 1).await;
    assert_eq!(returned_texts(&done), [("u1".into(), "first".into())]);
    assert!(completed(&live.turn("u3", "first").await));
    let (agent, _) = live.close().await;
    assert_eq!(provider.body(2), provider.body(0));
    assert_eq!(agent.history().len(), 2);
    assert_eq!(rebuilt(&store), &*agent.history());
}

#[tokio::test]
async fn refusals_name_their_kind_and_change_nothing() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        answer("Three.", 1_200),
        Step::hang(),
    ]);
    let mut live = Live::start(still(provider.clone(), &store));
    assert_eq!(
        refused(&live.rewind(1, 1).await),
        (RejectKind::NotRewindable, NOTHING_TO_REWIND.to_owned())
    );
    three_turns(&mut live).await;
    assert_eq!(
        refused(&live.rewind(2, 9).await),
        (RejectKind::NotRewindable, not_a_point(9))
    );
    assert!(matches!(
        live.rewind(3, 2).await,
        AgentEvent::Rewound { .. }
    ));
    assert_eq!(
        refused(&live.rewind(4, 3).await),
        (RejectKind::NotRewindable, rewound_away(3))
    );
    live.send(send("u4", "again")).await;
    live.until(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .await;
    assert_eq!(
        refused(&live.rewind(5, 1).await),
        (RejectKind::TurnRunning, REWIND_WHILE_RUNNING.to_owned())
    );
    live.send(ClientCommand::Cancel).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (agent, seen) = live.close().await;
    assert_eq!(agent.history().len(), 2);
    let rewound = seen
        .iter()
        .filter(|e| matches!(e, AgentEvent::Rewound { .. }))
        .count();
    assert_eq!(rewound, 1);
    assert_eq!(rebuilt(&store), &*agent.history());
}

#[tokio::test]
async fn the_points_list_each_turn_with_its_label_and_a_warm_quote() {
    let provider = answers_n(&["One.", "Two."]);
    let mut live = Live::start(still(provider, &MemoryStore::default()));
    assert!(live.points(1).await.is_empty());
    assert!(completed(&live.turn("u1", "first\nmore").await));
    assert!(completed(&live.turn("u2", "\n  second  ").await));
    let points = live.points(2).await;
    let shown: Vec<(RewindPoint, &str, &Warmth)> = points
        .iter()
        .map(|p| (p.point, p.label.as_str(), &p.quote.warm))
        .collect();
    assert_eq!(
        shown,
        [
            (
                RewindPoint::Turn { turn: 1 },
                "first",
                &Warmth::PrefixChanged
            ),
            (RewindPoint::Turn { turn: 2 }, "second", &Warmth::Warm),
        ]
    );
    assert_eq!(points[1].inputs_preview, ["second"]);
    assert!(points[1].quote.prefix_tokens > 0);
    live.close().await;
}

#[tokio::test]
async fn a_settings_change_sent_before_a_rewind_applies_first() {
    let provider = answers_n(&["One.", "Two."]);
    let mut live = Live::start(still(provider, &MemoryStore::default()));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.send(ClientCommand::shortcut(
        RequestId(1),
        None,
        None,
        Some(Effort::Max),
    ))
    .await;
    live.rewind(2, 2).await;
    let (agent, seen) = live.close().await;
    let changed = seen
        .iter()
        .position(|e| matches!(e, AgentEvent::SettingsChanged { .. }))
        .unwrap();
    let rewound = seen
        .iter()
        .position(|e| matches!(e, AgentEvent::Rewound { .. }))
        .unwrap();
    assert!(changed < rewound);
    assert_eq!(agent.history().len(), 2);
}

#[tokio::test]
async fn held_messages_are_withdrawn_by_the_rewind_and_returned_after_the_points_own() {
    let provider = scripted(vec![answer("One.", 1_000), Step::hang()]);
    let mut live = Live::start(still(provider, &MemoryStore::default()));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .await;
    live.send(ClientCommand::queue("u3", "held one")).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    live.send(ClientCommand::Cancel).await;
    live.until(|e| matches!(e, AgentEvent::InputHeld { .. }))
        .await;
    let done = live.rewind(1, 1).await;
    assert_eq!(
        returned_texts(&done),
        [
            ("u1".into(), "first".into()),
            ("u3".into(), "held one".into())
        ]
    );
    let (agent, seen) = live.close().await;
    let withdrawn = seen
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::InputWithdrawn { ids, by: WithdrawnBy::Rewound }
                    if ids == &[InputId("u3".into())]
            )
        })
        .unwrap();
    let rewound = seen
        .iter()
        .position(|e| matches!(e, AgentEvent::Rewound { .. }))
        .unwrap();
    assert!(withdrawn < rewound);
    assert!(agent.history().is_empty());
}

#[tokio::test]
async fn a_rewind_during_a_compaction_is_refused_and_changes_nothing() {
    let gate = Arc::new(Semaphore::new(0));
    let provider = scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        Step::from(compaction_events(SUMMARY)).gated(&gate),
    ]);
    let mut live = Live::start(still(provider, &MemoryStore::default()));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.send(compact(1, None)).await;
    live.until(|e| matches!(e, AgentEvent::CompactionStarted { .. }))
        .await;
    assert_eq!(
        refused(&live.rewind(2, 1).await),
        (RejectKind::Compacting, REWIND_WHILE_COMPACTING.to_owned())
    );
    gate.add_permits(1);
    live.until(|e| answers(e, 1)).await;
    let (agent, seen) = live.close().await;
    assert!(!seen.iter().any(|e| matches!(e, AgentEvent::Rewound { .. })));
    assert_eq!(agent.history().len(), 4);
}

async fn resumed_matches_live(rewinds: &[u64], words: &[&str]) {
    let store = MemoryStore::default();
    let mut steps: Vec<&str> = words.to_vec();
    steps.push("Again.");
    let provider = answers_n(&steps);
    let mut live = Live::start(still(provider.clone(), &store));
    for (n, text) in words.iter().enumerate() {
        let id = format!("u{}", n + 1);
        assert!(completed(&live.turn(&id, text).await));
    }
    for (n, turn) in rewinds.iter().enumerate() {
        let done = live.rewind(10 + n as u64, *turn).await;
        assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    }
    let records = records(&store);
    assert!(completed(&live.turn("u9", "after").await));
    let (agent, _) = live.close().await;

    let again = answers_n(&["Again."]);
    let mut back = Live::start(resumed(again.clone(), &records));
    assert!(completed(&back.turn("u9", "after").await));
    let (resumed_agent, _) = back.close().await;
    let last = provider.sent().len() - 1;
    assert_eq!(again.body(0), provider.body(last));
    assert_eq!(resumed_agent.history(), agent.history());
}

#[tokio::test]
async fn a_resumed_rewind_sends_what_the_live_session_sent() {
    resumed_matches_live(&[2], &["first", "second", "third"]).await;
}

#[tokio::test]
async fn a_resumed_session_after_two_rewinds_the_second_further_back_sends_what_live_sent() {
    resumed_matches_live(&[3, 2], &["first", "second", "third", "fourth"]).await;
}

#[tokio::test]
async fn a_resumed_session_keeps_the_points_before_the_rewind() {
    let store = MemoryStore::default();
    let provider = answers_n(&["One.", "Two.", "Three."]);
    let mut live = Live::start(still(provider, &store));
    three_turns(&mut live).await;
    live.rewind(1, 3).await;
    live.close().await;
    let records = records(&store);
    let mut back = Live::start(resumed(answers_n(&[]), &records));
    let points: Vec<RewindPoint> = back.points(2).await.iter().map(|p| p.point).collect();
    assert_eq!(
        points,
        [RewindPoint::Turn { turn: 1 }, RewindPoint::Turn { turn: 2 }]
    );
    assert_eq!(
        refused(&back.rewind(3, 3).await),
        (RejectKind::NotRewindable, rewound_away(3))
    );
    back.close().await;
}

async fn compacted_then_rewound(point: u64, original: usize) {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        compaction(SUMMARY),
        answer("Three.", 300),
        answer("Four.", 400),
        answer("Again.", 500),
        answer("Resumed.", 500),
    ]);
    let mut live = Live::start(still(provider.clone(), &store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    assert!(matches!(
        live.compact(1, None).await,
        AgentEvent::Compacted { .. }
    ));
    assert!(completed(&live.turn("u3", "third").await));
    assert!(completed(&live.turn("u4", "fourth").await));
    let texts = ["first", "second", "third", "fourth"];
    let text = texts[usize::try_from(point).unwrap() - 1];
    let done = live.rewind(2, point).await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    let records = records(&store);
    let edited = format!("{text}, edited");
    assert!(completed(&live.turn("u5", &edited).await));
    let (agent, _) = live.close().await;
    let after = provider.sent().len() - 1;
    assert_eq!(
        swapped(&provider.body(after), &edited, text),
        without_context(&provider.body(original)).to_string()
    );
    assert_eq!(rebuilt(&store), &*agent.history());

    let again = scripted(vec![answer("Again.", 500)]);
    let mut back = Live::start(resumed(again.clone(), &records));
    assert!(completed(&back.turn("u5", &edited).await));
    back.close().await;
    assert_eq!(again.body(0), provider.body(after));
}

fn compaction_events(summary: &str) -> Script {
    compaction(summary).script
}

#[tokio::test]
async fn a_rewind_after_a_compaction_keeps_the_head() {
    compacted_then_rewound(4, 4).await;
}

#[tokio::test]
async fn a_rewind_to_before_a_compaction_resends_the_uncompacted_prefix() {
    compacted_then_rewound(2, 1).await;
}

#[tokio::test]
async fn a_point_before_a_compaction_names_the_compaction() {
    let provider = scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        compaction(SUMMARY),
        answer("Three.", 300),
    ]);
    let mut live = Live::start(still(provider, &MemoryStore::default()));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.compact(1, None).await;
    assert!(completed(&live.turn("u3", "third").await));
    let points = live.points(2).await;
    let notes: Vec<Option<u64>> = points
        .iter()
        .map(|p| p.compaction.map(|c| c.turn))
        .collect();
    assert_eq!(notes, [Some(3), Some(3), None]);
    live.close().await;
}

#[tokio::test]
async fn rewinding_twice_resends_each_points_prefix() {
    let store = MemoryStore::default();
    let provider = answers_n(&["One.", "Two.", "Three.", "Three b.", "Again."]);
    let mut live = Live::start(still(provider.clone(), &store));
    three_turns(&mut live).await;
    live.rewind(1, 3).await;
    assert!(completed(&live.turn("u4", "third").await));
    assert_eq!(provider.body(3), provider.body(2));
    live.rewind(2, 1).await;
    assert!(completed(&live.turn("u5", "first").await));
    let (agent, _) = live.close().await;
    assert_eq!(provider.body(4), provider.body(0));
    assert_eq!(rebuilt(&store), &*agent.history());
}

#[tokio::test]
async fn a_model_switch_after_the_point_quotes_the_other_model_and_rebuilds() {
    let provider = answers_n(&["One.", "Two."]);
    let mut live = Live::start(still(provider, &MemoryStore::default()));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.send(ClientCommand::shortcut(
        RequestId(1),
        None,
        Some("claude-sonnet-5-5".into()),
        None,
    ))
    .await;
    live.until(|e| matches!(e, AgentEvent::SettingsChanged { .. }))
        .await;
    let points = live.points(2).await;
    assert_eq!(
        points[1].quote.warm,
        Warmth::OtherModel {
            model: DEFAULT_MODEL.into()
        }
    );
    let done = live.rewind(3, 2).await;
    assert!(matches!(
        done,
        AgentEvent::Rewound {
            cache: CacheEffect::Rebuilt {
                reason: RebuildReason::ModelSwitch
            },
            ..
        }
    ));
    live.close().await;
}

#[tokio::test]
async fn an_effort_raised_after_the_point_adds_its_marker_after_the_new_message() {
    let provider = answers_n(&["One.", "Two.", "Again."]);
    let mut live = Live::start(still(provider.clone(), &MemoryStore::default()));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.send(ClientCommand::shortcut(
        RequestId(1),
        None,
        None,
        Some(Effort::High),
    ))
    .await;
    live.until(|e| matches!(e, AgentEvent::SettingsChanged { .. }))
        .await;
    live.rewind(2, 2).await;
    assert!(completed(&live.turn("u3", "second").await));
    live.close().await;
    let before = without_context(&provider.body(1));
    let after = without_context(&provider.body(2));
    let kept = before["messages"].as_array().unwrap();
    let sent = after["messages"].as_array().unwrap();
    assert_eq!(sent.len(), kept.len() + 1, "{after}");
    assert_eq!(
        strip_cache_control(&Value::Array(sent[..kept.len()].to_vec())),
        strip_cache_control(&Value::Array(kept.clone()))
    );
    assert_eq!(sent[kept.len()]["role"], "system");
}

fn with_image(id: &str, text: &str) -> ClientCommand {
    ClientCommand::Send {
        id: InputId(id.into()),
        text: text.into(),
        images: vec![wisp_protocol::ImageData { data: PIXEL.into() }],
        mode: DeliveryMode::Steer,
        turn: None,
        replaces: None,
    }
}

fn holds_image(messages: &[Message]) -> bool {
    messages
        .iter()
        .flat_map(|m| &m.content)
        .any(|b| matches!(b, ContentBlock::Image { .. }))
}

#[tokio::test]
async fn a_rewind_after_a_withdrawn_image_keeps_the_note_and_resume_agrees() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        super::marker::image_refused(),
        answer("One.", 1_000),
        answer("Two.", 1_100),
        answer("Again.", 1_200),
    ]);
    let mut live = Live::start(still(provider.clone(), &store));
    live.send(with_image("u1", "look")).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(completed(&live.turn("u2", "second").await));
    live.rewind(1, 2).await;
    let records = records(&store);
    assert!(completed(&live.turn("u3", "second").await));
    let (agent, _) = live.close().await;
    assert!(!holds_image(&agent.history()));
    assert_eq!(provider.body(3), provider.body(2));
    assert_eq!(rebuilt(&store), &*agent.history());

    let again = scripted(vec![answer("Again.", 1_200)]);
    let mut back = Live::start(resumed(again.clone(), &records));
    assert!(completed(&back.turn("u3", "second").await));
    back.close().await;
    assert_eq!(again.body(0), provider.body(3));
}

#[tokio::test]
async fn going_back_to_a_turn_whose_image_was_withdrawn_returns_the_text_without_it() {
    let provider = scripted(vec![super::marker::image_refused(), answer("One.", 1_000)]);
    let mut live = Live::start(still(provider, &MemoryStore::default()));
    live.send(with_image("u1", "look")).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let done = live.rewind(1, 1).await;
    let AgentEvent::Rewound { returned, .. } = &done else {
        panic!("{done:?}");
    };
    assert_eq!(returned.len(), 1);
    assert_eq!(returned[0].text, "look");
    assert!(returned[0].images.is_empty());
    assert_eq!(returned[0].missing.len(), 1);
    live.close().await;
}

#[tokio::test]
async fn going_back_to_a_turn_returns_its_pasted_image() {
    let provider = scripted(vec![answer("One.", 1_000)]);
    let mut live = Live::start(still(provider, &MemoryStore::default()));
    live.send(with_image("u1", "look")).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let done = live.rewind(1, 1).await;
    let AgentEvent::Rewound { returned, .. } = &done else {
        panic!("{done:?}");
    };
    assert_eq!(returned[0].images.len(), 1);
    assert!(returned[0].missing.is_empty());
    live.close().await;
}

#[tokio::test]
async fn going_back_to_a_queued_turn_returns_its_pasted_image() {
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let provider = scripted(vec![
        answer("One.", 1_000).gated(&gate),
        answer("Two.", 1_100),
    ]);
    let mut live = Live::start(still(provider, &store));
    live.send(send("u1", "first")).await;
    live.until(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .await;
    let mut queued = with_image("u2", "look");
    if let ClientCommand::Send { mode, .. } = &mut queued {
        *mode = DeliveryMode::Queue;
    }
    live.send(queued).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    gate.add_permits(1);
    for _ in 0..2 {
        live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await;
    }
    let records = records(&store);
    assert!(records.iter().any(|r| matches!(
        &r.event,
        LogEvent::TurnStarted { turn: 2, inputs, .. } if inputs[0].note.is_some()
    )));
    let returned = |done: &AgentEvent| match done {
        AgentEvent::Rewound { returned, .. } => returned
            .iter()
            .map(|r| (r.text.clone(), r.images.len(), r.missing.len()))
            .collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    let image_back = [("look".to_owned(), 1, 0)];
    assert_eq!(returned(&live.rewind(1, 2).await), image_back);
    live.close().await;

    let mut copy = store.sharing_blobs();
    for record in &records {
        copy.append("s1", record).unwrap();
    }
    let seq = u64::try_from(records.len()).unwrap();
    let agent = Agent::new(scripted(Vec::new()), default_session())
        .with_clock(Arc::new(StillClock))
        .with_log(
            SessionLog::continuing(Box::new(copy), "s1", seq),
            log_context(),
        )
        .resume(&records, 0)
        .unwrap();
    let mut back = Live::start(agent);
    assert_eq!(returned(&back.rewind(1, 2).await), image_back);
    back.close().await;
}

fn points_view(
    points: &[PointInfo],
) -> Vec<(RewindPoint, String, RewindQuote, Option<CompactionNote>)> {
    points
        .iter()
        .map(|p| (p.point, p.label.clone(), p.quote.clone(), p.compaction))
        .collect()
}

#[tokio::test]
async fn a_resumed_session_keeps_the_baseline_of_a_point_after_a_rewind() {
    let store = MemoryStore::default();
    let provider = answers_n(&["One.", "Two.", "Three.", "Two b.", "Four.", "Four b."]);
    let mut live = Live::start(still(provider.clone(), &store));
    three_turns(&mut live).await;
    assert!(matches!(
        live.rewind(9, 2).await,
        AgentEvent::Rewound { .. }
    ));
    assert!(completed(&live.turn("u4", "second b").await));
    assert!(completed(&live.turn("u5", "fourth").await));
    let records = records(&store);
    let live_points = live.points(20).await;
    let live_done = live.rewind(10, 5).await;
    assert!(
        matches!(live_done, AgentEvent::Rewound { .. }),
        "{live_done:?}"
    );
    assert!(completed(&live.turn("u6", "fourth b").await));
    let (agent, _) = live.close().await;
    let last = provider.sent().len() - 1;

    let again = answers_n(&["Four b."]);
    let mut back = Live::start(resumed(again.clone(), &records));
    let back_points = back.points(20).await;
    assert_eq!(points_view(&back_points), points_view(&live_points));
    let back_done = back.rewind(11, 5).await;
    let strip = |e: &AgentEvent| match e {
        AgentEvent::Rewound {
            point,
            dropped,
            returned,
            cache,
            ..
        } => {
            format!("{point:?} {dropped:?} {returned:?} {cache:?}")
        }
        other => format!("{other:?}"),
    };
    assert_eq!(strip(&back_done), strip(&live_done));
    assert!(completed(&back.turn("u6", "fourth b").await));
    let (resumed_agent, _) = back.close().await;
    assert_eq!(again.body(0), provider.body(last));
    assert_eq!(resumed_agent.history(), agent.history());
}

#[tokio::test]
async fn a_resumed_rewind_never_resends_a_held_message() {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("One.", 1_000), Step::hang()]);
    let mut live = Live::start(still(provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .await;
    live.send(ClientCommand::queue("u3", "held one")).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    live.send(ClientCommand::Cancel).await;
    live.until(|e| matches!(e, AgentEvent::InputHeld { .. }))
        .await;
    let done = live.rewind(1, 1).await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    live.close().await;
    let records = records(&store);
    let again = answers_n(&["Again."]);
    let mut back = Live::start(resumed(again.clone(), &records));
    assert!(completed(&back.turn("u4", "first").await));
    let (agent, _) = back.close().await;
    let body = again.body(0).to_string();
    assert!(!body.contains("held one"), "{body}");
    assert_eq!(agent.history().len(), 2);
}

#[tokio::test]
async fn live_and_resumed_quotes_agree_before_any_rewind() {
    let store = MemoryStore::default();
    let provider = answers_n(&["One.", "Two.", "Three."]);
    let mut live = Live::start(still(provider, &store));
    three_turns(&mut live).await;
    let live_points = live.points(5).await;
    live.close().await;
    let records = records(&store);
    let mut back = Live::start(resumed(answers_n(&[]), &records));
    let back_points = back.points(6).await;
    back.close().await;
    assert_eq!(points_view(&back_points), points_view(&live_points));
}

#[tokio::test]
async fn a_compaction_after_the_last_turn_is_named_by_the_turn_it_runs_before() {
    let provider = scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        compaction(SUMMARY),
    ]);
    let mut live = Live::start(still(provider, &MemoryStore::default()));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.compact(1, None).await;
    let points = live.points(2).await;
    let notes: Vec<Option<u64>> = points
        .iter()
        .map(|p| p.compaction.map(|c| c.turn))
        .collect();
    live.close().await;
    assert_eq!(notes, [Some(3), Some(3)]);
}

fn request_view(last: Option<&LastRequest>) -> Option<String> {
    last.map(|l| {
        let unprinted = LastRequest {
            print: None,
            ..l.clone()
        };
        format!("{unprinted:?}")
    })
}

fn baseline_of(agent: &Agent) -> Option<String> {
    request_view(agent.last.as_ref())
}

type Marked = (u64, Option<String>, Option<String>, Option<jiff::Timestamp>);

fn marked(agent: &Agent, turns: &[u64]) -> Vec<Marked> {
    let rewind = &agent.rewind;
    rewind
        .marks
        .iter()
        .filter(|m| turns.contains(&m.turn))
        .map(|m| {
            let head = m.head.map(|i| format!("{:?}", rewind.heads[i]));
            (
                m.turn,
                head,
                request_view(m.baseline.as_ref()),
                m.warm_until,
            )
        })
        .collect()
}

fn at_end(agent: &Agent) -> (Option<String>, Option<String>) {
    let head = agent.compaction.head.as_ref().map(|h| format!("{h:?}"));
    (head, baseline_of(agent))
}

pub(super) fn assert_points_resume_as_live(
    live: &Agent,
    records: &[LogRecord],
    turns: &[u64],
    resume: impl Fn(&[LogRecord]) -> Agent,
) {
    let back = resume(records);
    assert_eq!(marked(live, turns).len(), turns.len());
    assert_eq!(marked(&back, turns), marked(live, turns));
    assert_eq!(at_end(&back), at_end(live));
    assert_cuts_resume_as_marked(&back, records, turns, resume);
}

pub(super) fn started_at(records: &[LogRecord], turn: u64) -> usize {
    records
        .iter()
        .position(|r| matches!(r.event, LogEvent::TurnStarted { turn: t, .. } if t == turn))
        .unwrap()
}

pub(super) fn assert_cuts_resume_as_marked(
    full: &Agent,
    records: &[LogRecord],
    turns: &[u64],
    resume: impl Fn(&[LogRecord]) -> Agent,
) {
    for (turn, head, baseline, _) in marked(full, turns) {
        let cut = started_at(records, turn);
        let back = resume(&records[..cut]);
        assert_eq!(at_end(&back), (head, baseline), "point {turn}");
    }
}

fn cached_reads() -> Arc<Fake> {
    scripted(vec![
        answer_read("One.", 100, 900),
        answer_read("Two.", 700, 300),
        answer_read("Three.", 50, 950),
    ])
}

#[tokio::test]
async fn a_warm_rewind_keeps_the_cached_share_live_and_on_resume() {
    let store = MemoryStore::default();
    let mut live = Live::start(still(cached_reads(), &store));
    three_turns(&mut live).await;
    assert!(matches!(
        live.rewind(2, 2).await,
        AgentEvent::Rewound { .. }
    ));
    let (agent, _) = live.close().await;
    let records = records(&store);
    let back = resumed(answers_n(&[]), &records);
    assert_eq!(agent.cached(), Some(90));
    assert_eq!(back.cached(), agent.cached());
}

#[tokio::test]
async fn a_warm_rewind_of_a_resumed_session_keeps_the_cached_share() {
    let store = MemoryStore::default();
    let mut live = Live::start(still(cached_reads(), &store));
    three_turns(&mut live).await;
    let (agent, _) = live.close().await;
    let records = records(&store);
    let back = resumed(answers_n(&[]), &records);
    assert_eq!(agent.cached(), Some(95));
    assert_eq!(back.cached(), Some(95));
    let mut back = Live::start(back);
    assert!(matches!(
        back.rewind(2, 2).await,
        AgentEvent::Rewound { .. }
    ));
    let (agent, _) = back.close().await;
    assert_eq!(agent.cached(), Some(90));
}

async fn rewound_baselines(switch: bool) -> (Option<String>, Agent) {
    let store = MemoryStore::default();
    let mut live = Live::start(still(answers_n(&["One.", "Two.", "Three."]), &store));
    three_turns(&mut live).await;
    if switch {
        live.send(ClientCommand::shortcut(
            RequestId(1),
            None,
            Some("claude-sonnet-5-5".into()),
            None,
        ))
        .await;
        live.until(|e| matches!(e, AgentEvent::SettingsChanged { .. }))
            .await;
    }
    assert!(matches!(
        live.rewind(2, 2).await,
        AgentEvent::Rewound { .. }
    ));
    let (agent, _) = live.close().await;
    let records = records(&store);
    (baseline_of(&agent), resumed(answers_n(&[]), &records))
}

#[tokio::test]
async fn a_warm_rewind_keeps_the_points_baseline_live_and_on_resume() {
    let (live, back) = rewound_baselines(false).await;
    assert!(live.is_some());
    assert_eq!(baseline_of(&back), live);
}

#[tokio::test]
async fn a_cold_rewind_drops_the_baseline_live_and_on_resume() {
    let (live, back) = rewound_baselines(true).await;
    assert_eq!(live, None);
    assert_eq!(baseline_of(&back), None);
}

async fn three_closed() -> Agent {
    let mut live = Live::start(still(
        answers_n(&["One.", "Two.", "Three."]),
        &MemoryStore::default(),
    ));
    three_turns(&mut live).await;
    live.close().await.0
}

fn opening(turn: u64) -> crate::agent::rewind::PointStart<'static> {
    crate::agent::rewind::PointStart {
        turn,
        inputs: &[],
        lead: 0,
        origin: None,
        retry_of: None,
        seq: None,
        at: String::new(),
        failed_tail: None,
        arrival: None,
    }
}

fn mark_turns(agent: &Agent) -> Vec<u64> {
    agent.rewind.marks.iter().map(|m| m.turn).collect()
}

#[tokio::test]
async fn a_point_that_added_nothing_is_replaced_by_the_next() {
    let mut agent = three_closed().await;
    agent.mark_point(opening(4));
    agent.mark_point(opening(5));
    assert_eq!(mark_turns(&agent), [1, 2, 3, 5]);
}

#[tokio::test]
async fn a_point_after_a_byte_change_has_no_baseline() {
    let mut agent = three_closed().await;
    agent.mark_point(opening(4));
    assert!(agent.rewind.marks[3].baseline.is_some());
    agent.rewind.marks.pop();
    agent
        .epoch
        .rebuilt(crate::agent::epoch::MarkReason::ToolsChange);
    agent.mark_point(opening(4));
    assert!(agent.rewind.marks[3].baseline.is_none());
}

#[tokio::test]
async fn a_cut_into_a_points_messages_makes_its_quote_a_changed_prefix() {
    let mut agent = three_closed().await;
    let mark = agent.rewind.marks[1].clone();
    assert_eq!(agent.warmth(&mark).0, Warmth::Warm);
    agent.search.cuts.push(wisp_protocol::log::Cut {
        from: wisp_protocol::log::BlockPosition {
            message: 0,
            block: 0,
        },
        through: 1,
        tools: false,
    });
    assert_eq!(agent.warmth(&mark).0, Warmth::PrefixChanged);
}

#[tokio::test]
async fn restoring_a_point_restores_its_epoch_its_accepted_end_and_its_baseline() {
    let mut agent = three_closed().await;
    let mark = agent.rewind.marks[1].clone();
    agent.restore_point(&mark, false);
    assert_eq!(agent.epoch.at(), mark.epoch);
    assert_eq!(
        agent.accepted,
        crate::agent::withdraw::end_of(&agent.history())
    );
    assert!(agent.last.is_none());
    agent.mark_point(opening(2));
    let again = agent.rewind.marks.last().unwrap();
    assert!(mark.baseline.is_some());
    assert_eq!(
        again.baseline.as_ref().map(|b| (b.turn, b.prompt_tokens)),
        mark.baseline.as_ref().map(|b| (b.turn, b.prompt_tokens))
    );
}

fn last_epoch(store: &MemoryStore) -> u64 {
    log_events(store)
        .iter()
        .rev()
        .find_map(|e| match e {
            LogEvent::ContextCompacted { epoch, .. } => Some(*epoch),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn a_compaction_after_rewinding_past_one_takes_a_new_epoch_live_and_resumed() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        compaction(SUMMARY),
        answer("Three.", 300),
        compaction(SUMMARY),
    ]);
    let mut live = Live::start(still(provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    assert!(matches!(
        live.compact(1, None).await,
        AgentEvent::Compacted { .. }
    ));
    assert_eq!(last_epoch(&store), 1);
    assert!(completed(&live.turn("u3", "third").await));
    assert!(matches!(
        live.rewind(2, 2).await,
        AgentEvent::Rewound { .. }
    ));
    let records = records(&store);
    assert!(matches!(
        live.compact(3, None).await,
        AgentEvent::Compacted { .. }
    ));
    live.close().await;
    assert_eq!(last_epoch(&store), 2);

    let (again, log) = continued(&records);
    let agent = Agent::new(scripted(vec![compaction(SUMMARY)]), default_session())
        .with_clock(Arc::new(StillClock))
        .with_log(log, log_context())
        .resume(&records, 0)
        .unwrap();
    let mut back = Live::start(agent);
    assert!(matches!(
        back.compact(4, None).await,
        AgentEvent::Compacted { .. }
    ));
    back.close().await;
    assert_eq!(last_epoch(&again), 2);
}

#[tokio::test]
async fn a_process_heard_only_in_the_turns_left_is_told_again_on_resume() {
    let mut agent = three_closed().await;
    let started: LogEvent = serde_json::from_value(json!({
        "type": "process_started",
        "turn": 1,
        "id": 1,
        "call_id": "c1",
        "reason": "started",
        "tty": false,
        "stdin": false,
    }))
    .unwrap();
    let heard = LogEvent::ProcessHeard {
        turn: 2,
        ids: vec![1],
        typed: Vec::new(),
    };
    let records: Vec<LogRecord> = [started, heard]
        .into_iter()
        .zip(0..)
        .map(|(event, seq)| LogRecord {
            schema_version: wisp_protocol::log::LOG_SCHEMA_VERSION,
            seq,
            at: "2026-10-25T12:00:00Z".into(),
            event,
        })
        .collect();
    agent.restore_processes((&records, &[false, false]), &[], 0);
    assert_eq!(agent.news(false).0, None);
    agent.restore_processes((&records, &[false, true]), &[], 0);
    let notice = wisp_plugin_shell::notice(&agent.news(false).0.unwrap()).unwrap();
    assert!(notice.contains("Process 1 was still running"), "{notice}");
}

fn three_images(id: &str) -> ClientCommand {
    let image = || wisp_protocol::ImageData { data: PIXEL.into() };
    ClientCommand::Send {
        id: InputId(id.into()),
        text: "look".into(),
        images: vec![image(), image(), image()],
        mode: DeliveryMode::Steer,
        turn: None,
        replaces: None,
    }
}

fn without_second_image(agent: &mut Agent) {
    let second = agent.history[0]
        .content
        .iter()
        .enumerate()
        .filter(|(_, b)| matches!(b, ContentBlock::Image { .. }))
        .nth(1)
        .map(|(i, _)| i)
        .unwrap();
    agent.history[0].content[second] = ContentBlock::Text {
        text: "[image #2 couldn't be processed: Could not process image]".into(),
    };
}

fn assert_second_missing(agent: &Agent) {
    let returned = agent.returned(&agent.rewind.marks[0]);
    assert_eq!(returned.len(), 1);
    assert_eq!(returned[0].images.len(), 2);
    let ordinals: Vec<u32> = returned[0].missing.iter().map(|m| m.ordinal).collect();
    assert_eq!(ordinals, [2]);
    assert_eq!(returned[0].missing[0].sha256.len(), 64);
}

#[tokio::test]
async fn a_missing_image_is_named_by_its_place_among_the_messages_images_live_and_resumed() {
    let store = MemoryStore::default();
    let mut live = Live::start(still(answers_n(&["One."]), &store));
    live.send(three_images("u1")).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (mut agent, _) = live.close().await;
    let whole = agent.returned(&agent.rewind.marks[0]);
    assert_eq!(whole[0].images.len(), 3);
    assert!(whole[0].missing.is_empty());
    without_second_image(&mut agent);
    assert_second_missing(&agent);

    let records = records(&store);
    let seq = u64::try_from(records.len()).unwrap();
    let mut back = Agent::new(answers_n(&[]), default_session())
        .with_clock(Arc::new(StillClock))
        .with_log(
            SessionLog::continuing(Box::new(store.clone()), "s1", seq),
            log_context(),
        )
        .resume(&records, 0)
        .unwrap();
    without_second_image(&mut back);
    assert_second_missing(&back);
}

fn quotes(points: &[PointInfo]) -> Vec<(u64, wisp_protocol::Warmth, u64)> {
    points
        .iter()
        .map(|p| (p.point.turn(), p.quote.warm.clone(), p.quote.prefix_tokens))
        .collect()
}

async fn after_a_cold_rewind(switch_back: bool) -> (Vec<PointInfo>, Vec<PointInfo>) {
    let first = default_session().model;
    let store = MemoryStore::default();
    let mut live = Live::start(still(
        answers_n(&["One.", "Two.", "Three.", "Again."]),
        &store,
    ));
    three_turns(&mut live).await;
    switch(&mut live, 1, None, Some("claude-sonnet-5-5")).await;
    assert!(matches!(
        live.rewind(2, 2).await,
        AgentEvent::Rewound { .. }
    ));
    if switch_back {
        switch(&mut live, 3, None, Some(&first)).await;
    }
    let records = records(&store);
    assert!(completed(&live.turn("u4", "again").await));
    let live_points = live.points(4).await;
    live.close().await;

    let mut back = Live::start(resumed(answers_n(&["Again."]), &records));
    assert!(completed(&back.turn("u4", "again").await));
    let back_points = back.points(4).await;
    back.close().await;
    (live_points, back_points)
}

#[tokio::test]
async fn the_point_after_a_cold_rewind_quotes_the_same_live_and_resumed() {
    let (live, back) = after_a_cold_rewind(false).await;
    assert_eq!(quotes(&back), quotes(&live));
    let again = live.last().unwrap();
    assert!(
        matches!(again.quote.warm, wisp_protocol::Warmth::OtherModel { .. }),
        "{live:?}"
    );
}

#[tokio::test]
async fn the_point_after_a_cold_rewind_stays_warm_on_the_first_model_after_a_resume() {
    let (live, back) = after_a_cold_rewind(true).await;
    assert_eq!(quotes(&back), quotes(&live));
    let again = live.last().unwrap();
    assert_eq!(again.quote.warm, wisp_protocol::Warmth::Warm, "{live:?}");
}

async fn switch_effort(live: &mut Live, request: u64, effort: Effort) {
    live.send(ClientCommand::shortcut(
        RequestId(request),
        None,
        None,
        Some(effort),
    ))
    .await;
    live.until(|e| matches!(e, AgentEvent::SettingsChanged { .. }))
        .await;
}

#[tokio::test]
async fn a_request_at_another_effort_never_keeps_a_points_cache_warm() {
    use super::resume::{agent_on, shifted};
    let ttl = crate::context::ttl_span(default_session().cache_ttl).as_mins();
    let clock = shifted();
    let provider = answers_n(&["One.", "Two.", "Three."]);
    let mut live = Live::start(agent_on(provider, &MemoryStore::default(), clock.clone()));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    switch_effort(&mut live, 1, Effort::High).await;
    clock.advance(jiff::SignedDuration::from_mins(ttl - 10));
    assert!(completed(&live.turn("u3", "third").await));
    switch_effort(&mut live, 2, Effort::Medium).await;
    clock.advance(jiff::SignedDuration::from_mins(20));
    let points = live.points(3).await;
    live.close().await;
    let two = points.iter().find(|p| p.point.turn() == 2).unwrap();
    assert_eq!(
        two.quote.warm,
        wisp_protocol::Warmth::MayHaveExpired,
        "{points:?}"
    );
}

#[tokio::test]
async fn a_cold_rewind_inside_a_warm_one_leaves_the_resumed_baseline_warm() {
    let first = default_session().model;
    let store = MemoryStore::default();
    let mut live = Live::start(still(
        answers_n(&["One.", "Two.", "Three.", "Again."]),
        &store,
    ));
    three_turns(&mut live).await;
    switch(&mut live, 1, None, Some("claude-sonnet-5-5")).await;
    assert!(matches!(
        live.rewind(2, 3).await,
        AgentEvent::Rewound { .. }
    ));
    switch(&mut live, 3, None, Some(&first)).await;
    assert!(completed(&live.turn("u4", "again").await));
    let outer = live.rewind(4, 2).await;
    assert!(
        matches!(
            outer,
            AgentEvent::Rewound {
                cache: CacheEffect::Kept,
                ..
            }
        ),
        "{outer:?}"
    );
    let (agent, _) = live.close().await;
    let records = records(&store);
    let inner_cold = records.iter().any(|r| {
        matches!(
            r.event,
            wisp_protocol::log::LogEvent::Rewound {
                cache: CacheEffect::Rebuilt { .. },
                ..
            }
        )
    });
    assert!(inner_cold);
    let back = resumed(answers_n(&[]), &records);
    assert!(baseline_of(&agent).is_some());
    assert_eq!(baseline_of(&back), baseline_of(&agent));
}

fn picture(side: u32) -> wisp_protocol::ImageData {
    let pixels = image::RgbImage::from_pixel(side, side, image::Rgb([10, 120, 200]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(pixels)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    wisp_protocol::ImageData {
        data: wisp_media::encode_base64(&bytes.into_inner()).into(),
    }
}

fn queued_pictures(id: &str, sides: &[u32]) -> ClientCommand {
    ClientCommand::Send {
        id: InputId(id.into()),
        text: id.into(),
        images: sides.iter().map(|s| picture(*s)).collect(),
        mode: DeliveryMode::Queue,
        turn: None,
        replaces: None,
    }
}

#[tokio::test]
async fn a_missing_image_is_counted_within_its_own_input() {
    let gate = Arc::new(Semaphore::new(0));
    let provider = scripted(vec![
        answer("One.", 1_000).gated(&gate),
        answer("Two.", 1_100),
    ]);
    let mut live = Live::start(still(provider, &MemoryStore::default()));
    live.send(send("u1", "first")).await;
    live.until(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .await;
    live.send(queued_pictures("u2", &[1, 2])).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    live.send(queued_pictures("u3", &[3, 4])).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    gate.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (mut agent, _) = live.close().await;
    let mark = agent.rewind.marks.last().unwrap().clone();
    let whole = agent.returned(&mark);
    let counts: Vec<usize> = whole.iter().map(|r| r.images.len()).collect();
    assert_eq!(counts, [2, 2]);
    let message = agent
        .history
        .iter()
        .rposition(|m| m.role == Role::User)
        .unwrap();
    let third = agent.history[message]
        .content
        .iter()
        .enumerate()
        .filter(|(_, b)| matches!(b, ContentBlock::Image { .. }))
        .nth(2)
        .map(|(i, _)| i)
        .unwrap();
    agent.history[message].content[third] = ContentBlock::Text {
        text: "[image #3 couldn't be processed: Could not process image]".into(),
    };
    let returned = agent.returned(&mark);
    let missing: Vec<Vec<u32>> = returned
        .iter()
        .map(|r| r.missing.iter().map(|m| m.ordinal).collect())
        .collect();
    assert_eq!(missing, [vec![], vec![1]]);
    assert_eq!(returned[1].images.len(), 1);
}

#[tokio::test]
async fn a_waiting_message_handed_back_on_resume_names_its_missing_image_by_place() {
    let store = MemoryStore::default();
    let provider = scripted(vec![Step::hang()]);
    let mut live = Live::start(still(provider, &store));
    live.send(send("u1", "first")).await;
    live.until(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .await;
    live.send(queued_pictures("u2", &[1, 2, 3])).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    let records = records(&store);
    let accepted = records
        .iter()
        .position(|r| matches!(r.event, LogEvent::InputAccepted { .. }))
        .unwrap();
    let cut = records[..=accepted].to_vec();
    let LogEvent::InputAccepted { images, .. } = &cut[accepted].event else {
        unreachable!()
    };
    assert_eq!(images.len(), 3);
    let (mut writer, log) = continued(&cut);
    for (n, image) in images.iter().enumerate() {
        let mime = image.media_type.into();
        let blob = store.get_blob(&image.sha256, &mime).unwrap();
        if n != 1 {
            writer.put_blob(&image.sha256, &mime, &blob.bytes).unwrap();
        }
    }
    live.send(ClientCommand::Cancel).await;
    live.close().await;
    let back = Agent::new(answers_n(&[]), default_session())
        .with_clock(Arc::new(StillClock))
        .with_log(log, log_context())
        .resume(&cut, 0)
        .unwrap();
    let returned = back.returned_inputs();
    assert_eq!(returned.len(), 1);
    assert_eq!(returned[0].images.len(), 2);
    let ordinals: Vec<u32> = returned[0].missing.iter().map(|m| m.ordinal).collect();
    assert_eq!(ordinals, [2]);
    assert_eq!(returned[0].missing[0].sha256, images[1].sha256);
}

fn visible(events: &[AgentEvent]) -> Vec<String> {
    use crate::agent::tool_tests::steering::transcript::shown;
    let mut out = Vec::new();
    let mut starts = std::collections::BTreeMap::new();
    let mut from = 0;
    for (at, event) in events.iter().enumerate() {
        match event {
            AgentEvent::TurnStarted { turn, .. } => {
                out.extend(shown(&events[from..at]));
                from = at;
                starts.insert(*turn, out.len());
            }
            AgentEvent::Rewound {
                point,
                reason,
                dropped,
                cache,
                ..
            } => {
                out.extend(shown(&events[from..at]));
                from = at + 1;
                if let Some(start) = starts.get(&point.turn()) {
                    out.truncate(*start);
                }
                out.push(format!(
                    "rewound {point:?} {reason:?} {dropped:?} {cache:?}"
                ));
            }
            _ => {}
        }
    }
    out.extend(shown(&events[from..]));
    out
}

#[tokio::test]
async fn a_resumed_transcript_shows_two_rewinds_the_second_further_back_as_live_did() {
    let store = MemoryStore::default();
    let provider = answers_n(&["One.", "Two.", "Three.", "Four.", "Again."]);
    let mut live = Live::start(still(provider.clone(), &store));
    for (n, text) in ["first", "second", "third", "fourth"].iter().enumerate() {
        assert!(completed(&live.turn(&format!("u{}", n + 1), text).await));
    }
    live.rewind(1, 3).await;
    live.rewind(2, 2).await;
    assert!(completed(&live.turn("u9", "again").await));
    let (_, seen) = live.close().await;
    let records = records(&store);
    let replayed = crate::resume::transcript(&records, &crate::tool::Toolbox::default(), None);
    let shown = visible(&replayed);
    assert_eq!(shown, visible(&seen), "{replayed:#?}");
    let points: Vec<u64> = pick!(&replayed, AgentEvent::Rewound { point, .. } => point.turn());
    assert_eq!(points, [3, 2]);
    assert_eq!(shown.iter().filter(|l| l.starts_with("rewound")).count(), 1);
    assert!(
        !shown
            .iter()
            .any(|l| l.contains("Two.") || l.contains("Three.")),
        "{shown:#?}"
    );
    assert!(replayed.iter().all(|e| match e {
        AgentEvent::Rewound {
            answers, returned, ..
        } => answers.is_none() && returned.is_empty(),
        _ => true,
    }));
}

#[tokio::test]
async fn a_cold_rewind_after_a_model_switch_replays_the_cache_effect_live_sent() {
    let store = MemoryStore::default();
    let mut live = Live::start(still(answers_n(&["One.", "Two."]), &store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.send(ClientCommand::shortcut(
        RequestId(1),
        None,
        Some("claude-sonnet-5-5".into()),
        None,
    ))
    .await;
    live.until(|e| matches!(e, AgentEvent::SettingsChanged { .. }))
        .await;
    let done = live.rewind(3, 2).await;
    live.close().await;
    let AgentEvent::Rewound { cache: sent, .. } = done else {
        panic!("{done:?}");
    };
    assert_eq!(
        sent,
        CacheEffect::Rebuilt {
            reason: RebuildReason::ModelSwitch
        }
    );
    let replayed: Vec<CacheEffect> = pick!(
        crate::resume::transcript(
            &records(&store),
            &crate::tool::Toolbox::default(),
            None,
        ),
        AgentEvent::Rewound { cache, .. } => cache
    );
    assert_eq!(replayed, [sent]);
}

fn damaged_resume(
    provider: Arc<Fake>,
    records: &[LogRecord],
    damaged: usize,
) -> (Agent, MemoryStore) {
    let lines: Vec<crate::session::Line> = records
        .iter()
        .enumerate()
        .map(|(index, r)| {
            if index == damaged {
                crate::session::Line::Damaged(wisp_protocol::log::DamageKind::Unreadable)
            } else {
                crate::session::Line::Record(Box::new(r.clone()))
            }
        })
        .collect();
    let (store, log) = continued(records);
    let agent = Agent::new(provider, default_session())
        .with_clock(Arc::new(StillClock))
        .with_log(log, log_context())
        .resume_lines(lines, 0)
        .unwrap();
    (agent, store)
}

async fn rewound_then_two_more(provider: Arc<Fake>, store: &MemoryStore) -> Agent {
    let mut live = Live::start(still(provider, store));
    three_turns(&mut live).await;
    let done = live.rewind(10, 2).await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    assert!(completed(&live.turn("u4", "fourth").await));
    assert!(completed(&live.turn("u5", "fifth").await));
    live.close().await.0
}

fn committed_of(records: &[LogRecord], text: &str) -> usize {
    records
        .iter()
        .position(|r| match &r.event {
            LogEvent::MessageCommitted { message, .. } => {
                serde_json::to_string(message).unwrap().contains(text)
            }
            _ => false,
        })
        .unwrap()
}

fn fell_back_to(store: &MemoryStore) -> Option<u64> {
    let fell: Vec<Option<u64>> = pick!(
        log_events(store),
        LogEvent::ResumeFellBack { last_turn, .. } => last_turn
    );
    assert_eq!(fell.len(), 1, "{fell:?}");
    fell[0]
}

fn texts_of(history: &[Message]) -> String {
    serde_json::to_string(history).unwrap()
}

#[tokio::test]
async fn damage_after_a_rewind_falls_back_with_the_rewind_kept_and_sends_what_live_sent() {
    let store = MemoryStore::default();
    let words = ["One.", "Two.", "Three.", "Four.", "Five."];
    rewound_then_two_more(answers_n(&words), &store).await;
    let records = records(&store);
    let fourth = records
        .iter()
        .find_map(|r| match &r.event {
            LogEvent::TurnStarted { turn, .. }
                if r.seq > records[committed_of(&records, "Three.")].seq =>
            {
                Some(*turn)
            }
            _ => None,
        })
        .unwrap();

    let twin = answers_n(&["One.", "Two.", "Three.", "Four.", "After."]);
    let twin_store = MemoryStore::default();
    let mut live = Live::start(still(twin.clone(), &twin_store));
    three_turns(&mut live).await;
    live.rewind(10, 2).await;
    assert!(completed(&live.turn("u4", "fourth").await));
    assert!(completed(&live.turn("u6", "after").await));
    let (twin_agent, _) = live.close().await;

    let again = answers_n(&["After."]);
    let (agent, back) = damaged_resume(again.clone(), &records, committed_of(&records, "Five."));
    let mut resumed = Live::start(agent);
    assert!(completed(&resumed.turn("u6", "after").await));
    let (resumed_agent, _) = resumed.close().await;

    assert_eq!(fell_back_to(&back), Some(fourth));
    let last = twin.sent().len() - 1;
    assert_eq!(again.body(0), twin.body(last));
    assert_eq!(resumed_agent.history(), twin_agent.history());
    let history = texts_of(&resumed_agent.history());
    assert!(
        !history.contains("Two.") && !history.contains("Five."),
        "{history}"
    );
}

#[tokio::test]
async fn damage_inside_a_rewound_range_falls_back_before_the_rewind_point() {
    let store = MemoryStore::default();
    let words = ["One.", "Two.", "Three.", "Four.", "Five."];
    rewound_then_two_more(answers_n(&words), &store).await;
    let records = records(&store);

    let twin = answers_n(&["One.", "After."]);
    let twin_store = MemoryStore::default();
    let mut live = Live::start(still(twin.clone(), &twin_store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u6", "after").await));
    let (twin_agent, _) = live.close().await;

    let again = answers_n(&["After."]);
    let (agent, back) = damaged_resume(again.clone(), &records, committed_of(&records, "Three."));
    let notice = agent.resume_notice_text().unwrap().to_owned();
    assert!(notice.contains("turns 4–5 weren't restored"), "{notice}");
    let mut resumed = Live::start(agent);
    assert!(completed(&resumed.turn("u6", "after").await));
    let (resumed_agent, _) = resumed.close().await;

    assert_eq!(fell_back_to(&back), Some(1));
    assert_eq!(again.body(0), twin.body(1));
    assert_eq!(resumed_agent.history(), twin_agent.history());
    let history = texts_of(&resumed_agent.history());
    for gone in ["Two.", "Three.", "Four.", "Five."] {
        assert!(!history.contains(gone), "{gone}: {history}");
    }
}

#[tokio::test]
async fn a_rewind_that_fails_its_check_falls_back_before_its_point() {
    let store = MemoryStore::default();
    let words = ["One.", "Two.", "Three.", "Four.", "Five."];
    rewound_then_two_more(answers_n(&words), &store).await;
    let mut records = records(&store);
    for record in &mut records {
        if let LogEvent::Rewound { messages, .. } = &mut record.event {
            *messages += 5;
        }
    }

    let twin = answers_n(&["One.", "After."]);
    let twin_store = MemoryStore::default();
    let mut live = Live::start(still(twin.clone(), &twin_store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u6", "after").await));
    let (twin_agent, _) = live.close().await;

    let again = answers_n(&["After."]);
    let (agent, back) = damaged_resume(again.clone(), &records, usize::MAX);
    let mut resumed = Live::start(agent);
    assert!(completed(&resumed.turn("u6", "after").await));
    let (resumed_agent, _) = resumed.close().await;

    assert_eq!(fell_back_to(&back), Some(1));
    let kinds: Vec<wisp_protocol::log::DamageKind> = log_events(&back)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::ResumeFellBack { damage, .. } => Some(damage),
            _ => None,
        })
        .flatten()
        .map(|d| d.kind)
        .collect();
    assert_eq!(kinds, [wisp_protocol::log::DamageKind::MalformedHistory]);
    assert_eq!(again.body(0), twin.body(1));
    assert_eq!(resumed_agent.history(), twin_agent.history());
}

fn fallback_records(store: &MemoryStore) -> Vec<LogEvent> {
    log_events(store)
        .into_iter()
        .filter(|e| matches!(e, LogEvent::ResumeFellBack { .. }))
        .collect()
}

fn first_committed(records: &[LogRecord]) -> usize {
    records
        .iter()
        .position(|r| matches!(r.event, LogEvent::MessageCommitted { .. }))
        .unwrap()
}

async fn resend_after(records: &[LogRecord], damaged: usize) -> (Value, Vec<Message>, MemoryStore) {
    let again = answers_n(&["After."]);
    let (agent, back) = damaged_resume(again.clone(), records, damaged);
    let mut resumed = Live::start(agent);
    assert!(completed(&resumed.turn("u9", "after").await));
    let (agent, _) = resumed.close().await;
    (again.body(0), agent.history().to_vec(), back)
}

#[tokio::test]
async fn a_head_restart_with_a_rewind_after_the_head_keeps_the_later_turns() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        compaction(SUMMARY),
        answer("Three.", 300),
        answer("Four.", 400),
        answer("Five.", 500),
    ]);
    let mut live = Live::start(still(provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.compact(1, None).await;
    assert!(completed(&live.turn("u3", "third").await));
    assert!(completed(&live.turn("u4", "fourth").await));
    let done = live.rewind(2, 4).await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    assert!(completed(&live.turn("u5", "fifth").await));
    live.close().await;
    let records = records(&store);

    let (clean_body, clean_history, _) = resend_after(&records, usize::MAX).await;
    let below = first_committed(&records);
    let (body, history, back) = resend_after(&records, below).await;
    let fell = fallback_records(&back);
    assert!(
        matches!(
            &fell[..],
            [LogEvent::ResumeFellBack {
                dropped_through: None,
                covered_through: Some(_),
                ..
            }]
        ),
        "{fell:#?}"
    );
    assert_eq!(history.len(), clean_history.len());
    assert_eq!(body, clean_body);
}

#[tokio::test]
async fn a_masked_head_with_a_later_head_restarts_from_the_later_head() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        compaction(SUMMARY),
        answer("Three.", 300),
        answer("Four.", 400),
        answer("Five.", 1_200),
        compaction(SUMMARY),
        answer("Six.", 300),
    ]);
    let mut live = Live::start(still(provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.compact(1, None).await;
    assert!(completed(&live.turn("u3", "third").await));
    assert!(completed(&live.turn("u4", "fourth").await));
    let done = live.rewind(2, 2).await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    assert!(completed(&live.turn("u5", "fifth").await));
    live.compact(3, None).await;
    assert!(completed(&live.turn("u6", "sixth").await));
    live.close().await;
    let records = records(&store);

    let (clean_body, clean_history, _) = resend_after(&records, usize::MAX).await;
    let below = first_committed(&records);
    let (body, history, back) = resend_after(&records, below).await;
    let fell = fallback_records(&back);
    let text = serde_json::to_string(&history).unwrap();
    for gone in ["Two.", "Three.", "Four."] {
        assert!(!text.contains(gone), "{gone}: {text}");
    }
    assert!(
        matches!(
            &fell[..],
            [LogEvent::ResumeFellBack {
                dropped_through: None,
                covered_through: Some(_),
                ..
            }]
        ),
        "{fell:#?}"
    );
    assert_eq!(history.len(), clean_history.len());
    assert_eq!(body, clean_body);
}

#[tokio::test]
async fn a_masked_head_alone_never_brings_back_rewound_turns() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        compaction(SUMMARY),
        answer("Three.", 300),
        answer("Four.", 400),
        answer("Five.", 500),
    ]);
    let mut live = Live::start(still(provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.compact(1, None).await;
    assert!(completed(&live.turn("u3", "third").await));
    assert!(completed(&live.turn("u4", "fourth").await));
    let done = live.rewind(2, 2).await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    assert!(completed(&live.turn("u5", "fifth").await));
    live.close().await;
    let records = records(&store);

    let below = first_committed(&records);
    let (_, history, back) = resend_after(&records, below).await;
    assert_eq!(fallback_records(&back).len(), 1);
    let text = serde_json::to_string(&history).unwrap();
    for gone in ["Two.", "Three.", "Four.", "So far: a greeting."] {
        assert!(!text.contains(gone), "{gone}: {text}");
    }
}

#[tokio::test]
async fn an_unended_rewound_turn_then_damage_never_brings_back_rewound_turns() {
    let store = MemoryStore::default();
    let words = ["One.", "Two.", "Three.", "Four.", "Five."];
    rewound_then_two_more(answers_n(&words), &store).await;
    let mut records = records(&store);
    for record in &mut records {
        if matches!(record.event, LogEvent::TurnEnded { turn: 3, .. }) {
            record.event = LogEvent::SessionEnded {
                turns: 3,
                interrupted: None,
            };
        }
    }
    let again = answers_n(&["After."]);
    let (agent, back) = damaged_resume(again.clone(), &records, committed_of(&records, "Four."));
    let mut resumed = Live::start(agent);
    assert!(completed(&resumed.turn("u6", "after").await));
    let (resumed_agent, _) = resumed.close().await;
    let history = texts_of(&resumed_agent.history());
    for gone in ["Two.", "Three.", "Four.", "Five."] {
        assert!(!history.contains(gone), "{gone}: {history}");
    }
    assert_eq!(fell_back_to(&back), Some(1));
}

#[tokio::test]
async fn rewound_away_messages_are_not_returned_to_the_composer() {
    let store = MemoryStore::default();
    let words = ["One.", "Two.", "Three.", "Four.", "Five."];
    rewound_then_two_more(answers_n(&words), &store).await;
    let records = records(&store);
    let again = answers_n(&["After."]);
    let (agent, _back) = damaged_resume(again, &records, committed_of(&records, "Three."));
    let texts: Vec<String> = agent
        .returned_inputs()
        .iter()
        .map(|r| r.text.clone())
        .collect();
    assert!(!texts.iter().any(|t| t == "third"), "{texts:?}");
}

#[tokio::test]
async fn a_setting_changed_inside_a_rewound_range_is_named_when_a_fallback_drops_it() {
    let store = MemoryStore::default();
    let mut live = Live::start(still(
        answers_n(&["One.", "Two.", "Three.", "Four."]),
        &store,
    ));
    three_turns(&mut live).await;
    live.send(ClientCommand::shortcut(
        RequestId(1),
        None,
        None,
        Some(Effort::Max),
    ))
    .await;
    let done = live.rewind(2, 2).await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    assert!(completed(&live.turn("u4", "fourth").await));
    live.close().await;
    let records = records(&store);
    let changed = records
        .iter()
        .position(|r| matches!(r.event, LogEvent::SettingsChanged { .. }))
        .unwrap();
    let rewound = records
        .iter()
        .position(|r| matches!(r.event, LogEvent::Rewound { .. }))
        .unwrap();
    assert!(changed < rewound);

    let (agent, back) = damaged_resume(
        answers_n(&["After."]),
        &records,
        committed_of(&records, "Three."),
    );
    drop(back);
    let notice = agent.resume_notice_text().unwrap();
    assert!(notice.contains("from turn 1 "), "{notice}");
    assert!(
        notice.contains("Settings changed after turn 1 weren't restored either."),
        "{notice}"
    );
}

#[tokio::test]
async fn live_dropped_turns_around_a_rewound_range_are_listed_one_by_one() {
    let store = MemoryStore::default();
    let words = ["One.", "Two.", "Three.", "Four.", "Five."];
    let mut live = Live::start(still(answers_n(&words), &store));
    three_turns(&mut live).await;
    assert!(completed(&live.turn("u4", "fourth").await));
    let done = live.rewind(10, 3).await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    assert!(completed(&live.turn("u5", "fifth").await));
    live.close().await;
    let records = records(&store);

    let (agent, back) = damaged_resume(
        answers_n(&["After."]),
        &records,
        committed_of(&records, "Two."),
    );
    drop(back);
    let notice = agent.resume_notice_text().unwrap();
    assert!(notice.contains("from turn 1 "), "{notice}");
    assert!(
        notice.contains("turns 2 and 5 weren't restored"),
        "{notice}"
    );
}

#[tokio::test]
async fn a_rewind_estimates_the_next_head_again_from_the_kept_turns() {
    let store = MemoryStore::default();
    let mut live = Live::start(still(answers_n(&["one", "two", "three"]), &store));
    three_turns(&mut live).await;
    assert!(matches!(
        live.rewind(1, 3).await,
        AgentEvent::Rewound { .. }
    ));
    let (agent, _) = live.close().await;
    let after = agent.strategy_estimate().unwrap().concat();
    assert!(after.contains("second"), "{after}");
    assert!(!after.contains("third"), "{after}");
}

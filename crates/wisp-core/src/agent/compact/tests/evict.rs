use std::io;

use wisp_protocol::log::LogRecord;
use wisp_protocol::{CompactionNote, ImageType, Mime, ReturnedInput, RewindPoint, RewindQuote};

use super::*;
use crate::clock::StillClock;
use crate::context::blocks_tokens;
use crate::session::{Blob, sha256_bytes_hex};
use crate::stack_alarm::{Timer, on_alarm_stack};
use crate::test_guard::{BLUE_PIXEL as BLUE, RED_PIXEL as RED, records, user};

#[derive(Clone, Default)]
struct Unread(MemoryStore);

impl SessionStore for Unread {
    fn append(&mut self, session: &str, record: &LogRecord) -> io::Result<()> {
        self.0.append(session, record)
    }

    fn sync(&mut self, session: &str) -> io::Result<()> {
        self.0.sync(session)
    }

    fn list(&self) -> io::Result<Vec<String>> {
        self.0.list()
    }

    fn read(&self, session: &str) -> io::Result<Vec<LogRecord>> {
        self.0.read(session)
    }

    fn put_blob(&mut self, sha256: &str, media_type: &Mime, bytes: &[u8]) -> io::Result<()> {
        self.0.put_blob(sha256, media_type, bytes)
    }

    fn get_blob(&self, _sha256: &str, _media_type: &Mime) -> io::Result<Blob> {
        Err(io::Error::new(io::ErrorKind::NotFound, "never read"))
    }
}

fn plain(provider: Arc<Fake>) -> Agent {
    Agent::new(provider, default_session()).with_clock(Arc::new(StillClock))
}

fn logged(provider: Arc<Fake>, store: impl SessionStore + 'static) -> Agent {
    plain(provider).with_log(SessionLog::new(Box::new(store), "s1"), log_context())
}

fn script() -> Arc<Fake> {
    scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        compaction(SUMMARY),
        answer("Three.", 300),
        answer("Again.", 400),
        answer("More.", 500),
    ])
}

fn with_image(id: &str, text: &str) -> ClientCommand {
    ClientCommand::Send {
        id: InputId(id.into()),
        text: text.into(),
        images: vec![wisp_protocol::ImageData { data: BLUE.into() }],
        mode: DeliveryMode::Steer,
        turn: None,
        replaces: None,
    }
}

fn verdict(event: &AgentEvent, request: u64) -> bool {
    match event {
        AgentEvent::Rewound { answers, .. } => *answers == Some(RequestId(request)),
        AgentEvent::RewindPoints { answers, .. } => *answers == RequestId(request),
        AgentEvent::Rejected { request: r, .. } => *r == Some(RequestId(request)),
        _ => false,
    }
}

impl Live {
    pub(super) async fn image_turn(&mut self, id: &str, text: &str) -> TurnOutcome {
        self.send(with_image(id, text)).await;
        match self
            .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await
        {
            AgentEvent::TurnEnded { outcome, .. } => outcome,
            _ => unreachable!(),
        }
    }

    async fn go_back(&mut self, request: u64, turn: u64) -> AgentEvent {
        self.send(ClientCommand::Rewind {
            to: RewindPoint::Turn { turn },
            request: RequestId(request),
        })
        .await;
        self.until(|e| verdict(e, request)).await
    }

    async fn listed(
        &mut self,
        request: u64,
    ) -> Vec<(RewindPoint, RewindQuote, Option<CompactionNote>)> {
        self.send(ClientCommand::RewindPoints {
            request: RequestId(request),
        })
        .await;
        match self.until(|e| verdict(e, request)).await {
            AgentEvent::RewindPoints { points, .. } => points
                .into_iter()
                .map(|p| (p.point, p.quote, p.compaction))
                .collect(),
            other => panic!("{other:?}"),
        }
    }
}

async fn compacted(live: &mut Live) {
    assert!(completed(&live.image_turn("u1", "look").await));
    assert!(completed(&live.turn("u2", "second").await));
    assert!(matches!(
        live.compact(1, None).await,
        AgentEvent::Compacted { .. }
    ));
    assert!(completed(&live.turn("u3", "third").await));
}

fn returned(event: &AgentEvent) -> Vec<ReturnedInput> {
    match event {
        AgentEvent::Rewound { returned, .. } => returned.clone(),
        other => panic!("{other:?}"),
    }
}

fn images_in(body: &Value) -> Vec<String> {
    match body {
        Value::Object(map) => match (map.get("type"), map.get("data")) {
            (Some(Value::String(kind)), Some(Value::String(data))) if kind == "base64" => {
                vec![data.clone()]
            }
            _ => map.values().flat_map(images_in).collect(),
        },
        Value::Array(items) => items.iter().flat_map(images_in).collect(),
        _ => Vec::new(),
    }
}

fn raw_images(agent: &Agent) -> Vec<String> {
    agent
        .history
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::Image {
                source: wisp_protocol::ImageSource::Base64 { data, .. },
            } => Some(data.to_string()),
            _ => None,
        })
        .collect()
}

fn image_sha(store: &MemoryStore) -> String {
    log_events(store)
        .iter()
        .find_map(|e| match e {
            LogEvent::TurnStarted { images, .. } => images.first().map(|i| i.sha256.clone()),
            _ => None,
        })
        .unwrap()
}

#[test]
fn compacted_away_media_is_held_on_disk_and_every_reader_sees_the_same_bytes() {
    on_alarm_stack(Timer::Real, || async {
        let store = MemoryStore::default();
        let provider = script();
        let mut live = Live::start(logged(provider.clone(), store.clone()));
        compacted(&mut live).await;
        let points = live.listed(7).await;
        let (agent, _) = live.close().await;

        let twin = script();
        let mut other = Live::start(logged(twin.clone(), Unread::default()));
        compacted(&mut other).await;
        let twin_points = other.listed(7).await;
        let (kept, _) = other.close().await;

        assert_eq!(agent.evicted_count(), 1);
        assert_eq!(kept.evicted_count(), 0);
        assert_eq!(raw_images(&agent), [""]);
        assert_ne!(raw_images(&kept), [""]);
        assert_eq!(*agent.history(), *kept.history());
        assert_eq!(rebuilt(&store), *agent.history());
        assert_eq!(provider.bodies(), twin.bodies());
        let betas = |fake: &Fake| fake.sent().iter().map(Sent::beta).collect::<Vec<_>>();
        assert_eq!(betas(&provider), betas(&twin));
        assert_eq!(points, twin_points);
    });
}

type Run = (Vec<ReturnedInput>, Vec<(Value, String)>);

async fn rewound_twins(point: u64) -> [Run; 2] {
    let mut runs = Vec::new();
    for evicts in [true, false] {
        let provider = script();
        let mut live = if evicts {
            Live::start(logged(provider.clone(), MemoryStore::default()))
        } else {
            Live::start(logged(provider.clone(), Unread::default()))
        };
        compacted(&mut live).await;
        let back = returned(&live.go_back(2, point).await);
        let (agent, _) = live.close().await;
        assert!(agent.nothing_evicted_in_view());
        assert!(!raw_images(&agent).contains(&String::new()));
        let mut live = Live::start(agent);
        let next = if point == 1 {
            live.image_turn("u4", "look").await
        } else {
            live.turn("u4", "again").await
        };
        assert!(completed(&next));
        let (agent, _) = live.close().await;
        assert_eq!(agent.evicted_count(), 0);
        let sent: Vec<(Value, String)> = provider
            .sent()
            .iter()
            .map(|s| (s.body(), s.beta()))
            .collect();
        assert_eq!(images_in(&sent.last().unwrap().0), images_in(&sent[0].0));
        runs.push((back, sent));
    }
    assert_eq!(runs[0], runs[1]);
    [runs.remove(0), runs.remove(0)]
}

#[test]
fn a_rewind_to_the_image_turn_returns_the_image_byte_for_byte() {
    on_alarm_stack(Timer::Real, || async {
        let [(back, sent), _] = rewound_twins(1).await;
        assert_eq!(back[0].images.len(), 1);
        assert!(back[0].missing.is_empty());
        assert_eq!(images_in(&sent[0].0), [back[0].images[0].data.to_string()]);
    });
}

#[test]
fn a_rewind_that_keeps_the_image_turn_resends_its_original_bytes() {
    on_alarm_stack(Timer::Real, || async {
        let [(_, sent), _] = rewound_twins(2).await;
        assert_eq!(images_in(&sent[0].0).len(), 1);
    });
}

#[test]
fn a_resumed_session_evicts_again_and_a_rewind_restores_the_image() {
    on_alarm_stack(Timer::Real, || async {
        let store = MemoryStore::default();
        let mut live = Live::start(logged(script(), store.clone()));
        compacted(&mut live).await;
        let (before, _) = live.close().await;
        let original = before.history().into_owned();
        let records = records(&store);
        let seq = u64::try_from(records.len()).unwrap();
        let provider = scripted(vec![answer("Again.", 400)]);
        let resumed = plain(provider.clone())
            .with_log(
                SessionLog::continuing(Box::new(store.clone()), "s1", seq),
                log_context(),
            )
            .resume(&records, 0)
            .unwrap();
        assert_eq!(resumed.evicted_count(), 1);
        assert_eq!(raw_images(&resumed), [""]);
        assert_eq!(*resumed.history(), *original);
        let mut back = Live::start(resumed);
        assert!(matches!(
            back.go_back(2, 2).await,
            AgentEvent::Rewound { .. }
        ));
        assert!(completed(&back.turn("u4", "again").await));
        let (agent, _) = back.close().await;
        assert_eq!(agent.evicted_count(), 0);
        let last = provider.body(0);
        let first_image = original
            .iter()
            .flat_map(|m| &m.content)
            .find_map(|b| match b {
                ContentBlock::Image {
                    source: wisp_protocol::ImageSource::Base64 { data, .. },
                } => Some(data.to_string()),
                _ => None,
            })
            .unwrap();
        assert_eq!(images_in(&last), [first_image]);
    });
}

#[test]
fn nothing_is_evicted_without_a_working_log() {
    on_alarm_stack(Timer::Real, || async {
        for agent in [plain(script()), logged(script(), MemoryStore::failing())] {
            let mut live = Live::start(agent);
            compacted(&mut live).await;
            let (agent, _) = live.close().await;
            assert_eq!(agent.evicted_count(), 0);
            assert_ne!(raw_images(&agent), [""]);
        }
    });
}

#[test]
fn a_missing_blob_refuses_the_rewind_and_changes_nothing() {
    on_alarm_stack(Timer::Real, || async {
        let store = MemoryStore::default();
        let mut live = Live::start(logged(script(), store.clone()));
        compacted(&mut live).await;
        let sha = image_sha(&store);
        store.remove_blob(&sha).unwrap();
        let refused = live.go_back(2, 1).await;
        let AgentEvent::Rejected { kind, reason, .. } = &refused else {
            panic!("{refused:?}");
        };
        assert_eq!(*kind, Some(RejectKind::NotRewindable));
        assert_eq!(
            *reason,
            format!(
                "Can't rewind: the image file {sha}.png is missing from the attachments folder."
            )
        );
        assert!(completed(&live.turn("u4", "fourth").await));
        let (agent, _) = live.close().await;
        assert_eq!(agent.evicted_count(), 1);
        assert_eq!(raw_images(&agent), [""]);
    });
}

fn png(data: &str) -> ContentBlock {
    ContentBlock::image(ImageType::Png, data)
}

fn reply() -> Message {
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text { text: "ok".into() }],
    }
}

#[test]
fn only_stored_canonical_media_before_the_cut_is_evicted() {
    let store = MemoryStore::default();
    let bytes = wisp_media::decode_base64(BLUE).unwrap();
    let mut writer = store.clone();
    writer
        .put_blob(
            &sha256_bytes_hex(&bytes),
            &Mime::from(ImageType::Png),
            &bytes,
        )
        .unwrap();
    let pdf = crate::tool::snap::mime("application/pdf");
    writer
        .put_blob(&sha256_bytes_hex(&bytes), &pdf, &bytes)
        .unwrap();
    let slice = Some(wisp_protocol::FileSlice::pages(14, 16).unwrap());
    let mut agent = logged(scripted(Vec::new()), store);
    let history = vec![
        user(vec![
            png(BLUE),
            ContentBlock::media(Mime::from(ImageType::Png), "not base64!"),
            png(RED),
        ]),
        reply(),
        user(vec![ContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: vec![ContentBlock::media_sliced(pdf, BLUE, slice)],
            is_error: false,
        }]),
        reply(),
        user(vec![png(BLUE)]),
        reply(),
    ];
    agent.history = history.clone();
    agent.compaction.head = Some(Head {
        epoch: 1,
        from: 6,
        message: reply(),
        summary: String::new(),
        native: true,
        turns: (1, 1),
        cut: 4,
        placement: Placement::Leading,
        additions: Vec::new(),
        join: None,
    });
    let tokens: Vec<u64> = history.iter().map(|m| blocks_tokens(&m.content)).collect();
    agent.evict_media();
    assert_eq!(agent.evicted_count(), 2);
    assert_eq!(agent.history[0].content[1], history[0].content[1]);
    assert_eq!(agent.history[0].content[2], history[0].content[2]);
    assert_eq!(agent.history[4], history[4]);
    assert_ne!(agent.history[0], history[0]);
    assert_ne!(agent.history[2], history[2]);
    let ContentBlock::ToolResult { content, .. } = &agent.history[2].content[0] else {
        panic!("expected a tool result")
    };
    let held = content[0].inline_media().unwrap();
    assert_eq!(held.slice, slice);
    assert_eq!(held.data, "");
    assert_eq!(*agent.history(), *history);
    let held: Vec<u64> = agent
        .history
        .iter()
        .enumerate()
        .map(|(i, m)| agent.evicted_tokens(i..i + 1, blocks_tokens(&m.content)))
        .collect();
    assert_eq!(held, tokens);
    agent.evict_media();
    assert_eq!(agent.evicted_count(), 2);
    agent.restore_media(4).unwrap();
    assert_eq!(agent.evicted_count(), 2);
    agent.restore_media(0).unwrap();
    assert_eq!(agent.evicted_count(), 0);
    assert_eq!(agent.history, history);
}

#[test]
fn a_head_whose_cut_passes_its_from_starts_the_view_at_from() {
    let mut agent = plain(scripted(Vec::new()));
    agent.compaction.head = Some(Head {
        epoch: 1,
        from: 2,
        message: reply(),
        summary: String::new(),
        native: false,
        turns: (1, 1),
        cut: 5,
        placement: Placement::Head,
        additions: Vec::new(),
        join: None,
    });
    assert_eq!(agent.view_from(), 2);
}

fn evicted_in_view(store: &MemoryStore, provider: Arc<Fake>) -> Agent {
    let bytes = wisp_media::decode_base64(BLUE).unwrap();
    let mut writer = store.clone();
    writer
        .put_blob(
            &sha256_bytes_hex(&bytes),
            &Mime::from(ImageType::Png),
            &bytes,
        )
        .unwrap();
    let mut agent = logged(provider, store.clone());
    agent.history = vec![
        user(vec![
            png(BLUE),
            ContentBlock::Text {
                text: "look".into(),
            },
        ]),
        reply(),
    ];
    agent.compaction.head = Some(Head {
        epoch: 1,
        from: 2,
        message: reply(),
        summary: String::new(),
        native: false,
        turns: (1, 1),
        cut: 2,
        placement: Placement::Head,
        additions: Vec::new(),
        join: None,
    });
    agent.evict_media();
    agent.compaction.head = None;
    assert_eq!(agent.evicted_count(), 1);
    agent
}

#[test]
fn the_step_restores_an_evicted_block_in_view_before_its_request() {
    on_alarm_stack(Timer::Real, || async {
        let provider = scripted(vec![answer("Seen.", 100)]);
        let agent = evicted_in_view(&MemoryStore::default(), provider.clone());
        let mut live = Live::start(agent);
        assert!(completed(&live.turn("u2", "again").await));
        let (agent, _) = live.close().await;
        assert_eq!(agent.evicted_count(), 0);
        assert_eq!(images_in(&provider.body(0)), [BLUE]);
    });
}

#[test]
fn the_step_refuses_to_send_a_block_it_cannot_restore() {
    on_alarm_stack(Timer::Real, || async {
        let store = MemoryStore::default();
        let provider = scripted(vec![answer("Seen.", 100)]);
        let agent = evicted_in_view(&store, provider.clone());
        let bytes = wisp_media::decode_base64(BLUE).unwrap();
        let sha = sha256_bytes_hex(&bytes);
        store.remove_blob(&sha).unwrap();
        let mut live = Live::start(agent);
        let TurnOutcome::Error { kind, message, .. } = live.turn("u2", "again").await else {
            panic!("the turn should fail");
        };
        assert_eq!(kind, ErrorKind::InvalidRequest);
        assert_eq!(
            message,
            format!(
                "Wisp can't send this conversation: the image file {sha}.png is missing from the attachments folder."
            )
        );
        assert!(provider.sent().is_empty());
        live.close().await;
    });
}

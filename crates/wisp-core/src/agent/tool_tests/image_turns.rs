use wisp_media::{MAX_SESSION_MEDIA_BASE64, Tier};
use wisp_protocol::log::{FeatureState, ImageRef, LogRecord};
use wisp_protocol::{ImageData, ImageSource, ImageType};

use super::*;
use crate::agent::images::PreparedImage;
use crate::agent::testing::{custom_caps, seeing, seeing_connection, text_of};
use crate::provider::anthropic::capabilities;
use crate::provider::{ImageCaps, ModelCapabilities};
use crate::session::{Blob, rebuild};
use crate::test_guard::{BLUE_PIXEL as BLUE, RED_PIXEL as RED};

fn real_caps() -> Option<&'static ModelCapabilities> {
    capabilities::row(DEFAULT_MODEL)
}

fn image_agent(
    scripted: Arc<Fake>,
    caps: Option<&'static ModelCapabilities>,
    env: &Env,
    store: Option<Box<dyn SessionStore>>,
) -> Agent {
    provider_agent(seeing(&scripted, caps), env, store)
}

fn provider_agent(
    provider: Arc<dyn Provider>,
    env: &Env,
    store: Option<Box<dyn SessionStore>>,
) -> Agent {
    let agent = Agent::new(provider, default_session())
        .with_clock(TickingClock::new())
        .with_tools(test_toolbox(), env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300));
    match store {
        Some(store) => agent.with_log(SessionLog::new(store, "s1"), log_context()),
        None => agent,
    }
}

fn data(images: &[&str]) -> Vec<ImageData> {
    images
        .iter()
        .map(|d| ImageData { data: (*d).into() })
        .collect()
}

async fn image_turn(
    agent: &mut Agent,
    text: &str,
    images: &[&str],
    cancel_when: Option<Trigger>,
) -> (Option<TurnOutcome>, Vec<AgentEvent>) {
    let (cmd_tx, mut cmd_rx) = mpsc::channel(4);
    let (ev_tx, mut ev_rx) = mpsc::channel(1024);
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
        .run_user_turn(text.into(), data(images), &mut cmd_rx, &ev_tx)
        .await;
    drop(ev_tx);
    (outcome, collector.await.unwrap())
}

fn normalised(image: &str) -> (String, String) {
    let bytes = wisp_media::decode_base64(image).unwrap();
    let n = wisp_media::normalise(&bytes, Tier::HIGH).unwrap();
    (
        wisp_media::encode_base64(&n.bytes),
        crate::session::sha256_bytes_hex(&n.bytes),
    )
}

fn png_base64(width: u32, height: u32) -> String {
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([
            u8::try_from(x % 256).unwrap(),
            u8::try_from(y % 256).unwrap(),
            200,
        ])
    });
    let mut out = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut out),
        image.as_raw(),
        width,
        height,
        image::ExtendedColorType::Rgb8,
    )
    .unwrap();
    wisp_media::encode_base64(&out)
}

fn sent_size(block: &ContentBlock) -> (u32, u32) {
    let ContentBlock::Image {
        source: ImageSource::Base64 { data, .. },
    } = block
    else {
        panic!("not an image: {block:?}")
    };
    let bytes = wisp_media::decode_base64(data).unwrap();
    let image = image::load_from_memory(&bytes).unwrap();
    (image.width(), image.height())
}

fn sent_block(image: &str) -> ContentBlock {
    ContentBlock::image(ImageType::Png, normalised(image).0)
}

fn images_logged(records: &[LogRecord]) -> Vec<Vec<ImageRef>> {
    records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::TurnStarted { images, .. } => Some(images.clone()),
            _ => None,
        })
        .collect()
}

fn texts(blocks: &[ContentBlock]) -> Vec<String> {
    blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.clone(),
            ContentBlock::Image {
                source: ImageSource::Base64 { data, .. },
            } => format!("image:{data}"),
            ContentBlock::ToolResult { tool_use_id, .. } => format!("result:{tool_use_id}"),
            other => format!("{other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn an_image_turn_sends_context_stamp_images_then_text_and_saves_first() {
    let env = Env::new("img-order");
    let wide = png_base64(3, 1);
    let scripted = scripted(vec![done("ok")]);
    let ops = Ops::default();
    let store = RecordingStore {
        read_gate: None,
        inner: MemoryStore::default(),
        ops: ops.clone(),
    };
    let mut agent = image_agent(
        scripted.clone(),
        real_caps(),
        &env,
        Some(Box::new(store.clone())),
    );
    let (outcome, events) = image_turn(&mut agent, "look", &[RED, &wide], None).await;
    assert!(
        matches!(outcome, Some(TurnOutcome::Completed { .. })),
        "{events:#?}"
    );
    let requests = scripted.sent();
    let content = &requests[0].messages[0].content;
    assert_eq!(content.len(), 6, "{content:#?}");
    let shown = texts(content);
    assert!(shown[0].starts_with("<environment>"), "{shown:?}");
    assert!(shown[1].starts_with("Sent: "), "{shown:?}");
    assert!(shown[2].starts_with("Context: "), "{shown:?}");
    assert_eq!(content[3], sent_block(RED));
    assert_eq!(content[4], sent_block(&wide));
    assert_eq!(shown[5], "look");
    assert_eq!(agent.history()[0].content, *content);

    let records = records(&store.inner);
    let logged = images_logged(&records);
    let (red_data, red_sha) = normalised(RED);
    let (_, blue_sha) = normalised(&wide);
    let red_bytes = wisp_media::decode_base64(&red_data).unwrap();
    assert_eq!(
        logged,
        [vec![
            ImageRef {
                sha256: red_sha.clone(),
                media_type: ImageType::Png,
                bytes: u64::try_from(red_bytes.len()).unwrap(),
                width: 1,
                height: 1,
            },
            ImageRef {
                sha256: blue_sha.clone(),
                media_type: ImageType::Png,
                bytes: logged[0][1].bytes,
                width: 3,
                height: 1,
            },
        ]]
    );
    let mut names = store.inner.blob_names().unwrap();
    names.sort();
    let mut want = vec![format!("{red_sha}.png"), format!("{blue_sha}.png")];
    want.sort();
    assert_eq!(names, want);
    let ops = ops.lock().unwrap().clone();
    assert_eq!(ops[..4], ["append", "blob", "blob", "append"], "{ops:?}");
    assert!(
        records[1..7]
            .iter()
            .all(|r| matches!(r.event, LogEvent::HookRan { .. }))
    );
    assert!(matches!(records[7].event, LogEvent::TurnStarted { .. }));
}

#[test]
fn the_wire_body_with_an_image_matches_its_fixture() {
    let session = SessionConfig {
        model: "claude-sonnet-5-5".into(),
        system: Some("You are Wisp.".into()),
        effort: Effort::Low,
        cache_ttl: CacheTtl::OneHour,
        max_tokens: 1024,
        message_time: MessageTime::Utc,
        instructions: None,
        context_note: true,
    };
    let messages = [Message {
        role: Role::User,
        content: vec![
            ContentBlock::Text {
                text: "Sent: Friday 2026-10-23 12:00 UTC\n".into(),
            },
            ContentBlock::image(ImageType::Png, RED),
            ContentBlock::image(ImageType::Jpeg, "/9j/AA=="),
            ContentBlock::Text {
                text: "What colour is this?".into(),
            },
        ],
    }];
    let request = Request {
        session: &session,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: &messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering: &crate::provider::UNRENDERED,
        compaction: None,
    };
    let body = text_of(&request) + "\n";
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/requests/image_message.json");
    if std::env::var_os("WISP_BLESS").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &body).unwrap();
    }
    let stored = fs::read_to_string(&path).unwrap();
    assert_eq!(stored, body);
    assert!(body.contains(&format!(
        r#"{{"type":"image","source":{{"type":"base64","media_type":"image/png","data":"{RED}"}}}}"#
    )));
}

async fn three_turns(env: &Env, store: &MemoryStore) -> (Agent, Arc<Fake>) {
    let scripted = scripted(vec![done("one"), done("two"), done("three")]);
    let mut agent = image_agent(
        scripted.clone(),
        real_caps(),
        env,
        Some(Box::new(store.clone())),
    );
    image_turn(&mut agent, "first", &[], None).await;
    image_turn(&mut agent, "what is this?", &[RED], None).await;
    image_turn(&mut agent, "and now?", &[], None).await;
    (agent, scripted)
}

#[tokio::test]
async fn an_image_turn_keeps_one_prefix_and_rebuilds_from_the_log_and_the_store() {
    let env = Env::new("img-prefix");
    let store = MemoryStore::default();
    let (agent, scripted) = three_turns(&env, &store).await;
    let bodies = scripted.texts();
    assert_eq!(bodies.len(), 3);
    for pair in bodies.windows(2) {
        let (head_a, msgs_a) = head_and_messages(&pair[0]);
        let (head_b, msgs_b) = head_and_messages(&pair[1]);
        assert_eq!(head_a, head_b);
        let open = msgs_a.strip_suffix(']').unwrap();
        assert!(msgs_b.starts_with(open), "\n{msgs_a}\n{msgs_b}");
    }
    assert!(bodies[2].contains(
        r#"{"type":"image","source":{"type":"base64","media_type":"image/png","data":""#
    ));

    let events = log_events(&store);
    let rebuilt = rebuild(&store, events.iter()).unwrap().messages;
    assert_eq!(
        serde_json::to_string(&rebuilt).unwrap(),
        serde_json::to_string(&*agent.history()).unwrap()
    );
    let requests = scripted.sent();
    let last = requests.last().unwrap();
    let n = last.messages.len();
    assert_eq!(
        rebuilt_body(last, &rebuilt[..n]),
        rebuilt_body(last, &last.messages)
    );
}

#[tokio::test]
async fn a_missing_or_corrupt_image_fails_the_rebuild_and_names_the_turn() {
    let env = Env::new("img-corrupt");
    let store = MemoryStore::default();
    let (agent, _) = three_turns(&env, &store).await;
    let events = log_events(&store);
    let (data, sha) = normalised(RED);
    let bytes = wisp_media::decode_base64(&data).unwrap();

    store.remove_blob(&sha).unwrap();
    let missing = rebuild(&store, events.iter()).unwrap_err();
    assert_eq!((missing.turn, missing.sha256.as_str()), (2, sha.as_str()));
    assert!(
        missing.to_string().starts_with("turn 2: attachment "),
        "{missing}"
    );

    store
        .replace_blob(
            &sha,
            Blob {
                media_type: ImageType::Png.into(),
                bytes: b"not the image".to_vec(),
            },
        )
        .unwrap();
    let corrupt = rebuild(&store, events.iter()).unwrap_err();
    assert_eq!(corrupt.turn, 2);
    assert!(
        corrupt.reason.contains("doesn't match its hash"),
        "{corrupt}"
    );

    store
        .replace_blob(
            &sha,
            Blob {
                media_type: ImageType::Jpeg.into(),
                bytes: bytes.clone(),
            },
        )
        .unwrap();
    let relabelled = rebuild(&store, events.iter()).unwrap().messages;
    assert_eq!(
        relabelled,
        &*agent.history(),
        "the block takes its type from the record; the hash proves the bytes"
    );

    let mut short = events.clone();
    for e in &mut short {
        if let LogEvent::TurnStarted { images, .. } = e {
            for image in images {
                image.bytes += 1;
            }
        }
    }
    store
        .replace_blob(
            &sha,
            Blob {
                media_type: ImageType::Png.into(),
                bytes,
            },
        )
        .unwrap();
    assert!(rebuild(&store, events.iter()).is_ok());
    let wrong_size = rebuild(&store, short.iter()).unwrap_err();
    assert!(wrong_size.reason.contains("bytes"), "{wrong_size}");
}

#[tokio::test]
async fn a_cancel_during_the_first_reply_leaves_out_the_message_and_its_images() {
    let env = Env::new("img-cancel1");
    let store = MemoryStore::default();
    let scripted = scripted(vec![hanging(&[B::Text("partial")]), done("ok")]);
    let mut agent = image_agent(
        scripted.clone(),
        real_caps(),
        &env,
        Some(Box::new(store.clone())),
    );
    let (outcome, events) = image_turn(&mut agent, "look", &[RED], Some(on_text)).await;
    assert!(
        matches!(
            outcome,
            Some(TurnOutcome::Cancelled {
                committed_steps: 0,
                ..
            })
        ),
        "{outcome:?}"
    );
    check_events(&events);
    assert!(agent.history().is_empty());
    let (_, sha) = normalised(RED);
    assert_eq!(store.blob_names().unwrap(), [format!("{sha}.png")]);
    assert_eq!(images_logged(&records(&store))[0].len(), 1);

    image_turn(&mut agent, "again", &[], None).await;
    let requests = scripted.sent();
    let second = &requests[1].messages;
    assert_eq!(second.len(), 1);
    assert!(
        second[0]
            .content
            .iter()
            .all(|b| !matches!(b, ContentBlock::Image { .. }))
    );

    store.remove_blob(&sha).unwrap();
    let rebuilt = rebuild(&store, log_events(&store).iter()).unwrap().messages;
    assert_eq!(rebuilt, &*agent.history());
}

#[tokio::test]
async fn a_cancel_after_a_committed_step_keeps_the_images() {
    let env = Env::new("img-cancel2");
    let store = MemoryStore::default();
    let scripted = scripted(vec![
        uses(&[call("a", "sleep", json!({"ms": 0, "text": "one\n"}))]),
        hanging(&[B::Text("partial")]),
    ]);
    let mut agent = image_agent(
        scripted.clone(),
        real_caps(),
        &env,
        Some(Box::new(store.clone())),
    );
    let (outcome, events) = image_turn(&mut agent, "look", &[RED], Some(on_text)).await;
    assert!(
        matches!(
            outcome,
            Some(TurnOutcome::Cancelled {
                committed_steps: 1,
                ..
            })
        ),
        "{outcome:?}"
    );
    check_events(&events);
    let history = agent.history();
    assert_eq!(history.len(), 3);
    assert!(history[0].content.contains(&sent_block(RED)));
    let rebuilt = rebuild(&store, log_events(&store).iter()).unwrap().messages;
    assert_eq!(rebuilt, *history);
}

#[tokio::test]
async fn an_image_turn_after_an_open_tail_joins_it_in_order() {
    let env = Env::new("img-tail");
    let store = MemoryStore::default();
    let scripted = scripted(vec![
        uses(&[call("a", "sleep", json!({"ms": 0, "text": "one\n"}))]),
        hanging(&[B::Text("partial")]),
        done("ok"),
    ]);
    let mut agent = image_agent(
        scripted.clone(),
        real_caps(),
        &env,
        Some(Box::new(store.clone())),
    );
    image_turn(&mut agent, "go", &[], Some(on_text)).await;
    assert_eq!(agent.history().last().unwrap().role, Role::User);
    let (outcome, _) = image_turn(&mut agent, "and this?", &[RED, BLUE], None).await;
    assert!(matches!(outcome, Some(TurnOutcome::Completed { .. })));
    let requests = scripted.sent();
    let tail = requests[2].messages.last().unwrap();
    let shown = texts(&tail.content);
    assert_eq!(shown.len(), 6, "{shown:?}");
    assert_eq!(shown[0], "result:a");
    assert!(shown[1].starts_with("Sent: "), "{shown:?}");
    assert!(shown[2].starts_with("Context: "), "{shown:?}");
    assert_eq!(tail.content[3], sent_block(RED));
    assert_eq!(tail.content[4], sent_block(BLUE));
    assert_eq!(shown[5], "and this?");
    check_layout(
        &requests[2].messages,
        &[],
        &Default::default(),
        scripted.as_ref(),
    )
    .unwrap();
    let rebuilt = rebuild(&store, log_events(&store).iter()).unwrap().messages;
    assert_eq!(rebuilt, &*agent.history());
}

#[tokio::test]
async fn too_many_images_or_a_model_without_images_is_refused() {
    let env = Env::new("img-caps");
    let store = MemoryStore::default();
    let scripted = scripted(Vec::new());
    let mut agent = image_agent(
        scripted.clone(),
        real_caps(),
        &env,
        Some(Box::new(store.clone())),
    );
    let eleven = [RED; 11];
    let (outcome, events) = image_turn(&mut agent, "many", &eleven, None).await;
    assert!(outcome.is_none());
    assert_eq!(
        reasons(&events),
        ["Too many images: a message can carry at most 10."]
    );

    for caps in [None, custom_caps(None)] {
        let mut agent = image_agent(scripted.clone(), caps, &env, None);
        let (outcome, events) = image_turn(&mut agent, "look", &[RED], None).await;
        assert!(outcome.is_none());
        assert_eq!(reasons(&events), [no_image_sentence(DEFAULT_MODEL)]);
        assert_eq!(agent.turns, 0);
    }
    assert!(scripted.sent().is_empty());
    assert!(store.blob_names().unwrap().is_empty());
    assert!(images_logged(&records(&store)).is_empty());
}

#[tokio::test]
async fn the_per_request_count_counts_images_already_in_history() {
    let env = Env::new("img-count");
    let caps = custom_caps(Some(ImageCaps {
        tier: Tier::HIGH,
        per_request: 2,
        many_images: 20,
        many_image_edge: 2000,
    }));
    let scripted = scripted(vec![done("ok")]);
    let mut agent = image_agent(scripted.clone(), caps, &env, None);
    let (outcome, _) = image_turn(&mut agent, "two", &[RED, BLUE], None).await;
    assert!(matches!(outcome, Some(TurnOutcome::Completed { .. })));
    let (outcome, events) = image_turn(&mut agent, "three", &[RED], None).await;
    assert!(outcome.is_none());
    assert_eq!(
        reasons(&events),
        [format!(
            "{DEFAULT_MODEL} takes at most 2 images in a request, and this conversation would send 3."
        )]
    );
    assert_eq!(scripted.sent().len(), 1);
}

#[tokio::test]
async fn past_the_many_image_threshold_each_image_must_fit_the_smaller_edge() {
    let env = Env::new("img-many");
    let caps = custom_caps(Some(ImageCaps {
        tier: Tier::HIGH,
        per_request: 600,
        many_images: 1,
        many_image_edge: 0,
    }));
    let scripted = scripted(vec![done("ok")]);
    let mut agent = image_agent(scripted.clone(), caps, &env, None);
    let (outcome, events) = image_turn(&mut agent, "two", &[RED, BLUE], None).await;
    assert!(outcome.is_none());
    assert_eq!(
        reasons(&events),
        ["With more than 1 images in a request, each must be at most 0 px on its long edge."]
    );
    let (outcome, _) = image_turn(&mut agent, "one", &[RED], None).await;
    assert!(matches!(outcome, Some(TurnOutcome::Completed { .. })));
}

#[test]
fn the_session_image_budget_counts_history_and_the_new_images() {
    let env = Env::new("img-budget");
    let caps = real_caps().unwrap().images.unwrap();
    let mut agent = image_agent(scripted(Vec::new()), real_caps(), &env, None);
    let image = |len: usize| PreparedImage {
        sha256: String::new(),
        media_type: ImageType::Png,
        bytes: Vec::new(),
        base64: "A".repeat(len),
        width: 1,
        height: 1,
    };
    let half = MAX_SESSION_MEDIA_BASE64 / 2;
    agent.history.push(Message {
        role: Role::User,
        content: vec![image(half).into_block()],
    });
    assert!(
        agent
            .check_caps(
                DEFAULT_MODEL,
                caps,
                &[image(MAX_SESSION_MEDIA_BASE64 - half)],
                None
            )
            .is_ok()
    );
    let refused = agent
        .check_caps(
            DEFAULT_MODEL,
            caps,
            &[image(MAX_SESSION_MEDIA_BASE64 - half + 1)],
            None,
        )
        .unwrap_err();
    assert_eq!(
        refused,
        "These images would take this conversation past 20 MB of images."
    );
}

#[tokio::test]
async fn an_image_that_fails_to_decode_is_rejected_before_the_turn_starts() {
    let env = Env::new("img-decode");
    let store = MemoryStore::default();
    let scripted = scripted(Vec::new());
    let mut agent = image_agent(
        scripted.clone(),
        real_caps(),
        &env,
        Some(Box::new(store.clone())),
    );
    let not_an_image = wisp_media::encode_base64(b"hello, world");
    let cases = [
        (vec!["@@@"], "Image 1: The image data isn't valid base64."),
        (
            vec![not_an_image.as_str()],
            "Image 1: This isn't a PNG, JPEG, GIF, WebP or BMP image.",
        ),
        (
            vec![RED, "iVBORw0KGgo="],
            "Image 2: The image is damaged and couldn't be read.",
        ),
    ];
    for (images, reason) in cases {
        let (outcome, events) = image_turn(&mut agent, "look", &images, None).await;
        assert!(outcome.is_none());
        assert_eq!(reasons(&events), [reason]);
    }
    assert_eq!(agent.turns, 0);
    assert!(scripted.sent().is_empty());
    assert!(store.blob_names().unwrap().is_empty());
    let kinds: Vec<&str> = log_events(&store)
        .iter()
        .map(|e| match e {
            LogEvent::SessionStarted { .. } => "started",
            LogEvent::Rejected { .. } => "rejected",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["started", "rejected", "rejected", "rejected"]);
}

#[tokio::test]
async fn an_image_alone_is_a_message_and_blank_text_alone_is_not() {
    let env = Env::new("img-only");
    let scripted = scripted(vec![done("ok")]);
    let mut agent = image_agent(scripted.clone(), real_caps(), &env, None);
    let (outcome, events) = image_turn(&mut agent, "  ", &[], None).await;
    assert!(outcome.is_none());
    assert_eq!(reasons(&events), ["empty message: nothing to send"]);
    let (outcome, _) = image_turn(&mut agent, "", &[RED], None).await;
    assert!(matches!(outcome, Some(TurnOutcome::Completed { .. })));
    let body: Value = serde_json::from_str(&scripted.texts()[0]).unwrap();
    let content = body["messages"][0]["content"].as_array().unwrap();
    let kinds: Vec<&str> = content
        .iter()
        .map(|b| b["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["text", "text", "text", "image"]);
}

#[tokio::test]
async fn an_unlogged_session_saves_no_images() {
    let env = Env::new("img-nolog");
    let scripted = scripted(vec![done("ok")]);
    let mut agent = image_agent(scripted.clone(), real_caps(), &env, None);
    let (outcome, _) = image_turn(&mut agent, "look", &[RED], None).await;
    assert!(matches!(outcome, Some(TurnOutcome::Completed { .. })));
    assert!(
        scripted.sent()[0].messages[0]
            .content
            .contains(&sent_block(RED))
    );
    let mut files = Vec::new();
    let mut dirs = vec![env.root.clone()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                files.push(path);
            }
        }
    }
    assert!(files.is_empty(), "{files:?}");
}

#[tokio::test]
async fn a_failing_store_turns_logging_off_and_still_sends_the_image() {
    let env = Env::new("img-full");
    let scripted = scripted(vec![done("ok")]);
    let mut agent = image_agent(
        scripted.clone(),
        real_caps(),
        &env,
        Some(Box::new(MemoryStore::failing())),
    );
    let (outcome, events) = image_turn(&mut agent, "look", &[RED], None).await;
    assert!(matches!(outcome, Some(TurnOutcome::Completed { .. })));
    let disabled: Vec<&str> = pick!(&events, AgentEvent::LogDisabled { reason } => reason.as_str());
    assert_eq!(disabled.len(), 1, "{disabled:?}");
    assert!(
        scripted.sent()[0].messages[0]
            .content
            .contains(&sent_block(RED))
    );
}

#[tokio::test]
async fn the_spawned_agent_takes_images_from_the_command() {
    let env = Env::new("img-spawn");
    let scripted = scripted(vec![done("ok")]);
    let agent = image_agent(scripted.clone(), real_caps(), &env, None);
    let (commands, mut events, _task) = spawn(agent);
    commands
        .send(ClientCommand::Send {
            id: wisp_protocol::InputId("look".into()),
            text: "look".into(),
            images: data(&[RED]),
            mode: wisp_protocol::DeliveryMode::Steer,
            turn: None,
            replaces: None,
        })
        .await
        .unwrap();
    until_turn_end(&mut events, &mut Vec::new()).await;
    drop(commands);
    while events.recv().await.is_some() {}
    assert!(
        scripted.sent()[0].messages[0]
            .content
            .contains(&sent_block(RED))
    );
}

#[test]
fn version_one_to_four_logs_still_load_and_rebuild() {
    let lines = [
        r#"{"schema_version":1,"seq":0,"at":"x","type":"turn_started","turn":1,"text":"a"}"#,
        r#"{"schema_version":1,"seq":1,"at":"x","type":"message_committed","turn":1,"message":{"role":"assistant","content":[{"type":"text","text":"A"}]}}"#,
        r#"{"schema_version":2,"seq":2,"at":"x","type":"turn_started","turn":2,"text":"b","stamp":"Sent: x\n"}"#,
        r#"{"schema_version":2,"seq":3,"at":"x","type":"message_committed","turn":2,"message":{"role":"assistant","content":[{"type":"text","text":"B"}]}}"#,
        r#"{"schema_version":3,"seq":4,"at":"x","type":"turn_started","turn":3,"text":"c","context":"<environment>\n</environment>\n"}"#,
        r#"{"schema_version":3,"seq":5,"at":"x","type":"message_committed","turn":3,"message":{"role":"assistant","content":[{"type":"text","text":"C"}]}}"#,
        r#"{"schema_version":4,"seq":6,"at":"x","type":"settings_changed","model":"claude-opus-5-5","effort":"low","cache":{"type":"kept_if_passed_on"}}"#,
        r#"{"schema_version":4,"seq":7,"at":"x","type":"turn_started","turn":4,"text":"d","model":"claude-opus-5-5","effort":"low","effort_marker":{"level":"low","rendered":true}}"#,
        r#"{"schema_version":4,"seq":8,"at":"x","type":"per_message_effort","model":"claude-opus-5-5","state":"fallen_back"}"#,
        r#"{"schema_version":4,"seq":9,"at":"x","type":"message_committed","turn":4,"message":{"role":"assistant","content":[{"type":"text","text":"D"}]}}"#,
    ];
    let records = crate::session::parse_jsonl(lines.join("\n") + "\n").unwrap();
    assert_eq!(records.len(), lines.len());
    assert!(matches!(
        records[8].event,
        LogEvent::PerMessageEffort {
            state: FeatureState::FallenBack,
            ..
        }
    ));
    let store = MemoryStore::default();
    let rebuilt = rebuild(&store, records.iter().map(|r| &r.event))
        .unwrap()
        .messages;
    let assistant = |t: &str| Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text { text: t.into() }],
    };
    assert_eq!(
        rebuilt,
        [
            Message::user_text("a"),
            assistant("A"),
            Message::user_stamped(Some("Sent: x\n"), "b"),
            assistant("B"),
            Message {
                role: Role::User,
                content: wisp_protocol::user_blocks_with_images(
                    Some("<environment>\n</environment>\n"),
                    None,
                    Vec::new(),
                    "c"
                ),
            },
            assistant("C"),
            Message::user_text("d"),
            Message::effort(EffortMarker {
                level: Effort::Low,
                rendered: false,
            }),
            assistant("D"),
        ]
    );
}

async fn switch_then_image(
    model: &str,
    image: &str,
) -> (Arc<Fake>, MemoryStore, Vec<AgentEvent>, Option<TurnOutcome>) {
    let env = Env::new("img-switch");
    let store = MemoryStore::default();
    let scripted = scripted(vec![done("ok")]);
    let mut agent = provider_agent(
        scripted.view(|k| {
            k.caps = Some(Caps::Fn(capabilities::row));
        }),
        &env,
        Some(Box::new(store.clone())),
    );
    let (ev_tx, mut ev_rx) = mpsc::channel(64);
    agent
        .change_settings(None, Some(model.to_owned()), None, &ev_tx)
        .await;
    drop(ev_tx);
    let mut changed = Vec::new();
    while let Some(e) = ev_rx.recv().await {
        changed.push(e);
    }
    assert!(
        changed
            .iter()
            .any(|e| matches!(e, AgentEvent::SettingsChanged { model: m, .. } if m == model)),
        "{changed:#?}"
    );
    let (outcome, events) = image_turn(&mut agent, "look", &[image], None).await;
    (scripted, store, events, outcome)
}

#[tokio::test]
async fn images_are_sized_for_the_model_the_turn_switches_to() {
    let wide = png_base64(2000, 10);
    let (scripted, store, events, outcome) = switch_then_image("claude-sonnet-4-6", &wide).await;
    assert!(
        matches!(outcome, Some(TurnOutcome::Completed { .. })),
        "{events:#?}"
    );
    let requests = scripted.sent();
    assert_eq!(requests[0].session.model, "claude-sonnet-4-6");
    let image = requests[0].messages[0]
        .content
        .iter()
        .find(|b| matches!(b, ContentBlock::Image { .. }))
        .unwrap();
    let (width, _) = sent_size(image);
    assert!(width <= Tier::STANDARD.max_edge, "{width}");
    let logged = images_logged(&records(&store));
    assert_eq!(logged[0][0].width, width);
}

#[tokio::test]
async fn a_pending_switch_to_a_model_without_a_row_refuses_images() {
    let (scripted, store, events, outcome) = switch_then_image("claude-unknown-9", RED).await;
    assert!(outcome.is_none(), "{outcome:?}");
    assert_eq!(reasons(&events), [no_image_sentence("claude-unknown-9")]);
    assert!(scripted.sent().is_empty());
    assert!(store.blob_names().unwrap().is_empty());
}

#[test]
fn images_inside_tool_results_count_toward_the_request() {
    let env = Env::new("img-results");
    let caps = ImageCaps {
        per_request: 1,
        ..real_caps().unwrap().images.unwrap()
    };
    let mut agent = image_agent(scripted(Vec::new()), real_caps(), &env, None);
    let image = PreparedImage {
        sha256: String::new(),
        media_type: ImageType::Png,
        bytes: Vec::new(),
        base64: "AAAA".into(),
        width: 1,
        height: 1,
    };
    assert_eq!(
        agent.check_caps(DEFAULT_MODEL, caps, std::slice::from_ref(&image), None),
        Ok(())
    );
    agent.history.push(Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "t".into(),
            content: vec![image.clone().into_block()],
            is_error: false,
        }],
    });
    assert_eq!(
        agent.check_caps(DEFAULT_MODEL, caps, &[image], None),
        Err(format!(
            "{DEFAULT_MODEL} takes at most 1 images in a request, and this conversation would send 2."
        ))
    );
}

#[tokio::test]
async fn images_are_checked_against_the_pending_endpoints_provider() {
    let env = Env::new("img-endpoint");
    for (a_caps, b_caps, accepted) in [
        (real_caps(), custom_caps(None), false),
        (custom_caps(None), real_caps(), true),
    ] {
        let (a, b) = (scripted(vec![done("ok")]), scripted(vec![done("ok")]));
        let mut agent = image_agent(a.clone(), a_caps, &env, None).with_connections(
            vec![
                seeing_connection("a", &a, a_caps),
                seeing_connection("b", &b, b_caps),
            ],
            Vec::new(),
            "a",
        );
        let (ev_tx, mut ev_rx) = mpsc::channel(16);
        agent
            .change_settings(Some("b".into()), None, None, &ev_tx)
            .await;
        drop(ev_tx);
        while ev_rx.recv().await.is_some() {}
        let (outcome, events) = image_turn(&mut agent, "look", &[RED], None).await;
        assert!(a.sent().is_empty());
        if accepted {
            assert!(
                matches!(outcome, Some(TurnOutcome::Completed { .. })),
                "{outcome:?}"
            );
            assert!(reasons(&events).is_empty());
            assert_eq!(b.sent().len(), 1);
        } else {
            assert!(outcome.is_none(), "{outcome:?}");
            assert_eq!(reasons(&events), [no_image_sentence(DEFAULT_MODEL)]);
            assert!(b.sent().is_empty());
        }
    }
}

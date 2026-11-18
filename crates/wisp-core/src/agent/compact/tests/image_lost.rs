use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use wisp_protocol::Mime;
use wisp_protocol::log::{ImageRef, LogRecord};

use super::*;
use crate::session::{JsonlStore, Line, ResumeLock, reread};
use crate::test_guard::{self, BLUE_PIXEL as BLUE, RED_PIXEL as RED, records};

fn agent_on(provider: Arc<Fake>, store: Box<dyn SessionStore>, seq: Option<u64>) -> Agent {
    agent_in(provider, store, "s1", seq)
}

fn agent_in(
    provider: Arc<Fake>,
    store: Box<dyn SessionStore>,
    id: &str,
    seq: Option<u64>,
) -> Agent {
    let log = match seq {
        Some(seq) => SessionLog::continuing(store, id, seq),
        None => SessionLog::new(store, id),
    };
    Agent::new(provider, default_session()).with_log(log, log_context())
}

async fn with_image() -> (Vec<LogRecord>, MemoryStore, ImageRef) {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("A red pixel.", 1_000)]);
    let mut live = Live::start(agent_on(provider, Box::new(store.clone()), None));
    live.send(ClientCommand::Send {
        id: InputId("u1".into()),
        text: "what is this?".into(),
        images: vec![wisp_protocol::ImageData { data: RED.into() }],
        mode: DeliveryMode::Steer,
        turn: None,
        replaces: None,
    })
    .await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    live.close().await;
    let records = records(&store);
    let image = records
        .iter()
        .find_map(|r| match &r.event {
            LogEvent::TurnStarted { images, .. } => images.first().cloned(),
            _ => None,
        })
        .unwrap();
    (records, store, image)
}

fn lines(records: &[LogRecord]) -> Vec<Line> {
    reread(records).unwrap()
}

async fn resumed_from(store: MemoryStore, steps: Vec<Step>) -> (Agent, Vec<AgentEvent>) {
    let records = records(&store);
    let seq = u64::try_from(records.len()).unwrap();
    let mut agent = agent_on(scripted(steps), Box::new(store), Some(seq))
        .resume_lines(lines(&records), 0)
        .unwrap();
    let (tx, mut rx) = mpsc::channel(4096);
    agent.start_log(&tx).await;
    drop(tx);
    let mut shown = Vec::new();
    while let Some(e) = rx.recv().await {
        shown.push(e);
    }
    (agent, shown)
}

fn lost(records: &[LogRecord]) -> Vec<(u64, String, String)> {
    records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::ImageLost { turn, sha256, note } => {
                Some((*turn, sha256.clone(), note.clone()))
            }
            _ => None,
        })
        .collect()
}

fn texts(agent: &Agent) -> String {
    serde_json::to_string(&*agent.history()).unwrap()
}

#[tokio::test]
async fn a_missing_image_becomes_a_note_and_the_resume_goes_on() {
    let (records, _, image) = with_image().await;
    let store = stored(&records);
    let (agent, _) = resumed_from(store.clone(), Vec::new()).await;
    let after = test_guard::records(&store);
    let found = lost(&after);
    assert_eq!(found.len(), 1, "{after:#?}");
    assert_eq!(found[0].0, 1);
    assert_eq!(found[0].1, image.sha256);
    assert!(
        found[0]
            .2
            .starts_with("[image no longer available: image/png"),
        "{}",
        found[0].2
    );
    assert!(
        !after
            .iter()
            .any(|r| matches!(r.event, LogEvent::ResumeFellBack { .. })),
        "an image is repaired, never a fallback"
    );
    let history = texts(&agent);
    assert!(history.contains("image no longer available"), "{history}");
    assert!(!history.contains(RED), "{history}");

    let (again, _) = resumed_from(store.clone(), Vec::new()).await;
    assert_eq!(again.history(), agent.history());
    assert_eq!(lost(&test_guard::records(&store)).len(), 1);
}

fn base64_decode(text: &str) -> Vec<u8> {
    wisp_media::decode_base64(text).unwrap()
}

struct Dir(PathBuf, String);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn on_disk(
    records: &[LogRecord],
    memory: &MemoryStore,
    image: &ImageRef,
    name: &str,
) -> (Dir, PathBuf) {
    let root = std::env::temp_dir().join(format!("wisp-image-lost-{}-{name}", std::process::id()));
    let id = crate::session::new_session_id();
    let _ = fs::remove_dir_all(&root);
    let mut store = JsonlStore::open(root.join("sessions")).unwrap();
    for record in records {
        store.append(&id, record).unwrap();
    }
    let media_type: Mime = image.media_type.into();
    let blob = memory.get_blob(&image.sha256, &media_type).unwrap();
    store
        .put_blob(&image.sha256, &blob.media_type, &blob.bytes)
        .unwrap();
    let path = store.blob_path(&image.sha256, &media_type).unwrap();
    (Dir(root, id), path)
}

async fn reopened_disk(dir: &Dir, steps: Vec<Step>) -> Agent {
    let mut store = JsonlStore::open(dir.0.join("sessions")).unwrap();
    let reopened = store.reopen(&dir.1, ResumeLock::Optional).unwrap();
    agent_in(
        scripted(steps),
        Box::new(store),
        &dir.1,
        Some(reopened.next_seq),
    )
    .resume_lines(reopened.lines, reopened.truncated_bytes)
    .unwrap()
}

fn disk_records(dir: &Dir) -> Vec<LogRecord> {
    JsonlStore::open(dir.0.join("sessions"))
        .unwrap()
        .read(&dir.1)
        .unwrap()
}

#[tokio::test]
async fn an_unreadable_image_waits_for_the_first_request_before_it_is_lost() {
    let (records, memory, image) = with_image().await;
    let (dir, blob) = on_disk(&records, &memory, &image, "unreadable");
    fs::set_permissions(&blob, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&blob).is_ok() {
        return;
    }

    let mut quit = reopened_disk(&dir, Vec::new()).await;
    let notice = quit.resume_notice_text().unwrap_or_default().to_owned();
    assert!(notice.contains("can't be read right now"), "{notice}");
    let (tx, mut rx) = mpsc::channel(4096);
    quit.start_log(&tx).await;
    drop(tx);
    while rx.recv().await.is_some() {}
    drop(quit);
    assert!(
        lost(&disk_records(&dir)).is_empty(),
        "quitting writes nothing"
    );

    fs::set_permissions(&blob, fs::Permissions::from_mode(0o600)).unwrap();
    let fixed = reopened_disk(&dir, Vec::new()).await;
    let history = texts(&fixed);
    assert!(
        !history.contains("no longer available"),
        "the next resume tries the file again: {history}"
    );
    drop(fixed);

    fs::set_permissions(&blob, fs::Permissions::from_mode(0o000)).unwrap();
    let sent = reopened_disk(&dir, vec![answer("Gone.", 1_000)]).await;
    let mut live = Live::start(sent);
    assert!(completed(&live.turn("u2", "and now?").await));
    live.close().await;
    fs::set_permissions(&blob, fs::Permissions::from_mode(0o600)).unwrap();
    let after = disk_records(&dir);
    assert_eq!(lost(&after).len(), 1, "{after:#?}");
    let lost_at = after
        .iter()
        .position(|r| matches!(r.event, LogEvent::ImageLost { .. }))
        .unwrap();
    let resumed_at = after
        .iter()
        .rposition(|r| matches!(r.event, LogEvent::SessionResumed { .. }))
        .unwrap();
    let step_at = after
        .iter()
        .rposition(|r| matches!(r.event, LogEvent::MessageCommitted { .. }))
        .unwrap();
    assert!(resumed_at < lost_at && lost_at < step_at, "{after:#?}");
}

#[tokio::test]
async fn a_swapped_image_becomes_a_note_too() {
    let (records, memory, image) = with_image().await;
    let (dir, blob) = on_disk(&records, &memory, &image, "swapped");
    fs::set_permissions(&blob, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&blob, base64_decode(BLUE)).unwrap();
    let agent = reopened_disk(&dir, Vec::new()).await;
    let live = Live::start(agent);
    let (agent, _) = {
        live.send(ClientCommand::DescribeSettings {
            request: wisp_protocol::RequestId(1),
        })
        .await;
        live.close().await
    };
    let after = disk_records(&dir);
    assert_eq!(lost(&after).len(), 1, "{after:#?}");
    assert!(texts(&agent).contains("image no longer available"));
}

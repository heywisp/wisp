use wisp_protocol::{Effort, ImageData};

use super::marker::{change, image_refused};
use super::*;
use crate::test_guard::{
    BLUE_PIXEL as BLUE, GREEN_PIXEL as GREEN, YELLOW_PIXEL as YELLOW, records,
};

fn with_image(id: &str, text: &str, data: &str) -> ClientCommand {
    ClientCommand::Send {
        id: InputId(id.into()),
        text: text.into(),
        images: vec![ImageData { data: data.into() }],
        mode: DeliveryMode::Steer,
        turn: None,
        replaces: None,
    }
}

fn empty_step(gate: &Arc<Semaphore>) -> Step {
    let mut empty = vec![start(600, 0)];
    empty.extend(finish(StopReason::StopSequence, 600, 0, 0));
    Step::from(empty).gated(gate)
}

fn withdrawn_ordinals(seen: &[AgentEvent]) -> Vec<Vec<usize>> {
    pick!(
        seen,
        AgentEvent::AttachmentsWithdrawn { items, .. } => items.iter().map(|i| i.ordinal).collect()
    )
}

fn logged_ordinals(store: &MemoryStore) -> Vec<Vec<usize>> {
    pick!(
        &log_events(store),
        LogEvent::AttachmentsWithdrawn { items, .. } => items.iter().map(|i| i.ordinal).collect()
    )
}

async fn steer_images(live: &mut Live, steers: &[(&str, &str)]) {
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    for (id, data) in steers {
        live.send(with_image(id, "a picture", data)).await;
        let want = InputId((*id).into());
        live.until(|e| matches!(e, AgentEvent::InputAccepted { id, .. } if *id == want))
            .await;
    }
}

fn check(store: &MemoryStore, agent: &Agent, seen: &[AgentEvent], ordinals: &[Vec<usize>]) {
    assert_eq!(withdrawn_ordinals(seen), ordinals);
    assert_eq!(logged_ordinals(store), ordinals);
    let rebuilt = crate::session::rebuild(store, log_events(store).iter());
    let rebuilt = match rebuilt {
        Ok(r) => r,
        Err(e) => panic!("rebuild failed: {e:?}\nlog {:#?}", log_events(store)),
    };
    assert_eq!(rebuilt.messages, *agent.history());
    assert_eq!(rebuilt.view(), agent.view_messages());
    let live = serde_json::to_vec(&agent.history()).unwrap();
    let back = serde_json::to_vec(&rebuilt.messages).unwrap();
    assert_eq!(live, back);
}

fn ended_ok(ended: &AgentEvent) {
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
}

async fn late_marker_turn(
    steps: Vec<Step>,
    gate: &Arc<Semaphore>,
    steers: &[(&str, &str)],
    ordinals: &[Vec<usize>],
) -> MemoryStore {
    let store = MemoryStore::default();
    let mut script = vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Low.", 300),
    ];
    script.extend(steps);
    let provider = scripted(script);
    let mut live = Live::start(logged(provider.clone(), &store));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    change(&mut live, 3, None, Some(Effort::Low)).await;
    assert!(completed(&live.turn("u2", "second").await));
    live.send(with_image("u3", "third", BLUE)).await;
    steer_images(&mut live, steers).await;
    gate.add_permits(1);
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (agent, seen) = live.close().await;
    ended_ok(&ended);
    check(&store, &agent, &seen, ordinals);
    store
}

#[tokio::test]
async fn late_marker_turn_image_and_two_steer_images_withdrawn_together() {
    let gate = Arc::new(Semaphore::new(0));
    let steps = vec![
        empty_step(&gate),
        block_refused(),
        image_refused(),
        answer("All.", 500),
    ];
    let steers = [("u4", GREEN), ("u5", YELLOW)];
    late_marker_turn(steps, &gate, &steers, &[vec![0, 1, 2]]).await;
}

#[tokio::test]
async fn late_marker_turn_image_withdrawn_first_then_one_steer_image() {
    let gate = Arc::new(Semaphore::new(0));
    let steps = vec![
        image_refused(),
        empty_step(&gate),
        block_refused(),
        image_refused(),
        answer("All.", 500),
    ];
    let steers = [("u4", GREEN)];
    late_marker_turn(steps, &gate, &steers, &[vec![0], vec![1]]).await;
}

#[tokio::test]
async fn late_marker_turn_image_withdrawn_first_then_two_steer_images() {
    let gate = Arc::new(Semaphore::new(0));
    let steps = vec![
        image_refused(),
        empty_step(&gate),
        block_refused(),
        image_refused(),
        answer("All.", 500),
    ];
    let steers = [("u4", GREEN), ("u5", YELLOW)];
    late_marker_turn(steps, &gate, &steers, &[vec![0], vec![1, 2]]).await;
}

async fn steer_during(live: &mut Live, step: u32, id: &str, data: &str) {
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: s } if *s == step))
        .await;
    live.send(with_image(id, "a picture", data)).await;
    let want = InputId(id.into());
    live.until(|e| matches!(e, AgentEvent::InputAccepted { id, .. } if *id == want))
        .await;
}

async fn convert_between_withdrawals(late_marker: bool) {
    let store = MemoryStore::default();
    let first = Arc::new(Semaphore::new(0));
    let second = Arc::new(Semaphore::new(0));
    let mut script = vec![answer("Hello.", 1_000), compaction(SUMMARY)];
    if late_marker {
        script.push(answer("Low.", 300));
    }
    script.extend([
        empty_step(&first),
        image_refused(),
        empty_step(&second),
        block_refused(),
        image_refused(),
        answer("All.", 500),
    ]);
    let provider = scripted(script);
    let mut live = Live::start(logged(provider.clone(), &store));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    if late_marker {
        change(&mut live, 3, None, Some(Effort::Low)).await;
        assert!(completed(&live.turn("u2", "second").await));
    }
    live.send(with_image("u3", "third", BLUE)).await;
    steer_during(&mut live, 1, "u4", GREEN).await;
    first.add_permits(1);
    steer_during(&mut live, 2, "u5", YELLOW).await;
    second.add_permits(1);
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (agent, seen) = live.close().await;
    ended_ok(&ended);
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Convert,
            ..
        }
    )));
    check(&store, &agent, &seen, &[vec![1], vec![0, 2]]);
}

#[tokio::test]
async fn a_convert_between_two_withdrawals_numbers_every_logged_image() {
    convert_between_withdrawals(false).await;
}

#[tokio::test]
async fn a_convert_with_a_late_marker_between_two_withdrawals_numbers_every_logged_image() {
    convert_between_withdrawals(true).await;
}

async fn image_refused_then_block(steps: Vec<Step>, ordinals: &[Vec<usize>]) -> Agent {
    let store = MemoryStore::default();
    let mut script = vec![answer("Hello.", 1_000), compaction(SUMMARY)];
    script.extend([image_refused(), block_refused()]);
    script.extend(steps);
    let provider = scripted(script);
    let mut live = Live::start(logged(provider.clone(), &store));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    live.send(with_image("u2", "second", BLUE)).await;
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (agent, seen) = live.close().await;
    ended_ok(&ended);
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Convert,
            ..
        }
    )));
    check(&store, &agent, &seen, ordinals);
    agent
}

fn holds_image(agent: &Agent) -> bool {
    agent
        .history
        .iter()
        .flat_map(|m| &m.content)
        .any(|b| matches!(b, ContentBlock::Image { .. }))
}

#[tokio::test]
async fn a_withdrawal_waiting_at_a_convert_is_undone_and_the_image_sent_again() {
    let agent = image_refused_then_block(vec![answer("Seen.", 500)], &[]).await;
    assert!(holds_image(&agent));
}

#[tokio::test]
async fn a_withdrawal_waiting_at_a_convert_is_redone_against_the_new_view() {
    let steps = vec![image_refused(), answer("Unseen.", 500)];
    let agent = image_refused_then_block(steps, &[vec![0]]).await;
    assert!(!holds_image(&agent));
}

#[tokio::test]
async fn an_altered_withdrawn_block_falls_back_before_its_turn() {
    let gate = Arc::new(Semaphore::new(0));
    let steps = vec![
        empty_step(&gate),
        block_refused(),
        image_refused(),
        answer("All.", 500),
    ];
    let steers = [("u4", GREEN), ("u5", YELLOW)];
    let store = late_marker_turn(steps, &gate, &steers, &[vec![0, 1, 2]]).await;
    let mut records = records(&store);
    let withdrawn = records
        .iter_mut()
        .find_map(|r| match &mut r.event {
            LogEvent::AttachmentsWithdrawn { turn, items, .. } => Some((*turn, items)),
            _ => None,
        })
        .unwrap();
    let turn = withdrawn.0;
    withdrawn.1[0].sha256 = "ff".repeat(32);
    let copy = store.sharing_blobs();
    let mut writer = copy.clone();
    for record in &records {
        writer.append("s1", record).unwrap();
    }
    let lines = crate::session::reread(&records).unwrap();
    let seq = u64::try_from(records.len()).unwrap();
    let mut agent = Agent::new(scripted(Vec::new()), default_session())
        .with_log(
            SessionLog::continuing(Box::new(copy.clone()), "s1", seq),
            log_context(),
        )
        .resume_lines(lines, 0)
        .unwrap();
    let (tx, mut rx) = mpsc::channel(4096);
    agent.start_log(&tx).await;
    drop(tx);
    while rx.recv().await.is_some() {}
    let fell: Vec<LogEvent> = log_events(&copy)
        .into_iter()
        .filter(|e| matches!(e, LogEvent::ResumeFellBack { .. }))
        .collect();
    let [
        LogEvent::ResumeFellBack {
            last_turn, damage, ..
        },
    ] = fell.as_slice()
    else {
        panic!("{fell:#?}");
    };
    assert_eq!(*last_turn, Some(turn - 1));
    assert_eq!(
        damage.last().map(|d| d.kind),
        Some(wisp_protocol::log::DamageKind::WithdrawnBlock)
    );
}

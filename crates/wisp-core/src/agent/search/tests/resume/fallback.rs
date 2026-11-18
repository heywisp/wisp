use std::path::{Path, PathBuf};

use super::cut::{ID, full_log, well_formed};
use super::*;
use crate::session::{JsonlStore, ResumeLock};

struct Back {
    agent: Agent,
    rx: mpsc::Receiver<ClientCommand>,
    provider: Arc<Fake>,
    path: PathBuf,
    shown: Vec<AgentEvent>,
}

impl Back {
    async fn turn(&mut self, text: &str) -> TurnOutcome {
        let (ev_tx, mut ev_rx) = mpsc::channel(4096);
        let drain = tokio::spawn(async move { while ev_rx.recv().await.is_some() {} });
        let outcome = self
            .agent
            .run_turn(text.into(), &mut self.rx, &ev_tx)
            .await
            .unwrap();
        drop(ev_tx);
        drain.await.unwrap();
        outcome
    }

    fn body(&self) -> Value {
        self.provider.bodies().last().cloned().unwrap()
    }
}

async fn reopen(dir: &Path, steps: Vec<Step>, env: &ScratchDir) -> Result<Back, String> {
    let mut store = JsonlStore::open(dir).map_err(|e| e.to_string())?;
    let path = store.path(ID);
    let reopened = store
        .reopen(ID, ResumeLock::Optional)
        .map_err(|e| format!("reopen: {e}"))?;
    let (tx, rx) = mpsc::channel(64);
    let provider = searching(steps, tx);
    let mut agent = base(provider.clone(), env, true)
        .with_log(
            SessionLog::continuing(Box::new(store), ID, reopened.next_seq),
            log_context(),
        )
        .resume_lines(reopened.lines, reopened.truncated_bytes)
        .map_err(|e| format!("resume: {e}"))?;
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    agent.start_log(&ev_tx).await;
    drop(ev_tx);
    let mut shown = Vec::new();
    while let Some(e) = ev_rx.recv().await {
        shown.push(e);
    }
    Ok(Back {
        agent,
        rx,
        provider,
        path,
        shown,
    })
}

async fn written(
    env: &ScratchDir,
    name: &str,
    bytes: &[u8],
    steps: Vec<Step>,
) -> Result<Back, String> {
    let dir = env.root.join(name);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let store = JsonlStore::open(&dir).map_err(|e| e.to_string())?;
    fs::write(store.path(ID), bytes).map_err(|e| e.to_string())?;
    drop(store);
    reopen(&dir, steps, env).await
}

fn lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes
        .split_inclusive(|b| *b == b'\n')
        .filter(|l| l.len() > 1)
        .collect()
}

fn garbled(bytes: &[u8], line: usize) -> Vec<u8> {
    lines(bytes)
        .iter()
        .enumerate()
        .flat_map(|(i, l)| {
            if i == line {
                b"#not json#\n".to_vec()
            } else {
                l.to_vec()
            }
        })
        .collect()
}

fn parsed(bytes: &[u8]) -> Vec<Option<LogRecord>> {
    lines(bytes)
        .iter()
        .map(|l| serde_json::from_slice(l).ok())
        .collect()
}

fn fell_back(path: &Path) -> Vec<LogEvent> {
    parsed(&fs::read(path).unwrap())
        .into_iter()
        .flatten()
        .filter(|r| matches!(r.event, LogEvent::ResumeFellBack { .. }))
        .map(|r| r.event)
        .collect()
}

fn ended_before(bytes: &[u8], line: usize) -> Option<u64> {
    parsed(bytes)[..line]
        .iter()
        .flatten()
        .filter_map(|r| match r.event {
            LogEvent::TurnEnded { turn, .. } => Some(turn),
            _ => None,
        })
        .max()
}

fn first_line_of_turn(bytes: &[u8], turn: u64, kind: &str) -> usize {
    parsed(bytes)
        .iter()
        .position(|r| {
            r.as_ref().is_some_and(|r| {
                let v = serde_json::to_value(r).unwrap();
                v["turn"] == turn && v["type"] == kind
            })
        })
        .unwrap()
}

fn last_turn(event: &LogEvent) -> Option<u64> {
    match event {
        LogEvent::ResumeFellBack { last_turn, .. } => *last_turn,
        _ => None,
    }
}

async fn live_through_compaction() -> (Value, Value) {
    let mut live = live_rig(
        vec![
            uses(&[B::Think("plan"), B::Search("s1", "rust"), echo("e1")]),
            done(&[B::Found("s1"), B::Text("found")]),
            uses(&[
                B::Text("running"),
                B::Call("c1", "sleep", json!({"ms": 0, "text": "slept\n"})),
                echo("e2"),
            ]),
            done(&[B::Text("ran")]),
            compacted(),
            plain_done(),
            plain_done(),
        ],
        true,
    );
    live.turn("go").await;
    live.turn("more").await;
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    live.agent
        .manual_compaction(None, wisp_protocol::RequestId(1), &mut live.rx, &ev_tx)
        .await;
    drop(ev_tx);
    while ev_rx.recv().await.is_some() {}
    live.turn("one more").await;
    let first = live.provider.bodies().last().cloned().unwrap();
    live.turn("again").await;
    (first, live.provider.bodies().last().cloned().unwrap())
}

#[tokio::test]
async fn a_corrupted_middle_record_resumes_from_the_turn_before_it() {
    let (live, bytes) = full_log().await;
    let line = first_line_of_turn(&bytes, 3, "turn_started") + 1;
    let mut back = written(
        &live._env,
        "middle",
        &garbled(&bytes, line),
        vec![plain_done()],
    )
    .await
    .unwrap();
    let records = fell_back(&back.path);
    let [
        LogEvent::ResumeFellBack {
            last_turn,
            turns_seen,
            damage,
            ..
        },
    ] = records.as_slice()
    else {
        panic!("{records:#?}");
    };
    assert_eq!(*last_turn, Some(2));
    assert_eq!(*turns_seen, 4);
    assert_eq!(damage.len(), 1);
    assert!(
        back.shown
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionResumed { turns: 4, .. })),
        "{:#?}",
        back.shown
    );
    assert!(matches!(
        back.turn("one more").await,
        TurnOutcome::Completed { .. }
    ));
    let (first, second) = live_through_compaction().await;
    assert_eq!(back.body(), first);
    let dir = back.path.parent().unwrap().to_path_buf();
    drop(back);

    let mut again = reopen(&dir, vec![plain_done()], &live._env).await.unwrap();
    assert_eq!(
        fell_back(&again.path).len(),
        1,
        "a second resume writes no record"
    );
    assert!(
        fs::read(&again.path)
            .unwrap()
            .windows(10)
            .any(|w| w == b"#not json#")
    );
    assert!(matches!(
        again.turn("again").await,
        TurnOutcome::Completed { .. }
    ));
    assert_eq!(again.body(), second);
}

#[tokio::test]
async fn every_garbled_line_resumes_once_and_a_second_resume_matches() {
    let (live, bytes) = full_log().await;
    let count = lines(&bytes).len();
    let head = parsed(&bytes)
        .iter()
        .position(|r| {
            r.as_ref()
                .is_some_and(|r| matches!(r.event, LogEvent::ContextCompacted { .. }))
        })
        .unwrap();
    let mut failures = Vec::new();
    for line in 0..count {
        let name = format!("fuzz-{line}");
        let first = match written(&live._env, &name, &garbled(&bytes, line), Vec::new()).await {
            Ok(back) => back,
            Err(why) => {
                failures.push(format!("line {line}: {why}"));
                continue;
            }
        };
        let records = fell_back(&first.path);
        if records.len() != 1 {
            failures.push(format!("line {line}: {} fallback records", records.len()));
            continue;
        }
        let expected = match line {
            0 => None,
            covered if covered < head => ended_before(&bytes, count),
            _ => ended_before(&bytes, line),
        };
        if last_turn(&records[0]) != expected {
            failures.push(format!(
                "line {line}: kept through {:?}, expected {expected:?}",
                last_turn(&records[0])
            ));
        }
        let history = first.agent.history().to_vec();
        drop(first);
        let dir = live._env.root.join(&name);
        match reopen(&dir, Vec::new(), &live._env).await {
            Ok(second) => {
                if fell_back(&second.path).len() != 1 {
                    failures.push(format!("line {line}: the second resume fell back again"));
                }
                if second.agent.history() != history.as_slice() {
                    failures.push(format!(
                        "line {line}: the second resume rebuilt other history"
                    ));
                }
            }
            Err(why) => failures.push(format!("line {line}: second resume: {why}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn a_garbled_start_begins_a_new_conversation_in_the_same_file() {
    let (live, bytes) = full_log().await;
    let mut back = written(&live._env, "fresh", &garbled(&bytes, 0), vec![plain_done()])
        .await
        .unwrap();
    let after: Vec<String> = parsed(&fs::read(&back.path).unwrap())
        .into_iter()
        .skip(lines(&bytes).len())
        .flatten()
        .map(|r| {
            serde_json::to_value(&r.event).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        after[..2],
        ["resume_fell_back", "session_started"],
        "{after:#?}"
    );
    assert!(back.agent.history().is_empty());
    assert!(
        back.shown
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionResumed { .. })),
        "{:#?}",
        back.shown
    );
    assert!(matches!(
        back.turn("hello").await,
        TurnOutcome::Completed { .. }
    ));
    well_formed(&back.body()).unwrap();
}

#[tokio::test]
async fn a_crash_before_a_fresh_start_s_session_started_starts_fresh_again() {
    let (live, bytes) = full_log().await;
    let first = written(&live._env, "fresh-crash", &garbled(&bytes, 0), Vec::new())
        .await
        .unwrap();
    let path = first.path.clone();
    let turns_seen = match fell_back(&path).as_slice() {
        [LogEvent::ResumeFellBack { turns_seen, .. }] => *turns_seen,
        other => panic!("{other:#?}"),
    };
    drop(first);
    let kept = lines(&bytes).len() + 1;
    let crashed: Vec<u8> = lines(&fs::read(&path).unwrap())[..kept].concat();
    fs::write(&path, &crashed).unwrap();
    let dir = path.parent().unwrap().to_path_buf();

    let mut again = reopen(&dir, vec![plain_done()], &live._env).await.unwrap();
    assert!(again.agent.history().is_empty());
    let after: Vec<String> = parsed(&fs::read(&again.path).unwrap())
        .into_iter()
        .skip(kept)
        .flatten()
        .map(|r| {
            serde_json::to_value(&r.event).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        after[..2],
        ["resume_fell_back", "session_started"],
        "{after:#?}"
    );
    assert!(matches!(
        again.turn("hello").await,
        TurnOutcome::Completed { .. }
    ));
    well_formed(&again.body()).unwrap();
    let turns: Vec<u64> = parsed(&fs::read(&again.path).unwrap())
        .into_iter()
        .skip(kept)
        .flatten()
        .filter_map(|r| match r.event {
            LogEvent::TurnStarted { turn, .. } => Some(turn),
            _ => None,
        })
        .collect();
    assert_eq!(turns, [turns_seen + 1]);
    drop(again);

    let third = reopen(&dir, Vec::new(), &live._env).await.unwrap();
    assert_eq!(
        fell_back(&third.path).len(),
        2,
        "the third resume writes no record"
    );
    assert_eq!(third.agent.history().len(), 2);
}

#[tokio::test]
async fn a_tail_of_nul_bytes_falls_back() {
    let (live, bytes) = full_log().await;
    let mut damaged = bytes.clone();
    damaged.extend_from_slice(&[0; 40]);
    damaged.push(b'\n');
    let back = written(&live._env, "nul", &damaged, Vec::new())
        .await
        .unwrap();
    assert_eq!(fell_back(&back.path).len(), 1);
}

#[tokio::test]
async fn a_newer_line_inside_the_damage_still_refuses() {
    let (live, bytes) = full_log().await;
    let line = first_line_of_turn(&bytes, 3, "turn_started") + 1;
    let mut damaged = garbled(&bytes, line + 1);
    let newer = format!(
        "{{\"schema_version\":{},\"seq\":999,\"at\":\"x\",\"type\":\"session_ended\",\"turns\":0}}\n",
        wisp_protocol::log::LOG_SCHEMA_VERSION + 1
    );
    damaged.extend_from_slice(newer.as_bytes());
    assert!(
        written(&live._env, "newer", &damaged, Vec::new())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_deleted_line_falls_back_and_numbering_continues_past_dropped_turns() {
    let (live, bytes) = full_log().await;
    let line = first_line_of_turn(&bytes, 3, "turn_started") + 1;
    let deleted: Vec<u8> = lines(&bytes)
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != line)
        .flat_map(|(_, l)| l.to_vec())
        .collect();
    let mut back = written(&live._env, "deleted", &deleted, vec![plain_done()])
        .await
        .unwrap();
    let records = fell_back(&back.path);
    let [
        LogEvent::ResumeFellBack {
            damage, spent_usd, ..
        },
    ] = records.as_slice()
    else {
        panic!("{records:#?}");
    };
    assert_eq!(damage[0].kind, wisp_protocol::log::DamageKind::OutOfOrder);
    assert!(*spent_usd >= 0.0);
    back.turn("next").await;
    let turns: Vec<u64> = parsed(&fs::read(&back.path).unwrap())
        .into_iter()
        .flatten()
        .filter_map(|r| match r.event {
            LogEvent::TurnStarted { turn, .. } => Some(turn),
            _ => None,
        })
        .collect();
    assert_eq!(turns.last(), Some(&5), "{turns:?}");
}

async fn two_tool_turns() -> (Rig, Vec<u8>) {
    let mut live = live_rig(
        vec![
            uses(&[B::Text("one"), echo("e1")]),
            done(&[B::Text("first done")]),
            uses(&[B::Text("two"), echo("e2")]),
            done(&[B::Text("second done")]),
        ],
        false,
    );
    assert!(matches!(
        live.turn("go").await.0,
        TurnOutcome::Completed { .. }
    ));
    assert!(matches!(
        live.turn("more").await.0,
        TurnOutcome::Completed { .. }
    ));
    live.end().await;
    let dir = live._env.root.join("two-turns");
    let mut store = JsonlStore::open(&dir).unwrap();
    for record in records(&live.store) {
        store.append(ID, &record).unwrap();
    }
    let bytes = fs::read(store.path(ID)).unwrap();
    (live, bytes)
}

fn replaced(bytes: &[u8], line: usize, with: &str) -> Vec<u8> {
    lines(bytes)
        .iter()
        .enumerate()
        .flat_map(|(i, l)| {
            if i == line {
                format!("{with}\n").into_bytes()
            } else {
                l.to_vec()
            }
        })
        .collect()
}

fn only_fallback(path: &Path) -> (Option<u64>, Vec<wisp_protocol::log::Damage>, f64) {
    let records = fell_back(path);
    match records.as_slice() {
        [
            LogEvent::ResumeFellBack {
                last_turn,
                damage,
                spent_usd,
                ..
            },
        ] => (*last_turn, damage.clone(), *spent_usd),
        other => panic!("{other:#?}"),
    }
}

#[tokio::test]
async fn a_lost_tool_result_falls_back_before_its_turn_and_nothing_malformed_is_sent() {
    let (live, bytes) = two_tool_turns().await;
    let records = parsed(&bytes);
    let line = records
        .iter()
        .position(|r| {
            r.as_ref().is_some_and(|r| {
                matches!(&r.event, LogEvent::MessageCommitted { turn: 2, message, .. }
                    if message.role == Role::User)
            })
        })
        .unwrap();
    let miss = json!({
        "schema_version": wisp_protocol::log::LOG_SCHEMA_VERSION,
        "seq": line,
        "at": "2026-10-23T12:00:00Z",
        "type": "cache_miss",
        "turn": 2,
        "previous_prompt_tokens": 1,
        "cache_read_input_tokens": 0,
        "cache_creation_input_tokens": 0,
        "diagnostics": null
    })
    .to_string();
    let mut back = written(
        &live._env,
        "layout",
        &replaced(&bytes, line, &miss),
        vec![plain_done()],
    )
    .await
    .unwrap();
    let (last_turn, damage, spent) = only_fallback(&back.path);
    assert_eq!(last_turn, Some(1));
    assert_eq!(
        damage.last().map(|d| d.kind),
        Some(wisp_protocol::log::DamageKind::MalformedHistory)
    );
    assert!(
        spent > 0.0,
        "the dropped turn's cost counts as a lower bound"
    );
    assert!(matches!(
        back.turn("next").await,
        TurnOutcome::Completed { .. }
    ));
    well_formed(&back.body()).unwrap();
}

#[tokio::test]
async fn damage_in_turn_one_keeps_the_start_and_no_turns() {
    let (live, bytes) = two_tool_turns().await;
    let line = first_line_of_turn(&bytes, 1, "turn_started") + 1;
    let mut back = written(
        &live._env,
        "turn-one",
        &garbled(&bytes, line),
        vec![plain_done()],
    )
    .await
    .unwrap();
    let (last_turn, _, _) = only_fallback(&back.path);
    assert_eq!(last_turn, None);
    assert!(back.agent.history().is_empty());
    let kinds: Vec<String> = parsed(&fs::read(&back.path).unwrap())
        .into_iter()
        .skip(lines(&bytes).len())
        .flatten()
        .map(|r| {
            serde_json::to_value(&r.event).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        kinds[..2],
        ["resume_fell_back", "session_resumed"],
        "{kinds:#?}"
    );
    assert!(
        back.shown
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionResumed { turns: 2, .. })),
        "{:#?}",
        back.shown
    );
    assert!(matches!(
        back.turn("again").await,
        TurnOutcome::Completed { .. }
    ));
    well_formed(&back.body()).unwrap();
}

#[tokio::test]
async fn a_crash_right_after_the_fallback_record_resumes_without_a_second_one() {
    let (live, bytes) = two_tool_turns().await;
    let line = first_line_of_turn(&bytes, 2, "turn_started") + 1;
    let first = written(
        &live._env,
        "after-record",
        &garbled(&bytes, line),
        Vec::new(),
    )
    .await
    .unwrap();
    let history = first.agent.history().to_vec();
    let path = first.path.clone();
    drop(first);
    let written_bytes = fs::read(&path).unwrap();
    let keep = lines(&bytes).len() + 1;
    let cut: Vec<u8> = lines(&written_bytes)
        .iter()
        .take(keep)
        .flat_map(|l| l.to_vec())
        .collect();
    fs::write(&path, &cut).unwrap();
    let second = reopen(path.parent().unwrap(), Vec::new(), &live._env)
        .await
        .unwrap();
    assert_eq!(fell_back(&second.path).len(), 1);
    assert_eq!(second.agent.history(), history.as_slice());
    let resumed = parsed(&fs::read(&second.path).unwrap())
        .into_iter()
        .flatten()
        .filter(|r| matches!(r.event, LogEvent::SessionResumed { .. }))
        .count();
    assert_eq!(resumed, 1);
}

#[tokio::test]
async fn a_second_fallback_composes_with_the_first() {
    let (live, bytes) = two_tool_turns().await;
    let line = first_line_of_turn(&bytes, 2, "turn_started") + 1;
    let mut first = written(
        &live._env,
        "twice",
        &garbled(&bytes, line),
        vec![plain_done()],
    )
    .await
    .unwrap();
    let kept = first.agent.history().to_vec();
    assert!(matches!(
        first.turn("third").await,
        TurnOutcome::Completed { .. }
    ));
    let path = first.path.clone();
    drop(first);
    let now = fs::read(&path).unwrap();
    let third = first_line_of_turn(&now, 3, "turn_started") + 1;
    fs::write(&path, garbled(&now, third)).unwrap();
    let mut second = reopen(path.parent().unwrap(), vec![plain_done()], &live._env)
        .await
        .unwrap();
    let records = fell_back(&second.path);
    assert_eq!(records.len(), 2, "{records:#?}");
    assert_eq!(last_turn(&records[1]), Some(1));
    assert_eq!(second.agent.history(), kept.as_slice());
    assert!(matches!(
        second.turn("fourth").await,
        TurnOutcome::Completed { .. }
    ));
    let turns: Vec<u64> = parsed(&fs::read(&second.path).unwrap())
        .into_iter()
        .flatten()
        .filter_map(|r| match r.event {
            LogEvent::TurnStarted { turn, .. } => Some(turn),
            _ => None,
        })
        .collect();
    assert_eq!(turns.last(), Some(&4), "{turns:?}");
}

#[tokio::test]
async fn a_dropped_compaction_resumes_from_the_view_before_it() {
    let (live, bytes) = full_log().await;
    let head = parsed(&bytes)
        .iter()
        .position(|r| {
            r.as_ref()
                .is_some_and(|r| matches!(r.event, LogEvent::ContextCompacted { .. }))
        })
        .unwrap();
    let mut back = written(
        &live._env,
        "head",
        &garbled(&bytes, head),
        vec![plain_done()],
    )
    .await
    .unwrap();
    assert_eq!(last_turn(&fell_back(&back.path)[0]), Some(2));
    assert!(matches!(
        back.turn("one more").await,
        TurnOutcome::Completed { .. }
    ));
    let mut never = live_rig(
        vec![
            uses(&[B::Think("plan"), B::Search("s1", "rust"), echo("e1")]),
            done(&[B::Found("s1"), B::Text("found")]),
            uses(&[
                B::Text("running"),
                B::Call("c1", "sleep", json!({"ms": 0, "text": "slept\n"})),
                echo("e2"),
            ]),
            done(&[B::Text("ran")]),
            plain_done(),
        ],
        true,
    );
    never.turn("go").await;
    never.turn("more").await;
    never.turn("one more").await;
    assert_eq!(
        back.body(),
        never.provider.bodies().last().cloned().unwrap()
    );
}

async fn failed_between(after: bool) -> (Rig, Vec<u8>) {
    let mut steps = vec![
        done(&[B::Text("first")]),
        failing(
            &[B::Text("partial")],
            mid_stream(ErrorKind::Overloaded, RetryClass::Never),
        ),
    ];
    if after {
        steps.push(done(&[B::Text("third")]));
    }
    let mut live = live_rig(steps, false);
    live.turn("go").await;
    assert!(matches!(
        live.turn("fail").await.0,
        TurnOutcome::Error { offer: Some(_), .. }
    ));
    if after {
        live.turn("after").await;
    }
    live.end().await;
    let dir = live._env.root.join(format!("failed-{after}"));
    let mut store = JsonlStore::open(&dir).unwrap();
    for record in records(&live.store) {
        store.append(ID, &record).unwrap();
    }
    let bytes = fs::read(store.path(ID)).unwrap();
    (live, bytes)
}

#[tokio::test]
async fn a_failed_last_kept_turn_offers_retry_only_when_no_turn_was_dropped() {
    let (live, bytes) = failed_between(false).await;
    let end = parsed(&bytes)
        .iter()
        .position(|r| {
            r.as_ref()
                .is_some_and(|r| matches!(r.event, LogEvent::SessionEnded { .. }))
        })
        .unwrap();
    let back = written(&live._env, "between", &garbled(&bytes, end), Vec::new())
        .await
        .unwrap();
    assert_eq!(fell_back(&back.path).len(), 1);
    assert_eq!(back.agent.retry_offer().map(|(t, _)| t), Some(2));

    let (live, bytes) = failed_between(true).await;
    let line = first_line_of_turn(&bytes, 3, "turn_started") + 1;
    let back = written(&live._env, "dropped", &garbled(&bytes, line), Vec::new())
        .await
        .unwrap();
    assert_eq!(last_turn(&fell_back(&back.path)[0]), Some(2));
    assert_eq!(back.agent.retry_offer(), None);
}

fn inserted(bytes: &[u8], after: usize, events: &[Value]) -> Vec<u8> {
    let mut values: Vec<Value> = lines(bytes)
        .iter()
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect();
    let template = values[after].clone();
    for (offset, event) in events.iter().enumerate() {
        let mut value = event.clone();
        value["schema_version"] = template["schema_version"].clone();
        value["at"] = template["at"].clone();
        values.insert(after + 1 + offset, value);
    }
    values
        .iter_mut()
        .enumerate()
        .flat_map(|(seq, value)| {
            value["seq"] = json!(seq);
            let mut line = serde_json::to_vec(value).unwrap();
            line.push(b'\n');
            line
        })
        .collect()
}

#[tokio::test]
async fn settings_and_hashes_in_a_dropped_turn_are_ignored_and_a_search_refusal_is_kept() {
    let (live, bytes) = full_log().await;
    let opened = first_line_of_turn(&bytes, 3, "turn_started");
    let switched = json!({
        "type": "settings_changed",
        "model": "claude-sonnet-5-5",
        "effort": "low",
        "cache": {"type": "kept"},
        "endpoint_name": "env",
        "changes": [],
    });
    let resumed = json!({
        "type": "session_resumed",
        "wisp_version": "0.1.0",
        "endpoint": "https://api.anthropic.com",
        "endpoint_name": "env",
        "system_prompt_sha256": "ab".repeat(32),
    });
    let refused = json!({
        "type": "native_web_search",
        "endpoint_name": "env",
        "state": "fallen_back",
    });

    let edited = inserted(&bytes, opened, &[switched.clone(), resumed.clone()]);
    let damaged = garbled(&edited, opened + 3);
    let mut back = written(&live._env, "dropped-settings", &damaged, vec![plain_done()])
        .await
        .unwrap();
    assert_eq!(last_turn(&fell_back(&back.path)[0]), Some(2));
    assert_eq!(back.agent.session().model, DEFAULT_MODEL);
    assert_eq!(back.agent.pending_settings(), None);
    assert!(
        back.agent
            .resume_notice_text()
            .is_some_and(|n| n.contains("Settings changed after turn 2 weren't restored either")),
        "{:?}",
        back.agent.resume_notice_text()
    );
    assert!(
        back.agent
            .resume_notice_text()
            .is_some_and(|n| !n.contains("prompt or tools changed"))
    );
    assert!(back.agent.search.refused.is_empty());
    assert!(matches!(
        back.turn("one more").await,
        TurnOutcome::Completed { .. }
    ));
    let (first, _) = live_through_compaction().await;
    assert_eq!(back.body(), first);

    let edited = inserted(&bytes, opened, &[switched, resumed, refused]);
    let damaged = garbled(&edited, opened + 4);
    let back = written(&live._env, "dropped-search", &damaged, Vec::new())
        .await
        .unwrap();
    assert_eq!(last_turn(&fell_back(&back.path)[0]), Some(2));
    assert_eq!(back.agent.session().model, DEFAULT_MODEL);
    assert!(back.agent.search.refused.contains("env"));
}

fn edited(bytes: &[u8], edit: impl Fn(&mut Value)) -> Vec<u8> {
    lines(bytes)
        .iter()
        .flat_map(|l| {
            let mut value: Value = serde_json::from_slice(l).unwrap();
            edit(&mut value);
            let mut line = serde_json::to_vec(&value).unwrap();
            line.push(b'\n');
            line
        })
        .collect()
}

#[tokio::test]
async fn returned_messages_skip_retries_and_duplicates_and_the_newest_three_come_back() {
    let steps = (1..=6).map(|_| done(&[B::Text("reply")])).collect();
    let mut live = live_rig(steps, false);
    for n in 1..=6 {
        live.turn(&format!("t{n}")).await;
    }
    live.end().await;
    let dir = live._env.root.join("returned");
    let mut store = JsonlStore::open(&dir).unwrap();
    for record in records(&live.store) {
        store.append(ID, &record).unwrap();
    }
    let bytes = fs::read(store.path(ID)).unwrap();
    drop(store);
    let id_of = |turn: u64| {
        let line = first_line_of_turn(&bytes, turn, "turn_started");
        parsed(&bytes)[line]
            .as_ref()
            .map(|r| serde_json::to_value(r).unwrap()["input_id"].clone())
            .unwrap()
    };
    let fourth = id_of(4);
    let bytes = edited(&bytes, |value| {
        if value["type"] == "turn_started" && value["turn"] == 5 {
            value["retry_of"] = json!(4);
            value["text"] = json!("t5-retry");
        }
    });
    let opened = first_line_of_turn(&bytes, 4, "turn_started");
    let duplicate = json!({
        "type": "input_accepted",
        "turn": 4,
        "id": fourth,
        "mode": "steer",
        "text": "t4-again",
    });
    let bytes = inserted(&bytes, opened, &[duplicate]);
    let stopped = json!({
        "type": "input_accepted", "turn": 6, "id": "stopped", "mode": "queue",
        "text": "queued while stopping",
    });
    let withdrawn = json!({"type": "input_withdrawn", "ids": ["stopped"], "by": "session_ended"});
    let opened = first_line_of_turn(&bytes, 6, "turn_started");
    let bytes = inserted(&bytes, opened, &[stopped, withdrawn]);
    let waiting = json!({
        "type": "input_accepted", "turn": 1, "id": "waiting", "mode": "queue", "text": "still waiting",
    });
    let opened = first_line_of_turn(&bytes, 1, "turn_started");
    let bytes = inserted(&bytes, opened, &[waiting]);
    let line = first_line_of_turn(&bytes, 2, "turn_started") + 1;
    let back = written(
        &live._env,
        "returned-back",
        &garbled(&bytes, line),
        Vec::new(),
    )
    .await
    .unwrap();
    assert_eq!(last_turn(&fell_back(&back.path)[0]), Some(1));

    let texts: Vec<&str> = back
        .agent
        .returned_inputs()
        .iter()
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(
        texts,
        ["still waiting", "t4", "t6", "queued while stopping"]
    );
    let notice = back.agent.resume_notice_text().unwrap_or_default();
    let saved = fs::read_dir(back.path.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.to_string_lossy().contains(".dropped-"))
        .unwrap();
    assert!(notice.contains(&saved.display().to_string()), "{notice}");
    let file = fs::read_to_string(&saved).unwrap();
    let order: Vec<usize> = ["t2", "t3", "t4", "t6", "queued while stopping"]
        .iter()
        .map(|t| file.find(&format!("\n{t}\n")).unwrap())
        .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{file}");
    assert!(!file.contains("t5-retry"), "{file}");
    assert!(!file.contains("t4-again"), "{file}");
    assert_eq!(file.matches("## Message").count(), 5, "{file}");
    let expected: Vec<String> = texts.into_iter().map(str::to_owned).collect();
    let dir = back.path.parent().unwrap().to_path_buf();
    drop(back);
    let again = reopen(&dir, Vec::new(), &live._env).await.unwrap();
    let texts: Vec<&str> = again
        .agent
        .returned_inputs()
        .iter()
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(texts, expected);
    assert_eq!(fell_back(&again.path).len(), 1);
}

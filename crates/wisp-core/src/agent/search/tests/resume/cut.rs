use super::*;
use crate::session::{JsonlStore, ResumeLock};

pub(super) const ID: &str = "0123abcd";

fn sleep_call(id: &'static str) -> B {
    B::Call(id, "sleep", json!({"ms": 0, "text": "slept\n"}))
}

pub(super) async fn full_log() -> (Rig, Vec<u8>) {
    let mut live = live_rig(
        vec![
            uses(&[B::Think("plan"), B::Search("s1", "rust"), echo("e1")]),
            done(&[B::Found("s1"), B::Text("found")]),
            uses(&[B::Text("running"), sleep_call("c1"), echo("e2")]),
            done(&[B::Text("ran")]),
            compacted(),
            failing(
                &[B::Text("partial")],
                mid_stream(ErrorKind::InvalidResponse, RetryClass::Never),
            ),
            done(&[B::Text("fine")]),
        ],
        true,
    );
    assert!(matches!(
        live.turn("go").await.0,
        TurnOutcome::Completed { .. }
    ));
    assert!(matches!(
        live.turn("more").await.0,
        TurnOutcome::Completed { .. }
    ));
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    live.agent
        .manual_compaction(None, wisp_protocol::RequestId(1), &mut live.rx, &ev_tx)
        .await;
    drop(ev_tx);
    let mut compaction = Vec::new();
    while let Some(e) = ev_rx.recv().await {
        compaction.push(e);
    }
    assert!(
        compaction
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. })),
        "{compaction:#?}"
    );
    assert!(matches!(
        live.turn("fail").await.0,
        TurnOutcome::Error { .. }
    ));
    assert!(matches!(
        live.turn("after").await.0,
        TurnOutcome::Completed { .. }
    ));
    live.end().await;
    let dir = live._env.root.join("full");
    let mut store = JsonlStore::open(&dir).unwrap();
    for record in records(&live.store) {
        store.append(ID, &record).unwrap();
    }
    let bytes = fs::read(store.path(ID)).unwrap();
    (live, bytes)
}

pub(super) fn well_formed(body: &Value) -> Result<(), String> {
    let messages = body["messages"].as_array().ok_or("no messages")?;
    let role = |m: &Value| m["role"].as_str().unwrap_or_default().to_owned();
    let blocks = |m: &Value| m["content"].as_array().cloned().unwrap_or_default();
    if messages.first().map(role).as_deref() != Some("user") {
        return Err("the first message isn't the user's".into());
    }
    if messages.last().map(role).as_deref() != Some("user") {
        return Err("the last message isn't the user's".into());
    }
    let mut asked: Vec<String> = Vec::new();
    for (i, message) in messages.iter().enumerate() {
        let content = blocks(message);
        if role(message) == "assistant" {
            if !asked.is_empty() {
                return Err(format!("message {i}: calls {asked:?} have no results"));
            }
            asked = content
                .iter()
                .filter(|b| b["type"] == "tool_use")
                .filter_map(|b| b["id"].as_str().map(str::to_owned))
                .collect();
            continue;
        }
        let results: Vec<String> = content
            .iter()
            .filter(|b| b["type"] == "tool_result")
            .filter_map(|b| b["tool_use_id"].as_str().map(str::to_owned))
            .collect();
        let leading = content
            .iter()
            .take_while(|b| b["type"] == "tool_result")
            .count();
        if leading != results.len() {
            return Err(format!("message {i}: a tool result follows other content"));
        }
        if results != asked {
            return Err(format!(
                "message {i}: results {results:?} don't answer calls {asked:?}"
            ));
        }
        asked.clear();
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Next {
    Message,
    Retry,
}

async fn one_more(live: &Rig, bytes: &[u8], cut: usize, next: Next) -> Result<bool, String> {
    let named = match next {
        Next::Message => "message",
        Next::Retry => "retry",
    };
    let dir = live._env.root.join(format!("cut-{named}-{cut}"));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut store = JsonlStore::open(&dir).map_err(|e| e.to_string())?;
    let path = store.path(ID);
    fs::write(&path, &bytes[..cut]).map_err(|e| e.to_string())?;
    let reopened = store
        .reopen(ID, ResumeLock::Optional)
        .map_err(|e| format!("reopen: {e}"))?;
    let (tx, mut rx) = mpsc::channel(64);
    let provider = searching(vec![plain_done()], tx);
    let mut agent = base(provider.clone(), &live._env, true)
        .with_log(
            SessionLog::continuing(Box::new(store), ID, reopened.next_seq),
            log_context(),
        )
        .resume_lines(reopened.lines, reopened.truncated_bytes)
        .map_err(|e| format!("resume: {e}"))?;
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    let drain = tokio::spawn(async move { while ev_rx.recv().await.is_some() {} });
    agent.start_log(&ev_tx).await;
    let outcome = match (next, agent.retry_offer()) {
        (Next::Message, _) => agent
            .run_turn("one more".into(), &mut rx, &ev_tx)
            .await
            .ok_or("the turn didn't run")?,
        (Next::Retry, None) => {
            drop(ev_tx);
            drain.await.map_err(|e| e.to_string())?;
            let written = fs::read_to_string(&path).map_err(|e| e.to_string())?;
            if written.contains(r#""type":"resume_fell_back""#) {
                return Err("a clean cut fell back".to_owned());
            }
            return Ok(false);
        }
        (Next::Retry, Some((turn, _))) => {
            provider
                .commands()
                .try_send(ClientCommand::Retry {
                    turn,
                    endpoint: None,
                    request: wisp_protocol::RequestId(7),
                })
                .map_err(|e| e.to_string())?;
            retried(&mut agent, &mut rx)
                .await
                .ok_or("the Retry didn't run")?
        }
    };
    drop(ev_tx);
    drain.await.map_err(|e| e.to_string())?;
    let written = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    if written.contains(r#""type":"resume_fell_back""#) {
        return Err("a clean cut fell back".to_owned());
    }
    if !matches!(outcome, TurnOutcome::Completed { .. }) {
        return Err(format!("the turn ended {outcome:?}"));
    }
    let bodies = provider.bodies();
    let [body] = bodies.as_slice() else {
        return Err(format!("{} requests were sent", bodies.len()));
    };
    well_formed(body).map(|()| true)
}

pub(super) async fn retried(
    agent: &mut Agent,
    commands: &mut mpsc::Receiver<ClientCommand>,
) -> Option<TurnOutcome> {
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    let serve = agent.serve(commands, &ev_tx);
    tokio::pin!(serve);
    loop {
        tokio::select! {
            () = &mut serve => return None,
            event = ev_rx.recv() => match event {
                Some(AgentEvent::TurnEnded { outcome, .. }) => return Some(outcome),
                Some(_) => {}
                None => return None,
            },
        }
    }
}

fn cuts(bytes: &[u8]) -> (Vec<usize>, Vec<usize>) {
    let ends: Vec<usize> = bytes
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .map(|(i, _)| i + 1)
        .collect();
    let starts: Vec<usize> = std::iter::once(0).chain(ends.iter().copied()).collect();
    let inside = [1, ends.len() / 3, ends.len() / 2, ends.len() - 1]
        .into_iter()
        .map(|line| starts[line] + (ends[line] - starts[line]) / 2);
    let all = ends.iter().copied().chain(inside).collect();
    (all, starts)
}

async fn every_cut(next: Next) -> usize {
    let (live, bytes) = full_log().await;
    let (all, starts) = cuts(&bytes);
    let mut failures = Vec::new();
    let mut ran = 0;
    for cut in all {
        match one_more(&live, &bytes, cut, next).await {
            Ok(true) => ran += 1,
            Ok(false) => {}
            Err(why) => {
                let line = starts.iter().rposition(|s| *s <= cut).unwrap_or(0);
                failures.push(format!("cut at byte {cut} (line {line}): {why}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    ran
}

#[tokio::test]
async fn a_log_cut_anywhere_resumes_and_takes_one_more_turn() {
    every_cut(Next::Message).await;
}

#[tokio::test]
async fn a_retry_offered_after_a_cut_anywhere_sends_a_well_formed_request() {
    assert!(every_cut(Next::Retry).await > 0, "some cut offers a Retry");
}

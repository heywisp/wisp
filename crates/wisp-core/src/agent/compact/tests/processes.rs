use std::path::Path;

use wisp_protocol::{BackgroundReason, ProcessEnd, StoppedBy, TurnOrigin};

use super::*;
use crate::agent::WAKE_SETTLE;
use crate::call::OutputSink;
use crate::jobs::exec::{self, HostSpawner, JobSpec, Launch, Spawner};
use crate::test_guard::{ScratchDir, records, wait_for};
use wisp_tool_text::jobs::{LIST_TARGET_CHARS, WAKE_LINE};
use wisp_tool_text::output::cut_chars;

fn scratch(name: &str) -> ScratchDir {
    ScratchDir::new("wisp-compact-processes-", name)
}

fn background(agent: &Agent, call_id: &str, command: &str, cwd: &Path) -> bool {
    background_to(agent, call_id, command, cwd, None)
}

fn background_to(
    agent: &Agent,
    call_id: &str,
    command: &str,
    cwd: &Path,
    file: Option<&Path>,
) -> bool {
    let Some(bash) = exec::find_program("bash") else {
        eprintln!("skipped: bash is not on PATH");
        return false;
    };
    let gate = format!("{call_id}.go");
    let script = format!("{}; {command}", wait_for(&gate));
    let args: Vec<std::ffi::OsString> = vec!["-c".into(), script.into()];
    let process = HostSpawner::detect()
        .spawn(&Launch {
            program: &bash,
            args: &args,
            cwd,
            env: &[],
            stdin: false,
            log: None,
        })
        .unwrap();
    let mut sink = match file {
        Some(path) => {
            let out = std::fs::File::create(path).unwrap();
            OutputSink::new(
                call_id,
                None,
                Ok((
                    path.to_path_buf(),
                    Box::new(out) as Box<dyn std::io::Write + Send>,
                )),
            )
        }
        None => OutputSink::detached(call_id),
    };
    sink.keep_lines();
    let (job, _end) = exec::spawn_job(
        process,
        sink,
        JobSpec {
            cancel: None,
            deadline: None,
            limit_ms: None,
            target: command.into(),
            log: None,
        },
    );
    agent
        .processes
        .number_at_spawn(call_id, job, BackgroundReason::Started);
    std::fs::write(cwd.join(gate), "").unwrap();
    true
}

fn session_ended(seen: &[AgentEvent]) -> bool {
    seen.iter().any(|e| {
        matches!(
            e,
            AgentEvent::ProcessEnded {
                end: ProcessEnd::Stopped {
                    by: StoppedBy::SessionEnded
                },
                ..
            }
        )
    })
}

#[tokio::test]
async fn process_events_drain_while_the_compaction_request_waits() {
    let dir = scratch("drain");
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let mut script = compaction(SUMMARY).script;
    script.retain(|e| !matches!(e, Ok(ProviderEvent::Sending)));
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        Step::from(script).gated(&gate),
    ]);
    let agent = logged(provider, &store);
    let command = format!("echo early; {}; echo late", wait_for("go"));
    if !background(&agent, "c1", &command, &dir.root) {
        return;
    }
    let mut live = Live::start(agent);
    live.until(|e| matches!(e, AgentEvent::ProcessStarted { id: 1, .. }))
        .await;
    assert!(completed(&live.turn("u1", "first").await));
    live.send(compact(2, None)).await;
    live.until(|e| matches!(e, AgentEvent::CompactionStarted { .. }))
        .await;
    std::fs::write(dir.root.join("go"), "").unwrap();
    live.until(
        |e| matches!(e, AgentEvent::ProcessOutput { id: 1, text, .. } if text.contains("late")),
    )
    .await;
    live.until(|e| matches!(e, AgentEvent::ProcessEnded { id: 1, .. }))
        .await;
    assert!(
        !live
            .seen
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. }))
    );
    gate.add_permits(1);
    let done = live.until(|e| answers(e, 2)).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    let (agent, _) = live.close().await;
    assert!(!head_text(&agent).contains("Running processes"));
    let ended = log_events(&store)
        .into_iter()
        .filter(|r| matches!(r, LogEvent::ProcessEnded { id: 1, .. }))
        .count();
    assert_eq!(ended, 1);
}

#[tokio::test]
async fn wisps_head_reads_the_running_processes_when_it_is_built_not_at_the_last_estimate() {
    let dir = scratch("written");
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let mut script = super::wisp::written(WRITTEN, 60_000).script;
    script.retain(|e| !matches!(e, Ok(ProviderEvent::Sending)));
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        Step::from(script).gated(&gate),
    ]);
    let agent = logged(provider, &store).with_model_settings(models(WISP));
    if !background(&agent, "c1", &wait_for("go"), &dir.root) {
        return;
    }
    let mut live = Live::start(agent);
    live.until(|e| matches!(e, AgentEvent::ProcessStarted { id: 1, .. }))
        .await;
    assert!(completed(&live.turn("u1", "first").await));
    live.send(compact(2, None)).await;
    live.until(|e| matches!(e, AgentEvent::CompactionStarted { .. }))
        .await;
    std::fs::write(dir.root.join("go"), "").unwrap();
    live.until(|e| matches!(e, AgentEvent::ProcessEnded { id: 1, .. }))
        .await;
    gate.add_permits(1);
    let done = live.until(|e| answers(e, 2)).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    let (agent, _) = live.close().await;
    let head = head_text(&agent);
    assert!(head.contains("<user_messages>"), "{head}");
    assert!(!head.contains("Running processes"), "{head}");
}

#[tokio::test]
async fn the_head_lists_running_processes_with_their_unread_lines() {
    let dir = scratch("head");
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000), compaction(SUMMARY)]);
    let agent = logged(provider, &store);
    let quiet = "exec sleep 30";
    let chatty = "printf 'a\\nb\\n'; exec sleep 30";
    if !background(&agent, "c1", quiet, &dir.root) || !background(&agent, "c2", chatty, &dir.root) {
        return;
    }
    let mut live = Live::start(agent);
    live.until(
        |e| matches!(e, AgentEvent::ProcessOutput { id: 2, text, .. } if text.contains('b')),
    )
    .await;
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    let (agent, seen) = live.close().await;
    let head = head_text(&agent);
    let line = "Running processes: 1 (exec sleep 30), 2 (printf 'a\\nb\\n'; exec sleep 30, 2 unread lines).\n";
    assert!(
        head.contains(&format!("{line}</compacted_context>")),
        "{head}"
    );
    let recorded = log_events(&store)
        .into_iter()
        .find_map(|r| match r {
            LogEvent::ContextCompacted { head, .. } => Some(head),
            _ => None,
        })
        .unwrap();
    assert_eq!(recorded, agent.compaction.head.as_ref().unwrap().message);
    assert!(session_ended(&seen));
}

#[tokio::test]
async fn the_pump_keeps_writing_while_a_compaction_request_waits() {
    let dir = scratch("chatty");
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let mut script = compaction(SUMMARY).script;
    script.retain(|e| !matches!(e, Ok(ProviderEvent::Sending)));
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        Step::from(script).gated(&gate),
    ]);
    let agent = logged(provider, &store);
    let file = dir.root.join("chatty.out");
    let command = "i=0; while [ $i -lt 2000000 ]; do i=$((i+1)); echo $i; done";
    if !background_to(&agent, "c1", command, &dir.root, Some(&file)) {
        return;
    }
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.send(compact(2, None)).await;
    live.until(|e| matches!(e, AgentEvent::CompactionStarted { .. }))
        .await;
    let before = std::fs::metadata(&file).unwrap().len();
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let after = std::fs::metadata(&file).unwrap().len();
    assert!(after > before, "{before} then {after}");
    live.until(|e| matches!(e, AgentEvent::ProcessOutput { .. }))
        .await;
    gate.add_permits(1);
    let done = live.until(|e| answers(e, 2)).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    let (_, seen) = live.close().await;
    let streamed: u64 = seen
        .iter()
        .map(|e| match e {
            AgentEvent::ProcessOutput { text, skipped, .. } => text.len() as u64 + skipped,
            _ => 0,
        })
        .sum();
    assert_eq!(streamed, std::fs::metadata(&file).unwrap().len());
    assert!(session_ended(&seen));
}

fn user_texts(body: &Value) -> Vec<String> {
    let messages = body["messages"].as_array().unwrap();
    let last = messages.last().unwrap();
    assert_eq!(last["role"], "user");
    last["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["text"].as_str().unwrap_or("<non-text>").to_owned())
        .collect()
}

#[tokio::test]
async fn a_wake_waits_for_a_compaction_and_rebuilds_after_it() {
    let dir = scratch("wake-after");
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let mut script = compaction(SUMMARY).script;
    script.retain(|e| !matches!(e, Ok(ProviderEvent::Sending)));
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        Step::from(script).gated(&gate),
        answer("Woke.", 1_300),
    ]);
    let agent = logged(provider.clone(), &store);
    let command = format!("{}; exit 3", wait_for("go"));
    if !background(&agent, "c1", &command, &dir.root) {
        return;
    }
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.send(compact(2, None)).await;
    live.until(|e| matches!(e, AgentEvent::CompactionStarted { .. }))
        .await;
    std::fs::write(dir.root.join("go"), "").unwrap();
    live.until(|e| matches!(e, AgentEvent::ProcessEnded { id: 1, .. }))
        .await;
    tokio::time::sleep(WAKE_SETTLE + Duration::from_millis(1500)).await;
    while let Ok(event) = live.events.try_recv() {
        live.seen.push(event);
    }
    let starts_before = live
        .seen
        .iter()
        .filter(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .count();
    assert_eq!(starts_before, 1, "{:#?}", live.seen);
    gate.add_permits(1);
    let done = live.until(|e| answers(e, 2)).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    let started = live
        .until(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .await;
    assert!(
        matches!(
            started,
            AgentEvent::TurnStarted {
                origin: Some(TurnOrigin::ProcessExit),
                ..
            }
        ),
        "{started:?}"
    );
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (agent, _) = live.close().await;
    let texts = user_texts(&provider.body(2));
    assert!(texts.iter().all(|t| !t.is_empty()), "{texts:?}");
    assert_eq!(
        texts.last().unwrap(),
        &format!(
            "<processes>\n{WAKE_LINE}\nProcess 1 ({}) exited with code 3.\n</processes>",
            cut_chars(&command, LIST_TARGET_CHARS)
        )
    );
    let rebuilt = rebuild_history(log_events(&store).iter(), no_images).unwrap();
    assert_eq!(rebuilt.messages, *agent.history());
    assert_eq!(rebuilt.view(), agent.view_messages());
}

fn reopened_from(
    provider: Arc<Fake>,
    records: &[wisp_protocol::log::LogRecord],
) -> (Agent, MemoryStore) {
    let (store, log) = continued(records);
    let agent = Agent::new(provider, default_session())
        .with_clock(Arc::new(crate::clock::StillClock))
        .with_log(log, log_context())
        .resume(records, 0)
        .unwrap();
    (agent, store)
}

const RESTARTED: &str = "Process 1 was still running when Wisp exited, so it has stopped; its output file remains if it still exists.";

fn last_user_text(body: &Value) -> String {
    let messages = body["messages"].as_array().unwrap();
    messages
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .unwrap()
        .to_string()
}

async fn a_session_with_process_one(
    dir: &ScratchDir,
    crash: bool,
) -> Option<Vec<wisp_protocol::log::LogRecord>> {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000)]);
    let agent = logged(provider, &store).with_clock(Arc::new(crate::clock::StillClock));
    if !background(&agent, "c1", "exec sleep 30", &dir.root) {
        return None;
    }
    let mut live = Live::start(agent);
    live.until(|e| matches!(e, AgentEvent::ProcessStarted { id: 1, .. }))
        .await;
    assert!(completed(&live.turn("u1", "first").await));
    let (_, seen) = live.close().await;
    assert!(session_ended(&seen));
    let mut records = records(&store);
    if crash {
        records.retain(|r| {
            !matches!(
                r.event,
                LogEvent::ProcessEnded { .. } | LogEvent::SessionEnded { .. }
            )
        });
        for (seq, record) in records.iter_mut().enumerate() {
            record.seq = u64::try_from(seq).unwrap();
        }
    }
    Some(records)
}

#[tokio::test]
async fn a_resumed_session_numbers_after_the_log_and_reports_the_stopped_process_once() {
    for crash in [false, true] {
        let dir = scratch(if crash { "restart-crash" } else { "restart" });
        let Some(earlier) = a_session_with_process_one(&dir, crash).await else {
            return;
        };

        let provider = scripted(vec![answer("Two.", 1_100), answer("Three.", 1_200)]);
        let (agent, store) = reopened_from(provider.clone(), &earlier);
        if !background(&agent, "c2", "true", &dir.root) {
            return;
        }
        let mut again = Live::start(agent);
        again
            .until(|e| matches!(e, AgentEvent::ProcessStarted { id: 2, .. }))
            .await;
        again
            .until(|e| matches!(e, AgentEvent::ProcessEnded { id: 2, .. }))
            .await;
        assert!(completed(&again.turn("u2", "second").await));
        let notice = last_user_text(&provider.body(0));
        assert!(
            notice.contains("<processes>") && notice.contains(RESTARTED),
            "{notice}"
        );
        let after_first = records(&store);
        assert!(completed(&again.turn("u3", "third").await));
        assert!(!last_user_text(&provider.body(1)).contains("<processes>"));
        let (_, seen) = again.close().await;
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, AgentEvent::ProcessStarted { id: 1, .. }))
        );

        let ends = ends_of(&store, 1);
        let expected = if crash {
            ProcessEnd::Lost {
                reason: wisp_protocol::LostReason::Gone,
            }
        } else {
            ProcessEnd::Stopped {
                by: StoppedBy::SessionEnded,
            }
        };
        assert_eq!(ends, [expected], "crash: {crash}");
        let heard: Vec<Vec<u32>> = pick!(
            log_events(&store),
            LogEvent::ProcessHeard { ids, .. } => ids
        );
        assert_eq!(
            heard.iter().filter(|ids| ids.contains(&1)).count(),
            1,
            "{heard:?}"
        );

        let twice = scripted(vec![answer("Three.", 1_200)]);
        let (agent, second) = reopened_from(twice.clone(), &after_first);
        let mut third = Live::start(agent);
        assert!(completed(&third.turn("u3", "third").await));
        third.close().await;
        assert_eq!(twice.body(0), provider.body(1), "crash: {crash}");
        assert_eq!(ends_of(&second, 1).len(), 1);
    }
}

fn user_messages(body: &Value) -> Vec<String> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "user")
        .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
        .filter_map(|b| b["text"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn a_restart_notice_survives_a_resume_whose_turn_never_committed() {
    for unchecked in [false, true] {
        a_restart_notice_survives_an_uncommitted_resume(unchecked).await;
    }
}

const UNCHECKED: &str = "Process 1 was still running when Wisp exited, and Wisp can't check here whether it still runs, so it wasn't stopped; its output file remains if it still exists.";

fn without_boot_ids(
    records: Vec<wisp_protocol::log::LogRecord>,
) -> Vec<wisp_protocol::log::LogRecord> {
    records
        .into_iter()
        .map(|mut record| {
            if let LogEvent::ProcessStarted { boot_id, .. } = &mut record.event {
                *boot_id = None;
            }
            record
        })
        .collect()
}

async fn a_restart_notice_survives_an_uncommitted_resume(unchecked: bool) {
    let dir = scratch(if unchecked {
        "restart-uncommitted-unchecked"
    } else {
        "restart-uncommitted"
    });
    let Some(mut earlier) = a_session_with_process_one(&dir, true).await else {
        return;
    };
    if unchecked {
        earlier = without_boot_ids(earlier);
    }
    let line = if unchecked { UNCHECKED } else { RESTARTED };
    let notice = format!("<processes>\n{line}\n</processes>");

    let failed = scripted(vec![bad_request()]);
    let (agent, store) = reopened_from(failed.clone(), &earlier);
    let mut again = Live::start(agent);
    let outcome = again.turn("u2", "second").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    again.close().await;
    assert!(user_messages(&failed.body(0)).contains(&notice));
    assert_eq!(
        ends_of(&store, 1),
        [ProcessEnd::Lost {
            reason: wisp_protocol::LostReason::Gone
        }]
    );
    assert!(
        !log_events(&store)
            .iter()
            .any(|e| matches!(e, LogEvent::ProcessHeard { .. }))
    );

    let between = records(&store);
    let provider = scripted(vec![answer("Two.", 1_100)]);
    let (agent, second) = reopened_from(provider.clone(), &between);
    let mut third = Live::start(agent);
    assert!(completed(&third.turn("u2", "second").await));
    third.close().await;
    assert!(
        user_messages(&provider.body(0)).contains(&notice),
        "{:?}",
        user_messages(&provider.body(0))
    );
    assert_eq!(ends_of(&second, 1).len(), 1);
}

#[tokio::test]
async fn numbers_continue_after_an_id_logged_only_on_a_finished_call() {
    let dir = scratch("numbered-call");
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000)]);
    let mut live =
        Live::start(logged(provider, &store).with_clock(Arc::new(crate::clock::StillClock)));
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    let mut earlier = records(&store);
    let at = earlier.len() - 1;
    let finished: LogEvent = serde_json::from_value(json!({
        "type": "tool_call_finished",
        "turn": 1,
        "call_id": "toolu_08",
        "outcome": {"status": "completed", "is_error": false, "exit": null},
        "output": {"tail": "", "bytes": 0, "lines": 0, "file": null, "file_problem": null, "inlined": false},
        "facts": [],
        "state": null,
        "duration_ms": 10,
        "result": "done",
        "process": 3,
    }))
    .unwrap();
    let mut record = earlier[at].clone();
    record.event = finished;
    earlier.insert(at, record);
    for (seq, record) in earlier.iter_mut().enumerate() {
        record.seq = u64::try_from(seq).unwrap();
    }

    let provider = scripted(Vec::new());
    let (agent, _) = reopened_from(provider, &earlier);
    if !background(&agent, "c4", "exec sleep 30", &dir.root) {
        return;
    }
    let mut again = Live::start(agent);
    again
        .until(|e| matches!(e, AgentEvent::ProcessStarted { id: 4, .. }))
        .await;
    again.close().await;
}

#[tokio::test]
async fn a_turn_a_crash_left_open_closes_before_its_lost_processes_end() {
    let dir = scratch("restart-open-turn");
    let Some(mut earlier) = a_session_with_process_one(&dir, true).await else {
        return;
    };
    earlier.retain(|r| !matches!(r.event, LogEvent::TurnEnded { .. }));
    for (seq, record) in earlier.iter_mut().enumerate() {
        record.seq = u64::try_from(seq).unwrap();
    }

    let (agent, store) = reopened_from(scripted(Vec::new()), &earlier);
    let (_, seen) = Live::start(agent).close().await;

    let written = log_events(&store).split_off(earlier.len());
    let at = |pick: &dyn Fn(&LogEvent) -> bool| written.iter().position(pick).unwrap();
    let resumed = at(&|e| matches!(e, LogEvent::SessionResumed { .. }));
    let closed = at(&|e| matches!(e, LogEvent::TurnEnded { turn: 1, .. }));
    let lost = at(&|e| {
        matches!(
            e,
            LogEvent::ProcessEnded {
                id: 1,
                end: ProcessEnd::Lost { .. },
                ..
            }
        )
    });
    assert!(resumed < closed && closed < lost, "{written:?}");

    let shown = |pick: &dyn Fn(&AgentEvent) -> bool| seen.iter().position(pick).unwrap();
    let turn_ended = shown(&|e| matches!(e, AgentEvent::TurnEnded { .. }));
    let process_ended = shown(&|e| matches!(e, AgentEvent::ProcessEnded { id: 1, .. }));
    assert!(turn_ended < process_ended, "{seen:?}");
    assert_eq!(ends_of(&store, 1).len(), 1);
}

pub(super) fn renumbered(
    mut records: Vec<wisp_protocol::log::LogRecord>,
) -> Vec<wisp_protocol::log::LogRecord> {
    for (seq, record) in records.iter_mut().enumerate() {
        record.seq = u64::try_from(seq).unwrap();
    }
    records
}

fn heard_ids(store: &MemoryStore) -> Vec<u32> {
    log_events(store)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::ProcessHeard { ids, .. } => Some(ids),
            _ => None,
        })
        .flatten()
        .collect()
}

async fn a_session_with_two_running(
    dir: &ScratchDir,
) -> Option<Vec<wisp_protocol::log::LogRecord>> {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000)]);
    let agent = logged(provider, &store).with_clock(Arc::new(crate::clock::StillClock));
    if !background(&agent, "c1", "exec sleep 30", &dir.root)
        || !background(&agent, "c2", "exec sleep 30", &dir.root)
    {
        return None;
    }
    let mut live = Live::start(agent);
    live.until(|e| matches!(e, AgentEvent::ProcessStarted { id: 2, .. }))
        .await;
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    Some(records(&store))
}

const BOTH_RESTARTED: &str = "Processes 1 and 2 were still running when Wisp exited, so they have stopped; their output files remain where the files still exist.";

#[tokio::test]
async fn a_shutdown_cut_after_its_first_record_gets_lost_only_for_the_rest() {
    for cut in [false, true] {
        let dir = scratch(if cut { "mix-cut" } else { "mix-quit" });
        let Some(mut earlier) = a_session_with_two_running(&dir).await else {
            return;
        };
        let first_end = earlier
            .iter()
            .position(|r| matches!(r.event, LogEvent::ProcessEnded { .. }))
            .unwrap();
        let LogEvent::ProcessEnded { id: first, .. } = earlier[first_end].event else {
            unreachable!()
        };
        if cut {
            earlier.truncate(first_end + 1);
        }

        let provider = scripted(vec![answer("Two.", 1_100)]);
        let (agent, store) = reopened_from(provider.clone(), &earlier);
        let mut again = Live::start(agent);
        assert!(completed(&again.turn("u2", "second").await));
        again.close().await;
        let notice = format!("<processes>\n{BOTH_RESTARTED}\n</processes>");
        assert!(
            user_messages(&provider.body(0)).contains(&notice),
            "cut: {cut}: {:?}",
            user_messages(&provider.body(0))
        );
        let ended = ProcessEnd::Stopped {
            by: StoppedBy::SessionEnded,
        };
        let other = 3 - first;
        assert_eq!(
            ends_of(&store, first),
            std::slice::from_ref(&ended),
            "cut: {cut}"
        );
        let expected_other = if cut {
            ProcessEnd::Lost {
                reason: wisp_protocol::LostReason::Gone,
            }
        } else {
            ended
        };
        assert_eq!(ends_of(&store, other), [expected_other], "cut: {cut}");
        let mut heard = heard_ids(&store);
        heard.sort_unstable();
        assert_eq!(heard, [1, 2], "cut: {cut}");

        let after = records(&store);
        let quiet = scripted(vec![answer("Three.", 1_200)]);
        let (agent, _) = reopened_from(quiet.clone(), &after);
        let mut third = Live::start(agent);
        assert!(completed(&third.turn("u3", "third").await));
        third.close().await;
        assert!(
            !last_user_text(&quiet.body(0)).contains("<processes>"),
            "cut: {cut}"
        );
    }
}

#[tokio::test]
async fn an_end_never_heard_before_a_quit_is_named_by_the_resume_notice() {
    let dir = scratch("never-heard");
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000)]);
    let agent = logged(provider, &store).with_clock(Arc::new(crate::clock::StillClock));
    if !background(&agent, "c1", "exit 4", &dir.root) {
        return;
    }
    let mut live = Live::start(agent);
    live.until(|e| matches!(e, AgentEvent::ProcessEnded { id: 1, .. }))
        .await;
    assert!(completed(&live.turn("u1", "first").await));
    assert_eq!(heard_ids(&store), [1]);
    let (agent, _) = live.close().await;
    let second = background(&agent, "c2", "exit 5", &dir.root);
    assert!(second);
    let mut live = Live::start(agent);
    live.until(|e| matches!(e, AgentEvent::ProcessEnded { id: 2, .. }))
        .await;
    let (_, seen) = live.close().await;
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnStarted { .. })),
        "{seen:#?}"
    );
    let earlier = records(&store);
    let one = "Process 1 exited with code 4 before Wisp restarted.";
    let two = "Process 2 exited with code 5 before Wisp restarted.";

    let provider = scripted(vec![answer("Two.", 1_100)]);
    let (agent, resumed) = reopened_from(provider.clone(), &earlier);
    let mut again = Live::start(agent);
    assert!(completed(&again.turn("u2", "second").await));
    let (_, seen) = again.close().await;
    let sent = user_messages(&provider.body(0));
    assert!(
        sent.contains(&format!("<processes>\n{two}\n</processes>")),
        "{sent:?}"
    );
    assert_eq!(heard_ids(&resumed), [1, 2]);
    let five = ProcessEnd::Exited {
        exit: wisp_protocol::ExitStatus::Code { code: 5 },
    };
    assert_eq!(started_ends(&seen), [vec![(2, five.clone())]]);
    assert_eq!(logged_ends(&resumed).last(), Some(&vec![(2, five)]));

    let after = records(&resumed);
    let quiet = scripted(vec![answer("Three.", 1_200)]);
    let (agent, _) = reopened_from(quiet.clone(), &after);
    let mut third = Live::start(agent);
    assert!(completed(&third.turn("u3", "third").await));
    third.close().await;
    assert!(!last_user_text(&quiet.body(0)).contains("<processes>"));

    let killed = renumbered(
        earlier
            .into_iter()
            .filter(|r| !matches!(r.event, LogEvent::ProcessHeard { .. }))
            .collect(),
    );
    let provider = scripted(vec![answer("Two.", 1_100)]);
    let (agent, _) = reopened_from(provider.clone(), &killed);
    let mut again = Live::start(agent);
    assert!(completed(&again.turn("u2", "second").await));
    again.close().await;
    let sent = user_messages(&provider.body(0));
    assert!(
        sent.contains(&format!("<processes>\n{one}\n{two}\n</processes>")),
        "{sent:?}"
    );
}

#[tokio::test]
async fn without_a_boot_id_the_notice_says_leftovers_were_not_checked() {
    let dir = scratch("no-boot");
    let Some(earlier) = a_session_with_process_one(&dir, true).await else {
        return;
    };
    let earlier = without_boot_ids(earlier);
    let provider = scripted(vec![answer("Two.", 1_100)]);
    let (agent, store) = reopened_from(provider.clone(), &earlier);
    let mut again = Live::start(agent);
    assert!(completed(&again.turn("u2", "second").await));
    let (_, seen) = again.close().await;
    let gone = ProcessEnd::Lost {
        reason: wisp_protocol::LostReason::Gone,
    };
    assert_eq!(started_ends(&seen), [vec![(1, gone)]]);
    let line = UNCHECKED;
    assert!(
        user_messages(&provider.body(0)).contains(&format!("<processes>\n{line}\n</processes>")),
        "{:?}",
        user_messages(&provider.body(0))
    );
    assert_eq!(
        ends_of(&store, 1),
        [ProcessEnd::Lost {
            reason: wisp_protocol::LostReason::Gone
        }]
    );
    assert_eq!(heard_ids(&store), [1]);
}

#[test]
fn a_notice_carries_the_restart_lines_only_when_it_holds_all_of_them_as_one_block() {
    use crate::agent::processes::carries_restart_lines;
    let lines = vec![
        "Process 1 was still running when Wisp exited, so it has stopped; its output file remains if it still exists.".to_owned(),
        "Process 4 (cargo test) exited with code 1 before Wisp restarted.".to_owned(),
    ];
    let notice = |body: &[&str]| format!("<processes>\n{}\n</processes>", body.join("\n"));
    let both = notice(&[&lines[0], &lines[1]]);
    assert!(carries_restart_lines(Some(&both), &lines));
    assert!(carries_restart_lines(
        Some(&notice(&[
            WAKE_LINE,
            &lines[0],
            &lines[1],
            "Process 5 exited."
        ])),
        &lines
    ));
    for partial in [
        notice(&[&lines[0]]),
        notice(&[&lines[1]]),
        notice(&[&lines[1], &lines[0]]),
        notice(&[&lines[0], "Process 5 exited.", &lines[1]]),
    ] {
        assert!(!carries_restart_lines(Some(&partial), &lines), "{partial}");
    }
    assert!(!carries_restart_lines(None, &lines));
}

#[test]
fn a_number_logged_on_a_finished_call_takes_its_end_from_the_calls_outcome() {
    use crate::agent::processes::foreground_end;
    use wisp_protocol::{ExitStatus, LostReason, NotRunReason, ToolOutcome};
    for exit in [
        ExitStatus::Code { code: 2 },
        ExitStatus::Signal { signal: 9 },
    ] {
        assert_eq!(
            foreground_end(&ToolOutcome::Completed {
                is_error: true,
                exit: Some(exit),
            }),
            ProcessEnd::Exited { exit }
        );
    }
    assert_eq!(
        foreground_end(&ToolOutcome::TimedOut { after_ms: 120_000 }),
        ProcessEnd::Stopped {
            by: StoppedBy::TimeLimit { after_ms: 120_000 }
        }
    );
    assert_eq!(
        foreground_end(&ToolOutcome::Cancelled),
        ProcessEnd::Stopped {
            by: StoppedBy::User
        }
    );
    for before_start in [false, true] {
        assert_eq!(
            foreground_end(&ToolOutcome::Failed {
                message: "no such file".into(),
                before_start,
            }),
            ProcessEnd::Failed {
                message: "no such file".into()
            }
        );
    }
    for other in [
        ToolOutcome::Completed {
            is_error: false,
            exit: None,
        },
        ToolOutcome::NotRun {
            reason: NotRunReason::TurnStopped,
        },
        ToolOutcome::InvalidInput {
            message: "bad".into(),
        },
        ToolOutcome::Backgrounded {
            reason: BackgroundReason::Started,
        },
    ] {
        assert_eq!(
            foreground_end(&other),
            ProcessEnd::Lost {
                reason: LostReason::Gone
            },
            "{other:?}"
        );
    }
}

fn started_record(
    template: &wisp_protocol::log::LogRecord,
    turn: u64,
    id: u32,
) -> wisp_protocol::log::LogRecord {
    let event: LogEvent = serde_json::from_value(json!({
        "type": "process_started",
        "turn": turn,
        "id": id,
        "call_id": format!("toolu_{id}"),
        "reason": "started",
        "tty": false,
        "stdin": true,
        "pid": 999_999,
        "sid": 999_999,
        "start_ticks": 1,
        "boot_id": "not-this-boot",
    }))
    .unwrap();
    let mut record = template.clone();
    record.event = event;
    record
}

async fn turns_with_process(turns: u64, process_turn: u64) -> Vec<wisp_protocol::log::LogRecord> {
    let store = MemoryStore::default();
    let steps = (0..turns).map(|_| answer("Done.", 1_000)).collect();
    let mut live =
        Live::start(logged(scripted(steps), &store).with_clock(Arc::new(crate::clock::StillClock)));
    for turn in 1..=turns {
        assert!(completed(&live.turn(&format!("u{turn}"), "next").await));
    }
    live.close().await;
    let mut records = records(&store);
    records.retain(|r| !matches!(r.event, LogEvent::SessionEnded { .. }));
    let opened = records
        .iter()
        .position(|r| matches!(r.event, LogEvent::TurnStarted { turn, .. } if turn == process_turn))
        .unwrap();
    let started = started_record(&records[opened], process_turn, 1);
    records.insert(opened + 1, started);
    for (seq, record) in records.iter_mut().enumerate() {
        record.seq = u64::try_from(seq).unwrap();
    }
    records
}

fn damaged_at(
    provider: Arc<Fake>,
    records: &[wisp_protocol::log::LogRecord],
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
    let (back, log) = continued(records);
    let agent = Agent::new(provider, default_session())
        .with_clock(Arc::new(crate::clock::StillClock))
        .with_log(log, log_context())
        .resume_lines(lines, 0)
        .unwrap();
    (agent, back)
}

fn lost_after(written: &[LogEvent], pick: &dyn Fn(&LogEvent) -> bool) {
    let before = written.iter().position(pick).unwrap();
    let lost = written
        .iter()
        .position(|e| {
            matches!(
                e,
                LogEvent::ProcessEnded {
                    id: 1,
                    end: ProcessEnd::Lost { .. },
                    ..
                }
            )
        })
        .unwrap();
    assert!(before < lost, "{written:?}");
    assert!(
        !written
            .iter()
            .any(|e| matches!(e, LogEvent::ProcessHeard { ids, .. } if ids.contains(&1)))
    );
}

#[tokio::test]
async fn a_process_from_a_dropped_turn_is_stopped_unnamed_and_its_number_kept() {
    let dir = scratch("dropped-turn");
    let earlier = turns_with_process(2, 2).await;
    let damaged = earlier
        .iter()
        .position(|r| matches!(r.event, LogEvent::MessageCommitted { turn: 2, .. }))
        .unwrap();
    let provider = scripted(vec![answer("Three.", 1_200)]);
    let (agent, back) = damaged_at(provider.clone(), &earlier, damaged);
    if !background(&agent, "c2", "exec sleep 30", &dir.root) {
        return;
    }
    let mut again = Live::start(agent);
    again
        .until(|e| matches!(e, AgentEvent::ProcessStarted { id: 2, .. }))
        .await;
    assert!(completed(&again.turn("u3", "third").await));
    again.close().await;

    assert_eq!(ends_of(&back, 1).len(), 1);
    let written = log_events(&back).split_off(earlier.len());
    lost_after(&written, &|e| matches!(e, LogEvent::ResumeFellBack { .. }));
    let texts = user_messages(&provider.body(0));
    assert!(!texts.iter().any(|t| t.contains("Process 1 ")), "{texts:?}");
}

fn ended_record(
    template: &wisp_protocol::log::LogRecord,
    id: u32,
) -> wisp_protocol::log::LogRecord {
    let event: LogEvent = serde_json::from_value(json!({
        "type": "process_ended",
        "id": id,
        "end": { "type": "exited", "exit": { "type": "code", "code": 0 } },
        "bytes": 0,
        "lines": 0,
        "duration_ms": 5,
    }))
    .unwrap();
    let mut record = template.clone();
    record.event = event;
    record
}

async fn first_request_after_dropping_turn_two(
    earlier: &[wisp_protocol::log::LogRecord],
) -> Vec<String> {
    let damaged = earlier
        .iter()
        .position(|r| matches!(r.event, LogEvent::MessageCommitted { turn: 2, .. }))
        .unwrap();
    let provider = scripted(vec![answer("Three.", 1_200)]);
    let (agent, back) = damaged_at(provider.clone(), earlier, damaged);
    let mut again = Live::start(agent);
    assert!(completed(&again.turn("u3", "third").await));
    again.close().await;
    assert!(
        log_events(&back)
            .iter()
            .any(|e| matches!(e, LogEvent::ResumeFellBack { .. }))
    );
    user_messages(&provider.body(0))
}

#[tokio::test]
async fn a_process_that_started_and_ended_in_a_dropped_turn_is_never_named() {
    let mut earlier = turns_with_process(2, 2).await;
    let started = earlier
        .iter()
        .position(|r| matches!(r.event, LogEvent::ProcessStarted { id: 1, .. }))
        .unwrap();
    let ended = ended_record(&earlier[started], 1);
    earlier.insert(started + 1, ended);
    let earlier = renumbered(earlier);
    let texts = first_request_after_dropping_turn_two(&earlier).await;
    assert!(!texts.iter().any(|t| t.contains("Process 1 ")), "{texts:?}");
}

#[tokio::test]
async fn an_unchecked_process_from_a_dropped_turn_is_never_named() {
    let mut earlier = turns_with_process(2, 2).await;
    for record in &mut earlier {
        if let LogEvent::ProcessStarted { boot_id, .. } = &mut record.event {
            *boot_id = None;
        }
    }
    let texts = first_request_after_dropping_turn_two(&earlier).await;
    assert!(!texts.iter().any(|t| t.contains("Process 1 ")), "{texts:?}");
}

#[tokio::test]
async fn a_fresh_start_stops_the_processes_of_the_log_it_left_behind() {
    let earlier = turns_with_process(1, 1).await;
    let provider = scripted(vec![answer("Two.", 1_100)]);
    let (agent, back) = damaged_at(provider.clone(), &earlier, 0);
    let mut again = Live::start(agent);
    assert!(completed(&again.turn("u2", "second").await));
    again.close().await;

    let written = log_events(&back).split_off(earlier.len());
    lost_after(&written, &|e| matches!(e, LogEvent::SessionStarted { .. }));
    let texts = user_messages(&provider.body(0));
    assert!(!texts.iter().any(|t| t.contains("Process 1 ")), "{texts:?}");
}

#[tokio::test]
async fn numbers_continue_after_the_processes_a_fallback_record_saw() {
    let dir = scratch("numbered-fallback");
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000)]);
    let mut live =
        Live::start(logged(provider, &store).with_clock(Arc::new(crate::clock::StillClock)));
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    let mut earlier = records(&store);
    let at = earlier.len() - 1;
    let mut record = earlier[at].clone();
    record.event = LogEvent::ResumeFellBack {
        billing: wisp_protocol::Billing::Metered,
        plan_usd: None,
        kept_through: None,
        dropped_through: None,
        covered_through: None,
        last_turn: Some(1),
        turns_seen: 1,
        processes_seen: 6,
        spent_usd: 0.0,
        damage: Vec::new(),
    };
    earlier.insert(at, record);
    for (seq, record) in earlier.iter_mut().enumerate() {
        record.seq = u64::try_from(seq).unwrap();
    }

    let (agent, _) = reopened_from(scripted(Vec::new()), &earlier);
    if !background(&agent, "c7", "exec sleep 30", &dir.root) {
        return;
    }
    let mut again = Live::start(agent);
    again
        .until(|e| matches!(e, AgentEvent::ProcessStarted { id: 7, .. }))
        .await;
    again.close().await;
}

fn started_ends(seen: &[AgentEvent]) -> Vec<Vec<(u32, ProcessEnd)>> {
    pick!(
        seen,
        AgentEvent::TurnStarted { ends, .. } => ends.iter().map(|r| (r.id, r.end.clone())).collect()
    )
}

fn logged_ends(store: &MemoryStore) -> Vec<Vec<(u32, ProcessEnd)>> {
    pick!(
        log_events(store),
        LogEvent::TurnStarted { ends, .. } => ends.iter().map(|r| (r.id, r.end.clone())).collect()
    )
}

#[tokio::test]
async fn a_process_stopped_by_the_restart_is_one_of_the_turns_reported_ends() {
    let dir = scratch("restart-ends");
    let Some(earlier) = a_session_with_process_one(&dir, true).await else {
        return;
    };
    let provider = scripted(vec![answer("Two.", 1_100)]);
    let (agent, store) = reopened_from(provider.clone(), &earlier);
    let mut again = Live::start(agent);
    assert!(completed(&again.turn("u2", "second").await));
    let (_, seen) = again.close().await;
    assert!(
        user_messages(&provider.body(0))
            .contains(&format!("<processes>\n{RESTARTED}\n</processes>")),
        "{:?}",
        user_messages(&provider.body(0))
    );
    let ends = started_ends(&seen);
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].len(), 1);
    assert_eq!(ends[0][0].0, 1);
    assert!(matches!(ends[0][0].1, ProcessEnd::Lost { .. }), "{ends:?}");
    let logged = logged_ends(&store);
    assert_eq!(logged.last(), ends.last());
}

#[tokio::test]
async fn a_resumed_session_hands_clients_its_processes_from_before_the_restart() {
    for crash in [false, true] {
        let dir = scratch(if crash { "listed-crash" } else { "listed" });
        let Some(earlier) = a_session_with_process_one(&dir, crash).await else {
            return;
        };
        let (agent, _) = reopened_from(scripted(Vec::new()), &earlier);
        let restored = agent.restored_processes();
        assert_eq!(restored.len(), 1, "{restored:?}");
        let AgentEvent::ProcessRestored {
            id, call_id, end, ..
        } = &restored[0]
        else {
            panic!("{restored:?}");
        };
        assert_eq!((*id, call_id.as_deref()), (1, Some("c1")));
        let expected = if crash {
            matches!(end, Some(ProcessEnd::Lost { .. }))
        } else {
            *end == Some(ProcessEnd::Stopped {
                by: wisp_protocol::StoppedBy::SessionEnded,
            })
        };
        assert!(expected, "{end:?}");
    }
}

#[tokio::test]
async fn the_turns_reported_ends_follow_the_restart_notices_order() {
    let mut earlier = turns_with_process(1, 1).await;
    let at = earlier
        .iter()
        .position(|r| matches!(r.event, LogEvent::ProcessStarted { id: 1, .. }))
        .unwrap();
    let second = started_record(&earlier[at], 1, 2);
    let ended = ended_record(&earlier[at], 1);
    earlier.insert(at + 1, second);
    earlier.insert(at + 2, ended);
    let earlier = renumbered(earlier);
    let provider = scripted(vec![answer("Two.", 1_100)]);
    let (agent, store) = reopened_from(provider.clone(), &earlier);
    let mut again = Live::start(agent);
    assert!(completed(&again.turn("u2", "second").await));
    let (_, seen) = again.close().await;
    let notice = last_user_text(&provider.body(0));
    let running = notice.find("Process 2 was still running").unwrap();
    let before = notice.find("Process 1 ").unwrap();
    assert!(running < before, "{notice}");
    let ends = started_ends(&seen);
    let ids: Vec<u32> = ends.concat().into_iter().map(|(id, _)| id).collect();
    assert_eq!(ids, [2, 1], "{ends:?}");
    assert_eq!(logged_ends(&store).last(), ends.last());
}

#[tokio::test]
async fn a_typed_line_from_a_dropped_turn_is_never_told() {
    let mut earlier = turns_with_process(2, 2).await;
    let started = earlier
        .iter()
        .position(|r| matches!(r.event, LogEvent::ProcessStarted { id: 1, .. }))
        .unwrap();
    let event: LogEvent = serde_json::from_value(json!({
        "type": "process_detached",
        "id": 1,
        "size": { "cols": 80, "rows": 24 },
        "reason": "user",
        "typed": true,
    }))
    .unwrap();
    let mut detached = earlier[started].clone();
    detached.event = event;
    earlier.insert(started + 1, detached);
    let earlier = renumbered(earlier);
    let texts = first_request_after_dropping_turn_two(&earlier).await;
    assert!(
        !texts.iter().any(|t| t.contains("You typed into process 1")),
        "{texts:?}"
    );
}

#[tokio::test]
async fn the_provisional_head_leaves_out_a_processs_notice() {
    let dir = scratch("provisional");
    let store = MemoryStore::default();
    let mut agent = logged(scripted(Vec::new()), &store);
    if !background(&agent, "c1", "echo early", &dir.root) {
        return;
    }
    let (events, mut heard) = mpsc::channel(1024);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        agent.drain_processes(&events).await;
        while heard.try_recv().is_ok() {}
        let head: Vec<String> = pick!(
            &agent.provisional_head(),
            ContentBlock::Text { text } => text.clone()
        );
        if let (Some(news), _) = agent.news(false) {
            let notice = wisp_plugin_shell::notice(&news).unwrap();
            assert!(notice.contains("<processes>"), "{notice}");
            assert!(head.iter().all(|t| !t.contains("<processes>")), "{head:?}");
            assert!(!head.is_empty());
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "no notice came");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

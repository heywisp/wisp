use wisp_protocol::log::ThreadSend;

use super::keep_warm::{an_hour_later, header, refreshes_read, without};
use super::*;
use crate::provider::anthropic::MESSAGE_THREADS_BETA;

fn thread_not_found() -> Reply {
    Reply {
        response: http::json(
            "404 Not Found",
            r#"{"type":"error","error":{"type":"not_found_error","message":"thread: not found","details":{"error_code":"thread_not_found"}}}"#,
        ),
        before: Vec::new(),
        gate: None,
    }
}

#[tokio::test]
async fn threads() {
    let setup = Setup {
        threads: true,
        clock: Clocks::Stepped,
        keep_warm: Some(8),
        ..Setup::default()
    };
    let rig = Rig::new(
        "threads",
        setup,
        vec![
            says("msg_01", 400, "Hello."),
            thread_not_found(),
            says("msg_02", 450, "Again."),
            says("msg_03", 500, "Still."),
            refreshes_read("msg_r1", 500),
            says("msg_04", 550, "Later."),
        ],
    )
    .await;
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.completed("u2", "again").await;
    live.completed("u3", "still").await;
    rig.steps.advance(jiff::SignedDuration::from_mins(55));
    an_hour_later().await;
    let refreshed = live
        .until(|e| matches!(e, AgentEvent::CacheRefreshed { .. }))
        .await;
    assert!(
        matches!(
            refreshed,
            AgentEvent::CacheRefreshed {
                outcome: wisp_protocol::RefreshOutcome::Read,
                ..
            }
        ),
        "{refreshed:?}"
    );
    live.completed("u4", "later").await;
    let (agent, _) = live.close().await;
    rig.check();
    assert_eq!(rig.server.unasked(), 0);

    let seen = rig.server.seen();
    let bodies: Vec<Value> = seen
        .bodies
        .iter()
        .map(|b| serde_json::from_str(b).unwrap())
        .collect();
    let threads: Vec<Option<&str>> = bodies
        .iter()
        .map(|b| b["thread"]["type"].as_str())
        .collect();
    assert_eq!(
        threads,
        [
            Some("create"),
            Some("continue"),
            Some("create"),
            Some("continue"),
            None,
            Some("create")
        ]
    );
    assert_eq!(bodies[1]["thread"]["previous_message_id"], "msg_01");
    assert_eq!(bodies[3]["thread"]["previous_message_id"], "msg_02");
    for (n, head) in seen.heads.iter().enumerate() {
        let beta = header(head, "anthropic-beta");
        assert_eq!(beta.contains(MESSAGE_THREADS_BETA), n != 4, "{n}: {beta}");
    }
    for continued in [&bodies[1], &bodies[3]] {
        assert!(continued.get("system").is_none() && continued.get("tools").is_none());
    }

    let full = bodies[5]["messages"].as_array().unwrap();
    let created = bodies[2]["messages"].as_array().unwrap();
    assert_eq!(full[..3], created[..]);
    assert_eq!(full[4..5], bodies[3]["messages"].as_array().unwrap()[..]);
    let mut logical = without(&seen.bodies[2], &["thread", "max_tokens", "fallbacks"]);
    logical["messages"] = Value::Array(full[..5].to_vec());
    assert_eq!(without(&seen.bodies[4], &["max_tokens"]), logical);

    let records = records(&rig.store);
    let steps: Vec<(Option<ThreadSend>, Option<String>)> = pick!(
        &records,
        LogRecord {
            event: LogEvent::StepEnded {
                thread,
                thread_error,
                ..
            },
            ..
        } => (*thread, thread_error.clone())
    );
    assert_eq!(
        steps,
        [
            (Some(ThreadSend::Create), None),
            (Some(ThreadSend::Replay), Some("thread_not_found".into())),
            (Some(ThreadSend::Continue), None),
            (Some(ThreadSend::Create), None),
        ]
    );
    assert!(
        !records
            .iter()
            .any(|r| matches!(r.event, LogEvent::Retrying { .. }))
    );
    let completed = records
        .iter()
        .filter(|r| {
            matches!(
                r.event,
                LogEvent::TurnEnded {
                    outcome: TurnOutcome::Completed { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(completed, 4);
    rig.assert_rebuilds(&agent);
}

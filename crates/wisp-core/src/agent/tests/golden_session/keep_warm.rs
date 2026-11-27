use super::*;

pub(super) async fn an_hour_later() {
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(55 * 60)).await;
    tokio::time::resume();
}

pub(super) fn refreshes_read(id: &str, read: u64) -> Reply {
    let mut out = event(
        "message_start",
        json!({"type": "message_start", "message": {
            "id": id, "type": "message", "role": "assistant", "model": DEFAULT_MODEL,
            "content": [], "stop_reason": null, "stop_sequence": null,
            "usage": {"input_tokens": 4, "cache_creation_input_tokens": 0, "cache_read_input_tokens": read, "output_tokens": 1}
        }}),
    );
    out.push_str(&block_start(
        0,
        &json!({"type": "thinking", "thinking": "", "signature": ""}),
    ));
    out.push_str(&event(
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": "max_tokens", "stop_sequence": null}, "usage": {"output_tokens": 1}}),
    ));
    out.push_str(&event("message_stop", json!({"type": "message_stop"})));
    Reply {
        response: http::sse(&out),
        before: Vec::new(),
        gate: None,
    }
}

pub(super) fn header(head: &str, name: &str) -> String {
    head.lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
        .unwrap_or_default()
}

pub(super) fn without(body: &str, fields: &[&str]) -> Value {
    let mut value: Value = serde_json::from_str(body).unwrap();
    if let Some(map) = value.as_object_mut() {
        for field in fields {
            map.remove(*field);
        }
    }
    value
}

fn messages_of(body: &str) -> &str {
    let at = body.find(r#""messages":"#).unwrap();
    &body[at + r#""messages":"#.len()..body.len() - 1]
}

fn assert_accepted_refresh(turn: &str, refresh: &str, heads: &[String]) -> Value {
    assert_eq!(
        without(refresh, &["max_tokens"]),
        without(turn, &["max_tokens", "fallbacks"])
    );
    let refresh_value: Value = serde_json::from_str(refresh).unwrap();
    assert_eq!(refresh_value["max_tokens"], 1);
    assert!(refresh_value.get("fallbacks").is_none());
    let betas = |head: &str| -> Vec<String> {
        header(head, "anthropic-beta")
            .split(',')
            .filter(|b| !b.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let turn_betas: Vec<String> = betas(&heads[0])
        .into_iter()
        .filter(|b| b != "server-side-fallback-2026-07-01")
        .collect();
    assert_eq!(betas(&heads[1]), turn_betas);
    assert_eq!(messages_of(refresh), messages_of(turn));
    refresh_value
}

#[tokio::test]
async fn keep_warm_refresh() {
    let setup = Setup {
        keep_warm: Some(8),
        ..Setup::default()
    };
    let rig = Rig::new(
        "keep_warm_refresh",
        setup,
        vec![
            says("msg_01", 400, "Hello."),
            refreshes_read("msg_r1", 400),
            says("msg_02", 520, "Again."),
        ],
    )
    .await;
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    an_hour_later().await;
    let refreshed = live
        .until(|e| matches!(e, AgentEvent::CacheRefreshed { .. }))
        .await;
    let AgentEvent::CacheRefreshed { outcome, cost, .. } = refreshed else {
        unreachable!()
    };
    assert_eq!(outcome, wisp_protocol::RefreshOutcome::Read);
    assert!(cost.is_some_and(|c| c.turn_usd > 0.0));
    live.completed("u2", "again").await;
    let (agent, _) = live.close().await;
    rig.check();

    let seen = rig.server.seen();
    let [turn, refresh, next] = seen.bodies.as_slice() else {
        panic!("{} requests", seen.bodies.len());
    };
    let refresh_value = assert_accepted_refresh(turn, refresh, &seen.heads);
    let open = messages_of(refresh).strip_suffix(']').unwrap();
    assert!(messages_of(next).starts_with(open));
    let (refresh_value, next_value): (Value, Value) =
        (refresh_value, serde_json::from_str(next).unwrap());
    assert_eq!(refresh_value["system"], next_value["system"]);
    assert_eq!(refresh_value["tools"], next_value["tools"]);

    let records = records(&rig.store);
    let refreshes: Vec<&LogRecord> = records
        .iter()
        .filter(|r| matches!(r.event, LogEvent::CacheRefresh { .. }))
        .collect();
    let [record] = refreshes.as_slice() else {
        panic!("{refreshes:#?}");
    };
    let LogEvent::CacheRefresh {
        turn,
        outcome,
        count,
        usage,
        cost_usd,
        ..
    } = &record.event
    else {
        unreachable!()
    };
    assert_eq!(
        (*turn, outcome.clone(), *count),
        (1, wisp_protocol::RefreshOutcome::Read, 1)
    );
    assert_eq!(usage.map(|u| u.cache_read_input_tokens), Some(400));
    assert!(cost_usd.is_some_and(|usd| usd > 0.0));
    let after = records.iter().skip_while(|r| r.seq != record.seq).skip(1);
    for r in after.take_while(|r| !matches!(r.event, LogEvent::TurnStarted { .. })) {
        assert!(
            !matches!(
                r.event,
                LogEvent::MessageCommitted { .. } | LogEvent::StepEnded { .. }
            ),
            "{r:?}"
        );
    }
    let turns = records
        .iter()
        .filter(|r| matches!(r.event, LogEvent::TurnStarted { .. }))
        .count();
    assert_eq!(turns, 2);
    let steps = records
        .iter()
        .filter(|r| matches!(r.event, LogEvent::StepEnded { .. }))
        .count();
    assert_eq!(steps, 2);
    rig.assert_rebuilds(&agent);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn memory_edit_waits_for_a_real_turn_not_the_keep_warm_refresh() {
    use super::sections::{assert_cache_order, memory::saved_note};
    use crate::agent::compact::tests::memory::{Session, tools};

    let server = Server::start(vec![
        says("msg_turn", 400, "Done."),
        refreshes_read("msg_refresh", 400),
        says("msg_next", 520, "Again."),
    ])
    .await;
    let mut tree = crate::test_support::rooted::tree();
    tree.keep_warm_main = true;
    let mut live = Session::new_seeded(
        server.provider(),
        tools(true),
        ModelSettings::default(),
        Default::default(),
        (false, true),
        Some(("fixture-note.md", &saved_note("The fixture dye is blue."))),
        tree,
    )
    .await;
    live.turn("u1", "Name a shape.").await;
    let edited = "The fixture dye is mauve.";
    fs::write(
        live.data.join("memory/user/fixture-note.md"),
        saved_note(edited),
    )
    .unwrap();
    let before = records(&live.store);
    an_hour_later().await;
    let refreshed = live
        .until(|e| matches!(e, AgentEvent::CacheRefreshed { .. }))
        .await;
    assert!(matches!(
        refreshed,
        AgentEvent::CacheRefreshed {
            outcome: wisp_protocol::RefreshOutcome::Read,
            ..
        }
    ));
    let idle = records(&live.store);
    let added = &idle[before.len()..];
    assert_eq!(
        added
            .iter()
            .filter(|r| matches!(r.event, LogEvent::CacheRefresh { .. }))
            .count(),
        1
    );
    assert!(!added.iter().any(|r| matches!(
        r.event,
        LogEvent::MessageCommitted { .. } | LogEvent::StepEnded { .. }
    )));
    live.turn("u2", "Name another shape.").await;
    live.finish().await;
    let seen = server.seen();
    let [turn, refresh, next] = seen.bodies.as_slice() else {
        panic!("expected a refresh between two turns")
    };
    assert!(turn.contains("<memory>") && turn.contains("fixture dye is blue"));
    assert_accepted_refresh(turn, refresh, &seen.heads);
    super::sections::memory::assert_raw_prefix(turn, refresh);
    for body in [turn, refresh, next] {
        assert_cache_order(&without(body, &[]));
    }
    assert!(!refresh.contains("<memory-update>") && !refresh.contains(edited));
    assert!(next.contains("<memory-update>") && next.contains(edited));
    let prefix = messages_of(refresh).strip_suffix(']').unwrap();
    assert!(
        messages_of(next)
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with(','))
    );
}

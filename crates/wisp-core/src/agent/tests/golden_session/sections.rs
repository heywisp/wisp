use std::sync::LazyLock;

use wisp_plugin_api::prompt::{At, Prior};
use wisp_plugin_testkit::hooks::{self as kit, Calls, Knob};
use wisp_protocol::RewindPoint;
use wisp_protocol::log::LogEvent;

use super::*;

const WISP: &str = "[models.\"claude-opus-5-5\"]\nnative_compaction = false\n";
const SMALL_WINDOW: &str =
    "[models.\"claude-opus-5-5\"]\ncontext_window = 104000\nnative_compaction = false\n";

static KNOB: LazyLock<Knob> = LazyLock::new(|| kit::knob("v1"));
static CALLS: LazyLock<Calls> = LazyLock::new(Calls::default);

fn notes() -> (String, crate::plugin::Make) {
    let body = kit::prompt_section("notes", "lead", "");
    let make: crate::plugin::Make =
        kit::prompted(kit::none(), kit::changing(KNOB.clone()), CALLS.clone());
    (kit::manifest_in("0.2.0", "bb", &body), make)
}

fn noted(setup: Setup) -> Setup {
    Setup {
        hooks: Some(notes),
        ..setup
    }
}

fn notes_in(body: &str, value: &str) -> usize {
    body.matches(&kit::section_text("notes", value).replace('\n', "\\n"))
        .count()
}

fn overloaded() -> Reply {
    Reply {
        response: http::json(
            "529 Overloaded",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        ),
        before: Vec::new(),
        gate: None,
    }
}

fn section_entries(records: &[LogRecord]) -> Vec<(u64, Option<String>)> {
    records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::TurnStarted { turn, lead, .. } => lead
                .entries
                .iter()
                .find(|e| e.by == "bb/notes")
                .map(|e| (*turn, e.snapshot.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_section_sends_its_text_only_on_the_turns_it_changes() {
    let script = vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
    ];
    let rig = Rig::new("section_changes", noted(Setup::default()), script).await;
    let mut live = rig.start();
    live.completed("u1", "one").await;
    live.completed("u2", "two").await;
    kit::turn_to(&KNOB, "v2");
    live.completed("u3", "three").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    let bodies = rig.server.seen().bodies;
    assert_eq!(
        bodies
            .iter()
            .map(|b| (notes_in(b, "v1"), notes_in(b, "v2")))
            .collect::<Vec<_>>(),
        [(1, 0), (1, 0), (1, 1)]
    );
    assert_eq!(
        section_entries(&records(&rig.store)),
        [(1, Some("v1".into())), (3, Some("v2".into()))]
    );
    let turns = CALLS.sections_at(At::Turn);
    assert!(
        turns
            .iter()
            .any(|c| c.previous == Prior::Snapshot("v1".into())),
        "{turns:?}"
    );
    assert_extends(rig.case, &bodies);
    rig.check();
}

#[tokio::test]
async fn a_retry_replays_the_failed_turns_section_and_after_its_own_head_does_not() {
    let setup = Setup {
        step_retries: Some(0),
        clock: Clocks::Stepped,
        models: Some(SMALL_WINDOW),
        ..Setup::default()
    };
    let script = vec![
        says("msg_01", 400, "One."),
        server_error(),
        says("msg_02", 100_000, "Two."),
        overloaded(),
        summary("msg_03", 100_000),
        says("msg_04", 2_000, "Three."),
    ];
    let rig = Rig::new("section_retry", noted(setup), script).await;
    let mut live = rig.start();
    live.completed("u1", "one").await;
    kit::turn_to(&KNOB, "v2");
    let failed = live.turn("u2", "two").await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    kit::turn_to(&KNOB, "v3");
    live.send(ClientCommand::Retry {
        turn: 2,
        endpoint: None,
        request: request(1),
    })
    .await;
    assert!(completed(&live.ended().await));
    let bodies = rig.server.seen().bodies;
    assert_eq!(
        (notes_in(&bodies[2], "v2"), notes_in(&bodies[2], "v3")),
        (1, 0),
        "{}",
        bodies[2]
    );
    kit::turn_to(&KNOB, "v4");
    let failed = live.turn("u3", "three").await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    live.send(ClientCommand::Retry {
        turn: 4,
        endpoint: None,
        request: request(2),
    })
    .await;
    assert!(completed(&live.ended().await));
    let (agent, seen) = live.close().await;
    rig.assert_rebuilds(&agent);
    assert!(
        seen.iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. })),
        "{seen:#?}"
    );
    let heads: Vec<_> = pick!(
        &log_events(&rig.store),
        LogEvent::ContextCompacted { sections, .. } => sections.clone()
    );
    assert_eq!(heads.len(), 1, "{heads:?}");
    assert_eq!(heads[0].len(), 1, "{heads:?}");
    assert_eq!(heads[0][0].snapshot, "v4");
    let bodies = rig.server.seen().bodies;
    let last = bodies.last().unwrap();
    assert_eq!(
        (notes_in(last, "v3"), notes_in(last, "v4")),
        (0, 1),
        "{last}"
    );
    assert_extends(rig.case, &[bodies[0].clone(), bodies[2].clone()]);
    rig.check();
}

#[tokio::test]
async fn a_head_re_bases_a_section_to_the_unsaved_messages_target() {
    let script = vec![
        says("msg_01", 400, "One."),
        says("msg_02", 100_000, "Two."),
        too_long(),
        summary("msg_03", 600),
        says("msg_04", 300, "Three."),
        says("msg_05", 400, "Four."),
    ];
    let rig = Rig::new(
        "section_head",
        noted(Setup {
            models: Some(WISP),
            ..Setup::default()
        }),
        script,
    )
    .await;
    let mut live = rig.start();
    live.completed("u1", "one").await;
    live.completed("u2", "two").await;
    kit::turn_to(&KNOB, "v2");
    live.completed("u3", "three").await;
    live.completed("u4", "four").await;
    let (agent, seen) = live.close().await;
    rig.assert_rebuilds(&agent);
    assert!(
        seen.iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. })),
        "{seen:#?}"
    );
    let heads = CALLS.sections_at(At::Head);
    assert!(
        heads
            .iter()
            .any(|c| c.target.as_deref() == Some("v2") && c.previous == Prior::None),
        "{heads:?}"
    );
    let compacted: Vec<_> = pick!(
        &log_events(&rig.store),
        LogEvent::ContextCompacted { sections, .. } => sections.clone()
    );
    assert_eq!(compacted.len(), 1);
    assert_eq!(
        compacted[0]
            .iter()
            .map(|s| (s.name.as_str(), s.snapshot.as_str()))
            .collect::<Vec<_>>(),
        [("bb/notes", "v2")]
    );
    let bodies = rig.server.seen().bodies;
    let resent = &bodies[4];
    assert_eq!(
        (notes_in(resent, "v1"), notes_in(resent, "v2")),
        (0, 2),
        "{resent}"
    );
    let last = bodies.last().unwrap();
    assert_eq!(notes_in(last, "v2"), 2, "{last}");
    assert_extends(rig.case, &bodies[4..]);
    rig.check();
}

#[tokio::test]
async fn a_rewind_after_a_resume_restores_the_committed_snapshot() {
    let setup = Setup {
        clock: Clocks::Stepped,
        ..Setup::default()
    };
    let script = vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
        says("msg_04", 760, "Four."),
    ];
    let rig = Rig::new("section_rewind_resume", noted(setup), script).await;
    let mut live = rig.start();
    live.completed("u1", "one").await;
    kit::turn_to(&KNOB, "v2");
    live.completed("u2", "two").await;
    let (_, _) = live.close().await;
    let mut back = rig.resume();
    back.completed("u3", "three").await;
    back.send(ClientCommand::Rewind {
        to: RewindPoint::Turn { turn: 2 },
        request: request(1),
    })
    .await;
    let done = back
        .until(|e| {
            matches!(e, AgentEvent::Rewound { .. })
                || matches!(e, AgentEvent::Rejected { request: Some(r), .. } if *r == request(1))
        })
        .await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    back.completed("u4", "four").await;
    let (agent, _) = back.close().await;
    rig.assert_rebuilds(&agent);
    let bodies = rig.server.seen().bodies;
    let last = bodies.last().unwrap();
    assert_eq!(
        (notes_in(last, "v1"), notes_in(last, "v2")),
        (1, 1),
        "{last}"
    );
    let calls = CALLS.sections_at(At::Turn);
    assert_eq!(
        calls.last().map(|c| c.previous.clone()),
        Some(Prior::Snapshot("v1".into())),
        "{calls:?}"
    );
    rig.check();
}

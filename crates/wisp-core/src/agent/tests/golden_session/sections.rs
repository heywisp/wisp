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

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod memory {
    use wisp_protocol::settings::{SettingChange, SettingValue};

    use super::*;
    use crate::agent::compact::tests::memory::{Session, tools};

    fn saved_note(fact: &str) -> String {
        format!("---\nname: fixture-note\ndescription: {fact}\nsaved: 2000-01-02\n---\n")
    }

    fn requests(server: &Server) -> Vec<Value> {
        server
            .seen()
            .bodies
            .iter()
            .map(|body| serde_json::from_str(body).unwrap())
            .collect()
    }

    #[derive(serde::Deserialize)]
    struct RawPrefix<'a> {
        #[serde(borrow)]
        system: &'a serde_json::value::RawValue,
        #[serde(borrow)]
        tools: &'a serde_json::value::RawValue,
    }

    async fn apply(live: &mut Session, id: u64, key: &str, value: SettingValue) {
        live.send(ClientCommand::ChangeSettings {
            request: RequestId(id),
            changes: vec![SettingChange::named(key, value).unwrap()],
            save: false,
        })
        .await;
        let event = live
            .until(|event| match event {
                AgentEvent::SettingsChanged { answers, .. } => *answers == Some(RequestId(id)),
                AgentEvent::Rejected { request, .. } => *request == Some(RequestId(id)),
                _ => false,
            })
            .await;
        assert!(
            matches!(event, AgentEvent::SettingsChanged { .. }),
            "{event:?}"
        );
    }

    #[tokio::test]
    async fn rooted_memory_serialized_prefix_is_append_only_until_the_new_head() {
        let server = Server::start(vec![
            says("msg_empty", 400, "No notes yet."),
            calls("msg_seed", 500, vec![call("toolu_save", "remember", json!({"action":"save","scope":"you","name":"fixture-note","fact":"Initial fixture fact."}))]),
            says("msg_saved", 600, "Saved."),
            says("msg_full", 700, "Continuing."),
            says("msg_changed", 800, "Continuing."),
            says("msg_unchanged", 900, "Continuing."),
            summary("msg_summary", 1000),
            says("msg_head", 500, "Continuing."),
            says("msg_append", 600, "Continuing."),
        ]).await;
        let mut live = Session::new(
            server.provider(),
            tools(true),
            models(WISP),
            Default::default(),
            false,
            true,
        )
        .await;
        live.repository();
        live.turn("empty", "First turn.").await;
        assert!(!live.data.join("core/project-secret").exists());
        live.turn("seed", "Please save a fixture note.").await;
        let file = live.data.join("memory/user/fixture-note.md");
        assert!(
            fs::read_to_string(&file)
                .unwrap()
                .contains("Initial fixture fact.")
        );
        live.turn("full", "Use the earlier note.").await;
        fs::write(file, saved_note("Hand-edited fixture fact.")).unwrap();
        live.turn("changed", "Another turn.").await;
        live.turn("unchanged", "Another turn.").await;
        assert!(matches!(
            live.compact(10).await,
            AgentEvent::Compacted { .. }
        ));
        live.turn("head", "After shortening.").await;
        live.turn("append", "One more.").await;
        live.finish().await;
        let bodies = server.seen().bodies;
        let seen = requests(&server);
        assert_eq!(seen.len(), 9);
        let first: RawPrefix<'_> = serde_json::from_str(&bodies[0]).unwrap();
        for (body, wire) in seen.iter().zip(&bodies) {
            assert_eq!(body["tools"], seen[0]["tools"]);
            assert_eq!(body["system"], seen[0]["system"]);
            let prefix: RawPrefix<'_> = serde_json::from_str(wire).unwrap();
            assert_eq!(prefix.tools.get(), first.tools.get());
            assert_eq!(prefix.system.get(), first.system.get());
        }
        for (i, (full, updates)) in [
            (0, 0),
            (0, 0),
            (0, 0),
            (1, 0),
            (1, 1),
            (1, 1),
            (1, 0),
            (1, 0),
            (1, 0),
        ]
        .into_iter()
        .enumerate()
        .filter(|(i, _)| *i != 6)
        {
            assert_eq!(bodies[i].matches("<memory>").count(), full, "request {i}");
            assert_eq!(
                bodies[i].matches("<memory-update>").count(),
                updates,
                "request {i}"
            );
        }
        assert!(bodies[7].contains("Hand-edited fixture fact."));
        for (case, window) in [("rooted memory", 0..6), ("rooted memory after head", 7..9)] {
            assert_extends(case, &bodies[window.clone()]);
            for pair in seen[window].windows(2) {
                assert!(
                    pair[0]["messages"].as_array().unwrap().len()
                        < pair[1]["messages"].as_array().unwrap().len()
                );
            }
        }
        assert_eq!(server.unasked(), 0);
    }

    #[tokio::test]
    async fn rooted_memory_reads_the_registered_limit_and_live_tool_capability() {
        let server = Server::start(vec![
            calls("msg_seed", 500, vec![call("toolu_save", "remember", json!({"action":"save","scope":"you","name":"fixture-note","fact":"A fixture fact."}))]),
            says("msg_saved", 650, "Saved."),
            says("msg_full", 750, "Continuing."),
            says("msg_tight", 850, "Continuing."),
            says("msg_enabled", 950, "Continuing."),
        ]).await;
        let mut live = Session::new(
            server.provider(),
            tools(true),
            models(WISP),
            crate::settings::host::StartLayers {
                full_screen: true,
                ..Default::default()
            },
            false,
            true,
        )
        .await;
        let limit = wisp_protocol::settings::find("plugins.memory.list_limit").unwrap();
        assert_eq!(limit.name, "plugins.memory.list_limit");
        live.turn("seed", "Please save a fixture note.").await;
        let dir = live.data.join("memory/user");
        for n in 0..18 {
            let name = format!("painted-border-on-the-far-side-of-the-wall-{n}");
            fs::write(
                dir.join(format!("{name}.md")),
                format!(
                    "---\nname: {name}\ndescription: '{}'\nsaved: 2000-01-01\n---\n",
                    "&".repeat(44)
                ),
            )
            .unwrap();
        }
        live.turn("full", "Use the saved notes.").await;
        apply(&mut live, 1, limit.name, SettingValue::Int(256)).await;
        apply(
            &mut live,
            2,
            "tools",
            SettingValue::List(vec!["echo".into()]),
        )
        .await;
        live.turn("tight", "Continue without the memory tool.")
            .await;
        let current = requests(&server);
        let candidates = current[3]["messages"].to_string();
        let (_, after) = candidates.rsplit_once("<memory-update>").unwrap();
        let (update, _) = after.split_once("</memory-update>").unwrap();
        assert!(
            update.contains("Replaces the previous list"),
            "lowering the limit must reset the advertised set"
        );
        assert!(
            !update.contains("Use remember"),
            "the disabled capability must not be advertised"
        );
        assert!(wisp_tool_text::compaction::text_tokens(update) < 256);
        apply(
            &mut live,
            3,
            "tools",
            SettingValue::List(vec!["echo".into(), "remember".into()]),
        )
        .await;
        fs::write(
            dir.join("fixture-note.md"),
            saved_note("The current fixture fact."),
        )
        .unwrap();
        live.turn("enabled", "Continue with the memory tool.").await;
        let current = requests(&server);
        let candidates = current[4]["messages"].to_string();
        let (_, after) = candidates.rsplit_once("<memory-update>").unwrap();
        assert!(after.contains("The current fixture fact.") && after.contains("Use remember"));
        live.finish().await;
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn rooted_memory_keeps_accepted_section_bytes_on_resume_and_rewind() {
    use crate::agent::compact::tests::memory::{Session, tools};

    let server = Server::start(vec![
        calls("msg_seed", 500, vec![call("toolu_save", "remember", json!({"action":"save","scope":"you","name":"fixture-note","fact":"Keep the fixture rosette cobalt."}))]),
        says("msg_saved", 600, "Saved."),
        says("msg_full", 700, "Continuing."),
        says("msg_before_close", 800, "Continuing."),
        says("msg_after_resume", 900, "Continuing."),
        says("msg_rewound", 1000, "Continuing."),
    ]).await;
    let mut live = Session::new(
        server.provider(),
        tools(true),
        models(WISP),
        Default::default(),
        false,
        true,
    )
    .await;
    live.turn("seed", "Please save a fixture note.").await;
    live.turn("full", "Use the saved note.").await;
    live.turn("before-close", "Continue before closing.").await;
    let mut live = live.resume().await;
    live.turn("after-resume", "Continue after opening again.")
        .await;
    live.send(ClientCommand::Rewind {
        to: RewindPoint::Turn { turn: 3 },
        request: RequestId(5),
    })
    .await;
    let rewound = live
        .until(|e| {
            matches!(
                e,
                AgentEvent::Rewound { .. }
                    | AgentEvent::Rejected {
                        request: Some(RequestId(5)),
                        ..
                    }
            )
        })
        .await;
    assert!(matches!(rewound, AgentEvent::Rewound { .. }), "{rewound:?}");
    live.turn("rewound", "Continue from the earlier point.")
        .await;
    live.finish().await;
    let bodies = server.seen().bodies;
    assert_eq!(bodies.len(), 6);
    let note_bytes = |body: &str| {
        body.split_once("<memory>")
            .unwrap()
            .1
            .split_once("</memory>")
            .unwrap()
            .0
            .to_owned()
    };
    let original = note_bytes(&bodies[2]);
    assert_eq!(note_bytes(&bodies[4]), original);
    assert_eq!(note_bytes(&bodies[5]), original);
    assert_extends("rooted memory on resume", &bodies[2..5]);
    assert_extends(
        "rooted memory on rewind",
        &[bodies[2].clone(), bodies[5].clone()],
    );
}

use wisp_protocol::log::LogEvent;

use super::*;

const SMALL_WINDOW: &str =
    "[models.\"claude-opus-5-5\"]\ncontext_window = 104000\nnative_compaction = false\n";
const OPENING: &str = "<instructions-update>\nThese files changed since they were loaded. Each version below replaces the earlier one.\n";
const IMPORT: &str = "Read @notes.md before you start.\n";

fn agents(rig: &Rig, rule: &str) {
    fs::write(
        rig.env.work().join("AGENTS.md"),
        format!("{rule}\n{IMPORT}"),
    )
    .unwrap();
}

fn update(rule: &str) -> String {
    format!(
        "{OPENING}<file scope=\"project\" name=\"AGENTS.md\" path=\"~/work/AGENTS.md\">\n{rule}\n{IMPORT}</file>\n</instructions-update>\n"
    )
}

fn json_text(text: &str) -> String {
    serde_json::to_string(text).unwrap()
}

fn count(body: &str, text: &str) -> usize {
    let quoted = json_text(text);
    body.matches(&quoted[1..quoted.len() - 1]).count()
}

fn system(body: &str) -> Value {
    serde_json::from_str::<Value>(body).unwrap()["system"].clone()
}

fn last_user(body: &str) -> Vec<String> {
    let json: Value = serde_json::from_str(body).unwrap();
    let messages = json["messages"].as_array().unwrap();
    let user = messages
        .iter()
        .rev()
        .find(|m| m["role"] == "user" && m["content"][0]["type"] == "text")
        .unwrap();
    user["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["text"].as_str().map(str::to_owned))
        .collect()
}

fn changed_records(records: &[LogRecord]) -> Vec<(Option<u64>, Option<u64>, bool, String)> {
    records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::InstructionsChanged {
                turn,
                epoch,
                ahead,
                text,
                ..
            } => Some((*turn, *epoch, *ahead, text.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_edit_goes_in_the_next_message_after_the_environment_and_before_the_stamp() {
    let script = vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
    ];
    let rig = Rig::new("instructions_delta_edit", in_sub(), script).await;
    rig.project();
    let mut live = rig.start();
    live.completed("u1", "one").await;
    agents(&rig, "Answer at length.");
    live.completed("u2", "two").await;
    live.completed("u3", "three").await;
    let (agent, seen) = live.close().await;
    rig.assert_rebuilds(&agent);
    let bodies = rig.server.seen().bodies;
    let delta = update("Answer at length.");
    assert_eq!(
        bodies.iter().map(|b| count(b, &delta)).collect::<Vec<_>>(),
        [0, 1, 1]
    );
    let opened = last_user(&bodies[1]);
    let at = opened.iter().position(|t| *t == delta).unwrap();
    assert!(
        at == 0 || opened[at - 1].starts_with("<environment>"),
        "{opened:?}"
    );
    assert!(opened[at + 1].starts_with("Sent: "), "{opened:?}");
    for body in &bodies {
        assert_eq!(
            system(body),
            system(&bodies[0]),
            "the project block is unchanged"
        );
        assert!(!json_text(&system(body).to_string()).contains("instructions-update"));
    }
    assert_extends(rig.case, &bodies);
    assert_eq!(
        changed_records(&records(&rig.store)),
        [(Some(2), None, false, delta)]
    );
    let events: Vec<_> = seen
        .iter()
        .filter(|e| matches!(e, AgentEvent::InstructionsChanged { .. }))
        .collect();
    assert!(
        matches!(
            events[..],
            [AgentEvent::InstructionsChanged {
                turn: Some(2),
                sent: true,
                ahead: false,
                ..
            }]
        ),
        "{events:#?}"
    );
    rig.check();
}

#[tokio::test]
async fn a_head_built_while_the_message_is_unsaved_goes_to_its_target_not_the_newer_files() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let script = vec![
        says("msg_01", 400, "One."),
        says("msg_02", 100_000, "Two."),
        reply_error(
            "400 Bad Request",
            "invalid_request_error",
            "prompt is too long",
        )
        .gated(&gate),
        greeted("msg_03", 100_000),
        says("msg_04", 300, "Three."),
        says("msg_05", 400, "Four."),
    ];
    let setup = Setup {
        models: Some("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n"),
        ..in_sub()
    };
    let rig = Rig::new("instructions_delta_head", setup, script).await;
    rig.project();
    let mut live = rig.start();
    live.completed("u1", "one").await;
    live.completed("u2", "two").await;
    agents(&rig, "Rule A.");
    live.send(ClientCommand::send("u3", "three")).await;
    rig.server.wait_bodies(3).await;
    agents(&rig, "Rule B.");
    gate.add_permits(1);
    let outcome = live.ended().await;
    assert!(completed(&outcome), "{outcome:?}\n{:#?}", live.seen);
    live.completed("u4", "four").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    let bodies = rig.server.seen().bodies;
    let (a, b) = (update("Rule A."), update("Rule B."));
    let resent = &bodies[4];
    let environment = resent.find("<environment>").unwrap();
    let opening = json_text(OPENING);
    let head_update = resent.find(&opening[1..opening.len() - 1]).unwrap();
    assert!(
        environment < head_update,
        "the update follows <environment>"
    );
    assert_eq!(
        (count(resent, &a), count(resent, "Rule B.")),
        (2, 0),
        "{resent}"
    );
    let last = bodies.last().unwrap();
    assert_eq!(count(last, &a), 2, "{last}");
    assert_eq!(
        count(
            last,
            &format!(
                "{OPENING}<file scope=\"project\" name=\"AGENTS.md\" path=\"~/work/AGENTS.md\">\nRule B.\n"
            )
        ),
        1,
        "{last}"
    );
    let compacted: Vec<_> = pick!(
        &log_events(&rig.store),
        LogEvent::ContextCompacted { sections, .. } => sections.clone()
    );
    let target = compacted[0]
        .iter()
        .find(|s| s.name == "instructions/instructions")
        .unwrap();
    assert!(
        target
            .snapshot
            .starts_with(&wisp_instructions::sha256_hex(&format!(
                "Rule A.\n{IMPORT}"
            )))
    );
    let records = changed_records(&records(&rig.store));
    assert!(
        records
            .iter()
            .any(|(turn, epoch, ahead, text)| *turn == Some(3)
                && epoch.is_some()
                && *ahead
                && *text == a),
        "{records:#?}"
    );
    assert!(
        records
            .iter()
            .any(|(turn, _, _, text)| *turn == Some(4) && *text == b)
    );
    assert_extends(rig.case, &bodies[4..]);
    rig.check();
}

#[tokio::test]
async fn a_retry_replays_the_delta_and_after_its_own_head_sends_none() {
    let setup = Setup {
        step_retries: Some(0),
        clock: Clocks::Stepped,
        models: Some(SMALL_WINDOW),
        ..in_sub()
    };
    let script = vec![
        says("msg_01", 400, "One."),
        server_error(),
        says("msg_02", 100_000, "Two."),
        reply_error("529 Overloaded", "overloaded_error", "Overloaded"),
        greeted("msg_03", 100_000),
        says("msg_04", 2_000, "Three."),
    ];
    let rig = Rig::new("instructions_delta_retry", setup, script).await;
    rig.project();
    let mut live = rig.start();
    live.completed("u1", "one").await;
    agents(&rig, "Rule A.");
    let failed = live.turn("u2", "two").await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    agents(&rig, "Rule B.");
    live.send(ClientCommand::Retry {
        turn: 2,
        endpoint: None,
        request: request(1),
    })
    .await;
    assert!(completed(&live.ended().await));
    let bodies = rig.server.seen().bodies;
    assert_eq!(count(&bodies[1], &update("Rule A.")), 1);
    assert_eq!(
        (
            count(&bodies[2], &update("Rule A.")),
            count(&bodies[2], "Rule B.")
        ),
        (1, 0),
        "{}",
        bodies[2]
    );
    agents(&rig, "Rule C.");
    let failed = live.turn("u3", "three").await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    agents(&rig, "Rule D.");
    live.send(ClientCommand::Retry {
        turn: 4,
        endpoint: None,
        request: request(2),
    })
    .await;
    assert!(completed(&live.ended().await));
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    let bodies = rig.server.seen().bodies;
    let last = bodies.last().unwrap();
    assert_eq!(
        (count(last, "Rule C."), count(last, &update("Rule D."))),
        (0, 1),
        "the Retry's own head carries the current files and the Retry no delta: {last}"
    );
    let records = changed_records(&records(&rig.store));
    assert_eq!(
        records
            .iter()
            .map(|(turn, epoch, ahead, _)| (*turn, epoch.is_some(), *ahead))
            .collect::<Vec<_>>(),
        [
            (Some(2), false, false),
            (Some(4), false, false),
            (Some(5), true, false)
        ],
        "{records:#?}"
    );
    rig.check();
}

#[tokio::test]
async fn an_edit_made_while_wisp_was_closed_arrives_as_a_delta_after_the_resume() {
    let setup = Setup {
        clock: Clocks::Stepped,
        ..in_sub()
    };
    let script = vec![says("msg_01", 400, "One."), says("msg_02", 520, "Two.")];
    let rig = Rig::new("instructions_delta_resume", setup, script).await;
    rig.project();
    let mut live = rig.start();
    live.completed("u1", "one").await;
    let (_, _) = live.close().await;
    agents(&rig, "Answer at length.");
    let mut back = rig.resume();
    back.completed("u2", "two").await;
    let (agent, _) = back.close().await;
    rig.assert_rebuilds(&agent);
    let bodies = rig.server.seen().bodies;
    assert_eq!(system(&bodies[1]), system(&bodies[0]));
    assert_eq!(
        count(&bodies[1], &update("Answer at length.")),
        1,
        "{}",
        bodies[1]
    );
    assert_extends(rig.case, &bodies);
    rig.check();
}

fn source_change(n: u64, source: &str) -> ClientCommand {
    ClientCommand::ChangeSettings {
        request: request(n),
        changes: vec![
            wisp_protocol::settings::SettingChange::named(
                "instructions.source",
                wisp_protocol::settings::SettingValue::Text(source.to_owned()),
            )
            .unwrap(),
        ],
        save: false,
    }
}

async fn changed(live: &mut Live, n: u64, command: ClientCommand) {
    live.send(command).await;
    let answer = live
        .until(|e| match e {
            AgentEvent::SettingsChanged { answers, .. } => *answers == Some(request(n)),
            AgentEvent::Rejected { request: r, .. } => *r == Some(request(n)),
            _ => false,
        })
        .await;
    assert!(
        matches!(answer, AgentEvent::SettingsChanged { .. }),
        "{answer:?}"
    );
}

fn default_file(rig: &Rig, text: &str) {
    fs::create_dir_all(rig.env.root.join("config")).unwrap();
    fs::write(rig.env.root.join("config").join("AGENTS.md"), text).unwrap();
}

#[tokio::test]
async fn a_source_switch_mid_turn_stays_pending_and_none_and_back_send_one_delta_each() {
    let script = vec![
        calls(
            "msg_01",
            400,
            vec![call("toolu_01", "bash", json!({"command": "echo hi"}))],
        )
        .then(source_change(1, "default")),
        says("msg_02", 520, "One."),
        says("msg_03", 640, "Two."),
        says("msg_04", 760, "Three."),
        says("msg_05", 880, "Four."),
    ];
    let rig = Rig::new("instructions_delta_edit", in_sub(), script).await;
    rig.project();
    default_file(&rig, "Prefer British spelling.\n");
    let mut live = rig.start();
    live.completed("u1", "one").await;
    live.completed("u2", "two").await;
    changed(&mut live, 2, source_change(2, "none")).await;
    live.completed("u3", "three").await;
    changed(&mut live, 3, source_change(3, "auto")).await;
    live.completed("u4", "four").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    let bodies = rig.server.seen().bodies;
    assert_eq!(
        bodies.iter().map(|b| count(b, OPENING)).collect::<Vec<_>>(),
        [0, 0, 1, 2, 3],
        "the switch made mid-turn waits for the next turn"
    );
    let switched = format!(
        "{OPENING}<file scope=\"default\" name=\"AGENTS.md\" path=\"~/config/AGENTS.md\">\nPrefer British spelling.\n</file>\n<removed scope=\"project\" path=\"~/work/AGENTS.md\"/>\n<removed scope=\"project\" path=\"~/work/notes.md\"/>\n</instructions-update>\n"
    );
    assert_eq!(count(&bodies[2], &switched), 1, "{}", bodies[2]);
    let none = format!(
        "{OPENING}<removed scope=\"default\" path=\"~/config/AGENTS.md\"/>\nNo instructions apply from now on.\n</instructions-update>\n"
    );
    assert_eq!(count(&bodies[3], &none), 1, "{}", bodies[3]);
    let back = format!(
        "{OPENING}<file scope=\"project\" name=\"AGENTS.md\" path=\"~/work/AGENTS.md\">\nAnswer briefly.\n{IMPORT}</file>\n<file scope=\"project\" name=\"notes.md\" path=\"~/work/notes.md\">\nKeep notes short.\n</file>\n</instructions-update>\n"
    );
    assert_eq!(count(&bodies[4], &back), 1, "{}", bodies[4]);
    for body in &bodies {
        assert_eq!(system(body), system(&bodies[0]));
    }
    assert_extends(rig.case, &bodies);
}

#[tokio::test]
async fn a_failed_first_request_keeps_the_base_and_the_next_turn_folds_a_later_edit_in() {
    let setup = Setup {
        step_retries: Some(0),
        ..in_sub()
    };
    let script = vec![
        says("msg_01", 400, "One."),
        server_error(),
        says("msg_02", 520, "Three."),
    ];
    let rig = Rig::new("instructions_delta_edit", setup, script).await;
    rig.project();
    let mut live = rig.start();
    live.completed("u1", "one").await;
    agents(&rig, "Rule A.");
    let failed = live.turn("u2", "two").await;
    assert!(matches!(failed, TurnOutcome::Error { .. }), "{failed:?}");
    agents(&rig, "Rule B.");
    live.completed("u3", "three").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    let bodies = rig.server.seen().bodies;
    assert_eq!(count(&bodies[1], &update("Rule A.")), 1);
    let last = bodies.last().unwrap();
    assert_eq!(
        (count(last, "Rule A."), count(last, &update("Rule B."))),
        (0, 1),
        "{last}"
    );
}

#[tokio::test]
async fn with_the_plugin_off_the_block_stays_and_nothing_is_sent_until_it_is_back_on() {
    use wisp_protocol::settings::ENABLED_KEY;
    let script = vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
    ];
    let rig = Rig::new("instructions_delta_edit", in_sub(), script).await;
    rig.project();
    let mut live = rig.start();
    live.completed("u1", "one").await;
    let off = crate::plugin::testing::plugin_switch(1, "instructions", ENABLED_KEY, false);
    changed(&mut live, 1, off).await;
    agents(&rig, "Rule A.");
    live.completed("u2", "two").await;
    let on = crate::plugin::testing::plugin_switch(2, "instructions", ENABLED_KEY, true);
    changed(&mut live, 2, on).await;
    live.completed("u3", "three").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    let bodies = rig.server.seen().bodies;
    assert_eq!(
        bodies.iter().map(|b| count(b, OPENING)).collect::<Vec<_>>(),
        [0, 0, 1]
    );
    assert_eq!(count(&bodies[2], &update("Rule A.")), 1);
    for body in &bodies {
        assert_eq!(system(body), system(&bodies[0]), "the project block stays");
    }
}

fn notes(seen: &[AgentEvent]) -> Vec<(Option<u64>, Vec<String>)> {
    seen.iter()
        .filter_map(|e| match e {
            AgentEvent::InstructionsChanged {
                retry_of: None,
                ahead: false,
                sent: true,
                epoch,
                changed,
                ..
            } if !changed.is_empty() => Some((
                *epoch,
                changed.iter().map(|c| c.display_path.clone()).collect(),
            )),
            _ => None,
        })
        .collect()
}

fn compacting() -> Setup {
    Setup {
        models: Some("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n"),
        ..in_sub()
    }
}

#[tokio::test]
async fn an_update_already_sent_is_carried_by_a_later_head_but_noted_once() {
    let script = vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        greeted("msg_03", 100_000),
        says("msg_04", 300, "Three."),
    ];
    let rig = Rig::new("instructions_delta_edit", compacting(), script).await;
    rig.project();
    let mut live = rig.start();
    live.completed("u1", "one").await;
    agents(&rig, "Rule A.");
    live.completed("u2", "two").await;
    live.compacted(1).await;
    live.completed("u3", "three").await;
    let (agent, seen) = live.close().await;
    rig.assert_rebuilds(&agent);
    let last = rig.server.seen().bodies.last().unwrap().clone();
    assert_eq!(count(&last, &update("Rule A.")), 1, "{last}");
    assert_eq!(
        notes(&seen),
        [(None, vec!["~/work/AGENTS.md".to_owned()])],
        "{:#?}",
        changed_records(&records(&rig.store))
    );
    assert_eq!(changed_records(&records(&rig.store)).len(), 2);
}

#[tokio::test]
async fn a_head_after_a_second_edit_notes_only_the_second() {
    let script = vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        greeted("msg_03", 100_000),
    ];
    let rig = Rig::new("instructions_delta_edit", compacting(), script).await;
    rig.project();
    let mut live = rig.start();
    live.completed("u1", "one").await;
    agents(&rig, "Rule A.");
    live.completed("u2", "two").await;
    fs::write(rig.env.work().join("notes.md"), "Keep notes long.\n").unwrap();
    live.compacted(1).await;
    let (agent, seen) = live.close().await;
    rig.assert_rebuilds(&agent);
    let notes = notes(&seen);
    assert_eq!(notes.len(), 2, "{notes:#?}");
    assert_eq!(notes[1].1, ["~/work/notes.md"]);
    assert!(notes[1].0.is_some());
    let head = changed_records(&records(&rig.store)).pop().unwrap().3;
    assert!(
        head.contains("Rule A.") && head.contains("Keep notes long."),
        "the head still carries both: {head}"
    );
}

#[tokio::test]
async fn an_update_whose_first_request_failed_appears_again_on_the_next_turn() {
    let setup = Setup {
        step_retries: Some(0),
        ..in_sub()
    };
    let script = vec![
        says("msg_01", 400, "One."),
        server_error(),
        says("msg_02", 520, "Three."),
    ];
    let rig = Rig::new("instructions_delta_edit", setup, script).await;
    rig.project();
    let mut live = rig.start();
    live.completed("u1", "one").await;
    agents(&rig, "Rule A.");
    let failed = live.turn("u2", "two").await;
    assert!(matches!(failed, TurnOutcome::Error { .. }), "{failed:?}");
    live.completed("u3", "three").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    let last = rig.server.seen().bodies.last().unwrap().clone();
    assert!(
        last_user(&last).contains(&update("Rule A.")),
        "{:?}",
        last_user(&last)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_update_whose_turn_was_cancelled_appears_again_on_the_next_turn() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let script = vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Never.").gated(&gate),
        says("msg_03", 640, "Three."),
    ];
    let rig = Rig::new("instructions_delta_edit", in_sub(), script).await;
    rig.project();
    let mut live = rig.start();
    live.completed("u1", "one").await;
    agents(&rig, "Rule A.");
    live.send(ClientCommand::send("u2", "two")).await;
    rig.server.wait_bodies(2).await;
    live.send(ClientCommand::Cancel).await;
    let cancelled = live.ended().await;
    assert!(
        matches!(cancelled, TurnOutcome::Cancelled { .. }),
        "{cancelled:?}"
    );
    gate.add_permits(1);
    live.completed("u3", "three").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    let last = rig.server.seen().bodies.last().unwrap().clone();
    assert!(
        last_user(&last).contains(&update("Rule A.")),
        "{:?}",
        last_user(&last)
    );
}

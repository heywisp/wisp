use std::fs;

use serde_json::json;
use wisp_protocol::{ExitStatus, LostReason, ProcessEnd, StoppedBy};

use super::{Rig, background, have_bash};
use crate::jobs::Restored;
use crate::jobs::state::Ended;

fn lost() -> Ended {
    Ended::of(ProcessEnd::Lost {
        reason: LostReason::StoppedAtResume,
    })
}

fn restored(
    rig: &Rig,
    id: u32,
    body: Option<&str>,
    ended: Ended,
    terminal: Option<bool>,
) -> String {
    let path = rig.scratch.root.join(format!("tmp/{id}.out"));
    if let Some(body) = body {
        fs::write(&path, body).unwrap();
    }
    rig.processes.restore(
        id,
        Restored {
            call_id: None,
            target: Some(format!("make {id}")),
            file: Some(path.clone()),
            ended: crate::jobs::RestoredEnd::Ended(ended),
            terminal,
        },
    );
    path.display().to_string()
}

#[tokio::test]
async fn a_lost_process_reads_its_file_and_says_the_position_was_lost() {
    let mut rig = Rig::new("restored-read");
    let path = restored(&rig, 3, Some("one\ntwo\nthree\nfour"), lost(), Some(false));

    let new = rig.process(json!({"id": 3})).await;
    assert_eq!(
        new.text,
        format!(
            "Process 3 ended when Wisp restarted.\nLast lines (lines 1–4):\none\ntwo\nthree\nfour\nOutput file: {path}\nThe read position was lost when Wisp restarted."
        )
    );
    let tail = rig
        .process(json!({"id": 3, "read": "tail", "lines": 2}))
        .await;
    assert_eq!(
        tail.text,
        format!(
            "Process 3 ended when Wisp restarted.\nLast lines (lines 3–4):\nthree\nfour\nOutput file: {path}"
        )
    );
    let lines = rig
        .process(json!({"id": 3, "read": "lines", "from": 2, "lines": 2}))
        .await;
    assert!(lines.text.contains("two\nthree"), "{}", lines.text);
    assert!(!lines.text.contains("four"), "{}", lines.text);
    let search = rig
        .process(json!({"id": 3, "read": "search", "pattern": "^t"}))
        .await;
    assert!(
        search
            .text
            .starts_with("Process 3 ended when Wisp restarted. "),
        "{}",
        search.text
    );
    assert!(
        search.text.contains("two") && search.text.contains("three"),
        "{}",
        search.text
    );
    rig.finish().await;
}

#[tokio::test]
async fn changes_to_a_restored_process_are_refused_as_ended() {
    let mut rig = Rig::new("restored-changes");
    restored(&rig, 2, Some("x\n"), lost(), Some(true));
    for input in [
        json!({"id": 2, "input": "y\n"}),
        json!({"id": 2, "keys": ["Up"]}),
        json!({"id": 2, "signal": "kill"}),
        json!({"id": 2, "timeout_ms": 5000}),
    ] {
        let refused = rig.process(input).await;
        assert_eq!(
            refused.invalid(),
            Some("process 2 has ended, so it takes no input, signals, size or time limit")
        );
    }
    let screen = rig.process(json!({"id": 2, "read": "screen"})).await;
    assert_eq!(
        screen.text,
        "Process 2 ended when Wisp restarted. Its screen was lost when Wisp restarted."
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_restored_process_of_unknown_kind_says_its_screen_was_lost() {
    let mut rig = Rig::new("restored-unknown");
    restored(&rig, 5, Some("x\n"), lost(), None);
    let screen = rig.process(json!({"id": 5, "read": "screen"})).await;
    assert_eq!(
        screen.text,
        "Process 5 ended when Wisp restarted. Its screen was lost when Wisp restarted."
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_missing_file_is_named_and_the_end_still_said() {
    let mut rig = Rig::new("restored-missing");
    let exited = Ended::of(ProcessEnd::Exited {
        exit: ExitStatus::Code { code: 1 },
    });
    let path = restored(&rig, 4, None, exited, Some(false));
    let tail = rig.process(json!({"id": 4, "read": "tail"})).await;
    assert_eq!(
        tail.text,
        format!(
            "Process 4 exited with code 1.\nOutput file is missing: {path} no longer exists, so there is no output to read."
        )
    );
    let screen = rig.process(json!({"id": 4, "read": "screen"})).await;
    assert!(screen.invalid().is_some(), "{}", screen.text);
    rig.finish().await;
}

#[tokio::test]
async fn the_list_shows_restored_processes_then_live_ones_in_number_order() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("restored-list");
    restored(&rig, 1, Some(""), lost(), Some(false));
    let stopped = Ended::of(ProcessEnd::Stopped {
        by: StoppedBy::User,
    });
    restored(&rig, 2, Some(""), stopped, Some(false));
    let live = rig.bash(background("exec sleep 30")).await;
    assert_eq!(live.number, Some(3));
    let listed = rig.process(json!({})).await;
    assert_eq!(
        listed.text,
        "Processes:\n1  ended when Wisp restarted  make 1\n2  stopped by the user        make 2\n3  running                    exec sleep 30"
    );
    rig.finish().await;
}

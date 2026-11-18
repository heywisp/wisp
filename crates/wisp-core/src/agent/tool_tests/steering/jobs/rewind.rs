use wisp_protocol::RewindPoint;
use wisp_tool_text::jobs::STARTED_IN_REWOUND_TURNS;

use super::*;
use crate::provider::reply_text;

fn texts_of(message: &Message) -> String {
    reply_text(&message.content).unwrap_or_default()
}

#[tokio::test]
async fn a_process_started_in_the_turns_you_left_keeps_running_and_the_model_is_told() {
    if !have_bash() {
        return;
    }
    let mut h = jobs(
        "rewind-process",
        vec![
            done("one"),
            uses(&[call(
                "c1",
                "bash",
                json!({"command": "exec sleep 30", "run_in_background": true}),
            )]),
            done("started"),
            done("again"),
        ],
        false,
    );
    h.run(vec![steer("first")], 1).await;
    h.run(vec![steer("start")], 1).await;
    let listed = h
        .drive(
            vec![ClientCommand::RewindPoints {
                request: RequestId(1),
            }],
            nothing,
            |seen| {
                seen.iter()
                    .any(|e| matches!(e, AgentEvent::RewindPoints { .. }))
            },
            PATIENT,
        )
        .await;
    let processes: Vec<Vec<Vec<u32>>> = pick!(
        &listed,
        AgentEvent::RewindPoints { points, .. } =>
            points.iter().map(|p| p.processes.clone()).collect()
    );
    assert_eq!(processes, [vec![vec![1], vec![1]]]);
    let back = h
        .drive(
            vec![ClientCommand::Rewind {
                to: RewindPoint::Turn { turn: 2 },
                request: RequestId(2),
            }],
            nothing,
            |seen| seen.iter().any(|e| matches!(e, AgentEvent::Rewound { .. })),
            PATIENT,
        )
        .await;
    assert!(ends(&back).is_empty(), "nothing is stopped");
    h.run(vec![steer("again")], 1).await;
    let requests = h.requests();
    let last = requests.last().unwrap().last().unwrap();
    let line = format!("Process 1 (exec sleep 30) is running; {STARTED_IN_REWOUND_TURNS}.");
    assert!(texts_of(last).contains(&line), "{}", texts_of(last));
    let stopped = h
        .drive(
            vec![ClientCommand::StopProcess { id: 1 }],
            nothing,
            process_ended(1),
            PATIENT,
        )
        .await;
    assert_eq!(ends(&stopped).len(), 1);
}

#[tokio::test]
async fn rewound_news_left_unworded_is_told_once_the_notice_is_back_on() {
    if !have_bash() {
        return;
    }
    let mut h = jobs(
        "rewind-unworded",
        vec![
            done("one"),
            uses(&[call(
                "c1",
                "bash",
                json!({"command": "exec sleep 30", "run_in_background": true}),
            )]),
            done("started"),
            done("off"),
            done("on"),
        ],
        false,
    );
    h.run(vec![steer("first")], 1).await;
    h.run(vec![steer("start")], 1).await;
    h.drive(
        vec![
            ClientCommand::Rewind {
                to: RewindPoint::Turn { turn: 2 },
                request: RequestId(1),
            },
            hook_switch(2, false),
        ],
        nothing,
        |seen| seen.iter().any(|e| matches!(e, AgentEvent::Rewound { .. })),
        PATIENT,
    )
    .await;
    let line = format!("Process 1 (exec sleep 30) is running; {STARTED_IN_REWOUND_TURNS}.");
    h.run(vec![steer("off")], 1).await;
    let requests = h.requests();
    let last = requests.last().unwrap().last().unwrap();
    assert!(!texts_of(last).contains(&line), "{}", texts_of(last));
    h.run(vec![hook_switch(3, true), steer("on")], 1).await;
    let requests = h.requests();
    let last = requests.last().unwrap().last().unwrap();
    assert!(texts_of(last).contains(&line), "{}", texts_of(last));
    h.drive(
        vec![ClientCommand::StopProcess { id: 1 }],
        nothing,
        process_ended(1),
        PATIENT,
    )
    .await;
}

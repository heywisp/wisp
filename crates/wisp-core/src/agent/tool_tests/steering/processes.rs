use wisp_protocol::log::LogEvent;
use wisp_protocol::{AttachInput, AttachItems, AttachKey, AttachKeyCode, ClientCommand};

use super::*;

fn typed() -> AttachItems {
    AttachItems(vec![
        AttachInput::Text {
            text: "hunter2".into(),
        },
        AttachInput::Key {
            key: AttachKey::new(AttachKeyCode::Enter, false, false, false).unwrap(),
        },
        AttachInput::Paste {
            text: "pasted secret".into(),
        },
    ])
}

fn process_commands() -> Vec<ClientCommand> {
    vec![
        ClientCommand::StopProcess { id: 7 },
        ClientCommand::Background {
            call_id: "toolu_01".into(),
        },
        ClientCommand::AttachProcess { id: 5, size: None },
        ClientCommand::ProcessInput {
            id: 5,
            items: typed(),
        },
        ClientCommand::DetachProcess { id: 5 },
        ClientCommand::DeclineAttach {
            call_id: "toolu_02".into(),
        },
        ClientCommand::AnswerSecret {
            call_id: "toolu_03".into(),
            value: wisp_protocol::Secret::new("hunter2-answer"),
        },
    ]
}

fn refused(events: &[AgentEvent]) -> Vec<(Option<RejectKind>, String)> {
    pick!(
        events,
        AgentEvent::Rejected {
            id: None,
            kind,
            reason,
            request: None,
        } => (kind.clone(), reason.clone())
    )
}

fn logged_refusals(records: &[LogRecord]) -> Vec<(Option<u64>, Option<RejectKind>, String)> {
    records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::Rejected {
                turn,
                reason,
                id: None,
                kind,
            } => Some((*turn, kind.clone(), reason.clone())),
            _ => None,
        })
        .collect()
}

fn expected(background: (RejectKind, &str)) -> Vec<(Option<RejectKind>, String)> {
    let no_process = |n: u32| {
        (
            Some(RejectKind::NoProcess),
            wisp_protocol::process_refusal(&RejectKind::NoProcess, n),
        )
    };
    vec![
        no_process(7),
        (Some(background.0), background.1.to_owned()),
        no_process(5),
        no_process(5),
        no_process(5),
        (
            Some(RejectKind::NoAttachRequest),
            "no take-over request toolu_02 is open".to_owned(),
        ),
        (
            Some(RejectKind::UnknownId),
            "no secret request toolu_03 is open".to_owned(),
        ),
    ]
}

fn assert_no_input_logged(records: &[LogRecord]) {
    let text: String = records
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect();
    assert!(!text.contains("hunter2"), "{text}");
    assert!(!text.contains("pasted secret"), "{text}");
}

#[tokio::test]
async fn process_commands_between_turns_are_refused_and_logged() {
    let mut h = harness(vec![]);
    let events = h.idle(process_commands()).await;
    let want = expected((RejectKind::NothingRunning, wisp_protocol::NOTHING_RUNNING));
    assert_eq!(refused(&events), want);
    assert!(started(&events).is_empty());
    let records = records(&h.store);
    let logged: Vec<_> = want.into_iter().map(|(k, r)| (None, k, r)).collect();
    assert_eq!(logged_refusals(&records), logged);
    assert_no_input_logged(&records);
}

#[tokio::test]
async fn process_commands_during_a_turn_are_refused_and_logged() {
    let mut h = harness(vec![hanging(&[B::Text("partial")])]);
    let mut sends = vec![steer("go")];
    sends.extend(process_commands());
    sends.push(ClientCommand::Cancel);
    let events = h.run(sends, 1).await;
    let want = expected((RejectKind::NotMovable, wisp_protocol::NOT_MOVABLE));
    assert_eq!(refused(&events), want);
    assert_eq!(started(&events), [1]);
    let records = records(&h.store);
    let logged: Vec<_> = want.into_iter().map(|(k, r)| (Some(1), k, r)).collect();
    assert_eq!(logged_refusals(&records), logged);
    assert_no_input_logged(&records);
}

#[tokio::test]
async fn an_idle_background_and_an_idle_steer_get_their_own_nothing_running_texts() {
    let mut h = harness(vec![hanging(&[B::Text("partial")])]);
    h.run(vec![steer("go"), queue("q1"), ClientCommand::Cancel], 1)
        .await;
    let idle = h
        .idle(vec![
            edit("q1s", "q1", DeliveryMode::Steer, "q1"),
            ClientCommand::Background {
                call_id: "toolu_01".into(),
            },
        ])
        .await;
    let texts: Vec<(Option<InputId>, Option<RejectKind>, String)> = pick!(
        &idle,
        AgentEvent::Rejected {
            id, kind, reason, ..
        } => (id.clone(), kind.clone(), reason.clone())
    );
    assert_eq!(
        texts,
        [
            (
                Some(id("q1s")),
                Some(RejectKind::NothingRunning),
                wisp_protocol::NOTHING_TO_STEER.to_owned()
            ),
            (
                None,
                Some(RejectKind::NothingRunning),
                wisp_protocol::NOTHING_RUNNING.to_owned()
            ),
        ]
    );
}

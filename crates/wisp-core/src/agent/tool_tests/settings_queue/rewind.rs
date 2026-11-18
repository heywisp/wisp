use wisp_protocol::{REWIND_CANCELLED, REWIND_SESSION_ENDED, RejectKind, SENT_BEFORE_REWIND};

use super::*;

fn refused(events: &[AgentEvent], request: u64) -> Option<(RejectKind, String)> {
    events.iter().find_map(|e| match e {
        AgentEvent::Rejected {
            kind: Some(kind),
            request: Some(r),
            reason,
            ..
        } if *r == RequestId(request) => Some((kind.clone(), reason.clone())),
        _ => None,
    })
}

async fn waiting_rewind(provider: Arc<Fake>) -> (Live, Arc<Host>) {
    let host = Host::new(false);
    let mut live = Live::start(provider, Some(host.clone()));
    live.send(send("tui-1", "first")).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    live.send(saved(1, "effort", "high")).await;
    live.send(rewind(2, 1)).await;
    live.quiet().await;
    (live, host)
}

#[tokio::test]
async fn a_message_sent_while_a_rewind_waits_cancels_it() {
    let (mut live, host) = waiting_rewind(scripted(vec![done("one"), done("two")])).await;
    live.send(send("tui-2", "second")).await;
    let seen = live
        .until(|e| matches!(e, AgentEvent::Rejected { .. }))
        .await;
    assert_eq!(
        refused(&seen, 2),
        Some((RejectKind::NotRewindable, SENT_BEFORE_REWIND.to_owned()))
    );
    host.open();
    let rest = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(!rest.iter().any(|e| matches!(e, AgentEvent::Rewound { .. })));
    live.end().await;
}

#[tokio::test]
async fn esc_while_a_rewind_waits_cancels_it() {
    let (mut live, host) = waiting_rewind(scripted(vec![done("one")])).await;
    live.send(ClientCommand::Cancel).await;
    let seen = live
        .until(|e| matches!(e, AgentEvent::Rejected { .. }))
        .await;
    assert_eq!(
        refused(&seen, 2),
        Some((RejectKind::NotRewindable, REWIND_CANCELLED.to_owned()))
    );
    host.open();
    let rest = live.end().await;
    assert!(!rest.iter().any(|e| matches!(e, AgentEvent::Rewound { .. })));
}

#[tokio::test]
async fn a_rewind_waits_for_a_settings_save_sent_before_it() {
    let (mut live, host) = waiting_rewind(scripted(vec![done("one")])).await;
    host.open();
    let seen = live
        .until(|e| matches!(e, AgentEvent::Rewound { .. }))
        .await;
    let changed = seen
        .iter()
        .position(|e| matches!(e, AgentEvent::SettingsChanged { .. }));
    assert!(changed.is_some_and(|at| at + 1 < seen.len()), "{seen:#?}");
    live.end().await;
}

#[tokio::test]
async fn a_rewind_waiting_when_the_session_ends_is_refused() {
    let (mut live, host) = waiting_rewind(scripted(vec![done("one")])).await;
    live.commands = None;
    host.open();
    let rest = live.end().await;
    assert_eq!(
        refused(&rest, 2),
        Some((RejectKind::NotRewindable, REWIND_SESSION_ENDED.to_owned())),
        "{rest:#?}"
    );
}

#[tokio::test]
async fn the_points_wait_for_a_settings_save_sent_before_them() {
    let host = Host::new(false);
    let mut live = Live::start(scripted(vec![done("one")]), Some(host.clone()));
    live.send(send("tui-1", "first")).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    live.send(saved(1, "effort", "high")).await;
    live.send(ClientCommand::RewindPoints {
        request: RequestId(2),
    })
    .await;
    let early = live.quiet().await;
    assert!(
        !early
            .iter()
            .any(|e| matches!(e, AgentEvent::RewindPoints { .. })),
        "{early:#?}"
    );
    host.open();
    let seen = live
        .until(|e| matches!(e, AgentEvent::RewindPoints { .. }))
        .await;
    assert!(
        seen.iter()
            .any(|e| matches!(e, AgentEvent::SettingsChanged { .. })),
        "{seen:#?}"
    );
    live.end().await;
}

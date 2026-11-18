use super::*;
use crate::resume::transcript;

fn agree(live: &[AgentEvent], h: &Harness) {
    let replayed = transcript(&records(&h.store), &crate::tool::Toolbox::default(), None);
    assert_eq!(reported(&replayed), reported(live), "{replayed:#?}");
}

#[tokio::test]
async fn a_replayed_wake_turn_reports_the_ends_it_reported_live() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "replayed-wake",
        vec![
            uses(&[background("a", &a.command)]),
            done("started"),
            done("woke"),
        ],
        false,
    );
    let mut live = h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    live.extend(woke(&mut h).await);
    assert_eq!(reported(&live), [Vec::new(), vec![exited(1, 2)]]);
    agree(&live, &h);
}

#[tokio::test]
async fn a_replayed_message_reports_the_ends_its_notice_carried_live() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "replayed-message",
        vec![
            uses(&[background("a", &a.command)]),
            done("started"),
            done("mine"),
        ],
        true,
    );
    let mut live = h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    let mut sent_once = false;
    live.extend(
        h.drive(
            Vec::new(),
            |e| match e {
                AgentEvent::ProcessEnded { id: 1, .. } if !sent_once => {
                    sent_once = true;
                    vec![steer("mine")]
                }
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await,
    );
    assert_eq!(wakes(&live), 0, "{live:#?}");
    assert_eq!(reported(&live), [Vec::new(), vec![exited(1, 2)]]);
    agree(&live, &h);
}

#[tokio::test]
async fn a_replayed_retry_reports_the_ends_its_resent_notice_carried_live() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 2);
    let mut h = jobs(
        "replayed-retry",
        vec![
            uses(&[background("a", &a.command)]),
            done("started"),
            server_error_step(),
            done("again"),
        ],
        false,
    );
    let mut live = h.run(vec![steer("go")], 1).await;
    a.open(&h._env.work);
    live.extend(woke(&mut h).await);
    live.extend(
        h.run(
            vec![ClientCommand::Retry {
                turn: 2,
                endpoint: None,
                request: RequestId(1),
            }],
            1,
        )
        .await,
    );
    assert_eq!(
        reported(&live),
        [Vec::new(), vec![exited(1, 2)], vec![exited(1, 2)]]
    );
    agree(&live, &h);
}

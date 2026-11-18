use super::*;
use crate::agent::retry::RetryPolicy;
use crate::test_guard::retries;

fn slow(step_retries: u32) -> RetryPolicy {
    RetryPolicy {
        step_retries: Some(step_retries),
        base_delay: Duration::from_secs(10),
        jitter: || 0.0,
        busy_window: Some(Duration::ZERO),
        ..RetryPolicy::default()
    }
}

fn retrying(h: &mut Harness, policy: RetryPolicy) {
    h.agent.retry = policy;
}

fn on_retry(commands: Vec<ClientCommand>) -> impl FnMut(&AgentEvent) -> Vec<ClientCommand> {
    let mut once = Some(commands);
    move |event| match event {
        AgentEvent::Retrying { .. } => once.take().unwrap_or_default(),
        _ => Vec::new(),
    }
}

#[tokio::test(start_paused = true)]
async fn a_steer_sent_during_the_backoff_waits_for_the_retried_step() {
    let mut h = harness(vec![turned_away(), done("one"), done("two")]);
    retrying(&mut h, slow(5));
    let events = h
        .drive(
            vec![steer("go")],
            on_retry(vec![steer("s")]),
            turns(1),
            PATIENT,
        )
        .await;
    assert!(
        matches!(outcomes(&events)[..], [TurnOutcome::Completed { .. }]),
        "{events:#?}"
    );
    assert_eq!(retries(&events), 1);
    assert_eq!(delivered(&events), [(ids(&["go"]), 1), (ids(&["s"]), 2)]);
    let bodies = h.provider.texts();
    assert_eq!(bodies.len(), 3);
    assert_eq!(
        bodies[0], bodies[1],
        "the retried request is the failed one"
    );
    assert!(
        !bodies[1].contains("\"s\""),
        "no steer slipped into the retry"
    );
    let requests = h.requests();
    assert_eq!(requests[0], requests[1]);
    assert_each_request_extends_the_last(&requests);
    let last = requests[2].last().unwrap();
    assert_eq!(texts(last), ["s", STEER_NOTE]);
    assert!(withdrawn(&events).is_empty());
    h.assert_rebuilds();
}

#[tokio::test(start_paused = true)]
async fn a_cancel_during_the_backoff_returns_steers_holds_the_queue_and_ends_once() {
    let mut h = harness(vec![turned_away(), done("never")]);
    retrying(&mut h, slow(5));
    let events = h
        .drive(
            vec![steer("go")],
            on_retry(vec![steer("s"), queue("q"), ClientCommand::Cancel]),
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(terminal(&events), 1, "{events:#?}");
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Error {
            gave_up: None,
            offer: Some(_),
            ..
        }]
    ));
    assert_eq!(
        withdrawn(&events),
        [(ids(&["go", "s"]), WithdrawnBy::TurnStopped)]
    );
    assert_eq!(held(&events), [(ids(&["q"]), WithdrawnBy::TurnStopped)]);
    before_end(&events, |e| matches!(e, AgentEvent::InputHeld { .. }));
    let later = h.idle(Vec::new()).await;
    assert!(started(&later).is_empty(), "{later:#?}");
    assert_eq!(terminal(&later), 0);
    assert_eq!(h.requests().len(), 1, "nothing is sent after the cancel");
    assert!(h.agent.history().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_turn_that_gives_up_returns_its_steers_and_holds_its_queue() {
    let mut h = harness(vec![turned_away(), turned_away()]);
    retrying(&mut h, slow(1));
    let events = h
        .drive(
            vec![steer("go")],
            on_retry(vec![steer("s"), queue("q")]),
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(terminal(&events), 1, "{events:#?}");
    assert!(
        matches!(
            outcomes(&events)[..],
            [TurnOutcome::Error {
                attempts: 2,
                gave_up: Some(GaveUp::StepLimit),
                ..
            }]
        ),
        "{events:#?}"
    );
    assert_eq!(
        withdrawn(&events),
        [(ids(&["go", "s"]), WithdrawnBy::TurnFailed)]
    );
    assert_eq!(held(&events), [(ids(&["q"]), WithdrawnBy::TurnFailed)]);
    let later = h.idle(Vec::new()).await;
    assert!(started(&later).is_empty());
    assert_eq!(h.requests().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_delivered_steer_whose_step_gives_up_is_returned_uncommitted() {
    let mut h = harness(vec![
        done("one").then(steer("s")),
        turned_away(),
        turned_away(),
    ]);
    retrying(&mut h, slow(1));
    let events = h.run(vec![steer("go")], 1).await;
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Error {
            committed_steps: 1,
            gave_up: Some(GaveUp::StepLimit),
            ..
        }]
    ));
    assert_eq!(delivered(&events), [(ids(&["go"]), 1), (ids(&["s"]), 2)]);
    assert_eq!(withdrawn(&events), [(ids(&["s"]), WithdrawnBy::TurnFailed)]);
    let requests = h.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1], requests[2]);
    assert_eq!(h.agent.history().len(), 2);
    h.assert_rebuilds();
}

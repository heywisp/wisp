use wisp_protocol::Billing;
use wisp_protocol::log::LogEvent;

use crate::agent::Agent;
use crate::agent::keep_warm::tests::{Rig, resumed, shifted, warm};
use crate::test_guard::{log_events, pick};
use crate::voice::testing::booked;

#[tokio::test(start_paused = true)]
async fn voice_calls_booked_while_idle_or_as_the_session_ends_are_kept_on_resume() {
    let mut calls = None;
    let rig = Rig::with(Vec::new(), |agent| {
        calls = Some(agent.voice_sender());
        agent
    });
    let calls = calls.unwrap();
    calls
        .send(booked("api", "gpt-live-1", Billing::Metered, 30))
        .unwrap();
    rig.idle(1).await;
    calls
        .send(booked("talk", "gpt-live-1-codex", Billing::ChatGptPlan, 60))
        .unwrap();
    let closed = rig.close().await;
    let logged = pick!(log_events(&closed.store), LogEvent::VoiceCall { seconds, .. } => seconds);
    assert_eq!(logged, [30, 60]);
    let spend = |a: &Agent| {
        (
            a.session_usd,
            a.session_plan_usd,
            a.accounts.clone(),
            a.session_lower_bound,
            a.session_billing,
        )
    };
    assert_eq!(closed.agent.session_billing, Some(Billing::Mixed));
    let back = resumed(&closed, &warm(Vec::new()), &shifted());
    assert_eq!(spend(&back), spend(&closed.agent));
}

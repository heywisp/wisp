use wisp_protocol::{
    AgentEvent, ErrorDetail, ErrorKind, FailurePhase, FailureSource, GaveUp, RequestId, RetryClass,
    RetryFocus, RetryOffer, StopReason, TurnOutcome, WithdrawnBy,
};
use wisp_render::chrome::Credential;
use wisp_render::theme::glyphs::GlyphSet;
use wisp_tui::term::KeyCode;

use super::{
    Look, MODES, Screen, Stage, WIDTHS, cancelled, completed, cost, described, ended, facts,
    on_accounts, on_manifest, settings_changed, snapshot, staged_with, truecolor, usage,
};
use crate::Drive;
use crate::events::{End, Ending, outcome};
use crate::shared::offer;

fn failure(kind: ErrorKind, committed_steps: u32, offer: RetryOffer) -> TurnOutcome {
    let (message, detail) = match kind {
        ErrorKind::Auth => (
            "invalid x-api-key".to_owned(),
            ErrorDetail {
                limit_reached: None,
                model_limit: None,
                status: Some(401),
                error_type: Some("authentication_error".to_owned()),
                ..ErrorDetail::new(
                    FailurePhase::Response,
                    FailureSource::Endpoint,
                    RetryClass::Never,
                )
            },
        ),
        _ => (
            "Upstream provider stream was interrupted.".to_owned(),
            ErrorDetail {
                limit_reached: None,
                model_limit: None,
                status: Some(200),
                error_type: Some("api_error".to_owned()),
                request_id: Some("req_011CTurn18".to_owned()),
                possibly_billed: true,
                ..ErrorDetail::new(
                    FailurePhase::Stream,
                    FailureSource::Endpoint,
                    RetryClass::Transient,
                )
            },
        ),
    };
    let gave_up = (kind == ErrorKind::Server).then_some(GaveUp::StepLimit);
    outcome(End {
        ending: Ending::Error(kind),
        message,
        committed_steps,
        detail: Some(detail),
        attempts: if gave_up.is_some() { 6 } else { 1 },
        gave_up,
        offer: Some(offer),
        ..End::default()
    })
}

async fn worked(s: &Stage, ask: &str) {
    s.send(ask).await;
    s.wait(100)
        .await
        .answer(0, "Reading the cache code first.\n")
        .await
        .stop(0)
        .await;
    s.event(AgentEvent::StepEnded {
        step: 1,
        usage: Some(usage()),
        cost_usd: Some(0.014),
        lower_bound: false,
    })
    .await;
}

async fn choice(s: Stage) {
    worked(&s, "Make cache misses name their cause").await;
    s.event(ended(
        failure(ErrorKind::Server, 1, offer(true, true, RetryFocus::Retry)),
        2000,
    ))
    .await;
    s.wait(100).await.shot("offered");
    s.event(described(1)).await;
    s.wait(100).await.shot("priced");
    s.key(KeyCode::Down);
    s.wait(100).await.shot("switch focused");
    s.key(KeyCode::Enter);
    s.wait(20).await.event(described(2)).await;
    s.wait(100).await.shot("retry on");
    s.key(KeyCode::Esc);
    s.wait(100).await;
    s.paste("x");
    s.wait(100).await.shot("dimmed");
    s.key(KeyCode::Backspace).key(KeyCode::Up);
    s.wait(100).await.key(KeyCode::Enter);
    s.wait(100).await.shot("retried");
    s.turns.set(s.turns.get() + 1);
    s.event(AgentEvent::retry_started(
        s.turns.get(),
        1,
        Some(RequestId(3)),
    ))
    .await;
    s.wait(100)
        .await
        .answer(0, "Misses now name their cause.\n")
        .await
        .stop(0)
        .await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.004, 0.018, false))),
        1500,
    ))
    .await;
    s.wait(500).await.shot("continued");
}

async fn variants(s: Stage) {
    worked(&s, "Make cache misses name their cause").await;
    s.event(ended(
        failure(ErrorKind::Auth, 0, offer(true, true, RetryFocus::Retry)),
        800,
    ))
    .await;
    s.event(described(1)).await;
    s.wait(100).await.shot("switch only");
    worked(&s, "Try again").await;
    s.event(settings_changed(
        "direct",
        wisp_protocol::RebuildReason::EndpointSwitch,
    ))
    .await;
    s.event(ended(
        failure(ErrorKind::Server, 1, offer(true, true, RetryFocus::Retry)),
        2000,
    ))
    .await;
    s.event(described(2)).await;
    s.wait(100).await.shot("pending endpoint");
    s.key(KeyCode::Esc);
    s.wait(100).await.shot("dismissed");
    worked(&s, "And once more").await;
    s.event(ended(cancelled(), 900)).await;
    s.wait(100).await.shot("stopped");
}

async fn limits(s: Stage) {
    worked(&s, "Read the cache code").await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.014, 0.014, false))),
        1000,
    ))
    .await;
    s.send("Continue the change").await;
    let now = crate::shared::fixed_now().as_second();
    let mut detail = ErrorDetail::new(
        FailurePhase::Response,
        FailureSource::Endpoint,
        RetryClass::After { ms: 7_200_000 },
    );
    detail.status = Some(429);
    detail.limit_reached = Some(Box::new(wisp_protocol::plan_usage::UsageWindow {
        key: wisp_protocol::plan_usage::RETRY_AFTER_KEY.into(),
        label: "Retry after".into(),
        duration_seconds: None,
        scope: wisp_protocol::plan_usage::UsageScope::Model("claude-fable-5-2".into()),
        used_percent: None,
        resets_at: Some(now + 7200),
        observed_at: now,
        source: wisp_protocol::plan_usage::UsageSource::RetryAfter,
    }));
    s.event(AgentEvent::Retrying {
        attempt: 1,
        delay_ms: 7_200_000,
        reason: "HTTP 429: Anthropic subscription request failed (rate limited)".into(),
        step: 1,
        kind: ErrorKind::RateLimit,
        detail: Some(detail.clone()),
        max_attempts: 0,
        discarded_blocks: 0,
        discarded_chars: 0,
        usage: None,
        model: None,
        cost_usd: None,
        lower_bound: false,
        resend: None,
        busy: Some(wisp_protocol::BusyWait {
            waited_ms: 0,
            window_ms: 0,
        }),
    })
    .await;
    s.wait(100).await.shot("limit waiting");
    detail.retry = RetryClass::After { ms: 508_200_000 };
    let at = "2026-10-30T09:00:00Z"
        .parse::<jiff::Timestamp>()
        .unwrap()
        .as_second();
    detail.limit_reached.as_mut().unwrap().resets_at = Some(at);
    detail.model_limit = Some(Box::new(wisp_protocol::plan_usage::UsageWindow {
        key: "fable-weekly".into(),
        label: "Fable weekly".into(),
        duration_seconds: Some(604_800),
        scope: wisp_protocol::plan_usage::UsageScope::ModelFamily("Fable".into()),
        used_percent: Some(100.0.try_into().unwrap()),
        resets_at: Some(now + 508_200),
        observed_at: now,
        source: wisp_protocol::plan_usage::UsageSource::UsageEndpoint,
    }));
    s.event(ended(
        outcome(End {
            ending: Ending::Error(ErrorKind::RateLimit),
            message: "Anthropic subscription request failed (rate limited)".into(),
            detail: Some(detail),
            attempts: 2,
            gave_up: Some(GaveUp::RetryAfterTooLong),
            offer: Some(RetryOffer {
                retries_at: Some(at + 60),
                ..offer(true, true, RetryFocus::Switch)
            }),
            ..End::default()
        }),
        2000,
    ))
    .await;
    s.event(described(1)).await;
    s.wait(100).await.shot("limit offered");
}

async fn retry_on(s: Stage) {
    worked(&s, "Make cache misses name their cause").await;
    s.event(ended(
        failure(ErrorKind::Billing, 1, offer(true, true, RetryFocus::Switch)),
        2000,
    ))
    .await;
    s.event(described(1)).await;
    s.wait(100).await.key(KeyCode::Enter);
    s.wait(20).await.event(described(2)).await;
    s.wait(100).await.shot("opens");
    s.key(KeyCode::Down);
    s.wait(100).await.shot("down");
    s.key(KeyCode::Down);
    s.wait(100).await.shot("down again");
}

#[tokio::test(start_paused = true)]
async fn the_retry_choice_in_unicode_and_ascii_at_40_80_and_120_in_dark_theme() {
    let all = [
        ("claude", Credential::AnthropicPlan),
        ("claude-work", Credential::AnthropicPlan),
        ("chatgpt", Credential::ChatGptPlan),
        ("claude-api", Credential::ApiKey),
    ];
    for (glyphs, width) in [
        (GlyphSet::Unicode, 40),
        (GlyphSet::Unicode, 80),
        (GlyphSet::Unicode, 120),
        (GlyphSet::Ascii, 40),
    ] {
        let look = truecolor(MODES[0], glyphs, width);
        let mut shots = Vec::new();
        for accounts in [&all[..], &[all[0], all[3]]] {
            let screen = Screen::new(look.rows, look.width);
            let facts = on_accounts(facts(), accounts);
            shots.extend(staged_with(screen, look, facts, |s| Box::pin(retry_on(s))).await);
        }
        snapshot("retry-on", look, &shots);
    }
    for mode in MODES {
        for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
            for width in [40, WIDTHS[0], WIDTHS[1]] {
                let look = truecolor(mode, glyphs, width);
                let mut shots = Vec::new();
                if width != 40 {
                    let screen = Screen::new(look.rows, look.width);
                    let retry =
                        staged_with(screen, look, on_manifest(), |s| Box::pin(choice(s))).await;
                    snapshot("retry", look, &retry);
                    let screen = Screen::new(look.rows, look.width);
                    shots =
                        staged_with(screen, look, on_manifest(), |s| Box::pin(variants(s))).await;
                }
                let screen = Screen::new(look.rows, look.width);
                let facts = on_accounts(
                    facts(),
                    &[
                        ("example", Credential::AnthropicPlan),
                        ("example2", Credential::AnthropicPlan),
                    ],
                );
                shots.extend(staged_with(screen, look, facts, |s| Box::pin(limits(s))).await);
                snapshot("retry-variants", look, &shots);
            }
        }
    }
}

async fn held_then_retry_on(s: Stage) {
    worked(&s, "Make cache misses name their cause").await;
    let mut ids = vec![];
    for text in [
        "check the log",
        "then the tests",
        "then the docs",
        "and push",
    ] {
        ids.push(s.queue(text).await);
    }
    s.event(AgentEvent::InputHeld {
        ids,
        by: WithdrawnBy::TurnFailed,
    })
    .await;
    s.event(ended(
        failure(ErrorKind::Server, 1, offer(true, true, RetryFocus::Switch)),
        2000,
    ))
    .await;
    s.event(described(1)).await;
    s.wait(100).await.shot("held");
    s.key(KeyCode::Enter);
    s.wait(20).await.event(described(2)).await;
    s.wait(100).await.shot("retry on");
}

#[tokio::test(start_paused = true)]
async fn retry_on_at_80_by_10_with_held_messages() {
    for mode in MODES {
        let look = Look {
            rows: 10,
            ..truecolor(mode, GlyphSet::Unicode, 80)
        };
        let screen = Screen::new(look.rows, look.width);
        let shots = staged_with(screen, look, on_manifest(), |s| {
            Box::pin(held_then_retry_on(s))
        })
        .await;
        snapshot("retry-held", look, &shots);
    }
}

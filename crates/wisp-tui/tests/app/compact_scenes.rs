use wisp_protocol::{
    AgentEvent, CacheEffect, CompactionEngine, CompactionFailure, CompactionTrigger, ErrorKind,
    RebuildReason, RequestId, StopReason,
};
use wisp_render::theme::glyphs::GlyphSet;
use wisp_tui::term::KeyCode;

use super::{
    MODES, Stage, WIDTHS, completed, cost, ended, finished_turn, snapshot, staged, truecolor, usage,
};
use crate::Drive;
use crate::shared::{changed, started};

const SUMMARY: &str = "Misses name their cause: the fingerprint compares the system prompt, the tools and the messages.\n\nNext: the prefix-stability test.";

fn compacted(request: u64) -> AgentEvent {
    AgentEvent::Compacted {
        trigger: CompactionTrigger::Manual,
        engine: CompactionEngine::Native,
        before: 182_000,
        after: 14_000,
        partial: false,
        converted: false,
        cache_effect: CacheEffect::Rebuilt {
            reason: RebuildReason::Compaction,
        },
        usage: Some(usage()),
        cost: Some(cost(0.28, 0.294, false)),
        answers: Some(RequestId(request)),
        turn: None,
        summary: Some(SUMMARY.to_owned()),
        paused: false,
    }
}

fn failed(request: u64, kind: CompactionFailure, usd: f64, session: f64) -> AgentEvent {
    let message = match kind {
        CompactionFailure::Cancelled => "The compaction was cancelled.",
        _ => "Anthropic returned 529 overloaded_error after 5 tries.",
    };
    AgentEvent::CompactionFailed {
        trigger: CompactionTrigger::Manual,
        engine: Some(CompactionEngine::Native),
        kind,
        message: message.to_owned(),
        usage: None,
        cost: Some(cost(usd, session, false)),
        answers: Some(RequestId(request)),
        turn: None,
        paused: false,
        requested_by: None,
    }
}

async fn compact(s: Stage) {
    finished_turn(&s, "Make cache misses name their cause").await;
    s.wait(4000)
        .await
        .say("/compact keep the fingerprint notes");
    s.wait(20).await.event(started()).await;
    s.wait(2500).await.shot("compacting");
    let id = s.steer("then add the tests").await;
    s.wait(100).await.shot("waiting");
    s.event(compacted(1)).await.event(changed()).await;
    s.wait(20).await;
    s.event(AgentEvent::TurnStarted {
        turn: 2,
        origin: None,
        answers: None,
        retry_of: None,
        ends: Vec::new(),
    })
    .await
    .event(AgentEvent::InputDelivered {
        ids: vec![id],
        step: 1,
    })
    .await;
    s.answer(0, "Added a test for each cause.\n")
        .await
        .stop(0)
        .await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.011, 0.305, false))),
        1500,
    ))
    .await;
    s.wait(4000).await.shot("compacted");
    s.say("/compact");
    s.wait(20).await.event(started()).await;
    s.wait(1500).await.key(KeyCode::Esc);
    s.wait(20)
        .await
        .event(failed(2, CompactionFailure::Cancelled, 0.004, 0.309))
        .await;
    s.wait(100).await.say("/compact");
    s.wait(20).await.event(started()).await;
    s.wait(1500)
        .await
        .event(failed(
            3,
            CompactionFailure::Error(ErrorKind::Overloaded),
            0.012,
            0.321,
        ))
        .await;
    s.wait(4000).await.shot("failed");
    s.open_transcript(100).await.wait(100).await.shot("details");
    s.key(KeyCode::Char('q'));
}

#[tokio::test(start_paused = true)]
async fn compaction_in_unicode_and_ascii_at_80_and_120_in_light_and_dark() {
    for mode in MODES {
        for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
            for width in WIDTHS {
                let look = truecolor(mode, glyphs, width);
                snapshot(
                    "compact",
                    look,
                    &staged(look, |s| Box::pin(compact(s))).await,
                );
            }
        }
    }
}

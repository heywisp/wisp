use std::time::Duration;

use tokio::time::sleep;
use wisp_protocol::{
    ALREADY_COMPACTING, AgentEvent, CacheEffect, ClientCommand, CompactionEngine,
    CompactionFailure, CompactionTrigger, DeliveryMode, NOTHING_TO_COMPACT, RebuildReason,
    RejectKind, RequestId, Usage,
};
use wisp_render::transcript::copy;
use wisp_tui::term::KeyCode;

use super::steering::{Agent, position, row_with, sent_id, steering};
use super::{Screen, options, session_with};
use crate::Drive;
use crate::events::cost;
use crate::shared::started;
use wisp_tui::app::highlight::Start;
use wisp_tui::app::replay;

const SUMMARY: &str = "So far: the parser handles nested lists.";

fn compacted(request: RequestId) -> AgentEvent {
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
        usage: Some(Usage::default()),
        cost: Some(cost(0.28, 1.4, false)),
        answers: Some(request),
        turn: None,
        summary: Some(SUMMARY.to_owned()),
        paused: false,
    }
}

fn stopped(request: RequestId) -> AgentEvent {
    AgentEvent::CompactionFailed {
        trigger: CompactionTrigger::Manual,
        engine: Some(CompactionEngine::Native),
        kind: CompactionFailure::Cancelled,
        message: "The compaction was cancelled.".to_owned(),
        usage: None,
        cost: Some(cost(0.004, 1.12, false)),
        answers: Some(request),
        turn: None,
        paused: false,
        requested_by: None,
    }
}

async fn compact(a: &mut Agent, typed: &str) -> (Option<String>, RequestId) {
    a.paste(typed).key(KeyCode::Enter);
    match a.next().await {
        ClientCommand::Compact { focus, request } => (focus, request),
        other => panic!("expected Compact, got {other:?}"),
    }
}

async fn working(a: &Agent) -> Vec<String> {
    sleep(Duration::from_millis(1_500)).await;
    a.screen.visible()
}

#[tokio::test(start_paused = true)]
async fn compact_sends_its_focus_and_esc_stops_it_once_it_runs() {
    steering(|mut a| async move {
        let (focus, request) = compact(&mut a, "/compact  keep the parser notes ").await;
        assert_eq!(focus.as_deref(), Some("keep the parser notes"));
        let lines = working(&a).await;
        assert!(row_with(&lines, copy::COMPACTING).is_none(), "{lines:#?}");
        a.emit(started()).await;
        let lines = working(&a).await;
        let line = row_with(&lines, "Compacting\u{2026}").unwrap_or_else(|| panic!("{lines:#?}"));
        assert!(line.contains(&copy::esc_to_stop().text()), "{lines:#?}");
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, ClientCommand::Cancel);
        a.emit_spent(stopped(request)).await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, "Compaction stopped. Nothing changed \u{b7} $0.004").is_some(),
            "{lines:#?}"
        );
        assert!(row_with(&lines, "Compacting").is_none(), "{lines:#?}");
        assert!(row_with(&lines, "$1.12").is_some(), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_compaction_inside_a_turn_moves_the_total_at_once() {
    steering(|a| async move {
        let total = |lines: &[String]| {
            let strip = lines.iter().find(|r| r.contains(" total "));
            strip.map(|r| super::unframed(r).trim().to_owned())
        };
        a.emit(AgentEvent::turn_started(1)).await;
        a.emit(super::agents::main_spent((0.42, Some(0.0), false), &[]))
            .await;
        let lines = a.settle().await;
        assert!(
            total(&lines).is_some_and(|t| t.ends_with("total $0.42+")),
            "{lines:#?}"
        );
        let mut inside = compacted(RequestId(1));
        if let AgentEvent::Compacted { turn, cost: c, .. } = &mut inside {
            *turn = Some(1);
            *c = Some(cost(0.28, 0.70, false));
        }
        a.emit_spent(inside).await;
        let lines = a.settle().await;
        assert!(
            total(&lines).is_some_and(|t| t.ends_with("total $0.70+")),
            "{lines:#?}"
        );
        a.emit_spent(crate::events::turn_ended(crate::events::End {
            model: "test-model".to_owned(),
            cost: Some(cost(0.05, 0.75, false)),
            ..crate::events::End::default()
        }))
        .await;
        let lines = a.settle().await;
        assert!(
            total(&lines).is_some_and(|t| t.ends_with("total $0.75")),
            "{lines:#?}"
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_finished_compaction_draws_its_divider_and_ctrl_o_shows_the_summary() {
    steering(|mut a| async move {
        let (focus, request) = compact(&mut a, "/compact").await;
        assert_eq!(focus, None);
        a.emit(started()).await;
        a.emit(compacted(request)).await;
        let lines = a.settle().await;
        let divider = position(
            &lines,
            "context compacted \u{b7} 182k \u{2192} 14k \u{b7} $0.28",
        );
        assert!(
            lines[divider - 1].contains("\u{2500}\u{2500}\u{2500}"),
            "{lines:#?}"
        );
        assert!(row_with(&lines, SUMMARY).is_none(), "{lines:#?}");
        assert!(row_with(&lines, "Compacting").is_none(), "{lines:#?}");
        a.ctrl('o');
        let pager = a.settle().await;
        assert!(row_with(&pager, SUMMARY).is_some(), "{pager:#?}");
        a.key(KeyCode::Char('q'));
        let lines = a.settle().await;
        assert!(row_with(&lines, SUMMARY).is_none(), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_message_sent_while_compacting_waits_for_the_next_turn() {
    steering(|mut a| async move {
        let (_, request) = compact(&mut a, "/compact").await;
        a.emit(started()).await;
        a.paste("then fix the tests").key(KeyCode::Enter);
        let send = a.next().await;
        let ClientCommand::Send { mode, turn, .. } = &send else {
            panic!("expected a send: {send:?}");
        };
        assert_eq!((*mode, *turn), (DeliveryMode::Steer, None));
        let id = a.accept(&send).await;
        let lines = working(&a).await;
        let waiting =
            row_with(&lines, "then fix the tests").unwrap_or_else(|| panic!("{lines:#?}"));
        assert!(waiting.contains(copy::NEXT_TURN), "{lines:#?}");
        assert!(
            row_with(&lines, "Compacting\u{2026}").is_some(),
            "{lines:#?}"
        );
        a.emit(compacted(request)).await;
        let lines = working(&a).await;
        assert!(row_with(&lines, "Compacting").is_none(), "{lines:#?}");
        assert!(
            row_with(&lines, "thinking with low effort").is_some(),
            "the next turn is awaited: {lines:#?}"
        );
        a.emit(AgentEvent::turn_started(1)).await;
        a.emit(AgentEvent::InputDelivered {
            ids: vec![id],
            step: 1,
        })
        .await;
        let lines = a.settle().await;
        let divider = position(&lines, "context compacted");
        assert!(
            position(&lines, "then fix the tests") > divider,
            "{lines:#?}"
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn compact_while_one_runs_or_a_turn_runs_is_refused_with_a_hint() {
    steering(|mut a| async move {
        let (_, request) = compact(&mut a, "/compact").await;
        a.emit(started()).await;
        a.paste("/compact again").key(KeyCode::Enter);
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, ALREADY_COMPACTING).is_some(), "{lines:#?}");
        assert!(a.screen.caret_line().contains("/compact again"));
        a.emit(compacted(request)).await;
        a.key(KeyCode::Enter);
        let (focus, _) = match a.next().await {
            ClientCommand::Compact { focus, request } => (focus, request),
            other => panic!("expected Compact, got {other:?}"),
        };
        assert_eq!(focus.as_deref(), Some("again"));
        a.start("go", 1).await;
        a.paste("/compact now").key(KeyCode::Enter);
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, copy::COMPACT_BETWEEN_TURNS).is_some(),
            "{lines:#?}"
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_refused_compact_is_a_note_and_never_an_unsent_message() {
    steering(|mut a| async move {
        let (_, request) = compact(&mut a, "/compact").await;
        a.emit(AgentEvent::Rejected {
            reason: NOTHING_TO_COMPACT.to_owned(),
            id: None,
            kind: Some(RejectKind::NothingToCompact),
            request: Some(request),
        })
        .await;
        let lines = a.settle().await;
        assert!(row_with(&lines, NOTHING_TO_COMPACT).is_some(), "{lines:#?}");
        assert!(row_with(&lines, "wasn't sent").is_none(), "{lines:#?}");
        a.paste("hello").key(KeyCode::Enter);
        assert!(!sent_id(&a.next().await).0.is_empty());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_compact_replay_fixture_plays_its_compaction_between_turns() {
    let events = replay::parse(include_str!("../fixtures/compact.jsonl")).unwrap();
    let screen = Screen::new(30, 80);
    let exit = session_with(
        screen.clone(),
        vec![events],
        options(Start::Stalled),
        |run| async move {
            run.say("one");
            sleep(Duration::from_secs(2)).await;
            run.say("/compact");
            sleep(Duration::from_millis(1_000)).await;
            let lines = run.screen.visible();
            assert!(
                row_with(&lines, "context compacted").is_none(),
                "{lines:#?}"
            );
            assert!(
                row_with(&lines, "Compacting\u{2026}").is_some(),
                "{lines:#?}"
            );
            sleep(Duration::from_secs(2)).await;
            let lines = run.screen.visible();
            assert!(
                row_with(
                    &lines,
                    "context compacted \u{b7} 182k \u{2192} 14k \u{b7} $0.28"
                )
                .is_some(),
                "{lines:#?}"
            );
            run.say("two");
            sleep(Duration::from_secs(2)).await;
            let lines = run.screen.visible();
            assert!(
                position(&lines, "Picking up from the summary")
                    > position(&lines, "context compacted"),
                "{lines:#?}"
            );
            run.say("/compact");
            sleep(Duration::from_millis(500)).await;
            let lines = run.screen.visible();
            assert!(
                row_with(&lines, replay::NO_MORE_COMPACTIONS).is_some(),
                "{lines:#?}"
            );
            run.quit();
        },
    )
    .await;
    assert!(exit.is_ok());
}

#[tokio::test(start_paused = true)]
async fn rewind_while_a_compaction_runs_says_it_waits_and_works_after() {
    steering(|mut a| async move {
        let (_, request) = compact(&mut a, "/compact").await;
        a.emit(started()).await;
        a.paste("/rewind").key(KeyCode::Enter);
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, copy::REWIND_WHILE_COMPACTING).is_some(),
            "{lines:#?}"
        );
        assert!(a.screen.caret_line().contains("/rewind"));
        a.emit(compacted(request)).await;
        a.key(KeyCode::Enter);
        assert!(matches!(a.next().await, ClientCommand::RewindPoints { .. }));
    })
    .await;
}

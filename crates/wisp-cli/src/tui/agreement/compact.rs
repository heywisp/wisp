use std::time::Duration;

use tokio::sync::mpsc;
use wisp_protocol::context::{ReportDetail, format_tokens};
use wisp_protocol::{
    AgentEvent, CacheTtl, ClientCommand, CompactionTrigger, Effort, NOTHING_TO_COMPACT, RejectKind,
    RequestId,
};
use wisp_render::chrome::Status;
use wisp_render::chrome::context::{CellView, context_cell};
use wisp_render::markdown::{Layout, Renderer};
use wisp_render::motion::{MotionCtx, MotionPolicy};
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::transcript::{Config, Detail, Transcript, copy};
use wisp_tui::app::replay::Script;

use super::{Harness, OPUS, Reply, SUMMARY, WAIT, answer, compaction};
use wisp_render::chrome::Endpoint;

const CX: MotionCtx = MotionCtx {
    policy: MotionPolicy::Full,
    ascii: false,
    blend: true,
};

async fn until(
    events: &mut mpsc::Receiver<AgentEvent>,
    seen: &mut Vec<AgentEvent>,
    done: impl Fn(&AgentEvent) -> bool,
) {
    loop {
        let event = tokio::time::timeout(WAIT, events.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out; seen {seen:#?}"))
            .unwrap();
        let stop = done(&event);
        seen.push(event);
        if stop {
            return;
        }
    }
}

fn compact(request: u64, focus: Option<&str>) -> ClientCommand {
    ClientCommand::Compact {
        focus: focus.map(str::to_owned),
        request: RequestId(request),
    }
}

async fn session(h: &mut Harness) -> Vec<AgentEvent> {
    let (commands, mut inbox) = mpsc::channel(8);
    let (sink, mut events) = mpsc::channel(1024);
    let driver = tokio::spawn(async move {
        let mut seen = vec![];
        commands.send(compact(1, None)).await.unwrap();
        until(&mut events, &mut seen, |e| {
            matches!(e, AgentEvent::Rejected { .. })
        })
        .await;
        commands
            .send(ClientCommand::send("t-1", "read the parser"))
            .await
            .unwrap();
        until(&mut events, &mut seen, |e| {
            matches!(e, AgentEvent::TurnEnded { .. })
        })
        .await;
        commands
            .send(compact(2, Some("keep the parser notes")))
            .await
            .unwrap();
        until(&mut events, &mut seen, |e| {
            matches!(
                e,
                AgentEvent::Compacted { .. } | AgentEvent::CompactionFailed { .. }
            )
        })
        .await;
        until(&mut events, &mut seen, |e| {
            matches!(e, AgentEvent::ContextChanged { .. })
        })
        .await;
        commands
            .send(ClientCommand::ContextReport {
                detail: ReportDetail::Summary,
                request: RequestId(3),
            })
            .await
            .unwrap();
        until(&mut events, &mut seen, |e| {
            matches!(e, AgentEvent::ContextReport(_))
        })
        .await;
        drop(commands);
        while let Some(event) = events.recv().await {
            seen.push(event);
        }
        seen
    });
    h.agent.serve(&mut inbox, &sink).await;
    drop(sink);
    driver.await.unwrap()
}

#[tokio::test]
async fn compact_agrees_between_the_kernel_the_tui_and_the_replay() {
    let mut h = Harness::new(Endpoint::Official, OPUS, Effort::Low, |request, n| {
        if request.contains("\"compaction\":") {
            compaction(OPUS, Reply::Compacts)
        } else {
            answer(OPUS, n)
        }
    });
    let events = session(&mut h).await;
    let refused = events.iter().find_map(|e| match e {
        AgentEvent::Rejected {
            request: Some(request),
            kind,
            reason,
            ..
        } => Some((*request, kind.clone(), reason.clone())),
        _ => None,
    });
    assert_eq!(
        refused,
        Some((
            RequestId(1),
            Some(RejectKind::NothingToCompact),
            NOTHING_TO_COMPACT.to_owned()
        ))
    );
    let Some((before, after, turn_usd, answers, summary)) = events.iter().find_map(|e| match e {
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Manual,
            before,
            after,
            cost,
            answers,
            summary,
            converted: false,
            ..
        } => Some((
            *before,
            *after,
            cost.map(|c| c.turn_usd),
            *answers,
            summary.clone(),
        )),
        _ => None,
    }) else {
        panic!("no compaction: {events:#?}");
    };
    assert_eq!(answers, Some(RequestId(2)));
    assert_eq!(summary.as_deref(), Some(SUMMARY));
    let report = events.iter().find_map(|e| match e {
        AgentEvent::ContextReport(report) => Some(report.clone()),
        _ => None,
    });
    let Some(report) = report else {
        panic!("no report: {events:#?}");
    };
    let spent = report.compaction_cost_usd.unwrap_or_default();
    assert_eq!(report.compactions, 1);
    assert!(
        (spent - turn_usd.unwrap_or_default()).abs() < 1e-9,
        "{report:#?}"
    );

    let mut status = Status::new();
    let mut transcript = Transcript::new(Config {
        provider: "Anthropic".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        logging: false,
    });
    transcript.compact_asked(RequestId(1), false);
    transcript.compact_asked(RequestId(2), true);
    let mut busy_while_compacting = vec![];
    for (i, e) in events.iter().enumerate() {
        let at = Duration::from_millis(u64::try_from(i).unwrap() * 10);
        status.apply(e, at, CX);
        transcript.apply(e, at);
        if matches!(e, AgentEvent::CompactionStarted { .. }) {
            busy_while_compacting.push(status.is_running() && status.is_compacting());
        }
        if matches!(e, AgentEvent::Compacted { .. }) {
            busy_while_compacting.push(status.is_running());
        }
    }
    assert_eq!(busy_while_compacting, [true, false]);
    let cell = crate::testing::text(&context_cell(
        &report,
        &CellView {
            model: OPUS,
            all: false,
            layout: Layout::new(200, GlyphSet::Unicode),
        },
    ));
    let footer_usd = wisp_protocol::format_usd(spent);
    let counted = copy::compactions_so_far(1, Some(&footer_usd));
    assert!(cell.contains(&counted), "{cell}");
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let mut text = |detail| crate::testing::text(&transcript.rows(&r, detail));
    let shown = text(Detail::Scrollback);
    let divider = format!(
        "context compacted \u{b7} {} \u{2192} {} \u{b7} {}",
        format_tokens(before),
        format_tokens(after),
        copy::usd(turn_usd.unwrap_or_default())
    );
    assert!(shown.contains(&divider), "{shown}");
    assert!(shown.contains(NOTHING_TO_COMPACT), "{shown}");
    assert!(!shown.contains("wasn't sent"), "{shown}");
    assert!(!shown.contains("focus not applied"), "{shown}");
    assert!(!shown.contains(SUMMARY), "{shown}");
    assert!(text(Detail::Full).contains(SUMMARY));

    let script = Script::new(events.clone());
    assert_eq!(script.turns.len(), 1);
    assert_eq!(script.compactions.len(), 2, "{:#?}", script.compactions);
    let run = &script.compactions[1];
    assert!(matches!(
        run.first(),
        Some(AgentEvent::CompactionStarted { .. })
    ));
    assert!(matches!(
        run.last(),
        Some(AgentEvent::ContextChanged { .. })
    ));
    assert!(
        script.preamble.iter().all(|e| !matches!(
            e,
            AgentEvent::Compacted { .. } | AgentEvent::Rejected { .. }
        )),
        "{:#?}",
        script.preamble
    );
}

#[test]
fn the_renderer_draws_the_refusal_line_at_the_cores_context_threshold_point() {
    let mut transcript = Transcript::new(Config {
        provider: "Anthropic".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        logging: false,
    });
    transcript.apply(
        &AgentEvent::HookRan {
            hook: "aa/ask".to_owned(),
            point: wisp_core::plugin::hooks::points::CONTEXT_THRESHOLD.to_owned(),
            turn: Some(1),
            step: Some(1),
            call_id: None,
            outcome: wisp_protocol::HookOutcome::Rejected {
                rule: "request_failed".to_owned(),
            },
            ms: 5,
        },
        Duration::ZERO,
    );
    let r = Renderer::new(Layout::new(200, GlyphSet::Unicode));
    let shown = crate::testing::text(&transcript.rows(&r, Detail::Scrollback));
    assert!(
        shown.contains("Wisp refused the compaction aa/ask asked for"),
        "{shown}"
    );
}

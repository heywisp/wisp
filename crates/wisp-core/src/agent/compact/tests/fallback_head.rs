use wisp_protocol::log::{DamageKind, LogRecord};

use super::background::background;
use super::*;
use crate::clock::StillClock;
use crate::resume::turn_of;
use crate::session::Line;
use crate::test_guard::{self, records};

fn garbled(records: &[LogRecord], at: &[usize]) -> Vec<Line> {
    records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            if at.contains(&index) {
                Line::Damaged(DamageKind::Unreadable)
            } else {
                Line::Record(Box::new(record.clone()))
            }
        })
        .collect()
}

struct Resumed {
    body: Value,
    history: Vec<Message>,
    written: Vec<LogRecord>,
}

async fn resume_and_send(records: &[LogRecord], lines: Vec<Line>) -> Resumed {
    let (store, log) = continued(records);
    let provider = scripted(vec![answer("Next.", 300)]);
    let agent = Agent::new(provider.clone(), default_session())
        .with_clock(Arc::new(StillClock))
        .with_log(log, log_context())
        .resume_lines(lines, 0)
        .unwrap();
    let history = agent.history().to_vec();
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("next", "next").await));
    live.close().await;
    Resumed {
        body: provider.body(0),
        history,
        written: test_guard::records(&store)[records.len()..].to_vec(),
    }
}

async fn clean(records: &[LogRecord]) -> Resumed {
    let lines = garbled(records, &[]);
    resume_and_send(records, lines).await
}

fn fell_back(written: &[LogRecord]) -> (Option<u64>, Option<u64>, Option<u64>) {
    written
        .iter()
        .find_map(|r| match &r.event {
            LogEvent::ResumeFellBack {
                dropped_through,
                covered_through,
                last_turn,
                ..
            } => Some((*dropped_through, *covered_through, *last_turn)),
            _ => None,
        })
        .unwrap()
}

fn line_of(records: &[LogRecord], turn: u64, pick: impl Fn(&LogEvent) -> bool) -> usize {
    records
        .iter()
        .position(|r| turn_of(&r.event) == Some(turn) && pick(&r.event))
        .unwrap()
}

fn committed(event: &LogEvent) -> bool {
    matches!(event, LogEvent::MessageCommitted { .. })
}

fn before_turn(records: &[LogRecord], turn: u64) -> Vec<LogRecord> {
    let end = records
        .iter()
        .position(|r| turn_of(&r.event).is_some_and(|t| t >= turn))
        .unwrap();
    records[..end].to_vec()
}

fn head_at(records: &[LogRecord]) -> usize {
    records
        .iter()
        .position(|r| matches!(r.event, LogEvent::ContextCompacted { .. }))
        .unwrap()
}

async fn manual_log() -> Vec<LogRecord> {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        answer("Again.", 1_100),
        compaction(SUMMARY),
        answer("Third.", 300),
        answer("Fourth.", 400),
    ]);
    let mut live = Live::start(logged(provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.compact(3, None).await;
    assert!(completed(&live.turn("u3", "third").await));
    assert!(completed(&live.turn("u4", "fourth").await));
    live.close().await;
    records(&store)
}

async fn kept_turns_log() -> Vec<LogRecord> {
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", 1_000),
        answer("Near.", background::NEAR),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.until(prepared).await;
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    records(&store)
}

#[tokio::test]
async fn damage_below_a_head_without_kept_turns_resumes_with_the_same_bytes() {
    let records = manual_log().await;
    let head = head_at(&records);
    let below = line_of(&records, 1, committed);
    assert!(below < head);
    let expected = clean(&records).await;
    let resumed = resume_and_send(&records, garbled(&records, &[below])).await;
    let (dropped, covered, last_turn) = fell_back(&resumed.written);
    assert_eq!(covered, Some(u64::try_from(below).unwrap()));
    assert_eq!(dropped, None);
    assert_eq!(last_turn, Some(4));
    assert_eq!(resumed.history.len(), expected.history.len());
    assert_eq!(resumed.body, expected.body);
}

#[tokio::test]
async fn damage_on_both_sides_of_a_head_keeps_the_head_and_the_turns_before_the_later_damage() {
    let records = manual_log().await;
    let head = head_at(&records);
    let below = line_of(&records, 1, committed);
    let after = line_of(&records, 4, committed);
    assert!(below < head && head < after);
    let expected = clean(&before_turn(&records, 4)).await;
    let resumed = resume_and_send(&records, garbled(&records, &[below, after])).await;
    let (dropped, covered, last_turn) = fell_back(&resumed.written);
    assert_eq!(covered, Some(u64::try_from(below).unwrap()));
    assert_eq!(last_turn, Some(3));
    assert_eq!(dropped, Some(u64::try_from(records.len() - 1).unwrap()));
    assert_eq!(resumed.history.len(), expected.history.len());
    assert_eq!(resumed.body, expected.body);
}

#[tokio::test]
async fn damage_below_a_head_with_kept_turns_falls_back_to_the_last_clean_turn() {
    let records = kept_turns_log().await;
    let head = head_at(&records);
    assert!(
        matches!(&records[head].event, LogEvent::ContextCompacted { cut: Some(cut), from, .. } if cut < from),
        "{:#?}",
        records[head]
    );
    let kept_reply = line_of(&records, 2, committed);
    assert!(kept_reply < head);
    let expected = clean(&before_turn(&records, 2)).await;
    let resumed = resume_and_send(&records, garbled(&records, &[kept_reply])).await;
    let (_, covered, last_turn) = fell_back(&resumed.written);
    assert_eq!(covered, None);
    assert_eq!(last_turn, Some(1));
    assert!(resumed.history.iter().all(|m| !m.content.is_empty()));
    assert_eq!(resumed.history, expected.history);
    assert_eq!(resumed.body, expected.body);
}

#[tokio::test]
async fn a_resume_through_the_fallback_keeps_the_plan_split_known() {
    use super::usage::billed_step;
    use wisp_protocol::Billing::{AnthropicPlan, Metered};
    for (first, damaged, last_turn) in [(Metered, Some(2), Some(1)), (AnthropicPlan, None, None)] {
        let store = MemoryStore::default();
        let provider = scripted(vec![
            billed_step(answer("Hello.", 1_000), first),
            billed_step(answer("Again.", 1_100), AnthropicPlan),
        ]);
        let mut live = Live::start(logged(provider, &store));
        assert!(completed(&live.turn("u1", "first").await));
        assert!(completed(&live.turn("u2", "second").await));
        live.close().await;
        let records = records(&store);
        let plan: f64 = records
            .iter()
            .filter_map(|r| match &r.event {
                LogEvent::StepEnded {
                    usage: Some(usage),
                    cost_usd,
                    ..
                } if !usage.billing.is_metered() => *cost_usd,
                _ => None,
            })
            .sum();
        let lost = damaged.map_or(0, |turn| line_of(&records, turn, committed));
        let resumed = resume_and_send(&records, garbled(&records, &[lost])).await;
        assert_eq!(fell_back(&resumed.written).2, last_turn);
        let cost = resumed.written.iter().find_map(|r| match &r.event {
            LogEvent::TurnEnded {
                outcome: TurnOutcome::Completed { cost, .. },
                ..
            } => *cost,
            _ => None,
        });
        let split = cost.and_then(|c| c.session_plan_usd);
        assert!(
            plan > 0.0 && split.is_some_and(|s| (s - plan).abs() < 1e-9),
            "damaged turn {damaged:?}: {split:?}, want {plan}"
        );
    }
}

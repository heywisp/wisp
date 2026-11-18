use crate::provider::Iteration;

use super::*;

pub(super) fn iteration(input: u64, read: u64, output: u64) -> Iteration {
    Iteration {
        kind: "compaction".into(),
        model: DEFAULT_MODEL.into(),
        usage: usage(input, read, output),
    }
}

fn only_iterations(iterations: Vec<Iteration>) -> Step {
    let mut script = vec![Ok(started(Some("msg_c"), DEFAULT_MODEL, Usage::default()))];
    script.extend(opaque(0, raw_block(SUMMARY)));
    script.extend(ended(StopReason::Compaction, Usage::default(), iterations));
    Step::from(script)
}

pub(super) fn cut_after_start(iterations: Vec<Iteration>, error: ProviderError) -> Step {
    Step::from(vec![
        Ok(ProviderEvent::Sending),
        Ok(ProviderEvent::MessageStart {
            id: Some("msg_cut".into()),
            model: DEFAULT_MODEL.into(),
            usage: Usage::default(),
            iterations,
            diagnostics: None,
        }),
        Err(error),
    ])
}

#[tokio::test]
async fn the_compaction_request_reads_the_cache_through_the_anchors_marker() {
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        answer("Fine.", 5_100),
        compaction(SUMMARY),
    ]);
    let mut live = Live::start(Agent::new(provider.clone(), default_session()));
    live.turn("u1", "first").await;
    live.turn("u2", "second").await;
    live.compact(3, None).await;
    live.close().await;
    let turn = provider.body(1);
    let asked = provider.body(2);
    assert!(turn.get("cache_control").is_some(), "{turn}");
    assert!(asked.get("cache_control").is_none(), "{asked}");
    let marks = asked["messages"]
        .to_string()
        .matches("cache_control")
        .count();
    assert_eq!(marks, 1, "{}", asked["messages"]);
    let anchor = turn["messages"].as_array().unwrap().len() - 1;
    let block = asked["messages"][anchor]["content"]
        .as_array()
        .unwrap()
        .last()
        .unwrap();
    assert_eq!(
        block["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
}

#[tokio::test]
async fn a_compaction_clears_the_request_list_so_nothing_anchors_on_the_old_view() {
    let provider = scripted(vec![answer("Hello.", 5_000), compaction(SUMMARY)]);
    let mut live = Live::start(Agent::new(provider, default_session()));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    let (agent, _) = live.close().await;
    assert!(agent.compaction_anchor().0.is_none());
    assert!(agent.tail_marker().is_none());
}

#[tokio::test]
async fn zero_top_level_usage_is_priced_from_iterations() {
    let provider = scripted(vec![
        answer("Hello.", 10_000),
        only_iterations(vec![iteration(200, 9_800, 700)]),
    ]);
    let mut live = Live::start(Agent::new(provider, default_session()));
    live.turn("u1", "first").await;
    let done = live.compact(2, None).await;
    let (_, seen) = live.close().await;
    let AgentEvent::Compacted {
        usage: Some(spent),
        cost: Some(cost),
        before,
        ..
    } = &done
    else {
        panic!("{done:?}");
    };
    assert_eq!(
        (
            spent.input_tokens,
            spent.cache_read_input_tokens,
            spent.output_tokens
        ),
        (200, 9_800, 700)
    );
    assert_eq!(*before, 10_000);
    assert!(cost.turn_usd > 0.0);
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::CacheMiss { .. })),
        "{seen:#?}"
    );
}

#[tokio::test]
async fn a_retried_attempt_counts_its_iterations_in_the_compactions_usage() {
    let provider = scripted(vec![
        answer("Hello.", 10_000),
        cut_after_start(
            vec![iteration(100, 9_000, 50)],
            mid_stream(wisp_protocol::RetryClass::Transient),
        ),
        only_iterations(vec![iteration(200, 9_800, 700)]),
    ]);
    let store = MemoryStore::default();
    let agent = logged(provider, &store).with_retry_policy(crate::agent::retry::quick());
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    let done = live.compact(2, None).await;
    let (_, seen) = live.close().await;
    let AgentEvent::Compacted {
        usage: Some(spent), ..
    } = &done
    else {
        panic!("{done:?}");
    };
    assert_eq!(spent.cache_read_input_tokens, 18_800);
    assert_eq!(spent.input_tokens, 300);
    let first = Some(usage(100, 9_000, 50));
    let retried: Vec<Option<Usage>> = pick!(&seen, AgentEvent::Retrying { usage, .. } => *usage);
    assert_eq!(retried, [first]);
    let logged: Vec<Option<Usage>> = pick!(
        &log_events(&store),
        LogEvent::Retrying { usage, .. } => *usage
    );
    assert_eq!(logged, [first]);
    assert!(log_events(&store).iter().any(|e| matches!(
        e,
        LogEvent::ContextCompacted {
            lower_bound: true,
            ..
        }
    )));
}

#[tokio::test]
async fn a_failed_compaction_counts_in_the_compaction_cost() {
    let provider = scripted(vec![
        answer("Hello.", 10_000),
        cut_after_start(
            vec![iteration(100, 9_000, 50)],
            mid_stream(wisp_protocol::RetryClass::Never),
        ),
    ]);
    let mut live = Live::start(Agent::new(provider, default_session()));
    live.turn("u1", "first").await;
    let failed = live.compact(2, None).await;
    let (agent, _) = live.close().await;
    let AgentEvent::CompactionFailed {
        cost: Some(cost), ..
    } = &failed
    else {
        panic!("{failed:?}");
    };
    assert!(cost.turn_usd > 0.0);
    let report = agent.report(ReportDetail::Summary, RequestId(3));
    assert_eq!(report.compactions, 0);
    assert_eq!(report.compaction_cost_usd, Some(cost.turn_usd));
}

#[tokio::test]
async fn a_failed_compaction_that_may_have_been_billed_logs_a_lower_bound() {
    let provider = scripted(vec![
        answer("Hello.", 10_000),
        cut_after_start(
            vec![iteration(100, 9_000, 50)],
            mid_stream(wisp_protocol::RetryClass::Never),
        ),
    ]);
    let store = MemoryStore::default();
    let mut live = Live::start(logged(provider, &store));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    live.close().await;
    let failed: Vec<bool> = pick!(
        &log_events(&store),
        LogEvent::CompactionFailed { lower_bound, .. } => *lower_bound
    );
    assert_eq!(failed, [true]);
}

pub(super) fn billed_step(mut step: Step, billing: wisp_protocol::Billing) -> Step {
    step.script = crate::agent::tests::billed(std::mem::take(&mut step.script), billing);
    step
}

fn cut_with_usage() -> Step {
    Step::from(vec![
        Ok(ProviderEvent::Sending),
        start(1_200, 0),
        Err(mid_stream(wisp_protocol::RetryClass::Never)),
    ])
}

#[test]
fn every_compaction_record_logs_its_estimate_and_model() {
    crate::stack_alarm::on_alarm_stack(crate::stack_alarm::Timer::Real, || async {
        for case in crate::agent::tests::COST_CASES {
            for (summary, kind) in [
                (compaction(SUMMARY), "context_compacted"),
                (cut_with_usage(), "compaction_failed"),
            ] {
                let store = MemoryStore::default();
                let provider =
                    scripted(vec![answer("Hello.", 10_000), billed_step(summary, case.0)]);
                let agent = crate::agent::tests::priced_as(logged(provider, &store), case.1);
                let mut live = Live::start(agent);
                live.turn("u1", "first").await;
                live.compact(2, None).await;
                live.close().await;
                let summaries: Vec<LogEvent> = log_events(&store)
                    .into_iter()
                    .filter(|e| !matches!(e, LogEvent::StepEnded { .. }))
                    .collect();
                crate::agent::tests::assert_logged_costs(&summaries, case, &[kind]);
            }
        }
    });
}

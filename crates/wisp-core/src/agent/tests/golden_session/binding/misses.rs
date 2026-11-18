use wisp_protocol::MissCause;

use super::*;

const STAGED_KEY: &str = "sk-golden";

fn causes(bench: &Bench) -> Vec<MissCause> {
    pick!(log_events(&bench.store), LogEvent::CacheMiss { cause, .. } => cause.unwrap())
}

fn repeats(bench: &Bench) -> Vec<(MissCause, u32, Option<f64>)> {
    pick!(
        log_events(&bench.store),
        LogEvent::CacheMissRepeated {
            cause, count, usd, ..
        } => (cause, count, usd)
    )
}

fn first_passes(bench: &Bench) -> Vec<Option<u64>> {
    pick!(log_events(&bench.store), LogEvent::StepEnded { first_pass, .. } => first_pass)
}

async fn searched(case: &str) -> (Bench, Live) {
    let bench = Bench::new(case, false).await;
    {
        let mut api = bench.api.lock().unwrap();
        api.iterations = true;
        api.search_once = true;
    }
    let mut live = bench.start(Build {
        search: true,
        ..Build::default()
    });
    live.completed("u1", "Look it up.").await;
    (bench, live)
}

fn summed_prompts(bench: &Bench) -> Vec<Option<u64>> {
    pick!(log_events(&bench.store), LogEvent::StepEnded { prompt_tokens, .. } => prompt_tokens)
}

fn searched_step(bench: &Bench) {
    let ok = settled(&bench.served());
    let passes = first_passes(bench);
    assert_eq!(passes[0], Some(ok[0].total), "{passes:?}");
    assert!(passes[1..].iter().all(Option::is_none), "{passes:?}");
    let summed = summed_prompts(bench)[0].unwrap();
    assert!(summed > ok[0].total * 2, "{summed}");
}

fn read_past_the_first_pass(bench: &Bench) {
    let ok = settled(&bench.served());
    let summed = summed_prompts(bench)[0].unwrap();
    assert!(ok[1].read >= ok[0].total, "{}", ok[1].read);
    assert!(ok[1].read < summed / 2, "{} {summed}", ok[1].read);
}

#[tokio::test]
async fn a_full_hit_after_a_search_step_is_no_miss() {
    let (bench, mut live) = searched("misses-search-hit").await;
    live.completed("u2", "And again.").await;
    finish(&bench, live).await;
    searched_step(&bench);
    read_past_the_first_pass(&bench);
    assert_eq!(causes(&bench), Vec::new());
}

#[tokio::test]
async fn a_full_hit_after_a_logged_search_step_is_no_miss_on_resume() {
    let (bench, live) = searched("misses-search-resume").await;
    finish(&bench, live).await;
    searched_step(&bench);
    let mut live = bench.resume(Build {
        search: true,
        ..Build::default()
    });
    live.completed("u2", "And again.").await;
    finish(&bench, live).await;
    read_past_the_first_pass(&bench);
    assert_eq!(causes(&bench), Vec::new());
}

#[tokio::test]
async fn a_real_miss_after_a_search_step_still_fires_with_its_cause() {
    let (bench, mut live) = searched("misses-search-miss").await;
    bench.api.lock().unwrap().cold = 1;
    live.completed("u2", "And again.").await;
    finish(&bench, live).await;
    searched_step(&bench);
    assert_eq!(causes(&bench), vec![MissCause::RemoteLoss]);
    assert_eq!(repeats(&bench), Vec::new());
}

#[tokio::test]
async fn two_misses_in_a_row_with_one_cause_are_flagged() {
    let bench = Bench::new("misses-repeat", false).await;
    let mut live = bench.start(Build::default());
    live.completed("u1", "Look around.").await;
    bench.api.lock().unwrap().cold = 2;
    live.completed("u2", "And again.").await;
    finish(&bench, live).await;
    assert_eq!(
        causes(&bench),
        vec![MissCause::RemoteLoss, MissCause::RemoteLoss]
    );
    let flagged = repeats(&bench);
    assert_eq!(flagged.len(), 1, "{flagged:?}");
    let (cause, count, usd) = &flagged[0];
    assert_eq!((cause, *count), (&MissCause::RemoteLoss, 2));
    assert!(usd.is_some_and(|usd| usd > 0.0), "{usd:?}");
}

#[tokio::test]
async fn one_miss_is_not_flagged() {
    let bench = Bench::new("misses-once", false).await;
    let mut live = bench.start(Build::default());
    live.completed("u1", "Look around.").await;
    bench.api.lock().unwrap().cold = 1;
    live.completed("u2", "And again.").await;
    finish(&bench, live).await;
    assert_eq!(causes(&bench), vec![MissCause::RemoteLoss]);
    assert_eq!(repeats(&bench), Vec::new());
}

#[tokio::test]
async fn a_resume_that_changes_the_tools_misses_as_expected() {
    let bench = Bench::new("misses-resume-tools", false).await;
    let mut live = bench.start(Build::default());
    two_turns(&mut live, 1).await;
    finish(&bench, live).await;
    let mut live = bench.resume(Build {
        tools: MORE_TOOLS,
        ..Build::default()
    });
    two_turns(&mut live, 3).await;
    finish(&bench, live).await;
    assert_eq!(
        causes(&bench),
        vec![MissCause::Expected {
            reason: "resume".into()
        }]
    );
    assert_eq!(repeats(&bench), Vec::new());
}

#[tokio::test]
async fn a_gateway_filling_thinking_into_a_cut_is_injected_thinking() {
    let bench = Bench::new("misses-injected", true).await;
    {
        let mut api = bench.api.lock().unwrap();
        api.refuse_binding_for = Some(GATEWAY_KEY);
        api.replay_for = Some(STAGED_KEY);
    }
    let mut live = bench.start_with(Build::default(), bench.both());
    let long = "Read all of this first. ".repeat(2_000);
    live.completed("u1", &long).await;
    live.completed("u2", &long).await;
    finish(&bench, live).await;
    let later = Build {
        tools: MORE_TOOLS,
        ..Build::default()
    };
    let mut live = bench.resume_with(later, bench.both());
    live.completed("u3", "Keep going.").await;
    {
        let mut api = bench.api.lock().unwrap();
        api.tools_per_turn = 0;
        api.still = true;
    }
    switch_endpoint(&mut live, 1, "gw").await;
    live.completed("u4", "Just answer.").await;
    assert_eq!(bench.cuts().len(), 1, "{}", bench.case);
    {
        let mut api = bench.api.lock().unwrap();
        api.tools_per_turn = 2;
        api.still = false;
    }
    switch_endpoint(&mut live, 2, "env").await;
    two_turns(&mut live, 5).await;
    let (_, seen) = finish_seen(&bench, live).await;
    let own = |count| wisp_protocol::HistoryChange::ThinkingDropped {
        count,
        reason: Some("prefix_binding_mismatch".into()),
    };
    assert_eq!(adaptations(&seen), vec![own(6), own(1), own(1)]);
    assert!(
        bench.served().iter().any(|s| s.inserted > 0),
        "{}",
        bench.case
    );
    let injected = MissCause::InjectedThinking {
        first_path: "messages.15.content.0".into(),
        count: 1,
    };
    let resumed = MissCause::Expected {
        reason: "resume".into(),
    };
    let switched = MissCause::Expected {
        reason: "endpoint_switch".into(),
    };
    assert_eq!(
        causes(&bench),
        vec![
            resumed,
            switched.clone(),
            switched,
            injected.clone(),
            injected.clone()
        ]
    );
    let flagged = repeats(&bench);
    assert_eq!(flagged.len(), 1, "{flagged:?}");
    assert_eq!((&flagged[0].0, flagged[0].1), (&injected, 2));
}

#[tokio::test]
async fn a_rebuild_marked_mid_session_makes_its_miss_expected() {
    let bench = Bench::new("misses-marked", false).await;
    let build = Build {
        search: true,
        ..Build::default()
    };
    let (mut live, _) = refused_mid_session(&bench, build).await;
    live.completed("u3", "One more.").await;
    finish(&bench, live).await;
    assert_eq!(
        causes(&bench),
        vec![MissCause::Expected {
            reason: "native_search_refused".into()
        }]
    );
}

#[tokio::test]
async fn a_hit_between_two_misses_ends_the_run() {
    let bench = Bench::new("misses-hit-between", false).await;
    let mut live = bench.start(Build::default());
    live.completed("u1", "Look around.").await;
    bench.api.lock().unwrap().cold = 1;
    live.completed("u2", "And again.").await;
    bench.api.lock().unwrap().cold = 1;
    live.completed("u3", "Once more.").await;
    finish(&bench, live).await;
    assert_eq!(
        causes(&bench),
        vec![MissCause::RemoteLoss, MissCause::RemoteLoss]
    );
    assert_eq!(repeats(&bench), Vec::new());
}

fn expected(reason: &str) -> MissCause {
    MissCause::Expected {
        reason: reason.into(),
    }
}

#[tokio::test]
async fn a_model_switch_misses_once_as_expected() {
    let bench = Bench::new("misses-model-switch", false).await;
    let mut live = bench.start(Build::default());
    two_turns(&mut live, 1).await;
    switch_model(&mut live, 1, SONNET).await;
    two_turns(&mut live, 3).await;
    finish(&bench, live).await;
    assert_eq!(causes(&bench), vec![expected("model_switch")]);
    assert_eq!(repeats(&bench), Vec::new());
}

#[tokio::test]
async fn an_endpoint_switch_that_loses_the_cache_misses_as_expected() {
    let bench = Bench::new("misses-endpoint-switch", false).await;
    let mut live = bench.start_with(Build::default(), bench.both());
    two_turns(&mut live, 1).await;
    switch_endpoint(&mut live, 1, "gw").await;
    bench.api.lock().unwrap().cold = 1;
    two_turns(&mut live, 3).await;
    finish(&bench, live).await;
    assert_eq!(causes(&bench), vec![expected("endpoint_switch")]);
    assert_eq!(repeats(&bench), Vec::new());
}

#[tokio::test]
async fn an_endpoint_switch_that_keeps_the_cache_is_no_miss() {
    let bench = Bench::new("misses-endpoint-kept", false).await;
    let mut live = bench.start_with(Build::default(), bench.both());
    two_turns(&mut live, 1).await;
    switch_endpoint(&mut live, 1, "gw").await;
    two_turns(&mut live, 3).await;
    finish(&bench, live).await;
    assert_eq!(causes(&bench), Vec::new());
}

#[tokio::test]
async fn a_refusal_fallback_and_the_request_after_it_are_no_remote_loss() {
    let bench = Bench::new("misses-fallback", false).await;
    let mut live = bench.start(Build::default());
    live.completed("u1", "Look around.").await;
    {
        let mut api = bench.api.lock().unwrap();
        api.fallback_to = Some(SONNET);
        api.cold = 2;
    }
    live.completed("u2", "And again.").await;
    finish(&bench, live).await;
    let served: Vec<u64> = settled(&bench.served()).iter().map(|s| s.read).collect();
    assert!(
        served.iter().filter(|read| **read == 0).count() >= 3,
        "{served:?}"
    );
    assert_eq!(causes(&bench), Vec::new());
    assert_eq!(repeats(&bench), Vec::new());
}

#[tokio::test]
async fn a_resume_onto_another_endpoint_misses_as_the_expected_switch() {
    let bench = Bench::new("misses-resume-endpoint", false).await;
    let mut live = bench.start(Build::default());
    two_turns(&mut live, 1).await;
    finish(&bench, live).await;
    bench.api.lock().unwrap().cold = 1;
    let mut live = bench.resume(Build {
        gateway: true,
        ..Build::default()
    });
    two_turns(&mut live, 3).await;
    finish(&bench, live).await;
    assert_eq!(
        causes(&bench),
        vec![MissCause::Expected {
            reason: "endpoint_switch".into()
        }]
    );
}

async fn compacted_cold(case: &str, idle_minutes: usize) -> Bench {
    let bench = Bench::new(case, false).await;
    let build = Build {
        models: Some("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n"),
        stepped: true,
        ..Build::default()
    };
    let mut live = bench.start(build);
    two_turns(&mut live, 1).await;
    for _ in 0..idle_minutes {
        bench.steps.advance(jiff::SignedDuration::from_mins(1));
    }
    bench.api.lock().unwrap().cold = 1;
    live.compacted(1).await;
    finish(&bench, live).await;
    bench
}

#[tokio::test]
async fn a_compaction_past_the_ttl_is_ttl_expired() {
    let bench = compacted_cold("misses-compact-idle", 60).await;
    assert_eq!(causes(&bench), vec![MissCause::TtlExpired { gap_s: 3_600 }]);
}

#[tokio::test]
async fn a_compaction_inside_the_ttl_that_loses_the_cache_is_a_remote_loss() {
    let bench = compacted_cold("misses-compact-warm", 0).await;
    assert_eq!(causes(&bench), vec![MissCause::RemoteLoss]);
}

async fn compacted_natively(case: &str, idle_minutes: usize) -> Bench {
    let bench = Bench::new(case, false).await;
    let build = Build {
        stepped: true,
        ..Build::default()
    };
    let mut live = bench.start(build);
    two_turns(&mut live, 1).await;
    for _ in 0..idle_minutes {
        bench.steps.advance(jiff::SignedDuration::from_mins(1));
    }
    live.compacted(1).await;
    finish(&bench, live).await;
    bench
}

#[test]
fn a_native_compaction_past_the_ttl_is_ttl_expired() {
    crate::stack_alarm::on_alarm_stack(crate::stack_alarm::Timer::Real, || async {
        let bench = compacted_natively("misses-native-compact-idle", 60).await;
        assert_eq!(causes(&bench), vec![MissCause::TtlExpired { gap_s: 3_600 }]);
    });
}

#[test]
fn a_native_compaction_inside_the_ttl_that_loses_the_cache_is_a_remote_loss() {
    crate::stack_alarm::on_alarm_stack(crate::stack_alarm::Timer::Real, || async {
        let bench = compacted_natively("misses-native-compact-warm", 0).await;
        assert_eq!(causes(&bench), vec![MissCause::RemoteLoss]);
    });
}

#[tokio::test]
async fn a_switch_to_a_model_with_a_larger_minimum_still_names_the_lost_cache() {
    let bench = Bench::new("misses-switch-minimum", false).await;
    let mut live = bench.start(Build {
        model: "claude-sonnet-4-6",
        tools: &[],
        system: Some("Be brief."),
        ..Build::default()
    });
    live.completed("u1", "Look around.").await;
    switch_model(&mut live, 1, "claude-opus-4-6").await;
    live.completed("u2", "And again.").await;
    finish(&bench, live).await;
    let ok = settled(&bench.served());
    let switched = ok
        .iter()
        .position(|s| s.body.contains("claude-opus-4-6"))
        .unwrap();
    let before = ok[switched - 1].total;
    assert!((1_024..4_096).contains(&before), "{before}");
    assert_eq!(causes(&bench), vec![expected("model_switch")]);
}

#[tokio::test]
async fn search_results_another_account_cant_read_miss_once_as_expected_then_hit() {
    let (bench, mut live) = searched("misses-unreadable").await;
    live.completed("u2", &"word ".repeat(20_000)).await;
    bench.api.lock().unwrap().unreadable = true;
    let change = bench.settled();
    two_turns(&mut live, 3).await;
    live.completed("u5", "Once more.").await;
    let (_, seen) = finish_seen(&bench, live).await;
    assert!(!settled(&bench.served())[change].drops.is_empty());
    assert_eq!(
        adaptations(&seen),
        vec![wisp_protocol::HistoryChange::SearchesAsText {
            thinking_cut: false
        }]
    );
    assert_eq!(causes(&bench), vec![expected("search_history_unreadable")]);
    let marks: Vec<String> = pick!(
        log_events(&bench.store),
        LogEvent::RebuildMarked { reason, .. } => reason
    );
    assert_eq!(marks, ["search_history_unreadable"]);
    let refused = bench.served().iter().filter(|s| s.status == 400).count();
    assert_eq!(refused, 1);
    assert_settles(&bench, &[change]);
}

#[tokio::test]
async fn foreign_search_results_rendered_on_a_warm_cache_miss_once_as_expected() {
    let bench = Bench::new("misses-rendered", false).await;
    {
        let mut api = bench.api.lock().unwrap();
        api.iterations = true;
        api.search_once = true;
    }
    let build = Build {
        search: true,
        ..Build::default()
    };
    let both = vec![
        bench.connection("env", false),
        bench.connection("env2", false),
    ];
    let mut live = bench.start_with(build, both.clone());
    live.completed("u1", "Look it up.").await;
    switch_endpoint(&mut live, 1, "env2").await;
    live.completed("u2", &"word ".repeat(20_000)).await;
    live.completed("u3", "Warm.").await;
    bench.api.lock().unwrap().unreadable = true;
    let change = bench.settled();
    two_turns(&mut live, 4).await;
    live.completed("u6", "Once more.").await;
    finish(&bench, live).await;
    assert_eq!(causes(&bench), vec![expected("search_history_rendered")]);
    assert_eq!(
        marks(&bench).last().map(String::as_str),
        Some("search_history_rendered")
    );
    let refused = bench.served().iter().filter(|s| s.status == 400).count();
    assert_eq!(refused, 1);
    assert_settles(&bench, &[change]);
}

#[tokio::test]
async fn a_resume_then_unreadable_search_results_miss_once_as_expected() {
    let (bench, mut live) = searched("misses-resumed-unreadable").await;
    live.completed("u2", &"word ".repeat(20_000)).await;
    finish(&bench, live).await;
    let mut live = bench.resume(Build {
        search: true,
        ..Build::default()
    });
    bench.api.lock().unwrap().unreadable = true;
    let change = bench.settled();
    two_turns(&mut live, 3).await;
    live.completed("u5", "Once more.").await;
    finish(&bench, live).await;
    assert_eq!(causes(&bench), vec![expected("search_history_unreadable")]);
    assert_eq!(marks(&bench), ["search_history_unreadable"]);
    assert_settles(&bench, &[change]);
}

#[tokio::test]
async fn drops_after_a_conversion_under_binding_controls_are_noted_on_a_later_request() {
    let (bench, mut live) = searched("misses-unreadable-then-model").await;
    live.completed("u2", &"word ".repeat(20_000)).await;
    bench.api.lock().unwrap().unreadable = true;
    live.completed("u3", "Look around.").await;
    switch_model(&mut live, 1, SONNET).await;
    live.completed("u4", "And again.").await;
    let (_, seen) = finish_seen(&bench, live).await;
    let changes = adaptations(&seen);
    assert_eq!(
        changes.first(),
        Some(&wisp_protocol::HistoryChange::SearchesAsText {
            thinking_cut: false
        }),
        "{changes:?}"
    );
    assert!(
        matches!(
            &changes[1..],
            [wisp_protocol::HistoryChange::ThinkingDropped { reason: Some(r), .. }]
                if r == "model_binding_mismatch"
        ),
        "{changes:?}"
    );
}

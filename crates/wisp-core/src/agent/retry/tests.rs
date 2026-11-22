use wisp_protocol::{
    CacheTtl, FailurePhase, FailureSource, ImageType, RetryFocus, RetryOffer, Role,
};

use crate::pricing;

use super::*;
use crate::agent::testing::classified;
use crate::agent::tests::policy;

fn detail(retry: RetryClass, billed: bool) -> ErrorDetail {
    let mut d = ErrorDetail::new(FailurePhase::Stream, FailureSource::Endpoint, retry);
    d.possibly_billed = billed;
    d
}

fn state(step: u32, turn: u32, spent_usd: f64) -> RetryState {
    RetryState {
        step,
        turn,
        tries: step,
        busy_waited: Duration::ZERO,
        spent_usd,
    }
}

fn delay(decision: Decision) -> Duration {
    match decision {
        Decision::Retry { delay } => delay,
        Decision::GiveUp(reason) => panic!("gave up: {reason:?}"),
    }
}

#[test]
fn defaults_have_no_retry_bounds_or_budget() {
    let p = RetryPolicy::default();
    assert_eq!(p.step_retries, None);
    assert_eq!(p.turn_retries, None);
    assert_eq!(p.budget_usd, None);
    assert_eq!(p.busy_window, None);
    assert_eq!(p.max_retry_after, None);
    assert_eq!(p.base_delay, Duration::from_secs(1));
}

#[test]
fn unlimited_retries_back_off_to_five_minutes_and_honour_any_retry_after() {
    let policy = RetryPolicy {
        jitter: || 0.0,
        ..RetryPolicy::default()
    };
    for (tries, seconds) in [
        (0, 1),
        (1, 2),
        (2, 4),
        (3, 8),
        (4, 16),
        (5, 32),
        (6, 64),
        (7, 128),
        (8, 256),
        (9, 300),
        (100, 300),
        (u32::MAX, 300),
    ] {
        let state = RetryState {
            step: tries,
            turn: tries,
            tries,
            busy_waited: Duration::MAX,
            spent_usd: 0.0,
        };
        for busy in [false, true] {
            for (retry, expected) in [
                (RetryClass::Transient, Duration::from_secs(seconds)),
                (
                    RetryClass::After { ms: u64::MAX },
                    Duration::from_millis(u64::MAX),
                ),
            ] {
                for child in [false, true] {
                    assert_eq!(
                        delay(decide(
                            &policy,
                            &state,
                            &detail(retry, false),
                            busy,
                            None,
                            child
                        )),
                        expected,
                        "{tries} {busy} {child} {retry:?}",
                    );
                }
            }
        }
    }
    for child in [false, true] {
        for busy in [false, true] {
            for (ms, root_retries, child_retries) in [
                (240_000, true, true),
                (300_000, true, true),
                (300_001, true, false),
                (360_000, true, false),
                (7_200_000, true, false),
                (18_000_000, true, false),
                (18_000_001, false, false),
                (21_600_000, false, false),
                (604_800_000, false, false),
            ] {
                let mut d = detail(RetryClass::After { ms }, false);
                crate::provider::limit::reached(
                    &mut d,
                    1_800_000_000,
                    Some(1_800_000_000 + i64::try_from(ms.div_ceil(1000)).unwrap()),
                    wisp_protocol::plan_usage::UsageSource::RetryAfter,
                    None,
                );
                d.retry = RetryClass::After { ms };

                let retries = if child { child_retries } else { root_retries };
                let expected = if retries {
                    Decision::Retry {
                        delay: Duration::from_millis(ms),
                    }
                } else {
                    Decision::GiveUp(GaveUp::RetryAfterTooLong)
                };
                assert_eq!(
                    decide(&policy, &state(0, 0, 0.0), &d, busy, None, child),
                    expected,
                    "{child} {busy} {ms}"
                );
                d.retry = RetryClass::Never;
                d.limit_reached.as_mut().unwrap().resets_at = None;
                assert_eq!(
                    decide(&policy, &state(0, 0, 0.0), &d, busy, None, child),
                    Decision::GiveUp(GaveUp::NotRetryable)
                );
            }
        }
    }
}

#[test]
fn a_step_retries_five_times_then_gives_up() {
    let d = detail(RetryClass::Transient, true);
    let delays: Vec<Duration> = (0..5)
        .map(|n| delay(decide(&policy(), &state(n, n, 0.0), &d, false, None, false)))
        .collect();
    let secs: Vec<u64> = delays.iter().map(Duration::as_secs).collect();
    assert_eq!(secs, [1, 2, 4, 8, 16]);
    assert_eq!(
        decide(&policy(), &state(5, 5, 0.0), &d, false, None, false),
        Decision::GiveUp(GaveUp::StepLimit)
    );
}

#[test]
fn a_turn_retries_ten_times_across_its_steps() {
    let d = detail(RetryClass::Transient, false);
    assert!(matches!(
        decide(&policy(), &state(1, 9, 0.0), &d, false, None, false),
        Decision::Retry { .. }
    ));
    assert_eq!(
        decide(&policy(), &state(1, 10, 0.0), &d, false, None, false),
        Decision::GiveUp(GaveUp::TurnLimit)
    );
}

#[test]
fn never_is_never_retried() {
    let d = detail(RetryClass::Never, false);
    assert_eq!(
        decide(&policy(), &state(0, 0, 0.0), &d, false, None, false),
        Decision::GiveUp(GaveUp::NotRetryable)
    );
}

#[test]
fn without_a_budget_cost_never_stops_a_retry() {
    let d = detail(RetryClass::Transient, true);
    assert!(matches!(
        decide(
            &policy(),
            &state(0, 0, 1_000.0),
            &d,
            false,
            Some(500.0),
            false
        ),
        Decision::Retry { .. }
    ));
}

#[test]
fn an_opt_in_budget_binds_possibly_billed_failures() {
    let p = policy().with_budget(Some(0.10));
    let billed = detail(RetryClass::Transient, true);
    assert!(matches!(
        decide(&p, &state(0, 0, 0.04), &billed, false, Some(0.05), false),
        Decision::Retry { .. }
    ));
    assert_eq!(
        decide(&p, &state(1, 1, 0.08), &billed, false, Some(0.05), false),
        Decision::GiveUp(GaveUp::Budget)
    );
}

#[test]
fn an_unpriced_model_runs_on_counts_only() {
    let p = policy().with_budget(Some(0.0));
    let billed = detail(RetryClass::Transient, true);
    assert!(matches!(
        decide(&p, &state(0, 0, 0.0), &billed, false, None, false),
        Decision::Retry { .. }
    ));
}

#[test]
fn a_zero_budget_still_retries_what_cannot_have_been_billed() {
    let p = policy().with_budget(Some(0.0));
    let free = detail(RetryClass::Transient, false);
    assert!(matches!(
        decide(&p, &state(0, 0, 0.0), &free, false, Some(0.05), false),
        Decision::Retry { .. }
    ));
    let billed = detail(RetryClass::Transient, true);
    assert_eq!(
        decide(&p, &state(0, 0, 0.0), &billed, false, Some(0.05), false),
        Decision::GiveUp(GaveUp::Budget)
    );
}

#[test]
fn a_bad_budget_is_no_budget_and_a_negative_one_is_zero() {
    assert_eq!(policy().with_budget(Some(f64::NAN)).budget_usd, None);
    assert_eq!(policy().with_budget(Some(-1.0)).budget_usd, Some(0.0));
}

#[test]
fn retry_after_is_honoured_up_to_a_minute() {
    let after = |ms| detail(RetryClass::After { ms }, false);
    assert_eq!(
        delay(decide(
            &policy(),
            &state(0, 0, 0.0),
            &after(2_000),
            false,
            None,
            false
        )),
        Duration::from_secs(2)
    );
    assert_eq!(
        delay(decide(
            &policy(),
            &state(3, 3, 0.0),
            &after(2_000),
            false,
            None,
            false
        )),
        Duration::from_secs(8),
        "a shorter Retry-After never shortens the backoff"
    );
    assert_eq!(
        delay(decide(
            &policy(),
            &state(0, 0, 0.0),
            &after(60_000),
            false,
            None,
            false
        )),
        Duration::from_secs(60)
    );
    assert_eq!(
        decide(
            &policy(),
            &state(0, 0, 0.0),
            &after(60_001),
            false,
            None,
            false
        ),
        Decision::GiveUp(GaveUp::RetryAfterTooLong)
    );
}

fn busy(tries: u32, waited_secs: u64) -> RetryState {
    RetryState {
        step: 0,
        turn: 0,
        tries,
        busy_waited: Duration::from_secs(waited_secs),
        spent_usd: 0.0,
    }
}

fn refusal(kind: ErrorKind, retry: RetryClass) -> ProviderError {
    classified(kind, "refused", FailurePhase::Response, retry)
}

#[test]
fn busy_failures_retry_past_the_step_and_turn_limits() {
    let d = detail(RetryClass::Transient, false);
    let worn = RetryState {
        step: 5,
        turn: 10,
        tries: 15,
        busy_waited: Duration::from_secs(100),
        spent_usd: 0.0,
    };
    assert_eq!(
        delay(decide(&policy(), &worn, &d, true, None, false)),
        MAX_DELAY
    );
    assert_eq!(
        decide(&policy(), &worn, &d, false, None, false),
        Decision::GiveUp(GaveUp::StepLimit)
    );
}

#[test]
fn busy_waits_double_to_five_minutes_and_the_last_is_cut_to_the_window() {
    let d = detail(RetryClass::Transient, false);
    let mut waited = Duration::ZERO;
    let mut waits = Vec::new();
    for tries in 0..100 {
        let state = RetryState {
            busy_waited: waited,
            ..busy(tries, 0)
        };
        match decide(&policy(), &state, &d, true, None, false) {
            Decision::Retry { delay } => {
                waits.push(delay.as_secs());
                waited += delay;
            }
            Decision::GiveUp(reason) => {
                assert_eq!(reason, GaveUp::BusyWindow);
                break;
            }
        }
    }
    let expected = vec![1, 2, 4, 8, 16, 32, 64, 128, 256, 89];
    assert_eq!(waits, expected);
    assert_eq!(Some(waited), policy().busy_window);
}

#[test]
fn a_busy_retry_after_is_a_floor_honoured_up_to_what_is_left_of_the_window() {
    let after = |ms| detail(RetryClass::After { ms }, false);
    assert_eq!(
        delay(decide(
            &policy(),
            &busy(0, 0),
            &after(120_000),
            true,
            None,
            false
        )),
        Duration::from_secs(120)
    );
    assert_eq!(
        delay(decide(
            &policy(),
            &busy(3, 0),
            &after(2_000),
            true,
            None,
            false
        )),
        Duration::from_secs(8),
        "a shorter Retry-After never shortens the backoff"
    );
    assert_eq!(
        delay(decide(
            &policy(),
            &busy(5, 500),
            &after(100_000),
            true,
            None,
            false
        )),
        Duration::from_secs(100)
    );
    assert_eq!(
        decide(&policy(), &busy(5, 500), &after(100_001), true, None, false),
        Decision::GiveUp(GaveUp::BusyWindow)
    );
    assert_eq!(
        decide(&policy(), &busy(0, 0), &after(600_001), true, None, false),
        Decision::GiveUp(GaveUp::RetryAfterTooLong)
    );
    assert_eq!(
        decide(&policy(), &busy(0, 0), &after(600_001), false, None, false),
        Decision::GiveUp(GaveUp::RetryAfterTooLong)
    );
}

#[test]
fn only_an_unbilled_unrefused_rate_limit_or_overload_is_busy() {
    let p = policy();
    assert!(is_busy(
        &p,
        &refusal(ErrorKind::RateLimit, RetryClass::Transient),
        false
    ));
    assert!(is_busy(
        &p,
        &refusal(ErrorKind::Overloaded, RetryClass::After { ms: 3_000 }),
        false
    ));
    assert!(
        !is_busy(
            &p,
            &refusal(ErrorKind::Overloaded, RetryClass::Transient),
            true
        ),
        "a billed overload counts"
    );
    assert!(!is_busy(
        &p,
        &refusal(ErrorKind::RateLimit, RetryClass::Never),
        false
    ));
    assert!(!is_busy(
        &p,
        &refusal(ErrorKind::Server, RetryClass::Transient),
        false
    ));
    let unavailable = refusal(ErrorKind::Overloaded, RetryClass::Transient)
        .refusing(crate::provider::Refused::CompactionUnavailable);
    assert!(!is_busy(&p, &unavailable, false));
}

#[test]
fn a_zero_window_is_todays_behaviour() {
    let p = quick();
    assert_eq!(p.busy_window, Some(Duration::ZERO));
    let overloaded = refusal(ErrorKind::Overloaded, RetryClass::Transient);
    assert!(!is_busy(&p, &overloaded, false));
    assert_eq!(
        decide(
            &p,
            &state(5, 5, 0.0),
            &overloaded.detail,
            false,
            None,
            false
        ),
        Decision::GiveUp(GaveUp::StepLimit)
    );
    assert_eq!(RetryPolicy::default().busy_window, None);
}

#[test]
fn jitter_takes_off_at_most_a_fifth() {
    for (jitter, retry, expected_ms) in [
        ((|| 0.0) as fn() -> f64, 1, 1_000),
        (|| 1.0, 1, 800),
        (|| 0.5, 3, 3_600),
        (|| 7.0, 1, 800),
        (|| -3.0, 1, 1_000),
        (|| f64::NAN, 2, 2_000),
    ] {
        let p = RetryPolicy {
            jitter,
            ..RetryPolicy::default()
        };
        assert_eq!(
            backoff(&p, retry, None),
            Duration::from_millis(expected_ms),
            "retry {retry}"
        );
    }
    for _ in 0..50 {
        let d = backoff(&RetryPolicy::default(), 1, None);
        assert!(d <= Duration::from_secs(1) && d >= Duration::from_millis(800));
    }
}

#[test]
fn a_raised_limit_is_capped_at_five_minutes() {
    assert_eq!(backoff(&policy(), 10, None), MAX_DELAY);
    assert_eq!(backoff(&policy(), u32::MAX, None), MAX_DELAY);
}

#[test]
fn a_first_request_counts_bytes_as_tokens_and_images_at_the_largest_tier() {
    let image = || ContentBlock::image(ImageType::Png, "a".repeat(100_000));
    let messages = [Message {
        role: Role::User,
        content: vec![
            ContentBlock::Text { text: "hi".into() },
            image(),
            ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: vec![image()],
                is_error: false,
            },
        ],
    }];
    let text = r#"{"type":"text","text":"hi"}"#.len() as u64;
    let image = u64::from(Tier::HIGH.max_tokens);
    assert_eq!(
        request_tokens(Some("abc"), None, &[], &[], &[], &messages),
        3 + 2 + text + image + 2 + image
    );
    assert_eq!(request_tokens(None, None, &[], &[], &[], &[]), 2);
    let block = "<instructions>\nrules\n</instructions>";
    assert_eq!(
        request_tokens(Some("abc"), Some(block), &[], &[], &[], &messages),
        request_tokens(Some("abc"), None, &[], &[], &[], &messages) + block.len() as u64,
        "the project block counts toward a failed request's size"
    );
}

#[test]
fn a_file_counts_like_an_image_in_a_failed_request_s_size() {
    let pdf = "A".repeat(10_000_000);
    let media = ContentBlock::media("application/pdf".parse().unwrap(), pdf);
    let messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: vec![media],
            is_error: false,
        }],
    }];
    assert_eq!(
        request_tokens(None, None, &[], &[], &[], &messages),
        2 + 2 + u64::from(Tier::HIGH.max_tokens)
    );
}

#[test]
fn a_first_request_is_priced_at_the_one_hour_write_rate() {
    let info = pricing::model_info("claude-opus-5-5").unwrap();
    let usage = first_request_usage(1_000_000);
    for ttl in [CacheTtl::FiveMinutes, CacheTtl::OneHour] {
        let usd = pricing::estimate_usd(info, &usage, ttl);
        assert!(
            (usd - info.rates.input_per_mtok * 2.0).abs() < 1e-9,
            "{usd}"
        );
    }
}

#[test]
fn a_failed_first_request_counts_the_server_tool() {
    let tools = vec![ToolSpec {
        description: "run".into(),
        input_schema: serde_json::json!({"type": "object"}),
        name: "bash".into(),
    }];
    let messages = [wisp_protocol::Message::user_text("hi")];
    let server = crate::agent::testing::search_tool("web_search_20250305");
    let with = request_tokens(Some("sys"), None, &tools, &[], server, &messages);
    let without = request_tokens(Some("sys"), None, &tools, &[], &[], &messages);
    let merged = crate::tool::tools_json(&tools, server).len() as u64;
    let alone = crate::tool::tools_json(&tools, &[]).len() as u64;
    assert!(with > without);
    assert_eq!(with - without, merged - alone);
}

fn endpoints(others: bool, suggest: Option<&str>) -> Endpoints {
    Endpoints {
        others,
        suggest: suggest.map(str::to_owned),
    }
}

fn offered(kind: ErrorKind, detail: Option<&ErrorDetail>, two: bool) -> Option<RetryOffer> {
    offer(kind, detail, None, &endpoints(two, None))
}

fn both(focus: RetryFocus) -> Option<RetryOffer> {
    Some(RetryOffer {
        retry: true,
        switch: true,
        focus,
        suggest: None,
        retries_at: None,
    })
}

fn retry_only() -> Option<RetryOffer> {
    Some(RetryOffer {
        retry: true,
        switch: false,
        focus: RetryFocus::Retry,
        suggest: None,
        retries_at: None,
    })
}

fn switch_only() -> Option<RetryOffer> {
    Some(RetryOffer {
        retry: false,
        switch: true,
        focus: RetryFocus::Switch,
        suggest: None,
        retries_at: None,
    })
}

#[test]
fn every_failure_that_can_succeed_again_offers_retry_and_switch_focused_on_retry() {
    let d = detail(RetryClass::Transient, true);
    for kind in [
        ErrorKind::Server,
        ErrorKind::Overloaded,
        ErrorKind::Network,
        ErrorKind::Timeout,
        ErrorKind::InvalidResponse,
        ErrorKind::Other,
        ErrorKind::Interrupted,
        ErrorKind::RateLimit,
        ErrorKind::Auth,
    ] {
        assert_eq!(
            offered(kind, Some(&d), true),
            both(RetryFocus::Retry),
            "{kind:?}"
        );
        assert_eq!(offered(kind, Some(&d), false), retry_only(), "{kind:?}");
    }
}

#[test]
fn a_long_rate_limit_wait_and_billing_focus_switch_endpoint() {
    let d = detail(RetryClass::Transient, false);
    let long = Some(GaveUp::RetryAfterTooLong);
    let two = endpoints(true, None);
    let one = endpoints(false, None);
    assert_eq!(
        offer(ErrorKind::RateLimit, Some(&d), long, &two),
        both(RetryFocus::Switch)
    );
    assert_eq!(
        offer(ErrorKind::RateLimit, Some(&d), long, &one),
        retry_only()
    );
    assert_eq!(
        offer(
            ErrorKind::RateLimit,
            Some(&d),
            Some(GaveUp::StepLimit),
            &two
        ),
        both(RetryFocus::Retry)
    );
    assert_eq!(
        offered(ErrorKind::Billing, Some(&d), true),
        both(RetryFocus::Switch)
    );
    assert_eq!(offered(ErrorKind::Billing, Some(&d), false), retry_only());
}

#[test]
fn switch_only_kinds_offer_nothing_with_one_endpoint() {
    let endpoint = detail(RetryClass::Never, false);
    let mut local = ErrorDetail::new(
        FailurePhase::Connect,
        FailureSource::Local,
        RetryClass::Never,
    );
    local.possibly_billed = false;
    for (kind, detail) in [
        (ErrorKind::ThinkingBinding, Some(&endpoint)),
        (ErrorKind::InvalidRequest, Some(&endpoint)),
        (ErrorKind::InvalidRequest, Some(&local)),
    ] {
        assert_eq!(offered(kind, detail, true), switch_only(), "{kind:?}");
        assert_eq!(offered(kind, detail, false), None, "{kind:?}");
    }
}

#[test]
fn overflow_refusal_and_a_local_layout_refusal_offer_nothing_even_with_two_endpoints() {
    let d = detail(RetryClass::Never, true);
    assert_eq!(offered(ErrorKind::ContextOverflow, Some(&d), true), None);
    assert_eq!(offered(ErrorKind::Refusal, Some(&d), true), None);
    assert_eq!(offered(ErrorKind::Refusal, None, true), None);
    assert_eq!(offered(ErrorKind::InvalidRequest, None, true), None);
}

#[test]
fn a_binding_failure_suggests_the_endpoint_a_turn_last_completed_on() {
    let d = detail(RetryClass::Never, false);
    let moved = endpoints(true, Some("direct"));
    let Some(binding) = offer(ErrorKind::ThinkingBinding, Some(&d), None, &moved) else {
        panic!("a binding failure with two endpoints offers a switch");
    };
    assert_eq!(binding.suggest.as_deref(), Some("direct"));
    assert_eq!(binding.focus, RetryFocus::Switch);
    assert!(!binding.retry);
    let Some(server) = offer(ErrorKind::Server, Some(&d), None, &moved) else {
        panic!("a server error offers a choice");
    };
    assert_eq!(server.suggest, None, "only a binding failure suggests one");
    let stayed = offer(
        ErrorKind::ThinkingBinding,
        Some(&d),
        None,
        &endpoints(true, None),
    );
    assert_eq!(stayed, switch_only());
}

#[test]
fn the_offer_allows_the_same_endpoint_only_for_retry() {
    let Some(binding) = switch_only() else {
        panic!("built above");
    };
    assert!(!binding.allows(true));
    assert!(binding.allows(false));
    let Some(lone) = retry_only() else {
        panic!("built above");
    };
    assert!(lone.allows(true));
    assert!(!lone.allows(false));
}

#[test]
fn a_failed_request_counts_the_deferred_entries_with_their_flags() {
    let spec = |name: &str| ToolSpec {
        description: "run".into(),
        input_schema: serde_json::json!({"type": "object"}),
        name: name.into(),
    };
    let tools = vec![spec("bash")];
    let deferred = vec![spec("show_diff")];
    let messages = [wisp_protocol::Message::user_text("hi")];
    let with = request_tokens(Some("sys"), None, &tools, &deferred, &[], &messages);
    let without = request_tokens(Some("sys"), None, &tools, &[], &[], &messages);
    let sent = crate::tool::array_json(&tools, &deferred, &[]);
    assert!(sent.contains(r#""defer_loading":true"#), "{sent}");
    assert_eq!(
        with - without,
        sent.len() as u64 - crate::tool::tools_json(&tools, &[]).len() as u64
    );
}

#[test]
fn invalid_response_caps_step_retries_even_under_default_policy() {
    let base = RetryPolicy {
        jitter: || 0.0,
        ..RetryPolicy::default()
    };
    let typed = |kind: ErrorKind, retry: RetryClass, malformed_call: bool| {
        let mut error =
            ProviderError::classified(kind, "err", detail(retry, retry != RetryClass::Never));
        error.malformed_call = malformed_call;
        error
    };
    let malformed = typed(ErrorKind::InvalidResponse, RetryClass::Transient, true);
    let plain = typed(ErrorKind::InvalidResponse, RetryClass::Transient, false);
    for (malformed_call, cap, expected_waits) in [
        (
            true,
            MALFORMED_CALL_RETRIES,
            &[1u64, 2, 4, 8, 16, 32, 64, 128][..],
        ),
        (false, INVALID_RESPONSE_RETRIES, &[1, 2, 4][..]),
    ] {
        let error = typed(
            ErrorKind::InvalidResponse,
            RetryClass::Transient,
            malformed_call,
        );
        let capped = base.for_kind(&error);
        assert_eq!(capped.step_retries, Some(cap), "malformed={malformed_call}");
        for child in [false, true] {
            let waits: Vec<_> = (0..cap)
                .map(|n| {
                    delay(decide(
                        &capped,
                        &state(n, n, 0.0),
                        &error.detail,
                        false,
                        None,
                        child,
                    ))
                })
                .collect();
            let expected: Vec<_> = expected_waits
                .iter()
                .copied()
                .map(Duration::from_secs)
                .collect();
            assert_eq!(waits, expected, "malformed={malformed_call} child={child}");
            assert_eq!(
                decide(
                    &capped,
                    &state(cap, cap, 0.0),
                    &error.detail,
                    false,
                    None,
                    child,
                ),
                Decision::GiveUp(GaveUp::StepLimit),
                "malformed={malformed_call} child={child}"
            );
        }
        let loose = RetryPolicy {
            step_retries: Some(cap + 4),
            ..base
        }
        .for_kind(&error);
        assert_eq!(loose.step_retries, Some(cap), "malformed={malformed_call}");
    }
    for tighter in [0, 1] {
        for error in [&plain, &malformed] {
            let narrow = RetryPolicy {
                step_retries: Some(tighter),
                ..base
            }
            .for_kind(error);
            assert_eq!(narrow.step_retries, Some(tighter));
        }
    }
    assert_eq!(
        policy().for_kind(&plain).step_retries,
        Some(INVALID_RESPONSE_RETRIES)
    );
    assert_eq!(
        policy().for_kind(&malformed).step_retries,
        policy().step_retries
    );
    assert_eq!(
        RetryPolicy::default()
            .for_fallback()
            .for_kind(&plain)
            .step_retries,
        Some(INVALID_RESPONSE_RETRIES)
    );
    assert_eq!(
        RetryPolicy::default()
            .for_fallback()
            .for_kind(&malformed)
            .step_retries,
        RetryPolicy::default().for_fallback().step_retries
    );
    for malformed_call in [false, true] {
        let never = typed(
            ErrorKind::InvalidResponse,
            RetryClass::Never,
            malformed_call,
        );
        let capped = base.for_kind(&never);
        assert_eq!(
            decide(
                &capped,
                &state(0, 0, 0.0),
                &never.detail,
                false,
                None,
                false
            ),
            Decision::GiveUp(GaveUp::NotRetryable)
        );
    }
    for kind in [
        ErrorKind::Server,
        ErrorKind::Network,
        ErrorKind::Timeout,
        ErrorKind::Overloaded,
        ErrorKind::RateLimit,
    ] {
        for malformed_call in [false, true] {
            let error = typed(kind, RetryClass::Transient, malformed_call);
            assert_eq!(base.for_kind(&error).step_retries, None, "{kind:?}");
        }
    }
}

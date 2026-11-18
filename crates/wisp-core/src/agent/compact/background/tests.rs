use super::*;

#[test]
fn the_band_needs_both_bounds() {
    assert!(!starts_band(59_000, 100_000, 15_000, 40_000));
    assert!(starts_band(60_000, 100_000, 15_000, 40_000));
    assert!(!starts_band(60_000, 100_000, 25_000, 40_000));
    assert!(!starts_band(50_000, 104_000 - 64_000, 15_000, 40_000));
    assert!(starts_band(55_000, 40_000, 15_000, 40_000));
}

#[test]
fn every_discard_reason_has_a_note_and_names_its_cost() {
    for reason in [
        DiscardReason::ViewChanged,
        DiscardReason::Bound,
        DiscardReason::Superseded,
        DiscardReason::Cancelled,
        DiscardReason::Resent,
        DiscardReason::Unsaved,
        DiscardReason::Refused,
        DiscardReason::SessionEnded,
    ] {
        assert_eq!(DiscardReason::parse(reason.as_str()), Some(reason));
        assert!(
            discard_message(reason, None, Default::default(), None).starts_with("Wisp discarded")
        );
        assert!(discard_message(reason, Some(0.28), Default::default(), None).contains("$0.28"));
    }
}

#[test]
fn discarded_plan_summary_labels_the_equivalent() {
    let text = discard_message(
        DiscardReason::Cancelled,
        Some(0.28),
        wisp_protocol::Billing::AnthropicPlan,
        Some(0.28),
    );
    assert!(text.ends_with("(it cost ~$0.28)."), "{text}");
}

#[test]
fn discarded_mixed_summary_keeps_spend_and_coverage_separate() {
    let text = discard_message(
        DiscardReason::Cancelled,
        Some(16.4),
        wisp_protocol::Billing::Mixed,
        Some(4.1),
    );
    assert!(text.contains("$12.30 · ~$4.10 subscriptions"), "{text}");
    assert!(!text.contains("16.4"), "{text}");
    let unknown = discard_message(
        DiscardReason::Cancelled,
        Some(16.4),
        wisp_protocol::Billing::Mixed,
        None,
    );
    assert!(unknown.contains("split unavailable"), "{unknown}");
    assert!(!unknown.contains('$'), "{unknown}");
}

#[test]
fn aggregated_compaction_costs_do_not_invent_a_missing_mixed_split() {
    let cost = |billing, plan| {
        serde_json::from_value::<Cost>(serde_json::json!({
            "turn_usd": 4.1, "session_usd": 8.2, "turn_billing": billing, "turn_plan_usd": plan
        }))
        .unwrap()
    };
    let metered = cost("metered", None);
    let known = cost("mixed", Some(1.0));
    assert_eq!(
        plus_cost(Some(metered), Some(known)).unwrap().turn_plan_usd,
        Some(1.0)
    );
    let unknown = cost("mixed", None);
    assert_eq!(
        plus_cost(Some(unknown), Some(known)).unwrap().turn_plan_usd,
        None
    );
}

#[test]
fn the_discard_notice_marks_a_subscription_estimate_with_a_tilde() {
    use wisp_protocol::{Billing, DiscardReason};
    assert_eq!(
        discard_message(
            DiscardReason::Cancelled,
            Some(0.12),
            Billing::AnthropicPlan,
            None
        ),
        "Wisp discarded the background summary: the turn was cancelled (it cost ~$0.12)."
    );
    assert_eq!(
        discard_message(DiscardReason::Cancelled, Some(0.12), Billing::Metered, None),
        "Wisp discarded the background summary: the turn was cancelled (it cost $0.12)."
    );
    assert_eq!(
        discard_message(
            DiscardReason::SessionEnded,
            Some(0.13),
            Billing::Mixed,
            Some(0.03)
        ),
        "Wisp discarded the background summary: the session ended (it cost $0.10 · ~$0.03 subscriptions)."
    );
}

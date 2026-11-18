use super::*;
use wisp_protocol::log::Cut;

fn input(text: &str, images: usize) -> MarkInput {
    MarkInput {
        id: InputId("tui-1".into()),
        text: text.into(),
        images,
        noted: false,
        shas: vec!["ab".repeat(32); images],
        from: None,
    }
}

#[test]
fn a_label_is_the_first_non_blank_line_of_the_first_message() {
    assert_eq!(
        label(&[input("\n  hello  \nthere", 0)], None, None),
        "hello"
    );
    assert_eq!(label(&[input("", 1)], None, None), "[image]");
    assert_eq!(label(&[], None, None), "");
}

#[test]
fn a_turn_without_a_message_is_labelled_by_its_origin() {
    assert_eq!(label(&[], None, Some(3)), "\u{21bb} Retried turn 3");
    assert_eq!(
        label(&[], Some(TurnOrigin::ProcessExit), None),
        "\u{21bb} a background process ended"
    );
    assert_eq!(label(&[input("again", 0)], None, Some(3)), "again");
}

#[test]
fn only_a_warm_point_keeps_the_cache() {
    assert_eq!(cache_of(&Warmth::Warm), CacheEffect::Kept);
    let rebuilt = |reason| CacheEffect::Rebuilt { reason };
    assert_eq!(
        cache_of(&Warmth::MayHaveExpired),
        rebuilt(RebuildReason::Rewind)
    );
    assert_eq!(
        cache_of(&Warmth::PrefixChanged),
        rebuilt(RebuildReason::Rewind)
    );
    assert_eq!(
        cache_of(&Warmth::OtherEndpoint { name: "b".into() }),
        rebuilt(RebuildReason::EndpointSwitch)
    );
    assert_eq!(
        cache_of(&Warmth::OtherModel { model: "m".into() }),
        rebuilt(RebuildReason::ModelSwitch)
    );
    assert_eq!(
        cache_of(&Warmth::OtherEffort {
            effort: wisp_protocol::Effort::High
        }),
        rebuilt(RebuildReason::EffortChange)
    );
}

#[test]
fn a_point_stays_warm_for_its_requests_cache_lifetime() {
    let at: Timestamp = "2026-10-25T12:00:00Z".parse().unwrap();
    assert_eq!(
        ends_at(Some(at), Some(CacheTtl::FiveMinutes)),
        Some("2026-10-25T12:05:00Z".parse().unwrap())
    );
    assert_eq!(
        ends_at(Some(at), Some(CacheTtl::OneHour)),
        Some("2026-10-25T13:00:00Z".parse().unwrap())
    );
    assert_eq!(ends_at(None, Some(CacheTtl::OneHour)), None);
    assert_eq!(ends_at(Some(at), None), None);
}

#[test]
fn quotes_are_rounded_to_whole_micros_half_up() {
    assert_eq!(pricing::micros(0.0000015), 2);
    assert_eq!(pricing::micros(0.0000025), 3);
    assert_eq!(pricing::micros(1.25), 1_250_000);
}

fn cut_at(message: usize, through: usize, tools: bool) -> Cut {
    Cut {
        from: BlockPosition { message, block: 0 },
        through,
        tools,
    }
}

#[test]
fn a_rewind_keeps_only_the_tools_cuts_made_later_inside_the_kept_messages_clamped_to_them() {
    let mut search = super::super::search::SearchState {
        cuts: vec![
            cut_at(0, 1, false),
            cut_at(1, 9, true),
            cut_at(1, 9, false),
            cut_at(7, 9, true),
        ],
        ..Default::default()
    };
    search.rewound_to(4, 1, (Closed::default(), None));
    assert_eq!(search.cuts, [cut_at(0, 1, false), cut_at(1, 3, true)]);
}

#[test]
fn a_rewind_keeps_a_later_cut_of_the_whole_history_clamped_to_the_kept_messages() {
    let mut search = super::super::search::SearchState {
        cuts: vec![cut_at(1, 9, true), cut_at(0, 9, false), cut_at(2, 9, false)],
        ..Default::default()
    };
    search.rewound_to(4, 0, (Closed::default(), None));
    assert_eq!(search.cuts, [cut_at(1, 3, true), cut_at(0, 3, false)]);
}

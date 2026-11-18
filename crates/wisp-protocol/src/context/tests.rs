use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::*;

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(
    wire: Value,
    expected: &T,
) {
    let read: T = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(&read, expected);
    assert_eq!(serde_json::to_value(&read).unwrap(), wire);
}

#[test]
fn a_token_point_grows_the_window_up_to_the_ceiling() {
    let base = 272_000;
    let max = Some(872_000);
    let set = |point| Some(TokensOrPercent::Tokens(point));
    for (point, window) in [(150_000, base), (400_000, 470_589), (900_000, 872_000)] {
        assert_eq!(window_for(base, max, 64_000, set(point)), window);
    }
    let share = Some(TokensOrPercent::Share(settings::Percent::new(90).unwrap()));
    assert_eq!(window_for(base, max, 64_000, share), base);
    assert_eq!(window_for(base, max, 64_000, None), base);
    assert_eq!(window_for(base, None, 64_000, set(400_000)), base);
    assert_eq!(window_for(base, Some(200_000), 64_000, set(400_000)), base);
}

#[test]
fn unit_enums_keep_an_unknown_value() {
    round_trip(json!("shipped"), &WindowSource::Shipped);
    round_trip(json!("subscription"), &WindowSource::Subscription);
    round_trip(json!("api"), &WindowSource::Api);
    round_trip(
        json!("models_api"),
        &WindowSource::Other("models_api".into()),
    );
    round_trip(json!("default"), &PointSource::Default);
    round_trip(json!("mode"), &PointSource::Other("mode".into()));
    round_trip(json!("user_messages"), &Category::UserMessages);
    round_trip(json!("memory"), &Category::Other("memory".into()));
    round_trip(json!("all"), &ReportDetail::All);
    round_trip(json!("by_turn"), &ReportDetail::Other("by_turn".into()));
    round_trip(json!("split"), &FigureSource::Split);
    round_trip(json!("measured"), &FigureSource::Measured);
    round_trip(json!("guessed"), &FigureSource::Other("guessed".into()));
}

#[test]
fn only_counted_and_measured_figures_are_unmarked() {
    assert!(!FigureSource::Counted.approximate());
    assert!(!FigureSource::Measured.approximate());
    assert!(FigureSource::Split.approximate());
    assert!(FigureSource::Estimated.approximate());
    assert!(FigureSource::Other("guessed".into()).approximate());
}

#[test]
fn an_older_report_loads_without_a_source_endpoint_or_basis_and_drops_old_fields() {
    let old = json!({
        "answers": 3,
        "fill": "none",
        "next": 10,
        "auto_compact": false,
        "compact_at": {"tokens": 208_000, "set_by": "config"},
        "categories": [{"category": "tools", "tokens": 10}],
        "not_attributed": -400,
        "endpoint_count": "pending",
        "epoch": 0
    });
    let mut report: ContextReport = serde_json::from_value(old).unwrap();
    assert_eq!(report.endpoint, "");
    assert_eq!(report.categories[0].source, FigureSource::Estimated);
    assert_eq!(
        report.basis,
        ReportBasis::Estimated {
            reason: EstimateReason::NoCounter
        }
    );
    assert!(report.basis.settled());
    assert!(!ReportBasis::Counting.settled());
    let back = serde_json::to_value(&report).unwrap();
    assert!(back.get("not_attributed").is_none(), "{back}");
    assert!(back.get("endpoint").is_none(), "{back}");
    assert!(back.get("endpoint_count").is_none(), "{back}");
    let point = report.compact_at.as_mut().unwrap();
    assert_eq!(point.capped_from, None);
    assert!(back["compact_at"].get("capped_from").is_none(), "{back}");
    point.capped_from = Some(400_000);
    let mut capped = back;
    capped["compact_at"]["capped_from"] = json!(400_000);
    round_trip(capped, &report);
}

#[test]
fn data_enums_keep_an_unknown_value() {
    round_trip(
        json!({"measured": {"used": 62_400, "turn": 9, "step": 4}}),
        &Fill::Measured {
            used: 62_400,
            turn: 9,
            step: 4,
        },
    );
    round_trip(
        json!({"estimated_after_compaction": {"tokens": 14_000, "epoch": 1}}),
        &Fill::EstimatedAfterCompaction {
            tokens: 14_000,
            epoch: 1,
        },
    );
    round_trip(json!("none"), &Fill::None);
    let later = json!({"counted_elsewhere": {"tokens": 3}});
    round_trip(later.clone(), &Fill::Other(later));
    round_trip(json!("counted"), &ReportBasis::Counted);
    round_trip(json!("counting"), &ReportBasis::Counting);
    round_trip(
        json!({"partly_estimated": {"reason": "timed out"}}),
        &ReportBasis::PartlyEstimated {
            reason: "timed out".into(),
        },
    );
    for (wire, reason) in [
        (json!("no_counter"), EstimateReason::NoCounter),
        (json!("off"), EstimateReason::Off),
        (json!("nothing_yet"), EstimateReason::NothingYet),
        (
            json!({"failed": "HTTP 500"}),
            EstimateReason::Failed("HTTP 500".into()),
        ),
        (json!("replay"), EstimateReason::Other("replay".into())),
    ] {
        round_trip(
            json!({"estimated": {"reason": wire}}),
            &ReportBasis::Estimated { reason },
        );
    }
    let later = json!({"sampled": {"of": 2}});
    round_trip(later.clone(), &ReportBasis::Other(later));
    let odd = json!({"estimated": {"reason": {"guessed": 1}}});
    round_trip(odd.clone(), &ReportBasis::Other(odd));
}

#[test]
fn categories_come_in_the_report_order() {
    let names: Vec<Value> = Category::ORDER
        .iter()
        .map(|c| serde_json::to_value(c).unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            json!("system_prompt"),
            json!("tools"),
            json!("instructions"),
            json!("environment"),
            json!("summary"),
            json!("user_messages"),
            json!("replies"),
            json!("tool_results"),
            json!("attachments"),
        ]
    );
}

#[test]
fn the_attachments_row_still_reads_its_old_name() {
    assert_eq!(
        serde_json::from_value::<Category>(json!("images")).unwrap(),
        Category::Attachments
    );
    assert_eq!(
        serde_json::to_value(Category::Attachments).unwrap(),
        json!("attachments")
    );
}

#[test]
fn the_unknown_window_message_names_the_key() {
    assert_eq!(
        unknown_window("my-model"),
        "Window unknown for my-model: set context_window under [models.\"my-model\"] in config.toml to enable automatic compaction."
    );
}

#[test]
fn the_unknown_window_message_never_names_a_key_given_as_the_model() {
    assert_eq!(
        unknown_window("sk-ant-api03-CANARYMODEL0000"),
        "Window unknown for the model (looks like a key, not shown), so automatic compaction is off."
    );
    assert_eq!(
        unknown_window("claude-x\u{1b}[2J"),
        "Window unknown for the model (not a model ID, not shown), so automatic compaction is off."
    );
}

#[test]
fn tokens_are_formatted_in_whole_thousands_and_tenths_of_a_million() {
    for (n, text) in [
        (0, "0"),
        (400, "400"),
        (999, "999"),
        (1_000, "1k"),
        (62_499, "62k"),
        (62_500, "63k"),
        (999_499, "999k"),
        (999_500, "1M"),
        (999_600, "1M"),
        (1_000_000, "1M"),
        (1_049_999, "1M"),
        (1_050_000, "1.1M"),
        (1_250_000, "1.3M"),
        (u64::MAX, "18446744073709.6M"),
    ] {
        assert_eq!(format_tokens(n), text, "{n}");
    }
}

#[test]
fn money_has_three_significant_digits_and_at_least_two_decimals() {
    for (usd, text) in [
        (0.0, "$0.00"),
        (0.00042, "$0.00042"),
        (0.0042, "$0.0042"),
        (0.005, "$0.005"),
        (0.012, "$0.012"),
        (0.014, "$0.014"),
        (0.0999, "$0.0999"),
        (0.21, "$0.21"),
        (0.6, "$0.60"),
        (0.6049, "$0.605"),
        (1.234, "$1.23"),
        (12.345, "$12.35"),
        (12.5, "$12.50"),
        (1234.5, "$1234.50"),
        (-1.0, "$0.00"),
        (f64::NAN, "$0.00"),
        (f64::INFINITY, "$0.00"),
    ] {
        assert_eq!(format_usd(usd), text, "{usd}");
    }
}

#[test]
fn the_limit_is_the_point_while_compaction_is_on_and_the_ceiling_while_it_is_off() {
    let (point, ceiling) = (Some(400_000), Some(850_000));
    assert_eq!(limit_of(point, ceiling, true), point);
    assert_eq!(limit_of(point, ceiling, false), ceiling);
    assert_eq!(limit_of(None, ceiling, true), ceiling);
    assert_eq!(limit_of(None, ceiling, false), ceiling);
    for auto_compact in [true, false] {
        assert_eq!(limit_of(None, None, auto_compact), None, "{auto_compact}");
    }
    assert_eq!(limit_of(point, None, true), point);
    assert_eq!(limit_of(point, None, false), None);
}

#[test]
fn an_amount_is_marked_tilde_for_an_estimate_and_plus_for_a_lower_bound() {
    for (estimate, lower, lead, tail, text) in [
        (false, false, "", "", "$0.129"),
        (true, false, "~", "", "~$0.129"),
        (false, true, "", "+", "$0.129+"),
        (true, true, "~", "+", "~$0.129+"),
    ] {
        let marked = marked_amount("$0.129", estimate, lower);
        assert_eq!(
            marked,
            Marked {
                lead,
                amount: "$0.129".to_owned(),
                tail
            }
        );
        assert_eq!(marked.to_string(), text);
    }
}

#[test]
fn every_category_belongs_to_one_part() {
    let parts: Vec<FillPart> = Category::ORDER.iter().map(Category::part).collect();
    assert_eq!(
        parts,
        [
            FillPart::System,
            FillPart::Tools,
            FillPart::System,
            FillPart::Chat,
            FillPart::Chat,
            FillPart::Chat,
            FillPart::Chat,
            FillPart::Chat,
            FillPart::Chat,
        ]
    );
    assert_eq!(Category::Other("memory".into()).part(), FillPart::Chat);
}

#[test]
fn parts_keep_an_unknown_value() {
    round_trip(json!("skills"), &FillPart::Skills);
    let read: FillPart = serde_json::from_value(json!("memory")).unwrap();
    assert_eq!(read, FillPart::Unknown);
}

#[test]
fn cache_shares_round_down_over_the_prompt() {
    assert_eq!(cache_percent(0, 0), None);
    assert_eq!(cache_percent(0, 100), Some(0));
    assert_eq!(cache_percent(1, 3), Some(33));
    assert_eq!(cache_percent(29, 100), Some(29));
    assert_eq!(cache_percent(999, 1_000), Some(99));
    assert_eq!(cache_percent(1_000, 1_000), Some(100));
    assert_eq!(cache_percent(150, 100), Some(100));
}

use wisp_protocol::{format_usd, marked_amount};

#[test]
fn tool_text_marks_amounts_as_the_formatter_does() {
    for (estimate, lower) in [(false, false), (true, false), (false, true), (true, true)] {
        assert_eq!(
            wisp_tool_text::agents::marked("$0.802", estimate, lower),
            marked_amount("$0.802", estimate, lower).to_string()
        );
    }
    for micros in [
        0,
        420,
        4_200,
        5_000,
        12_000,
        14_000,
        600_000,
        604_900,
        1_234_000,
        12_500_000,
        1_234_500_000,
    ] {
        assert_eq!(
            wisp_tool_text::agents::usd(micros),
            format_usd(micros as f64 / 1_000_000.0),
            "{micros}"
        );
    }
    assert_eq!(
        wisp_tool_text::agents::MIXED_BILLING_NOTE,
        wisp_protocol::MIXED_BILLING_NOTE
    );
    assert_eq!(
        wisp_tool_text::agents::SUBSCRIPTION_NOTE,
        wisp_protocol::SUBSCRIPTION_WORD
    );
}
